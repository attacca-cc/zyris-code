//! 에이전트가 파일을 고치는 **유일한** 길.
//!
//! `file_io`의 `write`·`remove`·`mkdir`은 내주지 않는다(`tools::readonly`). 파일을 바꾸는
//! 길이 둘이면 에이전트가 통째로 덮어쓰기를 골라 diff가 파일 전체로 번지고, 승인 게이트를
//! 두 곳에 걸어야 한다. 여기가 독점이라 **모든 변경이 한 승인 게이트와 한 diff 화면을 지난다.**
//!
//! **파일을 지우는 도구는 아예 없다.** 지우는 것은 사람이 한다.
//!
//! 메서드의 doc 주석만 와이어로 나가 에이전트가 읽는 설명이 된다. **그래서 주석이 곧 규약이다.**
//!
//! 여러 세션이 한 저장소를 동시에 고치면 서로 모르는 사이에 파일을 덮을 수 있다. 그 싸움은
//! `base_version`으로 막는다 — 읽은 시점의 버전 토큰을 편집에 실어 보내면 파일이 그 사이
//! 바뀌었을 때 조용히 덮지 않고 실패한다. 토큰은 `file_io.read` 응답의
//! `stat.modified_unix_ms:stat.size`, 또는 `code_edit.version`의 `version`에서 얻는다.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use zyris::WireError;
use zyris_capkit::resolve_under;

use crate::tools::diff::diff;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EditSpec {
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EditResult {
    pub path: String,
    pub added: u32,
    pub removed: u32,
    /// Unified diff of what changed.
    pub diff: String,
    /// 이 쓰기 이후의 파일 버전 토큰("mtime_ms:size"). 다음 편집의 base_version으로 쓴다.
    pub version: String,
}

/// 어떤 파일의 버전 토큰. `base_version` 인자에 그대로 넘긴다.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileVersion {
    pub path: String,
    /// 수정 시각(ms). `file_io.read` 응답의 `stat.modified_unix_ms`와 같은 값이다.
    pub mtime_ms: u64,
    /// 바이트 수. `stat.size`와 같은 값이다.
    pub size: u64,
    /// 내용의 SHA-256. `version`보다 강한 비교가 필요할 때 쓴다.
    pub sha256: String,
    /// `base_version`에 그대로 넘길 토큰 — "mtime_ms:size" 형태.
    pub version: String,
}

#[zyris::capability(name = "code_edit", version = 2)]
pub trait CodeEdit {
    /// Replace old_string with new_string in a file. old_string must appear exactly once;
    /// set replace_all to change every occurrence. Read the file with file_io.read first.
    ///
    /// `base_version`: the file's version token **as it was when you last read it** —
    /// either "`<stat.modified_unix_ms>:<stat.size>`" from the read response, or the
    /// `version`/`sha256:` token from `code_edit.version`. If the file changed on disk since
    /// you read it, this call FAILS instead of editing a file you haven't seen — re-read and
    /// retry with the new token. Pass null to skip the check (not recommended).
    ///
    /// `path` is relative to the working directory, or absolute when it starts with `/`.
    async fn edit(
        &self,
        path: String,
        old_string: String,
        new_string: String,
        replace_all: Option<bool>,
        base_version: Option<String>,
    ) -> zyris::Result<EditResult>;

    /// Several edits to one file in a single call. They apply in order, and if any of them
    /// fails nothing is written at all. `base_version` works as in `edit`.
    async fn multi_edit(
        &self,
        path: String,
        edits: Vec<EditSpec>,
        base_version: Option<String>,
    ) -> zyris::Result<EditResult>;

    /// Write a whole file. For a NEW file, pass `base_version: null`.
    /// For an EXISTING file, `base_version` is REQUIRED: pass the version you read; if the
    /// file changed since, the write fails instead of silently overwriting someone else's
    /// (or the user's) changes. Missing parent directories are created.
    async fn write(
        &self,
        path: String,
        content: String,
        base_version: Option<String>,
    ) -> zyris::Result<EditResult>;

    /// Return the file's current version token(s). Call this when you want a strong
    /// `sha256:` token to pass as `base_version`; the `version` field is the same
    /// "mtime_ms:size" token you can read from `file_io.read`'s stat.
    async fn version(&self, path: String) -> zyris::Result<FileVersion>;
}

