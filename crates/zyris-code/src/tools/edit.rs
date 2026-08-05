//! The **only** way an agent changes files.
//!
//! `file_io`'s `write`·`remove`·`mkdir` aren't granted (`tools::readonly`). If there were two ways to change a file,
//! the agent would pick whole-file overwrites, diffs would spread across the whole file, and the approval gate
//! would have to be hung in two places. With this as the only way, **every change passes one approval gate and one diff screen.**
//!
//! **There is no tool for deleting files at all.** Deleting is done by the human.
//!
//! Only the methods' doc comments go over the wire and become the description the agent reads. **So the comments ARE the contract.**
//!
//! If several sessions edit one repository at the same time, they can overwrite files without knowing. That fight is
//! stopped with `base_version` — send the version token from when you read with the edit, and if the file changed in between,
//! it fails instead of silently overwriting. The token comes from the `file_io.read` response's
//! `stat.modified_unix_ms:stat.size`, or from `code_edit.version`'s `version`.

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
    /// The file's version token after this write ("mtime_ms:size"). Use it as the base_version of the next edit.
    pub version: String,
}

/// A file's version token. Pass it straight to the `base_version` argument.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileVersion {
    pub path: String,
    /// Modification time (ms). The same value as `stat.modified_unix_ms` in the `file_io.read` response.
    pub mtime_ms: u64,
    /// Byte count. The same value as `stat.size`.
    pub size: u64,
    /// SHA-256 of the content. Used when a stronger comparison than `version` is needed.
    pub sha256: String,
    /// The token to pass straight to `base_version` — of the form "mtime_ms:size".
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
    /// Where the pre-change content is kept. **Lives outside the user's repo** (`~/.cache`).
    undo: crate::undo::Undo,
}

impl LocalEdit {
    pub fn new(root: PathBuf) -> LocalEdit {
        let undo = crate::undo::Undo::for_dir(&root);
        LocalEdit { root, undo }
    }

    /// The undo log. `/undo` uses it.
    pub fn undo(&self) -> crate::undo::Undo {
        self.undo.clone()
    }

    /// Read, change, write, and return a diff. All three tools meet here — so no matter which path,
    /// the result has the same shape.
    ///
    /// `base_version` is the version token from when this file was last read. If the file changed in between,
    /// it **fails without writing** — instead of silently overwriting, it forces a re-read.
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

        // **Concurrency check.** If the file changed after reading, don't silently overwrite.
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
                return Err(WireError::invalid_params(
                    crate::lang::current().edit_changed_after_read(&clip(path), base, &now_s),
                ));
            }
        }
        // Whole-file writes default to new files only — to overwrite an existing file you must
        // present proof via base_version that you've seen the file.
        if require_base_for_existing && existed && base_version.is_none() {
            return Err(WireError::invalid_params(
                crate::lang::current().edit_exists_no_base(&clip(path)),
            ));
        }

        let new = change(&old)?;
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                WireError::internal(crate::lang::current().edit_mkdir_error(&e.to_string()))
            })?;
        }
        // **Snapshot right before writing.** All three tools meet here, so there's a single spot.
        // A failure doesn't block the edit — if a missing safety net stopped work,
        // you'd end up with files that can't be fixed (see `undo::snapshot`'s comment).
        self.undo.snapshot(&full);
        atomic_write(&full, new.as_bytes()).await.map_err(|e| {
            WireError::internal(crate::lang::current().edit_write_error(&e.to_string()))
        })?;
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

/// The file's current version token ("mtime_ms:size"). The same value as what the read response's `stat` yields.
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

/// Write to a temp file and apply atomically via rename. A half-written file is never visible.
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

/// Replaces one fragment.
///
/// **How many times it appeared goes into the error.** The agent must be able to add more context and call again, and
/// a bare "failed" doesn't tell it what to fix.
fn substitute(body: &str, spec: &EditSpec) -> Result<String, WireError> {
    let hits = body.matches(&spec.old_string).count();
    match hits {
        0 => Err(WireError::invalid_params(
            crate::lang::current().edit_not_found(&clip(&spec.old_string)),
        )),
        1 => Ok(body.replacen(&spec.old_string, &spec.new_string, 1)),
        _ if spec.replace_all => Ok(body.replace(&spec.old_string, &spec.new_string)),
        n => Err(WireError::invalid_params(crate::lang::current().edit_ambiguous(n))),
    }
}

