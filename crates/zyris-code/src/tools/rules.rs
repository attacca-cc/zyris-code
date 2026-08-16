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
}

#[zyris::capability(name = "rules", version = 1)]
pub trait RulesCap {
    /// The `CLAUDE.md`·`AGENTS.md` conventions that apply to the working directory. Read it at
    /// the start of a task and again whenever the repo's conventions might have changed — the
    /// session preamble is fixed at creation and can go stale.
    async fn load(&self) -> zyris::Result<String>;
}

#[async_trait::async_trait]
impl RulesCap for Rules {
    async fn load(&self) -> zyris::Result<String> {
        Ok(match Rules::load(self) {
            Some(p) => p,
            None => "(이 작업 디렉터리에는 CLAUDE.md∙AGENTS.md 지침이 없습니다)".to_string(),
        })
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
}
