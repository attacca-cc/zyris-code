//! Git on this machine, as a capability of this node.
//!
//! **Why not leave this to `terminal.exec`?** The agent can already run `git`, and for reading
//! around a repository it should. Three things it cannot do that way, and they are the three
//! reasons this exists:
//!
//! - **Signing.** A verified commit needs a key, an address and a `user.signingkey` lined up
//!   (`crate::github::signing`). Spelling that out in every `git commit` the agent writes is a
//!   recipe for commits that are signed some of the time.
//! - **Pushing.** An HTTPS remote wants a credential. The person is signed in to GitHub inside
//!   this app, and handing that token to a `git push` **without putting it in a command line** —
//!   where `ps` and every shell history would see it — is something only this side can arrange.
//! - **Plan mode.** `terminal.exec` is a write however innocent the command is, so plan mode has
//!   to refuse `git log` along with `git push`. Split into tools, reading a repository stays
//!   possible while changing it does not (`tools::gate::only_reads`).
//!
//! **Nothing here edits the person's configuration.** Every setting rides on the single `git`
//! invocation that needs it, so a commit made from their own shell is signed, or not, exactly as
//! it was before this app was installed.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::github::signing::Signing;
use crate::github::ReviewNote;

/// The env var the credential helper reads the token out of.
///
/// **A variable, not an argument.** Anything on a command line is visible in `ps` to every other
/// process on the machine, and lands in shell history when a person copies the line to try it.
const TOKEN_VAR: &str = "ZYRIS_CODE_GIT_TOKEN";

/// Reads the token out of the environment when git asks for one, and says nothing otherwise.
///
/// `git` runs this through a shell, so the shape is fixed by git, not chosen here. It answers only
/// the `get` operation: `store` and `erase` would have it writing the token to disk somewhere this
/// app does not manage.
const CREDENTIAL_HELPER: &str = concat!(
    "!f() { test \"$1\" = get && ",
    "printf 'username=x-access-token\\npassword=%s\\n' \"$",
    "ZYRIS_CODE_GIT_TOKEN\"; }; f"
);

pub struct Git {
    cwd: PathBuf,
}

/// What came back from one run of git.
struct Ran {
    ok: bool,
    out: String,
    err: String,
}

impl Ran {
    /// The failure as a person would read it. **git's own last line**, which is where it says
    /// what to do — "updates were rejected", "nothing to commit", "pathspec did not match".
    fn why(&self) -> String {
        let last = |t: &str| {
            t.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default().trim().to_string()
        };
        match last(&self.err) {
            reason if !reason.is_empty() => reason,
            _ => match last(&self.out) {
                reason if !reason.is_empty() => reason,
                _ => "git said nothing about why".to_string(),
            },
        }
    }
}

impl Git {
    pub fn new(cwd: PathBuf) -> Git {
        Git { cwd }
    }

    async fn git(&self, args: &[String], token: Option<String>) -> zyris::Result<Ran> {
        let mut command = tokio::process::Command::new("git");
        command.current_dir(&self.cwd).args(args);
        if let Some(token) = token {
            command.env(TOKEN_VAR, token);
        }
        if let Some(home) = crate::github::signing::home() {
            command.env("GNUPGHOME", home);
        }
        // Nothing here can answer a prompt: this app owns the screen, so a terminal prompt behind
        // it is a hang. Better to fail with git's own "could not read Username".
        command.env("GIT_TERMINAL_PROMPT", "0");
        let out = command
            .output()
            .await
            .map_err(|e| zyris::WireError::internal(format!("could not run git: {e}")))?;
        Ok(Ran {
            ok: out.status.success(),
            out: String::from_utf8_lossy(&out.stdout).to_string(),
            err: String::from_utf8_lossy(&out.stderr).to_string(),
        })
    }

    /// A read. Answers the text or a failure carrying git's own words.
    async fn read(&self, args: &[&str]) -> zyris::Result<String> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let ran = self.git(&args, None).await?;
        match ran.ok {
            true => Ok(ran.out),
            false => Err(zyris::WireError::internal(ran.why())),
        }
    }
}

/// How a commit will be made, worked out before anything runs so the answer can say so.
///
/// **Pure.** What the agent is told about signing and what git is actually handed come from the
/// same place; two of these would drift, and the one that drifted would be the report.
pub fn signing_args(signing: Option<&Signing>) -> Vec<String> {
    let Some(signing) = signing else { return Vec::new() };
    vec![
        "-c".into(),
        format!("gpg.program={}", crate::github::signing::program()),
        "-c".into(),
        format!("user.signingkey={}", signing.fingerprint),
        // **The address has to be the key's.** GitHub checks the commit's email against the UIDs
        // on the key it has; a signature over any other address is shown as unverified, which is
        // the one outcome worse than not signing at all.
        "-c".into(),
        format!("user.email={}", signing.email),
        "-c".into(),
        "commit.gpgsign=true".into(),
    ]
}

