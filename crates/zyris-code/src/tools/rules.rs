//! The repo's conventions (`CLAUDE.md`·`AGENTS.md`) as a tool the agent can call.
//!
//! Those files also go into the **session preamble** — but the preamble is fixed when the
//! session is created (`ZNewSession`) and can't change later. If a repo adds or edits a
//! `CLAUDE.md` after the session started, or the session was opened from a different working
//! directory, the agent never sees the current rules. This tool reads them on demand, so the
//! agent can (re)load them at any point — for example, right after connecting, or after a
//! repo's conventions change.
//!
//! It only reads, so the approval gate lets it through (`gate::decide`).

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Rules {
    cwd: PathBuf,
}

impl Rules {
    pub fn new(cwd: PathBuf) -> Rules {
        Rules { cwd }
    }

    /// The current `CLAUDE.md`·`AGENTS.md` collected for the working directory, preamble-formatted.
    /// `None` when there is nothing.
    pub fn load(&self) -> Option<String> {
        crate::instructions::preamble(&self.cwd)
    }

    /// What the session preamble says, read now rather than when the session was made.
    ///
    /// **Which node this is comes first, and it is always there.** A session opened as a job or a
    /// work has no preamble at all — `ZNewJob` and `ZNewWork` have no such field — so for those
    /// this tool is the only way the agent can find out whose machine it is holding.
    pub fn whole(&self) -> String {
        let here = crate::conn::node_preamble(&self.cwd);
        match self.load() {
            Some(rules) => format!("{here}\n\n{rules}"),
            None => format!("{here}\n\n(이 작업 디렉터리에는 CLAUDE.md∙AGENTS.md 지침이 없습니다)"),
        }
    }
}

#[zyris::capability(name = "rules", version = 1)]
pub trait RulesCap {
    /// Which node this conversation is coming from, and the `CLAUDE.md`·`AGENTS.md` conventions
    /// of its working directory. Read it at the start of a task, and again whenever the repo's
    /// conventions might have changed — the session preamble is fixed at creation and can go
    /// stale, and a job or work session never had one.
    async fn load(&self) -> zyris::Result<String>;
}

#[async_trait::async_trait]
impl RulesCap for Rules {
    async fn load(&self) -> zyris::Result<String> {
        Ok(self.whole())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(at: &std::path::Path, name: &str, body: &str) {
        std::fs::create_dir_all(at).unwrap();
        std::fs::write(at.join(name), body).unwrap();
    }

    /// Reading the rules returns the repo's conventions, just like the session preamble would.
    #[tokio::test]
    async fn load_returns_the_conventions() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "CLAUDE.md", "cargo fmt를 돌리지 말 것");
        let r = Rules::new(d.path().to_path_buf());
        let out = RulesCap::load(&r).await.unwrap();
        assert!(out.contains("cargo fmt"), "{out}");
    }

    /// A directory with no conventions returns an explicit notice, not an error — an empty
    /// string would read as a broken tool.
    #[tokio::test]
    async fn load_with_nothing_says_so() {
        let d = tempfile::tempdir().unwrap();
        let r = Rules::new(d.path().to_path_buf());
        let out = RulesCap::load(&r).await.unwrap();
        assert!(out.contains("없습니다"), "{out}");
    }

    /// **Which node this is comes back even from a directory with no conventions.** A job or a
    /// work session gets no preamble at all, so this tool is the only place its agent can learn
    /// whose files it is about to open — and with several nodes on one account, "this repo" means
    /// nothing until it knows which of them is the person's.
    #[tokio::test]
    async fn load_says_which_node_it_is_speaking_from() {
        let d = tempfile::tempdir().unwrap();
        let r = Rules::new(d.path().to_path_buf());
        let out = RulesCap::load(&r).await.unwrap();
        assert!(out.contains(&crate::conn::node_name()), "the node is not named: {out}");
        assert!(out.contains(&d.path().display().to_string()), "the directory is missing: {out}");
        assert!(out.contains(std::env::consts::OS), "the platform is missing: {out}");
    }
}