/// Only as much as fits an error message. Loading a long fragment whole would let the message cover the screen.
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
        // Apply everything in memory, then write once. If it wrote halfway and failed, nobody would know
        // what state the file is in — neither the agent nor the human.
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
        // A trailing newline isn't added automatically — adding one would put the whole file in the diff.
        self.apply(&path, base_version.as_deref(), true, move |_| Ok(content)).await
    }

    async fn version(&self, path: String) -> zyris::Result<FileVersion> {
        let full = resolve_under(&self.root, &path);
        let content = tokio::fs::read(&full).await.map_err(|e| {
            WireError::invalid_params(
                crate::lang::current().edit_read_error(&clip(&path), &e.to_string()),
            )
        })?;
        let md = tokio::fs::metadata(&full).await.map_err(|e| {
            WireError::invalid_params(
                crate::lang::current().edit_stat_error(&clip(&path), &e.to_string()),
            )
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

    /// **An edit must be undoable.** In a directory without git, this is the only safety net.
    #[tokio::test]
    async fn an_edit_leaves_something_to_undo() {
        // Move the cache location — must not write to the real home.
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

    /// Creating a new file must be undoable too — reverting makes the file disappear.
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

    /// Replacing only one of two occurrences without a word would quietly change the wrong place.
    #[tokio::test]
    async fn two_matches_fail_and_say_how_many() {
        let (_d, edit, p) = scratch("x\nx\n");
        let e = edit.edit(p, "x".into(), "y".into(), None, None).await.unwrap_err();
        assert!(e.message.contains('2'), "몇 번 나왔는지 말해야 한다: {}", e.message);
    }

    /// When it's not found, it must also say what to do.
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

    /// If it wrote halfway and failed, nobody would know what state the file is in.
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

    /// If a trailing newline were added automatically, the whole file would land in the diff.
    #[tokio::test]
    async fn writing_does_not_add_a_trailing_newline() {
        let (dir, edit, _) = scratch("");
        edit.write("b.txt".into(), "한 줄".into(), None).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("b.txt")).unwrap(), "한 줄");
    }

    /// It must be possible to create a new file in a missing directory.
    #[tokio::test]
    async fn writing_creates_missing_parents() {
        let (dir, edit, _) = scratch("");
        edit.write("깊은/곳/c.txt".into(), "내용".into(), None).await.unwrap();
        assert!(dir.path().join("깊은/곳/c.txt").exists());
    }

    /// If the file changed after reading, it must fail instead of silently overwriting.
    #[tokio::test]
    async fn editing_with_a_stale_base_version_fails_and_leaves_the_file() {
        let (dir, edit, p) = scratch("before\n");
        // The version token the agent read
        let read_at = current_version(&dir.path().join("a.txt")).unwrap();
        // Pretend another session changed it in between
        std::fs::write(dir.path().join("a.txt"), "before\nother-session\n").unwrap();
        let e =
            edit.edit(p, "before".into(), "after".into(), None, Some(read_at)).await.unwrap_err();
        assert!(e.message.contains("바뀌었"), "{}", e.message);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "before\nother-session\n"
        );
    }

    /// If base_version matches the current disk, the edit goes through.
    #[tokio::test]
    async fn editing_with_a_matching_base_version_succeeds() {
        let (dir, edit, p) = scratch("before\n");
        let v = current_version(&dir.path().join("a.txt")).unwrap();
        edit.edit(p, "before".into(), "after".into(), None, Some(v)).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "after\n");
    }

    /// The same check works with a sha256 token — a content change fails it.
    #[tokio::test]
    async fn sha256_base_version_checks_content() {
        let (dir, edit, p) = scratch("before\n");
        let good = format!("sha256:{}", sha256_of(b"before\n"));
        std::fs::write(dir.path().join("a.txt"), "before\nother\n").unwrap();
        let e = edit.edit(p, "before".into(), "after".into(), None, Some(good)).await.unwrap_err();
        assert!(e.message.contains("바뀌었"), "{}", e.message);
    }

    /// Overwriting an existing file without base_version must be refused — it's the prime source of silent overwrites.
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

    /// The token version() returns must match the disk — it can be used as base_version as is.
    #[tokio::test]
    async fn version_matches_the_file_on_disk() {
        let (dir, edit, p) = scratch("abc\n");
        let fv = edit.version(p).await.unwrap();
        assert_eq!(fv.version, current_version(&dir.path().join("a.txt")).unwrap());
        assert_eq!(fv.size, 4);
        assert_eq!(fv.sha256, sha256_of(b"abc\n"));
    }

    /// The edit result carries the new version token — usable as the next edit's base_version.
    #[tokio::test]
    async fn edit_reports_the_new_version() {
        let (dir, edit, p) = scratch("x\n");
        let r = edit.edit(p, "x".into(), "y".into(), None, None).await.unwrap();
        assert_eq!(r.version, current_version(&dir.path().join("a.txt")).unwrap());
    }
}