#[derive(Clone)]
pub struct LocalEdit {
    root: PathBuf,
    /// 바꾸기 전 내용을 남기는 곳. **사용자 리포 바깥에 산다**(`~/.cache`).
    undo: crate::undo::Undo,
}

impl LocalEdit {
    pub fn new(root: PathBuf) -> LocalEdit {
        let undo = crate::undo::Undo::for_dir(&root);
        LocalEdit { root, undo }
    }

    /// 되돌림 기록. `/undo`가 이것을 쓴다.
    pub fn undo(&self) -> crate::undo::Undo {
        self.undo.clone()
    }

    /// 읽고, 바꾸고, 쓰고, diff를 돌려준다. 세 도구가 모두 여기로 모인다 — 그래야 어느
    /// 길로 바뀌든 같은 모양의 결과가 나온다.
    ///
    /// `base_version`은 이 파일을 마지막으로 읽었을 때의 버전 토큰이다. 파일이 그 사이
    /// 바뀌었으면 **쓰지 않고 실패한다** — 조용히 덮는 대신 다시 읽게 만든다.
    async fn apply<F>(
        &self,
        path: &str,
        base_version: Option<&str>,
        require_base_for_existing: bool,
        change: F,
    ) -> zyris::Result<EditResult>
    where
        F: FnOnce(&str) -> Result<String, WireError>,
    {
        let full = resolve_under(&self.root, path);
        let existed = tokio::fs::try_exists(&full).await.unwrap_or(false);
        let old = tokio::fs::read_to_string(&full).await.unwrap_or_default();

        // **동시성 검사.** 읽은 뒤 파일이 바뀌었으면 조용히 덮지 않는다.
        if let Some(base) = base_version {
            let now = current_version(&full);
            let ok = match (base.strip_prefix("sha256:"), now.as_ref()) {
                (Some(hex), _) => sha256_of(old.as_bytes()) == hex,
                (None, Ok(cur)) => cur == base,
                (None, Err(_)) => false,
            };
            if !ok {
                let now_s = match now {
                    Ok(v) => v,
                    Err(e) => e.to_string(),
                };
                return Err(WireError::invalid_params(format!(
                    "'{}'이(가) 읽은 뒤 바뀌었습니다 (base_version {base} ≠ 지금 {now_s}). \
                     file_io.read로 지금 내용을 다시 읽고, 새 버전 토큰을 base_version으로 다시 시도하세요.",
                    clip(path)
                )));
            }
        }
        // 전체 파일 쓰기는 새 파일 전용이 기본이다 — 이미 있는 파일을 덮으려면
        // base_version으로 "그 파일을 봤다"는 증거를 내야 한다.
        if require_base_for_existing && existed && base_version.is_none() {
            return Err(WireError::invalid_params(format!(
                "'{}'은(는) 이미 있는 파일입니다 — 덮어쓰려면 base_version을 주세요. \
                 읽은 응답의 stat.modified_unix_ms:stat.size 또는 code_edit.version의 version을 그대로 넘기세요.",
                clip(path)
            )));
        }

        let new = change(&old)?;
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                WireError::internal(format!("상위 디렉터리를 만들지 못했습니다: {e}"))
            })?;
        }
        // **쓰기 직전에 남긴다.** 세 도구가 모두 여기로 모이므로 자리는 하나다.
        // 실패해도 편집을 막지 않는다 — 안전망이 없다고 일을 막으면 고칠 수 없는
        // 파일이 생긴다(`undo::snapshot`의 주석).
        self.undo.snapshot(&full);
        atomic_write(&full, new.as_bytes())
            .await
            .map_err(|e| WireError::internal(format!("쓰지 못했습니다: {e}")))?;
        let shown = full.to_string_lossy().to_string();
        let d = diff(&old, &new, &shown);
        let version = current_version(&full).unwrap_or_else(|_| "?".into());
        Ok(EditResult {
            path: shown,
            added: d.added,
            removed: d.removed,
            diff: d.to_unified(),
            version,
        })
    }
}

/// 파일의 지금 버전 토큰("mtime_ms:size"). 읽은 응답의 `stat`으로 뽑은 것과 같은 값이다.
fn current_version(full: &Path) -> std::io::Result<String> {
    let md = std::fs::metadata(full)?;
    Ok(format!("{}:{}", mtime_ms(&md), md.len()))
}

