//! What this repo tells agents — `CLAUDE.md` and `AGENTS.md`.
//!
//! If a coding agent doesn't know a repo's conventions, it makes the same mistakes every time. On this machine alone,
//! rules that **can't be learned by reading the code**, like "don't run `cargo fmt` in attacca", are
//! written in `CLAUDE.md`.
//!
//! **Collected by walking upward.** `/home/ruma/CLAUDE.md` (the map of the work home) and
//! `/home/ruma/zyris-code/CLAUDE.md` (this repo's conventions) both apply to this directory.
//! Outer ones go first and inner ones last, so **the more specific one comes later**.
//!
//! **If both are in one directory, `CLAUDE.md` wins.** Usually the same thing is written twice,
//! so loading both would just double the context.
//!
//! It goes out as the session preamble, so **it's fixed when the session is created and can't change later**
//! (attacca's `ZNewSession`). After editing the files, you must open a new session for it to take effect.

use std::path::{Path, PathBuf};

/// Names looked for in one directory. The earlier one wins.
const NAMES: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];
/// Load at most this much in total. If over, **drop from the outside first** — the nearer one is more specific.
const TOTAL_LIMIT: usize = 32 * 1024;
/// Maximum length taken from one file.
const ONE_LIMIT: usize = 16 * 1024;

/// One instruction found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub text: String,
}

/// Walks upward from the working directory collecting instructions. **Outer ones first, inner ones last.**
pub fn collect(cwd: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    // Walk from inside out, then reverse — `ancestors` yields the opposite order.
    for dir in cwd.ancestors() {
        for name in NAMES {
            let at = dir.join(name);
            let Ok(text) = std::fs::read_to_string(&at) else { continue };
            if !text.trim().is_empty() {
                found.push(Found { path: at, text: clip(text, ONE_LIMIT) });
            }
            // Don't look at `AGENTS.md` in the same directory — it usually says the same thing.
            break;
        }
    }
    found.reverse();
    trim_to_budget(&mut found);
    found
}

/// The text to load when creating a session. `None` if there is nothing.
pub fn preamble(cwd: &Path) -> Option<String> {
    let found = collect(cwd);
    if found.is_empty() {
        return None;
    }
    let mut out = String::from(
        "이 작업 디렉터리에는 아래 지침이 있습니다. **코드보다 이것이 우선입니다** ‒ \
         저장소마다 다른 규약과, 코드를 읽어서는 알 수 없는 제약이 여기 적혀 있습니다. \
         뒤에 오는 것일수록 이 디렉터리에 가깝고, 겹치면 가까운 쪽을 따르세요.\n",
    );
    for f in found {
        out.push_str(&format!("\n--- {} ---\n{}\n", f.path.display(), f.text.trim_end()));
    }
    Some(out)
}

/// If over budget, **drop from the outside first**. The nearer one is the better fit for this job.
fn trim_to_budget(found: &mut Vec<Found>) {
    while found.len() > 1 && found.iter().map(|f| f.text.len()).sum::<usize>() > TOTAL_LIMIT {
        found.remove(0);
    }
    // If only one remains and it's still over, clip it.
    if let Some(only) = found.first_mut() {
        if only.text.len() > TOTAL_LIMIT {
            only.text = clip(std::mem::take(&mut only.text), TOTAL_LIMIT);
        }
    }
}

/// Clips to a byte cap. **Only cuts at character boundaries** — cutting by bytes would break Korean.
fn clip(text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut cut = limit;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    // Say that it was clipped. Otherwise rules in the tail would be read as "absent".
    format!("{}\n\n… (길어서 여기까지만 실었습니다)", &text[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(at: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(at).unwrap();
        std::fs::write(at.join(name), body).unwrap();
    }

    #[test]
    fn a_claude_md_in_the_working_directory_is_read() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "CLAUDE.md", "cargo fmt를 돌리지 말 것");
        let found = collect(d.path());
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].text.contains("cargo fmt"));
    }

    /// Some repos only have `AGENTS.md`. Looking at just one name would skip it entirely.
    #[test]
    fn an_agents_md_works_on_its_own() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "AGENTS.md", "여기 규약");
        let found = collect(d.path());
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].text.contains("여기 규약"));
    }

    /// **If both are in one directory, `CLAUDE.md` wins.** Usually the same thing is written twice,
    /// so loading both would just double the context.
    #[test]
    fn claude_md_wins_over_agents_md_in_the_same_directory() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "CLAUDE.md", "이것을 읽어라");
        write(d.path(), "AGENTS.md", "이것은 말고");
        let found = collect(d.path());
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].text.contains("이것을 읽어라"));
    }

    /// **Walks upward.** This machine actually looks like that —
    /// both `~/CLAUDE.md` (the map of the work home) and `~/repo/CLAUDE.md` (the repo conventions) apply.
    #[test]
    fn instructions_from_parent_directories_are_collected_too() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        write(root.path(), "CLAUDE.md", "작업 홈의 지도");
        write(&repo, "CLAUDE.md", "이 리포의 규약");

        let found = collect(&repo);
        assert_eq!(found.len(), 2, "{found:?}");
        // **The more specific one comes last.** The preamble says to follow the later one on conflict.
        assert!(found[0].text.contains("작업 홈"), "{found:?}");
        assert!(found[1].text.contains("이 리포"), "{found:?}");
    }

    /// An empty file only takes up space.
    #[test]
    fn an_empty_file_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "CLAUDE.md", "   \n\n");
        assert!(collect(d.path()).is_empty());
    }

    #[test]
    fn nothing_to_say_means_no_preamble() {
        let d = tempfile::tempdir().unwrap();
        assert!(preamble(d.path()).is_none());
    }

    /// The preamble also says which file each part came from — a person must check when rules collide.
    #[test]
    fn the_preamble_says_where_each_part_came_from() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "CLAUDE.md", "규약");
        let p = preamble(d.path()).unwrap();
        assert!(p.contains("CLAUDE.md"), "{p}");
        assert!(p.contains("규약"), "{p}");
    }

    /// **If it was clipped, say so.** Otherwise the rules in the tail get read as "absent".
    #[test]
    fn a_huge_file_is_clipped_and_says_so() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "CLAUDE.md", &"가".repeat(ONE_LIMIT));
        let found = collect(d.path());
        assert!(found[0].text.len() <= ONE_LIMIT + 64, "{} bytes", found[0].text.len());
        assert!(found[0].text.contains("여기까지만"), "it doesn't say it was clipped");
    }

    /// Korean must not be cut in half — cutting by bytes is how that happens.
    #[test]
    fn clipping_never_breaks_a_character() {
        let text = "가".repeat(100);
        // A spot aimed between the 3-byte characters.
        assert!(clip(text, 100).starts_with("가가"));
    }
}
