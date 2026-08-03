//! 에이전트가 파일을 고치는 **유일한** 길.
//!
//! `file_io`의 `write`·`remove`·`mkdir`은 내주지 않는다(`tools::readonly`). 파일을 바꾸는
//! 길이 둘이면 에이전트가 통째로 덮어쓰기를 골라 diff가 파일 전체로 번지고, 승인 게이트를
//! 두 곳에 걸어야 한다. 여기가 독점이라 **모든 변경이 한 승인 게이트와 한 diff 화면을 지난다.**
//!
//! **파일을 지우는 도구는 아예 없다.** 지우는 것은 사람이 한다.
//!
//! 메서드의 doc 주석만 와이어로 나가 에이전트가 읽는 설명이 된다. **그래서 주석이 곧 규약이다.**

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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
}

#[zyris::capability(name = "code_edit", version = 1)]
pub trait CodeEdit {
    /// Replace old_string with new_string in a file. old_string must appear exactly once;
    /// set replace_all to change every occurrence. Read the file with file_io.read first.
    ///
    /// `path` is relative to the working directory, or absolute when it starts with `/`.
    async fn edit(
        &self,
        path: String,
        old_string: String,
        new_string: String,
        replace_all: Option<bool>,
    ) -> zyris::Result<EditResult>;

    /// Several edits to one file in a single call. They apply in order, and if any of them
    /// fails nothing is written at all.
    async fn multi_edit(&self, path: String, edits: Vec<EditSpec>) -> zyris::Result<EditResult>;

    /// Write a whole file. Use this for new files; to change an existing one use edit.
    /// Missing parent directories are created.
    async fn write(&self, path: String, content: String) -> zyris::Result<EditResult>;
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
    async fn apply<F>(&self, path: &str, change: F) -> zyris::Result<EditResult>
    where
        F: FnOnce(&str) -> Result<String, WireError>,
    {
        let full = resolve_under(&self.root, path);
        let old = tokio::fs::read_to_string(&full).await.unwrap_or_default();
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
        tokio::fs::write(&full, &new)
            .await
            .map_err(|e| WireError::internal(format!("쓰지 못했습니다: {e}")))?;
        let shown = full.to_string_lossy().to_string();
        let d = diff(&old, &new, &shown);
        Ok(EditResult { path: shown, added: d.added, removed: d.removed, diff: d.to_unified() })
    }
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
    ) -> zyris::Result<EditResult> {
        let spec = EditSpec { old_string, new_string, replace_all: replace_all.unwrap_or(false) };
        self.apply(&path, |body| substitute(body, &spec)).await
    }

    async fn multi_edit(&self, path: String, edits: Vec<EditSpec>) -> zyris::Result<EditResult> {
        // 메모리에서 다 적용한 뒤 한 번만 쓴다. 중간까지 쓰고 실패하면 파일이 어느 상태인지
        // 아무도 모른다 — 에이전트도, 사람도.
        self.apply(&path, move |body| {
            let mut out = body.to_string();
            for spec in &edits {
                out = substitute(&out, spec)?;
            }
            Ok(out)
        })
        .await
    }

    async fn write(&self, path: String, content: String) -> zyris::Result<EditResult> {
        // 끝줄 개행을 저절로 붙이지 않는다 — 붙이면 파일 전체가 diff에 걸린다.
        self.apply(&path, move |_| Ok(content)).await
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

        edit.edit(path, "before".into(), "after".into(), None).await.unwrap();
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
        edit.write("새로.txt".into(), "내용\n".into()).await.unwrap();
        assert!(dir.path().join("새로.txt").exists());

        undo.revert_last().unwrap();
        assert!(!dir.path().join("새로.txt").exists(), "만든 파일이 남았다");
    }

    #[tokio::test]
    async fn editing_replaces_the_one_match() {
        let (_d, edit, p) = scratch("하나\n둘\n셋\n");
        let r = edit.edit(p, "둘".into(), "TWO".into(), None).await.unwrap();
        assert_eq!((r.added, r.removed), (1, 1));
        assert!(r.diff.contains("+TWO"), "{}", r.diff);
    }

    /// 두 번 나오는 것을 말없이 하나만 바꾸면 엉뚱한 곳이 조용히 바뀐다.
    #[tokio::test]
    async fn two_matches_fail_and_say_how_many() {
        let (_d, edit, p) = scratch("x\nx\n");
        let e = edit.edit(p, "x".into(), "y".into(), None).await.unwrap_err();
        assert!(e.message.contains('2'), "몇 번 나왔는지 말해야 한다: {}", e.message);
    }

    /// 못 찾았을 때도 무엇을 해야 할지 말해야 한다.
    #[tokio::test]
    async fn no_match_fails_and_says_what_to_do() {
        let (_d, edit, p) = scratch("x\n");
        let e = edit.edit(p, "없다".into(), "y".into(), None).await.unwrap_err();
        assert!(e.message.contains("read"), "다시 읽으라고 말해야 한다: {}", e.message);
    }

    #[tokio::test]
    async fn replace_all_takes_every_match() {
        let (_d, edit, p) = scratch("x\nx\n");
        let r = edit.edit(p, "x".into(), "y".into(), Some(true)).await.unwrap();
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
        assert!(edit.multi_edit(p, edits).await.is_err());
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "하나\n둘\n");
    }

    #[tokio::test]
    async fn a_whole_multi_edit_applies_in_order() {
        let (dir, edit, p) = scratch("하나\n둘\n");
        let edits = vec![
            EditSpec { old_string: "하나".into(), new_string: "ONE".into(), replace_all: false },
            EditSpec { old_string: "둘".into(), new_string: "TWO".into(), replace_all: false },
        ];
        edit.multi_edit(p, edits).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "ONE\nTWO\n");
    }

    /// 끝줄 개행이 저절로 붙으면 파일 전체가 diff에 걸린다.
    #[tokio::test]
    async fn writing_does_not_add_a_trailing_newline() {
        let (dir, edit, _) = scratch("");
        edit.write("b.txt".into(), "한 줄".into()).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("b.txt")).unwrap(), "한 줄");
    }

    /// 없는 디렉터리에 새 파일을 만들 수 있어야 한다.
    #[tokio::test]
    async fn writing_creates_missing_parents() {
        let (dir, edit, _) = scratch("");
        edit.write("깊은/곳/c.txt".into(), "내용".into()).await.unwrap();
        assert!(dir.path().join("깊은/곳/c.txt").exists());
    }
}