fn mtime_ms(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn sha256_of(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// temp 파일에 쓰고 rename으로 원자적으로 반영한다. 반쯤 쓰인 파일이 보이지 않는다.
async fn atomic_write(full: &Path, content: &[u8]) -> std::io::Result<()> {
    let dir = full.parent().unwrap_or_else(|| Path::new("."));
    let name = full.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let unique = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    );
    let tmp = dir.join(format!(".{name}.zyris-tmp-{unique}"));
    let r = async {
        let mut f = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).await?;
        f.write_all(content).await?;
        f.sync_all().await?;
        drop(f);
        tokio::fs::rename(&tmp, full).await
    }
    .await;
    if r.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    r
}

/// 한 조각을 바꾼다.
///
/// **몇 번 나왔는지를 오류에 담는다.** 에이전트가 앞뒤를 더 붙여 다시 부를 수 있어야 하고,
/// 그냥 "실패했다"로는 무엇을 고쳐야 할지 알 수 없다.
fn substitute(body: &str, spec: &EditSpec) -> Result<String, WireError> {
    let hits = body.matches(&spec.old_string).count();
    match hits {
        0 => Err(WireError::invalid_params(format!(
            "'{}'을(를) 파일에서 찾지 못했습니다. file_io.read로 지금 내용을 다시 읽으세요.",
            clip(&spec.old_string)
        ))),
        1 => Ok(body.replacen(&spec.old_string, &spec.new_string, 1)),
        _ if spec.replace_all => Ok(body.replace(&spec.old_string, &spec.new_string)),
        n => Err(WireError::invalid_params(format!(
            "'{}'이(가) 파일에 {n}번 나옵니다. 앞뒤를 더 붙여 한 곳만 가리키거나 \
             replace_all을 켜세요.",
            clip(&spec.old_string)
        ))),
    }
}

/// 오류 메시지에 들어갈 만큼만. 긴 조각을 통째로 실으면 메시지가 화면을 덮는다.
fn clip(s: &str) -> String {
    let head: String = s.chars().take(40).collect();
    if head.chars().count() < s.chars().count() {
        format!("{head}…")
    } else {
        head
    }
}

#[async_trait::async_trait]
impl CodeEdit for LocalEdit {
    async fn edit(
        &self,
        path: String,
        old_string: String,
        new_string: String,
        replace_all: Option<bool>,
        base_version: Option<String>,
    ) -> zyris::Result<EditResult> {
        let spec = EditSpec { old_string, new_string, replace_all: replace_all.unwrap_or(false) };
        self.apply(&path, base_version.as_deref(), false, |body| substitute(body, &spec)).await
    }

    async fn multi_edit(
        &self,
        path: String,
        edits: Vec<EditSpec>,
        base_version: Option<String>,
    ) -> zyris::Result<EditResult> {
        // 메모리에서 다 적용한 뒤 한 번만 쓴다. 중간까지 쓰고 실패하면 파일이 어느 상태인지
        // 아무도 모른다 — 에이전트도, 사람도.
        self.apply(&path, base_version.as_deref(), false, move |body| {
            let mut out = body.to_string();
            for spec in &edits {
                out = substitute(&out, spec)?;
            }
            Ok(out)
        })
        .await
    }

    async fn write(
        &self,
        path: String,
        content: String,
        base_version: Option<String>,
    ) -> zyris::Result<EditResult> {
        // 끝줄 개행을 저절로 붙이지 않는다 — 붙이면 파일 전체가 diff에 걸린다.
        self.apply(&path, base_version.as_deref(), true, move |_| Ok(content)).await
    }

    async fn version(&self, path: String) -> zyris::Result<FileVersion> {
        let full = resolve_under(&self.root, &path);
        let content = tokio::fs::read(&full).await.map_err(|e| {
            WireError::invalid_params(format!("'{}'을(를) 읽지 못했습니다: {e}", clip(&path)))
        })?;
        let md = tokio::fs::metadata(&full).await.map_err(|e| {
            WireError::invalid_params(format!("'{}'을(를) 확인하지 못했습니다: {e}", clip(&path)))
        })?;
        let mtime_ms = mtime_ms(&md);
        let size = md.len();
        let sha256 = sha256_of(&content);
        Ok(FileVersion { path, mtime_ms, size, sha256, version: format!("{mtime_ms}:{size}") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(body: &str) -> (tempfile::TempDir, LocalEdit, String) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), body).unwrap();
        let edit = LocalEdit::new(dir.path().to_path_buf());
        (dir, edit, "a.txt".to_string())
    }

