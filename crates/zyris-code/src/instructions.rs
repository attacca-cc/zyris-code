//! 이 리포가 에이전트에게 하는 말 — `CLAUDE.md`와 `AGENTS.md`.
//!
//! 코딩 에이전트가 리포의 규약을 모르면 매번 같은 것을 틀린다. 이 머신만 해도
//! "attacca에서 `cargo fmt`를 돌리지 말 것" 같은, **코드를 읽어서는 알 수 없는** 규칙이
//! `CLAUDE.md`에 적혀 있다.
//!
//! **위로 거슬러 올라가며 모은다.** `/home/ruma/CLAUDE.md`(작업 홈의 지도)와
//! `/home/ruma/zyris-code/CLAUDE.md`(이 리포의 규약)가 둘 다 이 디렉터리에 걸린다.
//! 바깥것부터 싣고 안것을 뒤에 실어, **구체적인 쪽이 나중에 오게** 한다.
//!
//! **한 디렉터리에 둘 다 있으면 `CLAUDE.md`가 이긴다.** 대개 같은 말을 두 번 적어 둔
//! 것이라 다 실으면 컨텍스트만 두 배로 먹는다.
//!
//! 세션 preamble로 나가므로 **세션을 만들 때 한 번 정해지고 뒤에 바꿀 수 없다**
//! (attacca의 `ZNewSession`). 파일을 고쳤으면 새 세션을 열어야 반영된다.

use std::path::{Path, PathBuf};

/// 한 디렉터리에서 볼 이름. 앞의 것이 이긴다.
const NAMES: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];
/// 다 합쳐 이만큼까지만 싣는다. 넘으면 **바깥쪽부터** 버린다 — 가까운 것이 더 구체적이다.
const TOTAL_LIMIT: usize = 32 * 1024;
/// 한 파일에서 가져올 최대 길이.
const ONE_LIMIT: usize = 16 * 1024;

/// 찾은 지침 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub text: String,
}

/// 작업 디렉터리에서 위로 거슬러 올라가며 모은다. **바깥것이 앞, 안것이 뒤다.**
pub fn collect(cwd: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    // 안에서 밖으로 훑은 뒤 뒤집는다 — `ancestors`가 주는 순서가 그 반대다.
    for dir in cwd.ancestors() {
        for name in NAMES {
            let at = dir.join(name);
            let Ok(text) = std::fs::read_to_string(&at) else { continue };
            if !text.trim().is_empty() {
                found.push(Found { path: at, text: clip(text, ONE_LIMIT) });
            }
            // 같은 디렉터리의 `AGENTS.md`는 보지 않는다 — 대개 같은 말이다.
            break;
        }
    }
    found.reverse();
    trim_to_budget(&mut found);
    found
}

/// 세션을 만들 때 실을 글. 아무것도 없으면 `None`이다.
pub fn preamble(cwd: &Path) -> Option<String> {
    let found = collect(cwd);
    if found.is_empty() {
        return None;
    }
    let mut out = String::from(
        "이 작업 디렉터리에는 아래 지침이 있습니다. **코드보다 이것이 우선입니다** — \
         저장소마다 다른 규약과, 코드를 읽어서는 알 수 없는 제약이 여기 적혀 있습니다. \
         뒤에 오는 것일수록 이 디렉터리에 가깝고, 겹치면 가까운 쪽을 따르세요.\n",
    );
    for f in found {
        out.push_str(&format!("\n--- {} ---\n{}\n", f.path.display(), f.text.trim_end()));
    }
    Some(out)
}

/// 예산을 넘으면 **바깥것부터** 버린다. 가까운 것이 이 일에 더 맞는 말이다.
fn trim_to_budget(found: &mut Vec<Found>) {
    while found.len() > 1 && found.iter().map(|f| f.text.len()).sum::<usize>() > TOTAL_LIMIT {
        found.remove(0);
    }
    // 하나만 남았는데도 넘으면 그것을 자른다.
    if let Some(only) = found.first_mut() {
        if only.text.len() > TOTAL_LIMIT {
            only.text = clip(std::mem::take(&mut only.text), TOTAL_LIMIT);
        }
    }
}

/// 바이트 상한에 맞춰 자른다. **문자 경계에서만 자른다** — 바이트로 자르면 한글이 깨진다.
fn clip(text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut cut = limit;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    // 잘렸다는 것을 말한다. 모르면 뒷부분에 있던 규칙을 "없다"로 읽는다.
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

    /// `AGENTS.md`만 있는 리포도 있다. 이름 하나만 보면 그쪽이 통째로 안 읽힌다.
    #[test]
    fn an_agents_md_works_on_its_own() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "AGENTS.md", "여기 규약");
        let found = collect(d.path());
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].text.contains("여기 규약"));
    }

    /// **한 디렉터리에 둘 다 있으면 `CLAUDE.md`가 이긴다.** 대개 같은 말이라
    /// 다 실으면 컨텍스트만 두 배로 먹는다.
    #[test]
    fn claude_md_wins_over_agents_md_in_the_same_directory() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "CLAUDE.md", "이것을 읽어라");
        write(d.path(), "AGENTS.md", "이것은 말고");
        let found = collect(d.path());
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].text.contains("이것을 읽어라"));
    }

    /// **위로 거슬러 올라간다.** 이 머신이 실제로 그런 모양이다 —
    /// `~/CLAUDE.md`(작업 홈의 지도)와 `~/repo/CLAUDE.md`(리포 규약)가 둘 다 걸린다.
    #[test]
    fn instructions_from_parent_directories_are_collected_too() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        write(root.path(), "CLAUDE.md", "작업 홈의 지도");
        write(&repo, "CLAUDE.md", "이 리포의 규약");

        let found = collect(&repo);
        assert_eq!(found.len(), 2, "{found:?}");
        // **구체적인 쪽이 뒤다.** 겹칠 때 나중 것을 따르라고 preamble이 말한다.
        assert!(found[0].text.contains("작업 홈"), "{found:?}");
        assert!(found[1].text.contains("이 리포"), "{found:?}");
    }

    /// 빈 파일은 자리만 먹는다.
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

    /// preamble에는 어느 파일에서 왔는지도 실린다 — 규칙이 부딪힐 때 사람이 확인해야 한다.
    #[test]
    fn the_preamble_says_where_each_part_came_from() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "CLAUDE.md", "규약");
        let p = preamble(d.path()).unwrap();
        assert!(p.contains("CLAUDE.md"), "{p}");
        assert!(p.contains("규약"), "{p}");
    }

    /// **잘렸으면 잘렸다고 말한다.** 모르면 뒷부분의 규칙을 "없다"로 읽는다.
    #[test]
    fn a_huge_file_is_clipped_and_says_so() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "CLAUDE.md", &"가".repeat(ONE_LIMIT));
        let found = collect(d.path());
        assert!(found[0].text.len() <= ONE_LIMIT + 64, "{}바이트", found[0].text.len());
        assert!(found[0].text.contains("여기까지만"), "잘렸다는 말이 없다");
    }

    /// 한글이 반 토막 나면 안 된다 — 바이트로 자르면 그렇게 된다.
    #[test]
    fn clipping_never_breaks_a_character() {
        let text = "가".repeat(100);
        // 3바이트짜리 글자 사이를 노린 자리.
        assert!(clip(text, 100).starts_with("가가"));
    }
}
