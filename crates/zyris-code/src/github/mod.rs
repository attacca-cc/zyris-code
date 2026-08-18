//! What the GitHub half of the `git` capability is built on.
//!
//! **The tools live in [`crate::git`].** git and GitHub were announced as two capabilities until
//! 0.3.1, which put three pairs of near-identical tools in front of the agent; they are one
//! subject and are now one capability. What stays here is the client, signing in, the signing key
//! and the small pure decisions the tools make.
//!
//! **Why not just let the agent run `gh`?** Two reasons, and both were the point of building this.
//! `gh` has to be installed, which on a fresh machine it is not — and when it is, `gh pr view
//! --json …` hands back every field GitHub felt like sending, most of them URLs to further calls,
//! and the agent pays for all of it. The calls here answer with what a person would have read
//! (`api::slim_*`), which is a fraction of the tokens for the same work.
//!
//! Signing in happens in the app (`auth`, device flow) and the token is kept beside the node
//! credentials. **Nothing here works without it**, and saying so plainly is better than an
//! unauthenticated call failing further down.

pub mod api;
pub mod auth;
pub mod signing;

use api::Repo;

/// The GitHub repository the working directory pushes to, if any.
///
/// **Read from `.git/config` rather than by running git.** This is called inside a tool call's
/// budget, and spawning a process to learn a string that is sitting in a file is a cost with
/// nothing to show for it.
pub fn repo_of(cwd: &std::path::Path) -> Option<Repo> {
    let mut at = Some(cwd);
    while let Some(dir) = at {
        if let Ok(text) = std::fs::read_to_string(dir.join(".git/config")) {
            // `url = …` under any remote. **`origin` is not required** — a fork checkout often
            // calls the upstream something else, and the first GitHub URL is the right guess.
            for line in text.lines() {
                let Some((key, value)) = line.split_once('=') else { continue };
                if key.trim() != "url" {
                    continue;
                }
                let value = value.trim();
                if value.contains("github.com") {
                    return Repo::parse(value);
                }
            }
        }
        at = dir.parent();
    }
    None
}

/// GitHub's own words for a failure, as a wire error.
pub fn err(e: anyhow::Error) -> zyris::WireError {
    zyris::WireError::internal(e.to_string())
}

/// One remark against a line of the diff.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ReviewNote {
    /// The file, as the diff names it.
    pub path: String,
    /// The line **in the diff**, not in the file on disk — `git.diff` is where to read it from.
    pub line: u32,
    pub body: String,
}

/// **An unknown state is `open`, not an error.** A model that writes `"opened"` should get the
/// list it obviously meant rather than a failure it has to recover from.
pub fn normalise_state(given: &str) -> String {
    match given.trim().to_ascii_lowercase().as_str() {
        "closed" | "close" => "closed".into(),
        "all" | "any" | "*" => "all".into(),
        _ => "open".into(),
    }
}

/// What GitHub calls the verdict. **An unknown word is refused rather than guessed at** — the
/// difference between `approve` and `request_changes` is not something to be generous about.
pub fn review_event(given: &str) -> Option<&'static str> {
    match given.trim().to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
        "comment" | "" => Some("COMMENT"),
        "approve" | "approved" => Some("APPROVE"),
        "request_changes" | "changes" | "reject" => Some("REQUEST_CHANGES"),
        _ => None,
    }
}

/// **Zero means "as many as usual", not none.** An unset number arrives as 0, and answering with
/// an empty list would read as a repository with no issues.
pub fn limit_of(given: u32) -> usize {
    match given {
        0 => 30,
        n => (n as usize).min(100),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_or_odd_state_means_open() {
        assert_eq!(normalise_state(""), "open");
        assert_eq!(normalise_state("opened"), "open");
        assert_eq!(normalise_state("CLOSED"), "closed");
        assert_eq!(normalise_state("all"), "all");
    }

    /// **Zero means "as many as usual".** An unset number arrives as 0, and an empty list would
    /// read as a repository with nothing in it.
    #[test]
    fn an_unset_limit_is_not_a_limit_of_none() {
        assert_eq!(limit_of(0), 30);
        assert_eq!(limit_of(5), 5);
        // GitHub's own ceiling, so asking past it is not an error either.
        assert_eq!(limit_of(1000), 100);
    }

    /// **The repository is read out of `.git/config`, not by running git.** This happens inside a
    /// tool call's budget, and spawning a process to learn a string sitting in a file costs time
    /// with nothing to show for it.
    #[test]
    fn the_repository_here_is_read_from_the_git_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".git/config"),
            "[remote \"origin\"]\n\turl = git@github.com:attacca-cc/zyris-code.git\n",
        )
        .unwrap();
        let repo = repo_of(dir.path()).expect("the remote was not read");
        assert_eq!((repo.owner.as_str(), repo.name.as_str()), ("attacca-cc", "zyris-code"));

        // **From a subdirectory too** — an agent's working directory is often not the repo root.
        let deep = dir.path().join("crates/inner");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(repo_of(&deep).map(|r| r.name), Some("zyris-code".to_string()));
    }

    /// A directory with no GitHub remote answers nothing, rather than guessing.
    #[test]
    fn somewhere_that_is_not_a_github_checkout_answers_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "[remote \"origin\"]\n\turl = /srv/bare\n")
            .unwrap();
        assert_eq!(repo_of(dir.path()), None);
    }

    /// **A verdict is not guessed at.** The difference between approving a change and asking for
    /// more work is exactly the sort of thing to be strict about; being generous here would let a
    /// typo approve something.
    #[test]
    fn a_review_verdict_is_read_strictly() {
        assert_eq!(review_event("approve"), Some("APPROVE"));
        assert_eq!(review_event("request changes"), Some("REQUEST_CHANGES"));
        assert_eq!(review_event("request-changes"), Some("REQUEST_CHANGES"));
        assert_eq!(review_event("comment"), Some("COMMENT"));
        // **An unset verdict is a plain comment**, the one that changes nothing.
        assert_eq!(review_event(""), Some("COMMENT"));
        assert_eq!(review_event("lgtm"), None);
        assert_eq!(review_event("yes"), None);
    }

    /// **Not signed in is said, not failed at further down.** The message has to name the thing
    /// the person must do, because the agent cannot do it.
    #[test]
    fn without_a_sign_in_the_tools_say_what_is_missing() {
        // Only meaningful on a machine with no token; on one with a token this is vacuous, which
        // is why the message is asserted rather than the branch.
        let tools = crate::git::Git::new(std::path::PathBuf::from("/"));
        if auth::Accounts::load().for_role(auth::Role::User).is_none() {
            let Err(why) = tools.github_client(auth::Role::User) else { panic!("it must refuse") };
            let why = why.to_string();
            assert!(why.contains("/github login"), "{why}");
        }
    }
}