    /// **편집하면 되돌릴 수 있어야 한다.** git 없는 디렉터리에서는 이것이 유일한 안전망이다.
    #[tokio::test]
    async fn an_edit_leaves_something_to_undo() {
        // 캐시 자리를 옮긴다 — 진짜 홈에 쓰면 안 된다.
        let cache = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CACHE_HOME", cache.path());

        let (dir, edit, path) = scratch("before\n");
        let undo = edit.undo();
        assert!(undo.is_empty(), "아직 아무것도 안 고쳤다");

        edit.edit(path, "before".into(), "after".into(), None, None).await.unwrap();
        assert!(!undo.is_empty(), "고쳤는데 되돌릴 것이 없다");

        undo.revert_last().unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "before\n");
    }

    /// 새 파일을 만든 것도 되돌릴 수 있어야 한다 — 되돌리면 그 파일이 없어진다.
    #[tokio::test]
    async fn creating_a_file_can_be_undone_too() {
        let cache = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CACHE_HOME", cache.path());

        let (dir, edit, _) = scratch("아무거나\n");
        let undo = edit.undo();
        edit.write("새로.txt".into(), "내용\n".into(), None).await.unwrap();
        assert!(dir.path().join("새로.txt").exists());

        undo.revert_last().unwrap();
        assert!(!dir.path().join("새로.txt").exists(), "만든 파일이 남았다");
    }

    #[tokio::test]
    async fn editing_replaces_the_one_match() {
        let (_d, edit, p) = scratch("하나\n둘\n셋\n");
        let r = edit.edit(p, "둘".into(), "TWO".into(), None, None).await.unwrap();
        assert_eq!((r.added, r.removed), (1, 1));
        assert!(r.diff.contains("+TWO"), "{}", r.diff);
    }

    /// 두 번 나오는 것을 말없이 하나만 바꾸면 엉뚱한 곳이 조용히 바뀐다.
    #[tokio::test]
    async fn two_matches_fail_and_say_how_many() {
        let (_d, edit, p) = scratch("x\nx\n");
        let e = edit.edit(p, "x".into(), "y".into(), None, None).await.unwrap_err();
        assert!(e.message.contains('2'), "몇 번 나왔는지 말해야 한다: {}", e.message);
    }

    /// 못 찾았을 때도 무엇을 해야 할지 말해야 한다.
    #[tokio::test]
    async fn no_match_fails_and_says_what_to_do() {
        let (_d, edit, p) = scratch("x\n");
        let e = edit.edit(p, "없다".into(), "y".into(), None, None).await.unwrap_err();
        assert!(e.message.contains("read"), "다시 읽으라고 말해야 한다: {}", e.message);
    }

    #[tokio::test]
    async fn replace_all_takes_every_match() {
        let (_d, edit, p) = scratch("x\nx\n");
        let r = edit.edit(p, "x".into(), "y".into(), Some(true), None).await.unwrap();
        assert_eq!(r.added, 2);
    }

    /// 중간까지 쓰고 실패하면 파일이 어느 상태인지 아무도 모른다.
    #[tokio::test]
    async fn a_failing_multi_edit_leaves_the_file_alone() {
        let (dir, edit, p) = scratch("하나\n둘\n");
        let edits = vec![
            EditSpec { old_string: "하나".into(), new_string: "ONE".into(), replace_all: false },
            EditSpec { old_string: "없다".into(), new_string: "X".into(), replace_all: false },
        ];
        assert!(edit.multi_edit(p, edits, None).await.is_err());
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "하나\n둘\n");
    }

    #[tokio::test]
    async fn a_whole_multi_edit_applies_in_order() {
        let (dir, edit, p) = scratch("하나\n둘\n");
        let edits = vec![
            EditSpec { old_string: "하나".into(), new_string: "ONE".into(), replace_all: false },
            EditSpec { old_string: "둘".into(), new_string: "TWO".into(), replace_all: false },
        ];
        edit.multi_edit(p, edits, None).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "ONE\nTWO\n");
    }

    /// 끝줄 개행이 저절로 붙으면 파일 전체가 diff에 걸린다.
    #[tokio::test]
    async fn writing_does_not_add_a_trailing_newline() {
        let (dir, edit, _) = scratch("");
        edit.write("b.txt".into(), "한 줄".into(), None).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("b.txt")).unwrap(), "한 줄");
    }

    /// 없는 디렉터리에 새 파일을 만들 수 있어야 한다.
    #[tokio::test]
    async fn writing_creates_missing_parents() {
        let (dir, edit, _) = scratch("");
        edit.write("깊은/곳/c.txt".into(), "내용".into(), None).await.unwrap();
        assert!(dir.path().join("깊은/곳/c.txt").exists());
    }

    /// 읽은 뒤 파일이 바뀌었으면 조용히 덮지 않고 실패해야 한다.
    #[tokio::test]
    async fn editing_with_a_stale_base_version_fails_and_leaves_the_file() {
        let (dir, edit, p) = scratch("before\n");
        // 에이전트가 읽은 버전 토큰
        let read_at = current_version(&dir.path().join("a.txt")).unwrap();
        // 다른 세션이 그 사이 고쳤다고 치자
        std::fs::write(dir.path().join("a.txt"), "before\nother-session\n").unwrap();
        let e =
            edit.edit(p, "before".into(), "after".into(), None, Some(read_at)).await.unwrap_err();
        assert!(e.message.contains("바뀌었"), "{}", e.message);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "before\nother-session\n"
        );
    }

    /// base_version이 지금 디스크와 같으면 편집은 통과한다.
    #[tokio::test]
    async fn editing_with_a_matching_base_version_succeeds() {
        let (dir, edit, p) = scratch("before\n");
        let v = current_version(&dir.path().join("a.txt")).unwrap();
        edit.edit(p, "before".into(), "after".into(), None, Some(v)).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "after\n");
    }

    /// sha256 토큰으로도 같은 검사가 된다 — 내용이 달라지면 실패한다.
    #[tokio::test]
    async fn sha256_base_version_checks_content() {
        let (dir, edit, p) = scratch("before\n");
        let good = format!("sha256:{}", sha256_of(b"before\n"));
        std::fs::write(dir.path().join("a.txt"), "before\nother\n").unwrap();
        let e = edit.edit(p, "before".into(), "after".into(), None, Some(good)).await.unwrap_err();
        assert!(e.message.contains("바뀌었"), "{}", e.message);
    }

    /// 이미 있는 파일을 base_version 없이 덮어쓰면 거부되어야 한다 — 조용한 덮어쓰기의 주범이다.
    #[tokio::test]
    async fn writing_over_an_existing_file_requires_base_version() {
        let (dir, edit, p) = scratch("keep\n");
        let e = edit.write(p.clone(), "clobber\n".into(), None).await.unwrap_err();
        assert!(e.message.contains("base_version"), "{}", e.message);
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "keep\n");

        let v = current_version(&dir.path().join("a.txt")).unwrap();
        edit.write(p, "clobber\n".into(), Some(v)).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "clobber\n");
    }

    /// version()이 돌려주는 토큰은 디스크와 일치해야 한다 — base_version으로 그대로 쓸 수 있다.
    #[tokio::test]
    async fn version_matches_the_file_on_disk() {
        let (dir, edit, p) = scratch("abc\n");
        let fv = edit.version(p).await.unwrap();
        assert_eq!(fv.version, current_version(&dir.path().join("a.txt")).unwrap());
        assert_eq!(fv.size, 4);
        assert_eq!(fv.sha256, sha256_of(b"abc\n"));
    }

    /// 편집 결과가 새 버전 토큰을 실어 나른다 — 다음 편집의 base_version으로 쓸 수 있다.
    #[tokio::test]
    async fn edit_reports_the_new_version() {
        let (dir, edit, p) = scratch("x\n");
        let r = edit.edit(p, "x".into(), "y".into(), None, None).await.unwrap();
        assert_eq!(r.version, current_version(&dir.path().join("a.txt")).unwrap());
    }
}
