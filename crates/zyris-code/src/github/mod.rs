//! GitHub, as a capability of this node.
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

use serde_json::{json, Value};

use api::{Github, Repo};

/// The GitHub tools, as announced.
pub struct GithubTools {
    /// Where the app is running, so the repository can be worked out when none is given.
    cwd: std::path::PathBuf,
}

impl GithubTools {
    pub fn new(cwd: std::path::PathBuf) -> GithubTools {
        GithubTools { cwd }
    }

    /// The client, or a refusal that says what to do about it.
    ///
    /// **Read at call time, not held.** Signing in with `/github login` has to take effect without
    /// restarting, and a client built at startup would have captured "not signed in" for good.
    fn client(&self) -> zyris::Result<Github> {
        let Some(account) = auth::Account::load() else {
            return Err(zyris::WireError::invalid_params(
                "not signed in to GitHub — the person running this node needs to run `/github login`",
            ));
        };
        Github::new(account.token).map_err(|e| zyris::WireError::internal(e.to_string()))
    }

    /// Which repository a call is about.
    ///
    /// **An empty argument means "the one we are in".** The agent usually has no idea what the
    /// remote is called, and making it find out first would cost a `terminal.exec` round trip to
    /// learn something this process can read directly.
    fn repo(&self, given: &str) -> zyris::Result<Repo> {
        if !given.trim().is_empty() {
            return Repo::parse(given).ok_or_else(|| {
                zyris::WireError::invalid_params(format!(
                    "`{given}` is not an owner/repo, and not a GitHub remote either"
                ))
            });
        }
        repo_of(&self.cwd).ok_or_else(|| {
            zyris::WireError::invalid_params(
                "no repository given, and this working directory has no GitHub remote",
            )
        })
    }
}

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

fn err(e: anyhow::Error) -> zyris::WireError {
    zyris::WireError::internal(e.to_string())
}

#[zyris::capability(name = "github", version = 1)]
pub trait GithubCap {
    /// Which GitHub account this node is signed in as. Answers the login, or says nobody is signed
    /// in — call it first if a later call fails on authentication.
    async fn me(&self) -> zyris::Result<Value>;

    /// Issues on a repository, newest first. `repo` is `owner/name`; leave it empty for the
    /// repository this working directory pushes to. `state` is `open`, `closed` or `all`.
    async fn issues(&self, repo: String, state: String, limit: u32) -> zyris::Result<Value>;

    /// One issue with its body and comments.
    async fn issue(&self, repo: String, number: u32) -> zyris::Result<Value>;

    /// Pull requests on a repository. `state` is `open`, `closed` or `all`.
    async fn pulls(&self, repo: String, state: String, limit: u32) -> zyris::Result<Value>;

    /// One pull request: its body, which files changed and by how much, and the review comments.
    /// **The patch is not included** — ask for it with `pull_diff` when it is actually wanted.
    async fn pull(&self, repo: String, number: u32) -> zyris::Result<Value>;

    /// The unified diff of a pull request. This can be large; read `pull` first to see whether it
    /// is worth it.
    async fn pull_diff(&self, repo: String, number: u32) -> zyris::Result<String>;

    /// Adds a comment to an issue or a pull request — they share a numbering.
    async fn comment(&self, repo: String, number: u32, body: String) -> zyris::Result<Value>;

    /// Opens an issue.
    async fn create_issue(
        &self,
        repo: String,
        title: String,
        body: String,
        labels: Vec<String>,
    ) -> zyris::Result<Value>;

    /// Opens a pull request. `head` is the branch with the changes, `base` the branch to merge
    /// into. **The branch must already be pushed** — this does not push anything.
    async fn create_pull(
        &self,
        repo: String,
        title: String,
        body: String,
        head: String,
        base: String,
        draft: bool,
    ) -> zyris::Result<Value>;
}