/// The `-c` pair that lets a push authenticate, when there is a token and the remote wants one.
///
/// **Only for HTTPS.** An SSH remote uses the person's own key and has no use for a credential
/// helper; installing one there would be noise in the one place a failure is hard to read.
pub fn push_args(remote_url: &str, have_token: bool) -> Vec<String> {
    let https = remote_url.starts_with("http://") || remote_url.starts_with("https://");
    if !https || !have_token {
        return Vec::new();
    }
    vec![
        // Emptying the list first drops whatever helper the machine has configured, so a stale
        // saved password cannot win over the account that is signed in here.
        "-c".into(),
        "credential.helper=".into(),
        "-c".into(),
        format!("credential.helper={CREDENTIAL_HELPER}"),
    ]
}

/// The paths in `git status --porcelain=v1` output, split by what has happened to them.
///
/// Pure, and the reason `status` costs one process rather than three.
pub fn split_status(text: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut staged, mut unstaged, mut untracked) = (Vec::new(), Vec::new(), Vec::new());
    for line in text.lines() {
        if line.len() < 4 {
            continue;
        }
        let (marks, path) = line.split_at(2);
        let path = path.trim().to_string();
        let mut marks = marks.chars();
        let (index, work) = (marks.next().unwrap_or(' '), marks.next().unwrap_or(' '));
        if index == '?' {
            untracked.push(path);
            continue;
        }
        if index != ' ' {
            staged.push(path.clone());
        }
        if work != ' ' {
            unstaged.push(path);
        }
    }
    (staged, unstaged, untracked)
}

/// One line of `git log --format=…`, as the agent sees it.
pub fn parse_log(text: &str) -> Vec<Value> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let mut fields = line.splitn(4, '\x1f');
            let sha = fields.next()?;
            let author = fields.next().unwrap_or_default();
            let at = fields.next().unwrap_or_default();
            let subject = fields.next().unwrap_or_default();
            Some(json!({"sha": sha, "author": author, "at": at, "subject": subject}))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **With nothing set up, git is handed nothing.** An empty `user.signingkey` is not "no
    /// signing", it is a signing attempt with no key, and git fails the commit over it.
    #[test]
    fn without_a_key_no_signing_settings_are_passed() {
        assert!(signing_args(None).is_empty());
    }

    /// **The address goes with the key.** GitHub matches the commit's email against the UIDs on
    /// the key it holds, so a commit signed but authored as someone else shows as unverified —
    /// which costs the key and buys nothing.
    #[test]
    fn signing_carries_the_key_and_the_address_it_was_made_for() {
        let signing = Signing {
            fingerprint: "AAAA1111".into(),
            email: "1+ruma@users.noreply.github.com".into(),
            login: "ruma".into(),
        };
        let args = signing_args(Some(&signing)).join(" ");
        assert!(args.contains("user.signingkey=AAAA1111"), "{args}");
        assert!(args.contains("user.email=1+ruma@users.noreply.github.com"), "{args}");
        assert!(args.contains("commit.gpgsign=true"), "{args}");
    }

    /// **The token never appears in an argument.** Everything on a command line is readable by
    /// every process on the machine through `ps`, and lands in shell history the moment a person
    /// copies the line to try it by hand. The helper reads it out of the environment instead.
    #[test]
    fn a_push_credential_is_never_written_into_the_command_line() {
        let args = push_args("https://github.com/attacca-cc/zyris-code.git", true).join(" ");
        assert!(args.contains("credential.helper"), "{args}");
        assert!(args.contains(TOKEN_VAR), "the helper must read the variable: {args}");
        assert!(!args.contains("ghp_"), "{args}");
        assert!(!args.contains("ghu_"), "{args}");
    }

    /// An SSH remote has the person's own key and no use for a helper; installing one there is
    /// noise in the place a failure is hardest to read.
    #[test]
    fn an_ssh_remote_is_left_to_the_key_the_person_already_has() {
        assert!(push_args("git@github.com:attacca-cc/zyris-code.git", true).is_empty());
        assert!(push_args("ssh://git@github.com/attacca-cc/zyris-code.git", true).is_empty());
        // And with nobody signed in there is nothing to offer either way.
        assert!(push_args("https://github.com/attacca-cc/zyris-code.git", false).is_empty());
    }

    /// **A file can be in two lists at once.** Staging a change and then editing the file again is
    /// ordinary, and reporting only one of the two hides the half that is about to be left behind.
    #[test]
    fn a_file_staged_and_then_edited_again_shows_up_on_both_sides() {
        let (staged, unstaged, untracked) = split_status(
            "M  src/app.rs\n M src/repo.rs\nMM src/git.rs\n?? notes.txt\nA  src/new.rs\n",
        );
        assert_eq!(staged, ["src/app.rs", "src/git.rs", "src/new.rs"]);
        assert_eq!(unstaged, ["src/repo.rs", "src/git.rs"]);
        assert_eq!(untracked, ["notes.txt"]);
    }

    /// Nothing changed is three empty lists, not a failure.
    #[test]
    fn a_clean_tree_lists_nothing() {
        let (staged, unstaged, untracked) = split_status("");
        assert!(staged.is_empty() && unstaged.is_empty() && untracked.is_empty());
    }

    /// **A subject may contain anything, including the separator's neighbours.** Splitting on a
    /// unit separator and stopping at four fields keeps a subject with tabs or colons in it whole.
    #[test]
    fn a_log_line_keeps_a_subject_that_has_punctuation_in_it() {
        let text = "abc123\x1fruma\x1f2026-08-18\x1ffeat(git): add a: thing\x1f and more\n";
        let got = parse_log(text);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["sha"], "abc123");
        assert_eq!(got[0]["subject"], "feat(git): add a: thing\x1f and more");
    }
}

