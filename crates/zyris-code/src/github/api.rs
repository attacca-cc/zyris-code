//! The GitHub REST calls the tools are built on.
//!
//! **This exists to spend fewer tokens than `gh` does.** An agent asked to look at a pull request
//! through `terminal.exec` runs `gh pr view --json …`, gets a wall of JSON back, and pays for all
//! of it — every field GitHub felt like sending, most of them URLs to other things. So each call
//! here reduces the answer to what a person would have read, and nothing else. It also means the
//! machine does not need `gh` installed at all.
//!
//! **Reduction happens here, not at the tool.** `slim_*` are pure functions over the JSON, so what
//! the agent ends up seeing can be tested without a network.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

const API: &str = "https://api.github.com";

/// What GitHub wants to see. **Without a `user-agent` every request is rejected**, which reads as
/// a broken token rather than a missing header.
const AGENT: &str = concat!("zyris-code/", env!("CARGO_PKG_VERSION"));

pub struct Github {
    http: reqwest::Client,
    token: String,
}

impl Github {
    pub fn new(token: String) -> Result<Github> {
        let http = reqwest::Client::builder()
            // **Inside the tool call's own budget.** attacca cuts a node call at 60s, so a request
            // that outlasts that is a worse answer than saying it timed out.
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .context("could not build the HTTP client")?;
        Ok(Github { http, token })
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let response = self
            .http
            .get(format!("{API}{path}"))
            .bearer_auth(&self.token)
            .header("user-agent", AGENT)
            .header("accept", "application/vnd.github+json")
            .query(query)
            .send()
            .await
            .with_context(|| format!("could not reach GitHub for {path}"))?;
        read(response).await
    }