#[async_trait::async_trait]
impl GithubCap for GithubTools {
    async fn me(&self) -> zyris::Result<Value> {
        let login = self.client()?.me().await.map_err(err)?;
        let here = repo_of(&self.cwd).map(|r| format!("{}/{}", r.owner, r.name));
        Ok(json!({"login": login, "repository_here": here}))
    }

    async fn issues(&self, repo: String, state: String, limit: u32) -> zyris::Result<Value> {
        let repo = self.repo(&repo)?;
        let state = normalise_state(&state);
        self.client()?.issues(&repo, &state, limit_of(limit)).await.map_err(err)
    }

    async fn issue(&self, repo: String, number: u32) -> zyris::Result<Value> {
        let repo = self.repo(&repo)?;
        self.client()?.issue(&repo, number as u64).await.map_err(err)
    }

    async fn pulls(&self, repo: String, state: String, limit: u32) -> zyris::Result<Value> {
        let repo = self.repo(&repo)?;
        let state = normalise_state(&state);
        self.client()?.pulls(&repo, &state, limit_of(limit)).await.map_err(err)
    }

    async fn pull(&self, repo: String, number: u32) -> zyris::Result<Value> {
        let repo = self.repo(&repo)?;
        self.client()?.pull(&repo, number as u64).await.map_err(err)
    }

    async fn pull_diff(&self, repo: String, number: u32) -> zyris::Result<String> {
        let repo = self.repo(&repo)?;
        self.client()?.pull_diff(&repo, number as u64).await.map_err(err)
    }

    async fn comment(&self, repo: String, number: u32, body: String) -> zyris::Result<Value> {
        if body.trim().is_empty() {
            return Err(zyris::WireError::invalid_params("a comment with no words in it"));
        }
        let repo = self.repo(&repo)?;
        self.client()?.comment(&repo, number as u64, &body).await.map_err(err)
    }

    async fn create_issue(
        &self,
        repo: String,
        title: String,
        body: String,
        labels: Vec<String>,
    ) -> zyris::Result<Value> {
        if title.trim().is_empty() {
            return Err(zyris::WireError::invalid_params("an issue needs a title"));
        }
        let repo = self.repo(&repo)?;
        self.client()?.create_issue(&repo, &title, &body, &labels).await.map_err(err)
    }

    async fn create_pull(
        &self,
        repo: String,
        title: String,
        body: String,
        head: String,
        base: String,
        draft: bool,
    ) -> zyris::Result<Value> {
        if title.trim().is_empty() || head.trim().is_empty() || base.trim().is_empty() {
            return Err(zyris::WireError::invalid_params(
                "a pull request needs a title, a head branch and a base branch",
            ));
        }
        let repo = self.repo(&repo)?;
        self.client()?.create_pull(&repo, &title, &body, &head, &base, draft).await.map_err(err)
    }
}

/// **An unknown state is `open`, not an error.** A model that writes `"opened"` should get the
/// list it obviously meant rather than a failure it has to recover from.
fn normalise_state(given: &str) -> String {
    match given.trim().to_ascii_lowercase().as_str() {
        "closed" | "close" => "closed".into(),
        "all" | "any" | "*" => "all".into(),
        _ => "open".into(),
    }
}

/// **Zero means "as many as usual", not none.** An unset number arrives as 0, and answering with
/// an empty list would read as a repository with no issues.
fn limit_of(given: u32) -> usize {
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

    /// **Not signed in is said, not failed at further down.** The message has to name the thing
    /// the person must do, because the agent cannot do it.
    #[test]
    fn without_a_sign_in_the_tools_say_what_is_missing() {
        // Only meaningful on a machine with no token; on one with a token this is vacuous, which
        // is why the message is asserted rather than the branch.
        let tools = GithubTools::new(std::path::PathBuf::from("/"));
        if auth::Account::load().is_none() {
            let Err(why) = tools.client() else { panic!("it must refuse") };
            let why = why.to_string();
            assert!(why.contains("/github login"), "{why}");
        }
    }
}