// ── The capability ───────────────────────────────────────────────────────────────────────────
//
// **One capability, not two.** git and GitHub were announced separately, which put eighteen tools
// in front of the agent and three pairs among them that did the same job from different sides:
// `pull_diff` next to `diff`, `me` next to `status`, and `issue`/`issues` and `pull`/`pulls`
// differing by a single letter. A model reaching for one and getting the other is not a mistake it
// can see it made. They are one subject — the repository you are in and the place it is hosted —
// so they are one capability, and where two tools overlapped an argument now says which is wanted.

fn oops(text: impl Into<String>) -> zyris::WireError {
    zyris::WireError::invalid_params(text)
}

impl Git {
    /// The GitHub client, or a refusal that says what to do about it.
    ///
    /// **Read at call time, not held.** Signing in with `/github login` has to take effect without
    /// restarting, and a client built at startup would have captured "not signed in" for good.
    pub(crate) fn github_client(
        &self,
        role: crate::github::auth::Role,
    ) -> zyris::Result<crate::github::api::Github> {
        let accounts = crate::github::auth::Accounts::load();
        let Some(account) = accounts.for_role(role) else {
            return Err(oops(
                "not signed in to GitHub ‒ the person running this node needs to run `/github login`",
            ));
        };
        crate::github::api::Github::new(account.token.clone())
            .map_err(|e| zyris::WireError::internal(e.to_string()))
    }

    /// Which repository a call is about.
    ///
    /// **An empty argument means "the one we are in".** The agent usually has no idea what the
    /// remote is called, and making it find out first would cost a round trip to learn something
    /// this process can read out of `.git/config`.
    fn github_repo(&self, given: &str) -> zyris::Result<crate::github::api::Repo> {
        if !given.trim().is_empty() {
            return crate::github::api::Repo::parse(given).ok_or_else(|| {
                oops(format!("`{given}` is not an owner/repo, and not a GitHub remote either"))
            });
        }
        crate::github::repo_of(&self.cwd).ok_or_else(|| {
            oops("no repository given, and this working directory has no GitHub remote")
        })
    }
}

#[zyris::capability(name = "git", version = 2)]
pub trait GitCap {
    /// Where this is and who it is: the branch, how far it is from its upstream, which files are
    /// staged, changed but not staged, or untracked — and the GitHub account and repository the
    /// calls below would act as and on.
    ///
    /// **Nothing here goes over the network.** The account is read from where signing in put it,
    /// so this stays the cheap call it looks like.
    async fn status(&self) -> zyris::Result<Value>;

    /// Recent commits, newest first.
    async fn log(&self, limit: u32) -> zyris::Result<Value>;

    /// A unified diff. **Three sources, one tool** — give `number` for a pull request's diff as
    /// GitHub has it, `against` for how far this branch has come from another, or neither for
    /// what is uncommitted here. `staged` shows what a commit would take; leave `path` empty for
    /// everything. A pull request's diff can be large — read `pulls` first to see whether it is
    /// worth it.
    async fn diff(
        &self,
        staged: bool,
        path: String,
        against: String,
        number: u32,
        repo: String,
    ) -> zyris::Result<String>;