    /// A GET whose answer is text, not JSON — a diff or a patch.
    async fn get_raw(&self, path: &str, accept: &str) -> Result<String> {
        let response = self
            .http
            .get(format!("{API}{path}"))
            .bearer_auth(&self.token)
            .header("user-agent", AGENT)
            .header("accept", accept)
            .send()
            .await
            .with_context(|| format!("could not reach GitHub for {path}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("GitHub answered {status}: {}", first_line(&text));
        }
        Ok(text)
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let response = self
            .http
            .post(format!("{API}{path}"))
            .bearer_auth(&self.token)
            .header("user-agent", AGENT)
            .header("accept", "application/vnd.github+json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("could not reach GitHub for {path}"))?;
        read(response).await
    }

    /// Who the token belongs to.
    pub async fn me(&self) -> Result<String> {
        let body = self.get("/user", &[]).await?;
        Ok(body.get("login").and_then(Value::as_str).unwrap_or_default().to_string())
    }

    /// The account, and what this token is allowed to do with it.
    ///
    /// **The scopes come off the header, not from anything remembered.** A token saved before a
    /// feature existed has the scopes it was granted then, and asking GitHub is the only way to
    /// tell that apart from one granted today — guessing produces a request that fails with a 404
    /// that reads like the endpoint is gone.
    pub async fn account(&self) -> Result<Account> {
        let response = self
            .http
            .get(format!("{API}/user"))
            .bearer_auth(&self.token)
            .header("user-agent", AGENT)
            .header("accept", "application/vnd.github+json")
            .send()
            .await
            .context("could not reach GitHub for /user")?;
        let scopes = response
            .headers()
            .get("x-oauth-scopes")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = read(response).await?;
        Ok(Account {
            id: body.get("id").and_then(Value::as_u64).unwrap_or_default(),
            login: text_at(&body, "login"),
            scopes: split_scopes(&scopes),
        })
    }

    /// The pull request `branch` is on, with the counts and what CI says about its head.
    ///
    /// **Three calls, because no one of them answers all of it.** The list endpoint finds the
    /// number but leaves out `additions`/`deletions`; the single pull request has those and the
    /// head sha; the checks hang off that sha. This is why it is polled slowly and not with git.
    ///
    /// `Ok(None)` is the ordinary answer for a branch nobody has opened anything for.
    pub async fn branch_pull(
        &self,
        repo: &Repo,
        branch: &str,
    ) -> Result<Option<crate::repo::Pull>> {
        let listed = self
            .get(
                &format!("/repos/{}/{}/pulls", repo.owner, repo.name),
                &[
                    ("head", format!("{}:{branch}", repo.owner)),
                    ("state", "all".into()),
                    ("per_page", "10".into()),
                ],
            )
            .await?;
        // **The highest number, not the first.** A branch reused after its pull request landed
        // has two, and the old one is the one that is over.
        let Some(number) = listed
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.get("number").and_then(Value::as_u64))
            .max()
        else {
            return Ok(None);
        };

        let pull =
            self.get(&format!("/repos/{}/{}/pulls/{number}", repo.owner, repo.name), &[]).await?;
        let head = pull
            .get("head")
            .and_then(|h| h.get("sha"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // **Checks failing to answer is `Quiet`, not an error.** A repository with no CI, or a
        // token that cannot see it, must not take the pull request off the strip with it.
        let checks = match head.is_empty() {
            true => crate::repo::Checks::Quiet,
            false => self
                .get(
                    &format!("/repos/{}/{}/commits/{head}/check-runs", repo.owner, repo.name),
                    &[("per_page", "100".into())],
                )
                .await
                .map(|body| fold_checks(&body))
                .unwrap_or_default(),
        };
        Ok(Some(crate::repo::Pull {
            number,
            added: pull.get("additions").and_then(Value::as_u64).unwrap_or_default() as usize,
            removed: pull.get("deletions").and_then(Value::as_u64).unwrap_or_default() as usize,
            checks,
            merged: pull.get("merged_at").is_some_and(|m| !m.is_null()),
        }))
    }

    /// Puts a public key on the account. **Answers what is already there as success** — turning
    /// signing on twice must not fail on the second try.
    pub async fn add_gpg_key(&self, armored: &str, name: &str) -> Result<Value> {
        match self
            .post("/user/gpg_keys", json!({"armored_public_key": armored, "name": name}))
            .await
        {
            Ok(added) => Ok(added),
            Err(e) if already_there(&e) => Ok(Value::Null),
            Err(e) => Err(e),
        }
    }

    pub async fn issues(&self, repo: &Repo, state: &str, limit: usize) -> Result<Value> {
        let body = self
            .get(
                &format!("/repos/{}/{}/issues", repo.owner, repo.name),
                &[("state", state.into()), ("per_page", limit.min(100).to_string())],
            )
            .await?;
        Ok(json!(slim_list(&body, slim_issue)))
    }

    pub async fn issue(&self, repo: &Repo, number: u64) -> Result<Value> {
        let issue =
            self.get(&format!("/repos/{}/{}/issues/{number}", repo.owner, repo.name), &[]).await?;
        let comments = self
            .get(
                &format!("/repos/{}/{}/issues/{number}/comments", repo.owner, repo.name),
                &[("per_page", "50".into())],
            )
            .await
            .unwrap_or(Value::Null);
        let mut out = slim_issue(&issue);
        out["body"] = json!(issue.get("body").and_then(Value::as_str).unwrap_or_default());
        out["comments"] = json!(slim_list(&comments, slim_comment));
        Ok(out)
    }

    pub async fn pulls(&self, repo: &Repo, state: &str, limit: usize) -> Result<Value> {
        let body = self
            .get(
                &format!("/repos/{}/{}/pulls", repo.owner, repo.name),
                &[("state", state.into()), ("per_page", limit.min(100).to_string())],
            )
            .await?;
        Ok(json!(slim_list(&body, slim_pull)))
    }

    pub async fn pull(&self, repo: &Repo, number: u64) -> Result<Value> {
        let pull =
            self.get(&format!("/repos/{}/{}/pulls/{number}", repo.owner, repo.name), &[]).await?;
        let files = self
            .get(
                &format!("/repos/{}/{}/pulls/{number}/files", repo.owner, repo.name),
                &[("per_page", "100".into())],
            )
            .await
            .unwrap_or(Value::Null);
        let reviews = self
            .get(
                &format!("/repos/{}/{}/pulls/{number}/comments", repo.owner, repo.name),
                &[("per_page", "50".into())],
            )
            .await
            .unwrap_or(Value::Null);
        let mut out = slim_pull(&pull);
        out["body"] = json!(pull.get("body").and_then(Value::as_str).unwrap_or_default());
        out["files"] = json!(slim_list(&files, slim_file));
        out["review_comments"] = json!(slim_list(&reviews, slim_review_comment));
        // **What CI says, on the request itself.** Reading a pull request to decide what to do
        // next and then having to ask separately whether it is even green is a round trip for
        // something GitHub was going to be asked about anyway.
        if let Some(head) = pull.get("head").and_then(|h| h.get("sha")).and_then(Value::as_str) {
            let checks = self
                .get(
                    &format!("/repos/{}/{}/commits/{head}/check-runs", repo.owner, repo.name),
                    &[("per_page", "100".into())],
                )
                .await
                .map(|body| fold_checks(&body))
                .unwrap_or_default();
            out["checks"] = json!(match checks {
                crate::repo::Checks::Quiet => "none",
                crate::repo::Checks::Running => "running",
                crate::repo::Checks::Passed => "passed",
                crate::repo::Checks::Failed => "failed",
            });
        }
        Ok(out)
    }

    /// The patch itself. **Not folded into `pull`** — a big diff is exactly the thing that should
    /// be asked for on purpose rather than arriving with every look at a pull request.
    pub async fn pull_diff(&self, repo: &Repo, number: u64) -> Result<String> {
        self.get_raw(
            &format!("/repos/{}/{}/pulls/{number}", repo.owner, repo.name),
            "application/vnd.github.diff",
        )
        .await
    }

    pub async fn comment(&self, repo: &Repo, number: u64, body: &str) -> Result<Value> {
        let posted = self
            .post(
                &format!("/repos/{}/{}/issues/{number}/comments", repo.owner, repo.name),
                json!({ "body": body }),
            )
            .await?;
        Ok(json!({"url": posted.get("html_url").and_then(Value::as_str).unwrap_or_default()}))
    }

    pub async fn create_issue(
        &self,
        repo: &Repo,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<Value> {
        let mut payload = json!({"title": title, "body": body});
        if !labels.is_empty() {
            payload["labels"] = json!(labels);
        }
        let created =
            self.post(&format!("/repos/{}/{}/issues", repo.owner, repo.name), payload).await?;
        Ok(slim_issue(&created))
    }

    /// Submits a review. **A different endpoint from a comment** — this one carries a verdict and
    /// can hang remarks off particular lines of the diff.
    pub async fn review(
        &self,
        repo: &Repo,
        number: u64,
        event: &str,
        body: &str,
        comments: &[crate::github::ReviewNote],
    ) -> Result<Value> {
        let mut payload = json!({"event": event, "body": body});
        if !comments.is_empty() {
            payload["comments"] = json!(comments
                .iter()
                .map(|c| json!({"path": c.path, "line": c.line, "body": c.body}))
                .collect::<Vec<_>>());
        }
        let posted = self
            .post(&format!("/repos/{}/{}/pulls/{number}/reviews", repo.owner, repo.name), payload)
            .await?;
        Ok(json!({
            "state": posted.get("state").and_then(Value::as_str).unwrap_or_default(),
            "url": posted.get("html_url").and_then(Value::as_str).unwrap_or_default(),
        }))
    }

    /// Asks people to review. **Answers who is now on the hook**, because GitHub silently drops a
    /// name it will not accept — someone without access to the repository, or the author.
    pub async fn request_review(
        &self,
        repo: &Repo,
        number: u64,
        reviewers: &[String],
    ) -> Result<Value> {
        let posted = self
            .post(
                &format!("/repos/{}/{}/pulls/{number}/requested_reviewers", repo.owner, repo.name),
                json!({ "reviewers": reviewers }),
            )
            .await?;
        let asked: Vec<String> = posted
            .get("requested_reviewers")
            .and_then(Value::as_array)
            .map(|r| r.iter().map(|u| text_at(u, "login")).collect())
            .unwrap_or_default();
        Ok(json!({ "requested": asked }))
    }

    pub async fn create_pull(
        &self,
        repo: &Repo,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
        draft: bool,
    ) -> Result<Value> {
        let created = self
            .post(
                &format!("/repos/{}/{}/pulls", repo.owner, repo.name),
                json!({"title": title, "body": body, "head": head, "base": base, "draft": draft}),
            )
            .await?;
        Ok(slim_pull(&created))
    }
}

/// Reads a response, turning GitHub's own error wording into the failure.
///
/// **GitHub's message is passed through.** Rewriting "Validation Failed: base is invalid" into
/// "could not create the pull request" would throw away the only part that says what to fix.
async fn read(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let said = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| first_line(&text));
        bail!("GitHub answered {status}: {said}");
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// What a pile of check runs adds up to.
///
/// **Anything still going wins, then anything failed.** A run that is half green and half still
/// working is not passing yet, and saying so early is how a person comes to stop reading the mark.
///
/// Pure, so every combination can be checked without a network.
pub fn fold_checks(body: &Value) -> crate::repo::Checks {
    use crate::repo::Checks;
    let runs = body.get("check_runs").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    if runs.is_empty() {
        return Checks::Quiet;
    }
    let mut failed = false;
    let mut counted = 0usize;
    for run in runs {
        let status = text_at(run, "status");
        if status != "completed" {
            return Checks::Running;
        }
        counted += 1;
        match text_at(run, "conclusion").as_str() {
            "failure" | "timed_out" | "cancelled" | "action_required" | "startup_failure" => {
                failed = true
            }
            // `neutral`, `skipped` and `success` are all "not in the way".
            _ => {}
        }
    }
    match (counted, failed) {
        (0, _) => Checks::Quiet,
        (_, true) => Checks::Failed,
        (_, false) => Checks::Passed,
    }
}

/// Whether a failure is GitHub saying it already has this.
///
/// **Not an error to us.** Setting signing up again after a reinstall finds the same key already
/// on the account, and failing there would leave the person with no way forward that is not
/// "delete the key on github.com first".
fn already_there(e: &anyhow::Error) -> bool {
    let said = e.to_string().to_ascii_lowercase();
    said.contains("already been taken") || said.contains("key_id already exists")
}

/// The `x-oauth-scopes` header, as a list.
///
/// Pure, because the whole decision about whether to send somebody through the browser again
/// hangs off it.
pub fn split_scopes(header: &str) -> Vec<String> {
    header.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// An account and what this token may do with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Account {
    pub id: u64,
    pub login: String,
    pub scopes: Vec<String>,
}

impl Account {
    pub fn may(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    line.chars().take(200).collect()
}

// ── Cutting the answers down ─────────────────────────────────────────────────────────────────

/// An owner and a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub owner: String,
    pub name: String,
}

impl Repo {
    /// Reads `owner/repo` out of what the agent passed, or out of a git remote URL.
    ///
    /// **Both shapes, because both are what is at hand.** The agent knows `attacca-cc/zyris-code`;
    /// the working directory knows `git@github.com:attacca-cc/zyris-code.git`.
    pub fn parse(text: &str) -> Option<Repo> {
        let text = text.trim().trim_end_matches('/').trim_end_matches(".git");
        // **A bare name must be exactly two pieces.** Taking the last two of anything would read
        // `../../etc/passwd` as the repository `etc/passwd` — a wrong guess dressed up as an
        // answer. Only a URL is allowed to have a path in front of it.
        let is_url = text.contains("://") || text.starts_with("git@");
        if !is_url && text.split('/').count() != 2 {
            return None;
        }
        let tail = text.rsplit(['/', ':']).take(2).collect::<Vec<_>>();
        if tail.len() != 2 {
            return None;
        }
        let (name, owner) = (tail[0], tail[1]);
        let ok = |s: &str| {
            !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        };
        (ok(owner) && ok(name)).then(|| Repo { owner: owner.into(), name: name.into() })
    }
}

fn slim_list(body: &Value, one: fn(&Value) -> Value) -> Vec<Value> {
    body.as_array().map(|items| items.iter().map(one).collect()).unwrap_or_default()
}

fn text_at(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn login(v: &Value) -> String {
    v.get("user").and_then(|u| u.get("login")).and_then(Value::as_str).unwrap_or_default().into()
}

/// **The head of an issue, and nothing else.** GitHub's own answer is around sixty fields, most of
/// them URLs to other API calls; the body is fetched only when one issue is asked for by number.
pub fn slim_issue(v: &Value) -> Value {
    json!({
        "number": v.get("number").and_then(Value::as_u64).unwrap_or_default(),
        "title": text_at(v, "title"),
        "state": text_at(v, "state"),
        "author": login(v),
        "labels": v.get("labels").and_then(Value::as_array).map(|l| {
            l.iter().map(|x| text_at(x, "name")).collect::<Vec<_>>()
        }).unwrap_or_default(),
        "comments": v.get("comments").and_then(Value::as_u64).unwrap_or_default(),
        "updated_at": text_at(v, "updated_at"),
        "url": text_at(v, "html_url"),
    })
}

pub fn slim_pull(v: &Value) -> Value {
    let branch = |side: &str| {
        v.get(side)
            .and_then(|b| b.get("ref"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut out = json!({
        "number": v.get("number").and_then(Value::as_u64).unwrap_or_default(),
        "title": text_at(v, "title"),
        // **Merged is not a state GitHub reports.** It answers `state: "closed"` and a separate
        // `merged_at`, and an agent reading only the state would call a merged branch abandoned.
        "state": match v.get("merged_at").is_some_and(|m| !m.is_null()) {
            true => "merged".to_string(),
            false => text_at(v, "state"),
        },
        "draft": v.get("draft").and_then(Value::as_bool).unwrap_or(false),
        "author": login(v),
        "head": branch("head"),
        "base": branch("base"),
        "updated_at": text_at(v, "updated_at"),
        "url": text_at(v, "html_url"),
    });
    // **Only when GitHub sent them.** The list endpoint leaves these out entirely, and writing
    // `+0 -0` there would say a pull request changed nothing rather than that nobody asked.
    for (ours, theirs) in [("added", "additions"), ("removed", "deletions")] {
        if let Some(n) = v.get(theirs).and_then(Value::as_u64) {
            out[ours] = json!(n);
        }
    }
    out
}

fn slim_comment(v: &Value) -> Value {
    json!({"author": login(v), "at": text_at(v, "created_at"), "body": text_at(v, "body")})
}

/// A review comment carries where it is, which is most of what makes it worth reading.
fn slim_review_comment(v: &Value) -> Value {
    json!({
        "author": login(v),
        "path": text_at(v, "path"),
        "line": v.get("line").and_then(Value::as_u64),
        "body": text_at(v, "body"),
    })
}

/// **The counts, not the patch.** A `files` answer carries every changed file's whole diff inline,
/// which is the single largest thing GitHub will hand back — `pull_diff` is for when that is
/// actually wanted.
fn slim_file(v: &Value) -> Value {
    json!({
        "path": text_at(v, "filename"),
        "status": text_at(v, "status"),
        "added": v.get("additions").and_then(Value::as_u64).unwrap_or_default(),
        "removed": v.get("deletions").and_then(Value::as_u64).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The sizes appear only when GitHub sent them.** The list endpoint leaves `additions` and
    /// `deletions` out entirely, and answering `+0 -0` there would tell an agent a pull request
    /// changed nothing rather than that nobody asked.
    #[test]
    fn a_pull_request_carries_its_size_only_when_there_is_one_to_carry() {
        let listed = slim_pull(&json!({"number": 53, "title": "t", "state": "open"}));
        assert!(listed.get("added").is_none(), "{listed}");
        assert!(listed.get("removed").is_none(), "{listed}");

        let one = slim_pull(&json!({
            "number": 53, "title": "t", "state": "open", "additions": 118, "deletions": 30,
        }));
        assert_eq!(one["added"], 118);
        assert_eq!(one["removed"], 30);

        // **Merged is still not a state GitHub reports.** It answers closed and a separate
        // merged_at, and an agent reading only the state calls a merged branch abandoned.
        let merged = slim_pull(&json!({
            "number": 53, "state": "closed", "merged_at": "2026-08-18T00:00:00Z",
        }));
        assert_eq!(merged["state"], "merged");
    }

    /// **Still running beats failed beats passed.** A pull request whose checks are half green
    /// and half still working is not passing, and a mark that says it is gets stopped being read.
    #[test]
    fn checks_add_up_to_the_least_settled_thing_among_them() {
        use crate::repo::Checks;
        let runs = |v: Value| json!({ "check_runs": v });
        assert_eq!(fold_checks(&runs(json!([]))), Checks::Quiet, "no CI draws nothing");
        assert_eq!(fold_checks(&json!({})), Checks::Quiet, "and neither does an unreadable answer");
        assert_eq!(
            fold_checks(&runs(json!([
                {"status": "completed", "conclusion": "success"},
                {"status": "in_progress", "conclusion": null},
            ]))),
            Checks::Running,
        );
        assert_eq!(
            fold_checks(&runs(json!([
                {"status": "completed", "conclusion": "success"},
                {"status": "completed", "conclusion": "failure"},
            ]))),
            Checks::Failed,
        );
        assert_eq!(
            fold_checks(&runs(json!([
                {"status": "completed", "conclusion": "success"},
                {"status": "completed", "conclusion": "skipped"},
                {"status": "completed", "conclusion": "neutral"},
            ]))),
            Checks::Passed,
            "skipped and neutral are not in the way",
        );
        // A queued run has not started, which is still "not settled".
        assert_eq!(
            fold_checks(&runs(json!([{"status": "queued", "conclusion": null}]))),
            Checks::Running,
        );
    }

    /// **Scopes are read off the header, because a saved token carries what it was granted then.**
    /// A token from before signing existed cannot add a key, and the only way to tell it from one
    /// granted today is to ask — guessing produces a 404 that reads like a broken endpoint.
    #[test]
    fn what_a_token_may_do_is_read_from_what_github_answered() {
        let account = Account {
            id: 7,
            login: "ruma".into(),
            scopes: split_scopes("repo, read:user, write:gpg_key"),
        };
        assert!(account.may("write:gpg_key"));
        assert!(account.may("repo"));
        assert!(!account.may("workflow"));

        // An older token, and the header a token with nothing at all comes back with.
        let older = Account { scopes: split_scopes("repo,read:user"), ..Account::default() };
        assert!(!older.may("write:gpg_key"));
        assert!(Account { scopes: split_scopes(""), ..Account::default() }.scopes.is_empty());
        assert!(Account { scopes: split_scopes("  ,  "), ..Account::default() }.scopes.is_empty());
    }

    #[test]
    fn a_repo_is_read_from_either_a_name_or_a_remote() {
        let want = Repo { owner: "attacca-cc".into(), name: "zyris-code".into() };
        for text in [
            "attacca-cc/zyris-code",
            "https://github.com/attacca-cc/zyris-code",
            "https://github.com/attacca-cc/zyris-code.git",
            "git@github.com:attacca-cc/zyris-code.git",
            "attacca-cc/zyris-code/",
        ] {
            assert_eq!(Repo::parse(text).as_ref(), Some(&want), "{text}");
        }
        assert_eq!(Repo::parse("nonsense"), None);
        assert_eq!(Repo::parse(""), None);
        // **Nothing that could climb a path.** A repo name is not a path fragment.
        assert_eq!(Repo::parse("../../etc/passwd"), None);
    }

    /// **A merged pull request must not read as abandoned.** GitHub answers `state: "closed"` with
    /// a separate `merged_at`, and an agent reading only the state gets it exactly wrong.
    #[test]
    fn a_merged_pull_request_says_merged() {
        let merged = json!({"number": 1, "state": "closed", "merged_at": "2026-08-01T00:00:00Z"});
        assert_eq!(slim_pull(&merged)["state"], json!("merged"));
        let closed = json!({"number": 2, "state": "closed", "merged_at": Value::Null});
        assert_eq!(slim_pull(&closed)["state"], json!("closed"));
        let open = json!({"number": 3, "state": "open"});
        assert_eq!(slim_pull(&open)["state"], json!("open"));
    }

    /// **The point of this module is what it leaves out.** GitHub's issue answer is around sixty
    /// fields, nearly all of them URLs to further calls, and every one of them would be paid for.
    #[test]
    fn an_issue_is_cut_down_to_what_a_person_would_read() {
        let full = json!({
            "number": 7, "title": "제목", "state": "open",
            "user": {"login": "ruma", "id": 1, "avatar_url": "…", "followers_url": "…"},
            "labels": [{"name": "bug", "id": 9, "url": "…", "color": "f00"}],
            "comments": 3, "updated_at": "2026-08-01T00:00:00Z", "html_url": "https://x",
            "body": "본문", "reactions": {"url": "…"}, "timeline_url": "…", "repository_url": "…",
        });
        let slim = slim_issue(&full);
        assert_eq!(slim["number"], json!(7));
        assert_eq!(slim["author"], json!("ruma"));
        assert_eq!(slim["labels"], json!(["bug"]));
        assert_eq!(slim["url"], json!("https://x"));
        // The list form carries no body, and none of the machine-facing URLs survive.
        assert!(slim.get("body").is_none(), "{slim}");
        let keys: Vec<&String> = slim.as_object().unwrap().keys().collect();
        assert!(keys.len() <= 8, "too much came through: {keys:?}");
        assert!(!slim.to_string().contains("timeline_url"), "{slim}");
    }

    /// A changed file is counted, not quoted. GitHub inlines every file's whole patch in that
    /// answer, which is the largest thing it will hand back.
    #[test]
    fn a_changed_file_is_counted_rather_than_quoted() {
        let file = json!({
            "filename": "src/a.rs", "status": "modified", "additions": 3, "deletions": 1,
            "patch": "@@ -1 +1 @@\n-a\n+b",
        });
        let slim = slim_file(&file);
        assert_eq!(slim["path"], json!("src/a.rs"));
        assert_eq!(slim["added"], json!(3));
        assert!(!slim.to_string().contains("@@"), "the patch came through: {slim}");
    }

    /// Missing fields must not panic — what comes back over the wire can be anything.
    #[test]
    fn an_answer_missing_everything_still_reads() {
        let empty = json!({});
        assert_eq!(slim_issue(&empty)["number"], json!(0));
        assert_eq!(slim_pull(&empty)["author"], json!(""));
        assert_eq!(slim_file(&empty)["path"], json!(""));
    }
}