    /// The branches here, and which one is checked out.
    async fn branches(&self) -> zyris::Result<Value>;

    /// Moves to a branch, making it first when `create` is set.
    async fn switch(&self, branch: String, create: bool) -> zyris::Result<Value>;

    /// Commits. `paths` are staged first; with none given and `all` set, everything already
    /// tracked goes in.
    ///
    /// **Signed when the person has turned signing on** (`/github` on this node) — the answer says
    /// which it was, so a commit that could not be signed is never reported as one that was.
    async fn commit(&self, message: String, paths: Vec<String>, all: bool) -> zyris::Result<Value>;

    /// Pushes the branch that is checked out. **Sets the upstream when there is none**, which is
    /// what a first push of a new branch needs and what is otherwise a second call to find out.
    ///
    /// `force_with_lease` overwrites the remote branch **only if it is where we last saw it** —
    /// a plain force would throw away work that arrived in between.
    async fn push(&self, force_with_lease: bool) -> zyris::Result<Value>;

    /// Issues. **One tool for the list and for one of them**: give `number` for that issue with
    /// its body and comments, or leave it at 0 for the newest. `repo` is `owner/name`; leave it
    /// empty for the repository this working directory pushes to. `state` is `open`, `closed` or
    /// `all`, and is ignored when a number is given.
    async fn issues(
        &self,
        repo: String,
        number: u32,
        state: String,
        limit: u32,
    ) -> zyris::Result<Value>;

    /// Pull requests, the same way. With `number`, that one in full: its body, which files
    /// changed and by how much, what the checks say, and the review comments — **but not the
    /// patch**, which is `diff`.
    async fn pulls(
        &self,
        repo: String,
        number: u32,
        state: String,
        limit: u32,
    ) -> zyris::Result<Value>;

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
    /// into. **The branch must already be pushed** — use `push` first.
    async fn create_pull(
        &self,
        repo: String,
        title: String,
        body: String,
        head: String,
        base: String,
        draft: bool,
    ) -> zyris::Result<Value>;

    /// Submits a review on a pull request. **Not the same as a comment** — a review carries a
    /// verdict and can hang remarks off particular lines.
    ///
    /// `event` is `comment`, `approve` or `request_changes`. Each entry in `comments` needs a file
    /// path and a line number **as the diff numbers it**, which is what `diff` shows.
    ///
    /// **You cannot approve your own pull request.** GitHub refuses, and this node acts as the
    /// person who signed in — use `comment` there.
    async fn review(
        &self,
        repo: String,
        number: u32,
        event: String,
        body: String,
        comments: Vec<ReviewNote>,
    ) -> zyris::Result<Value>;

    /// Asks people to review a pull request. `reviewers` are GitHub logins.
    ///
    /// **Accounts and teams only.** There is no way to name an app: an OAuth app is not an
    /// identity of its own, it acts as whoever signed in.
    async fn request_review(
        &self,
        repo: String,
        number: u32,
        reviewers: Vec<String>,
    ) -> zyris::Result<Value>;
}

#[async_trait::async_trait]
impl GitCap for Git {
    async fn status(&self) -> zyris::Result<Value> {
        let text = self.read(&["--no-optional-locks", "status", "--porcelain=v1"]).await?;
        let (staged, unstaged, untracked) = split_status(&text);
        let branch = self.read(&["branch", "--show-current"]).await?.trim().to_string();
        // **No upstream is an ordinary answer, not a failure.** A branch that has never been
        // pushed is exactly the branch somebody is about to push.
        let upstream = self
            .read(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"])
            .await
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let (mut ahead, mut behind) = (0u64, 0u64);
        if upstream.is_some() {
            if let Ok(counts) =
                self.read(&["rev-list", "--left-right", "--count", "@{upstream}...HEAD"]).await
            {
                let mut parts = counts.split_whitespace();
                behind = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                ahead = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            }
        }
        // **Who, from disk.** This used to be a tool of its own that asked GitHub for a login it
        // had already written down when the token was saved — a round trip to repeat itself.
        let accounts = crate::github::auth::Accounts::load();
        let login = accounts.exactly(crate::github::auth::Role::User).map(|a| a.login.clone());
        let reviewer =
            accounts.exactly(crate::github::auth::Role::Reviewer).map(|a| a.login.clone());
        Ok(json!({
            "branch": branch,
            "upstream": upstream,
            "ahead": ahead,
            "behind": behind,
            "staged": staged,
            "unstaged": unstaged,
            "untracked": untracked,
            "clean": text.trim().is_empty(),
            "signing": Signing::load().map(|s| json!({"email": s.email, "key": s.fingerprint})),
            "github": json!({
                "login": login.clone(),
                "reviews_as": reviewer.clone().or(login),
                "separate_reviewer": reviewer.is_some(),
                "repository": crate::github::repo_of(&self.cwd)
                    .map(|r| format!("{}/{}", r.owner, r.name)),
            }),
        }))
    }

    async fn log(&self, limit: u32) -> zyris::Result<Value> {
        let count = match limit {
            0 => 20,
            n => n.min(200),
        };
        let text = self
            .read(&[
                "--no-optional-locks",
                "log",
                &format!("-{count}"),
                "--format=%h\x1f%an\x1f%ad\x1f%s",
                "--date=short",
            ])
            .await?;
        Ok(json!(parse_log(&text)))
    }

    async fn diff(
        &self,
        staged: bool,
        path: String,
        against: String,
        number: u32,
        repo: String,
    ) -> zyris::Result<String> {
        // A number means the diff lives on GitHub, and the branch may not even be checked out.
        if number > 0 {
            let repo = self.github_repo(&repo)?;
            return self
                .github_client(crate::github::auth::Role::User)?
                .pull_diff(&repo, number as u64)
                .await
                .map_err(crate::github::err);
        }
        let mut args: Vec<String> =
            ["--no-optional-locks", "diff"].iter().map(|a| a.to_string()).collect();
        if staged {
            args.push("--cached".into());
        }
        if !against.trim().is_empty() {
            // Three dots: measured from where the two last parted, so pulling the base in does not
            // credit this branch with everybody else's work.
            args.push(format!("{}...HEAD", against.trim()));
        }
        if !path.trim().is_empty() {
            args.push("--".into());
            args.push(path.trim().to_string());
        }
        let ran = self.git(&args, None).await?;
        match ran.ok {
            true => Ok(ran.out),
            false => Err(zyris::WireError::internal(ran.why())),
        }
    }

    async fn branches(&self) -> zyris::Result<Value> {
        let text = self
            .read(&["--no-optional-locks", "branch", "--format=%(refname:short)\x1f%(HEAD)"])
            .await?;
        let mut all = Vec::new();
        let mut current = String::new();
        for line in text.lines() {
            let (name, head) = line.split_once('\x1f').unwrap_or((line, ""));
            let name = name.trim().to_string();
            if name.is_empty() {
                continue;
            }
            if head.trim() == "*" {
                current = name.clone();
            }
            all.push(name);
        }
        Ok(json!({"current": current, "branches": all}))
    }

    async fn switch(&self, branch: String, create: bool) -> zyris::Result<Value> {
        let branch = branch.trim();
        if branch.is_empty() {
            return Err(oops("no branch named"));
        }
        let mut args = vec!["switch".to_string()];
        if create {
            args.push("-c".into());
        }
        args.push(branch.to_string());
        let ran = self.git(&args, None).await?;
        if !ran.ok {
            return Err(zyris::WireError::internal(ran.why()));
        }
        Ok(json!({"branch": branch}))
    }

    async fn commit(&self, message: String, paths: Vec<String>, all: bool) -> zyris::Result<Value> {
        if message.trim().is_empty() {
            return Err(oops("a commit needs a message"));
        }
        if !paths.is_empty() {
            let mut args = vec!["add".to_string(), "--".to_string()];
            args.extend(paths.iter().cloned());
            let ran = self.git(&args, None).await?;
            if !ran.ok {
                return Err(zyris::WireError::internal(ran.why()));
            }
        }

        let signing = Signing::load();
        let mut args = signing_args(signing.as_ref());
        args.push("commit".into());
        if all && paths.is_empty() {
            args.push("-a".into());
        }
        args.push("-m".into());
        args.push(message.clone());
        let ran = self.git(&args, None).await?;
        if !ran.ok {
            return Err(zyris::WireError::internal(ran.why()));
        }
        // **Asked, not assumed.** A key can be on the ring and still fail to sign — expired, or
        // gpg missing on this machine — and a commit reported as signed when it is not is worse
        // than one reported plainly.
        let sha = self.read(&["rev-parse", "--short", "HEAD"]).await.unwrap_or_default();
        let signed = self
            .read(&["log", "-1", "--format=%G?"])
            .await
            .map(|g| matches!(g.trim(), "G" | "U" | "E"))
            .unwrap_or(false);
        Ok(json!({
            "sha": sha.trim(),
            "subject": message.lines().next().unwrap_or_default(),
            "signed": signed,
            "signed_as": signing.map(|s| s.email),
        }))
    }

    async fn push(&self, force_with_lease: bool) -> zyris::Result<Value> {
        let branch = self.read(&["branch", "--show-current"]).await?.trim().to_string();
        if branch.is_empty() {
            return Err(oops("not on a branch ‒ nothing to push"));
        }
        let remote =
            self.read(&["remote"]).await?.lines().next().unwrap_or_default().trim().to_string();
        if remote.is_empty() {
            return Err(oops("this checkout has no remote to push to"));
        }
        let url = self.read(&["remote", "get-url", &remote]).await.unwrap_or_default();
        let token = crate::github::auth::Accounts::load()
            .for_role(crate::github::auth::Role::User)
            .map(|a| a.token.clone());

        let mut args = push_args(url.trim(), token.is_some());
        args.push("push".into());
        if force_with_lease {
            args.push("--force-with-lease".into());
        }
        // Setting the upstream every time is harmless and saves the second call a first push
        // would otherwise need.
        args.push("--set-upstream".into());
        args.push(remote.clone());
        args.push(branch.clone());
        let ran = self.git(&args, token).await?;
        if !ran.ok {
            return Err(zyris::WireError::internal(ran.why()));
        }
        Ok(json!({"remote": remote, "branch": branch, "said": ran.err.trim()}))
    }

    async fn issues(
        &self,
        repo: String,
        number: u32,
        state: String,
        limit: u32,
    ) -> zyris::Result<Value> {
        let repo = self.github_repo(&repo)?;
        let client = self.github_client(crate::github::auth::Role::User)?;
        match number {
            0 => client
                .issues(
                    &repo,
                    &crate::github::normalise_state(&state),
                    crate::github::limit_of(limit),
                )
                .await
                .map_err(crate::github::err),
            n => client.issue(&repo, n as u64).await.map_err(crate::github::err),
        }
    }

    async fn pulls(
        &self,
        repo: String,
        number: u32,
        state: String,
        limit: u32,
    ) -> zyris::Result<Value> {
        let repo = self.github_repo(&repo)?;
        let client = self.github_client(crate::github::auth::Role::User)?;
        match number {
            0 => client
                .pulls(
                    &repo,
                    &crate::github::normalise_state(&state),
                    crate::github::limit_of(limit),
                )
                .await
                .map_err(crate::github::err),
            n => client.pull(&repo, n as u64).await.map_err(crate::github::err),
        }
    }

    async fn comment(&self, repo: String, number: u32, body: String) -> zyris::Result<Value> {
        if body.trim().is_empty() {
            return Err(oops("a comment with no words in it"));
        }
        let repo = self.github_repo(&repo)?;
        self.github_client(crate::github::auth::Role::User)?
            .comment(&repo, number as u64, &body)
            .await
            .map_err(crate::github::err)
    }

    async fn create_issue(
        &self,
        repo: String,
        title: String,
        body: String,
        labels: Vec<String>,
    ) -> zyris::Result<Value> {
        if title.trim().is_empty() {
            return Err(oops("an issue needs a title"));
        }
        let repo = self.github_repo(&repo)?;
        self.github_client(crate::github::auth::Role::User)?
            .create_issue(&repo, &title, &body, &labels)
            .await
            .map_err(crate::github::err)
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
            return Err(oops("a pull request needs a title, a head branch and a base branch"));
        }
        let repo = self.github_repo(&repo)?;
        self.github_client(crate::github::auth::Role::User)?
            .create_pull(&repo, &title, &body, &head, &base, draft)
            .await
            .map_err(crate::github::err)
    }

    async fn review(
        &self,
        repo: String,
        number: u32,
        event: String,
        body: String,
        comments: Vec<ReviewNote>,
    ) -> zyris::Result<Value> {
        let event = crate::github::review_event(&event).ok_or_else(|| {
            oops(format!(
                "`{event}` is not a review verdict ‒ use comment, approve or request_changes"
            ))
        })?;
        // **A verdict with nothing to say is fine; a comment with nothing at all is not.** GitHub
        // rejects an empty COMMENT review, and the message it gives back says nothing useful.
        if event == "COMMENT" && body.trim().is_empty() && comments.is_empty() {
            return Err(oops("a review with no words and no line remarks says nothing"));
        }
        let repo = self.github_repo(&repo)?;
        self.github_client(crate::github::auth::Role::Reviewer)?
            .review(&repo, number as u64, event, &body, &comments)
            .await
            .map_err(crate::github::err)
    }

    async fn request_review(
        &self,
        repo: String,
        number: u32,
        reviewers: Vec<String>,
    ) -> zyris::Result<Value> {
        if reviewers.is_empty() {
            return Err(oops("no one to ask"));
        }
        let repo = self.github_repo(&repo)?;
        self.github_client(crate::github::auth::Role::Reviewer)?
            .request_review(&repo, number as u64, &reviewers)
            .await
            .map_err(crate::github::err)
    }
}

#[cfg(test)]
mod what_is_announced {
    use super::*;

    fn tools() -> Vec<String> {
        let server = GitCapServer(Git::new(std::path::PathBuf::from("/")));
        zyris::ServeCapability::descriptor(&server).tools.into_iter().map(|t| t.name).collect()
    }

    /// **One capability, and one tool per job.** git and GitHub were announced separately until
    /// 0.3.1, which put eighteen tools in front of the agent with three pairs among them doing the
    /// same job from different sides. The pairs are what this holds shut: a model reaching for
    /// `issue` when it meant `issues` is not a mistake it can see it made.
    #[test]
    fn the_tools_that_did_the_same_job_are_one_tool() {
        let tools = tools();
        for gone in ["me", "pull_diff", "issue", "pull"] {
            assert!(
                !tools.iter().any(|t| t == gone),
                "`{gone}` is back, and `{}` already does it: {tools:?}",
                match gone {
                    "me" => "status",
                    "pull_diff" => "diff",
                    _ => "the plural",
                },
            );
        }
        let mut want = [
            "status",
            "log",
            "diff",
            "branches",
            "switch",
            "commit",
            "push",
            "issues",
            "pulls",
            "comment",
            "create_issue",
            "create_pull",
            "review",
            "request_review",
        ]
        .map(str::to_string)
        .to_vec();
        want.sort();
        let mut got = tools;
        got.sort();
        assert_eq!(got, want);
    }

    /// **The wire name has to split into exactly four.** attacca builds
    /// `zyris__{node}__{capability}__{tool}` and reads it back by splitting on `__`, so a name
    /// carrying `__` inside it, or ending in `_`, breaks apart somewhere else. This repository has
    /// shipped that mistake twice, both times green locally and only visible live — and a new
    /// capability name is exactly when it happens again.
    #[test]
    fn every_wire_name_splits_into_four() {
        let server = GitCapServer(Git::new(std::path::PathBuf::from("/")));
        let descriptor = zyris::ServeCapability::descriptor(&server);
        assert_eq!(descriptor.name, "git");
        for tool in &descriptor.tools {
            let wire = format!("zyris__arch-zyris-code__{}__{}", descriptor.name, tool.name);
            let parts: Vec<&str> = wire.split("__").collect();
            assert_eq!(parts.len(), 4, "`{wire}` split into {parts:?}");
            assert_eq!(parts[3], tool.name);
        }
    }
}

/// Against a real repository, because the whole of this module is what git does when it is run.
///
/// **A repository made here, never the one this checkout sits in.** A test that commits into the
/// working tree it is running from is a test that eventually commits something nobody meant to.
#[cfg(test)]
mod against_a_real_repository {
    use super::*;

    /// A repository with one commit in it, and an identity of its own.
    ///
    /// **The identity is written into the repository, not into the environment.** Tests run beside
    /// each other in one process, so a `set_var` here would be read by whatever else is running.
    async fn repo() -> (tempfile::TempDir, Git) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let at = dir.path().to_path_buf();
        let run = |args: Vec<&str>| {
            let at = at.clone();
            let args: Vec<String> = args.into_iter().map(str::to_string).collect();
            async move {
                let out = tokio::process::Command::new("git")
                    .current_dir(&at)
                    .args(&args)
                    .output()
                    .await
                    .expect("git has to be installed to run these");
                assert!(
                    out.status.success(),
                    "git {args:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        };
        run(vec!["init", "-q", "-b", "main"]).await;
        run(vec!["config", "user.name", "A Tester"]).await;
        run(vec!["config", "user.email", "tester@example.invalid"]).await;
        // Signing is a machine-wide setting here; a repository under test must not pick it up.
        run(vec!["config", "commit.gpgsign", "false"]).await;
        std::fs::write(at.join("first.txt"), "one\n").expect("write");
        run(vec!["add", "."]).await;
        run(vec!["commit", "-q", "-m", "first"]).await;
        let git = Git::new(at);
        (dir, git)
    }

    /// **A clean tree says so, and a dirty one names what changed.** This is the answer a plan is
    /// written from, so "clean" has to mean it.
    #[tokio::test]
    async fn status_reads_the_branch_and_what_changed() {
        let (dir, git) = repo().await;
        let clean = git.status().await.expect("status");
        assert_eq!(clean["branch"], "main");
        assert_eq!(clean["clean"], true);
        assert_eq!(clean["upstream"], serde_json::Value::Null, "a fresh repository has no remote");

        std::fs::write(dir.path().join("first.txt"), "two\n").expect("write");
        std::fs::write(dir.path().join("new.txt"), "new\n").expect("write");
        let dirty = git.status().await.expect("status");
        assert_eq!(dirty["clean"], false);
        assert_eq!(dirty["unstaged"][0], "first.txt");
        assert_eq!(dirty["untracked"][0], "new.txt");
    }

    /// **Committing names the files it takes.** Staging by hand first and committing second is two
    /// round trips for what is one intention.
    #[tokio::test]
    async fn a_commit_takes_the_paths_it_was_given_and_answers_the_sha() {
        let (dir, git) = repo().await;
        std::fs::write(dir.path().join("second.txt"), "two\n").expect("write");
        let made = git
            .commit("feat: a second file".into(), vec!["second.txt".into()], false)
            .await
            .expect("commit");
        assert!(!made["sha"].as_str().unwrap_or_default().is_empty(), "{made}");
        assert_eq!(made["subject"], "feat: a second file");
        assert_eq!(git.status().await.expect("status")["clean"], true);

        let log = git.log(5).await.expect("log");
        assert_eq!(log[0]["subject"], "feat: a second file");
        assert_eq!(log[1]["subject"], "first");
    }

    /// **A commit with nothing staged is a failure, and it says git's own words.** Answering
    /// success there would have the agent believe work was saved that was not.
    #[tokio::test]
    async fn a_commit_with_nothing_to_commit_says_so() {
        let (_dir, git) = repo().await;
        let why = git.commit("nothing".into(), vec![], false).await.expect_err("must refuse");
        assert!(why.to_string().to_lowercase().contains("nothing"), "{why}");
        // And a commit with no message never reaches git at all.
        assert!(git.commit("  ".into(), vec![], false).await.is_err());
    }

    /// Making a branch, and being on it afterwards. **`branches` has to agree** — the two answers
    /// are read together and disagreeing is worse than either being missing.
    #[tokio::test]
    async fn a_branch_can_be_made_and_switched_to() {
        let (_dir, git) = repo().await;
        git.switch("feature".into(), true).await.expect("switch -c");
        assert_eq!(git.status().await.expect("status")["branch"], "feature");
        let listed = git.branches().await.expect("branches");
        assert_eq!(listed["current"], "feature");
        let names: Vec<String> = listed["branches"]
            .as_array()
            .expect("a list")
            .iter()
            .map(|b| b.as_str().unwrap_or_default().to_string())
            .collect();
        assert!(names.contains(&"main".to_string()) && names.contains(&"feature".to_string()));

        // Switching to something that is not there fails with git's reason, not with a guess.
        assert!(git.switch("nowhere".into(), false).await.is_err());
    }

    /// **`against` measures from where the two last parted.** Pulling the base in must not credit
    /// this branch with everybody else's work — the same three-dot rule the strip uses.
    #[tokio::test]
    async fn a_diff_can_be_taken_against_another_branch() {
        let (dir, git) = repo().await;
        git.switch("feature".into(), true).await.expect("switch -c");
        std::fs::write(dir.path().join("first.txt"), "one\ntwo\n").expect("write");
        git.commit("more".into(), vec!["first.txt".into()], false).await.expect("commit");

        let patch =
            git.diff(false, String::new(), "main".into(), 0, String::new()).await.expect("diff");
        assert!(patch.contains("+two"), "{patch}");
        // Nothing uncommitted, so the plain diff is empty while the one against main is not.
        let here = git.diff(false, String::new(), String::new(), 0, String::new());
        assert!(here.await.expect("diff").is_empty());
    }

    /// **Nowhere to push is said before anything is attempted.** A push that fails inside git
    /// answers with git's networking wording, which does not mention the missing remote.
    #[tokio::test]
    async fn pushing_without_a_remote_says_that_rather_than_failing_inside_git() {
        let (_dir, git) = repo().await;
        let why = git.push(false).await.expect_err("must refuse");
        assert!(why.to_string().contains("remote"), "{why}");
    }
}
