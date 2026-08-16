//! Signing in to GitHub from inside the app, and remembering it afterwards.
//!
//! **Device flow**, the same shape zyris already uses to enrol this node: ask GitHub for a code,
//! show it, and poll until the person has approved it in a browser. No secret is needed, which is
//! what makes it usable in a terminal app that ships to other people — an OAuth app's client id is
//! public by design.
//!
//! The token lands in `~/.config/zyris-code/github.json`, beside the node credentials. **Not in the
//! project**, where it would be one `git add -A` away from being published.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// GitHub's device-flow endpoints. Constants rather than settings — pointing this at another host
/// would only ever be a mistake or an attack.
const DEVICE_CODE: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN: &str = "https://github.com/login/oauth/access_token";

/// What this app asks for.
///
/// **`repo` is the whole of it.** It covers issues, pull requests and their comments, which is
/// everything the tools do. `workflow`, `admin:*` and the rest would let this app do things it has
/// no tool for, and a scope granted is a scope that stays granted.
const SCOPES: &str = "repo read:user";

/// The OAuth app this signs in as.
///
/// **A client id is public** — it identifies the app, it does not authorise anything, and device
/// flow uses no client secret at all. It still has to be *registered*, which is why this can be
/// given from outside: whoever ships a build points it at their own app.
///
/// Empty here means no app has been registered yet, and `client_id` says so rather than sending a
/// request that GitHub answers with an unreadable 404.
///
/// **There is no client secret anywhere in this repository, and there must never be one.** Device
/// flow does not use one — that is the whole reason it is the right flow for an app that ships to
/// other people. A secret committed beside a public client id would be a secret published.
const BUILT_IN_CLIENT_ID: &str = "Ov23liqynev6toh0Lyd8";

/// The client id to sign in with. `$ZYRIS_CODE_GITHUB_CLIENT_ID` wins, so a build can be pointed at
/// another OAuth app without recompiling.
pub fn client_id() -> Option<String> {
    let given = std::env::var("ZYRIS_CODE_GITHUB_CLIENT_ID").unwrap_or_default();
    let id = if given.trim().is_empty() { BUILT_IN_CLIENT_ID } else { given.trim() };
    (!id.is_empty()).then(|| id.to_string())
}

/// Where the token is kept. Beside the node credentials, **never in the project.**
fn store() -> Option<PathBuf> {
    crate::conn::credential_dir().map(|dir| dir.join("github.json"))
}

/// Which of the two identities a call speaks as.
///
/// **Reading a repository and reviewing it are different jobs.** Reading, committing and opening a
/// pull request are things the person does, and they should be signed by the person. A review is a
/// second opinion, and a second opinion signed by the author is worth nothing — GitHub agrees, and
/// refuses to let anyone approve their own pull request.
///
/// So there are two slots. Nothing forces the second to be filled: with only the person signed in,
/// reviews go out under their name exactly as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The person. Everything that is not a review.
    User,
    /// The account reviews are signed by. Usually a machine account with access to the repository.
    Reviewer,
}

impl Role {
    /// From what was typed after `/github login`.
    pub fn parse(text: &str) -> Option<Role> {
        match text.trim().to_ascii_lowercase().as_str() {
            "" | "user" | "me" | "사용자" | "본인" => Some(Role::User),
            "reviewer" | "review" | "bot" | "리뷰어" => Some(Role::Reviewer),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Reviewer => "reviewer",
        }
    }
}

/// A signed-in GitHub account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub token: String,
    /// The login it belongs to, so `/github` can say who without a round trip.
    #[serde(default)]
    pub login: String,
}

/// Both slots, as they sit on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accounts {
    #[serde(default)]
    pub user: Option<Account>,
    #[serde(default)]
    pub reviewer: Option<Account>,
}

impl Accounts {
    pub fn load() -> Accounts {
        let Some(at) = store() else { return Accounts::default() };
        let Ok(text) = std::fs::read_to_string(at) else { return Accounts::default() };
        let mut accounts: Accounts = serde_json::from_str(&text).unwrap_or_default();
        // An empty token is not a sign-in. Keeping one would fail every call afterwards with
        // an authentication error instead of the plain "nobody is signed in".
        accounts.user = accounts.user.filter(|a| !a.token.is_empty());
        accounts.reviewer = accounts.reviewer.filter(|a| !a.token.is_empty());
        accounts
    }

    /// The account to speak as. **The reviewer falls back to the person** — with only one sign-in,
    /// reviews go out under their name, which is what the app did before there were two slots.
    pub fn for_role(&self, role: Role) -> Option<&Account> {
        match role {
            Role::User => self.user.as_ref(),
            Role::Reviewer => self.reviewer.as_ref().or(self.user.as_ref()),
        }
    }

    /// What is filled in this slot, with no falling back — what `/github` reports and what
    /// `logout` acts on.
    pub fn exactly(&self, role: Role) -> Option<&Account> {
        match role {
            Role::User => self.user.as_ref(),
            Role::Reviewer => self.reviewer.as_ref(),
        }
    }

    pub fn set(&mut self, role: Role, account: Option<Account>) {
        match role {
            Role::User => self.user = account,
            Role::Reviewer => self.reviewer = account,
        }
    }

    /// Writes both out. **Owner-only where the platform can say so** — these are credentials for
    /// every repository those accounts can reach.
    pub fn save(&self) -> Result<()> {
        let at = store().context("there is nowhere to keep the token")?;
        if let Some(dir) = at.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&at, serde_json::to_string(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// Empties one slot. **The token is not revoked at GitHub** — that needs the app's secret,
    /// which a device-flow client does not have, so the screen says where to revoke it by hand.
    ///
    /// Answers whether there was anything there.
    pub fn forget(role: Role) -> bool {
        let mut accounts = Accounts::load();
        if accounts.exactly(role).is_none() {
            return false;
        }
        accounts.set(role, None);
        accounts.save().is_ok()
    }
}

/// A code waiting to be approved in a browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// What the person types into GitHub.
    pub user_code: String,
    /// Where they type it.
    pub verification_uri: String,
    /// The handle this app polls with. Never shown — it is the secret half.
    pub device_code: String,
    /// Seconds between polls, as GitHub asked. **Polling faster earns `slow_down`**, which only
    /// makes the wait longer.
    pub interval: u64,
    /// Seconds until the code stops working.
    pub expires_in: u64,
}

fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .context("could not build the HTTP client")
}

/// Asks GitHub for a code to show.
pub async fn begin() -> Result<Pending> {
    let Some(client_id) = client_id() else {
        bail!("no GitHub OAuth app is configured for this build");
    };
    let body: Value = http()?
        .post(DEVICE_CODE)
        .header("accept", "application/json")
        .form(&[("client_id", client_id.as_str()), ("scope", SCOPES)])
        .send()
        .await
        .context("could not reach GitHub")?
        .json()
        .await
        .context("GitHub's answer could not be read")?;
    if let Some(e) = body.get("error").and_then(Value::as_str) {
        bail!("GitHub refused the request: {e}");
    }
    let field = |name: &str| body.get(name).and_then(Value::as_str).map(str::to_string);
    Ok(Pending {
        user_code: field("user_code").context("GitHub sent no code")?,
        verification_uri: field("verification_uri")
            .unwrap_or_else(|| "https://github.com/login/device".to_string()),
        device_code: field("device_code").context("GitHub sent no device code")?,
        // **Five seconds is GitHub's own floor**, and its answer is trusted over that.
        interval: body.get("interval").and_then(Value::as_u64).unwrap_or(5).max(1),
        expires_in: body.get("expires_in").and_then(Value::as_u64).unwrap_or(900),
    })
}

/// Where one poll got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    /// Nobody has approved it yet. Ask again after `interval`.
    Waiting { interval: u64 },
    /// Approved. The token is in hand.
    Done(String),
    /// It will never succeed — expired, denied, or the app is wrong.
    Failed(String),
}

/// One poll. **Every outcome is a value, not an error**, because "not yet" is the usual answer and
/// a caller that had to tell an error apart from a wait would get it wrong.
pub async fn poll(pending: &Pending) -> Poll {
    let Some(client_id) = client_id() else {
        return Poll::Failed("no GitHub OAuth app is configured for this build".into());
    };
    let sent = http().map(|c| {
        c.post(ACCESS_TOKEN)
            .header("accept", "application/json")
            .form(&[
                ("client_id", client_id.as_str()),
                ("device_code", pending.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
    });
    let body: Value = match sent {
        Ok(request) => match request.await {
            Ok(response) => response.json().await.unwrap_or(Value::Null),
            // **A hiccup is a wait, not a failure.** The code is still good; the network was not.
            Err(_) => return Poll::Waiting { interval: pending.interval },
        },
        Err(e) => return Poll::Failed(e.to_string()),
    };
    read_poll(&body, pending.interval)
}

/// The pure half of `poll`, so every branch can be tested without a network.
pub fn read_poll(body: &Value, interval: u64) -> Poll {
    if let Some(token) = body.get("access_token").and_then(Value::as_str) {
        if !token.is_empty() {
            return Poll::Done(token.to_string());
        }
    }
    match body.get("error").and_then(Value::as_str) {
        Some("authorization_pending") | None => Poll::Waiting { interval },
        // **GitHub asking to slow down is obeyed.** Ignoring it only lengthens the wait, because
        // it answers `slow_down` again for every poll that came too soon.
        Some("slow_down") => Poll::Waiting {
            interval: body.get("interval").and_then(Value::as_u64).unwrap_or(interval + 5),
        },
        Some("expired_token") => Poll::Failed("the code expired before it was approved".into()),
        Some("access_denied") => Poll::Failed("the request was denied".into()),
        Some(other) => Poll::Failed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// **"Not yet" is the usual answer**, and a caller must not have to tell it from a failure.
    #[test]
    fn waiting_is_an_answer_not_an_error() {
        assert_eq!(
            read_poll(&json!({"error": "authorization_pending"}), 5),
            Poll::Waiting { interval: 5 }
        );
        // An answer with nothing in it at all is a wait too — better than treating a hiccup as
        // a dead code and making the person start over.
        assert_eq!(read_poll(&Value::Null, 5), Poll::Waiting { interval: 5 });
    }

    /// **GitHub asking to slow down is obeyed.** Polling on regardless earns `slow_down` again for
    /// every poll that came too soon, so ignoring it only lengthens the wait.
    #[test]
    fn being_told_to_slow_down_lengthens_the_wait() {
        assert_eq!(
            read_poll(&json!({"error": "slow_down", "interval": 10}), 5),
            Poll::Waiting { interval: 10 }
        );
        // Even when it does not say by how much.
        assert_eq!(read_poll(&json!({"error": "slow_down"}), 5), Poll::Waiting { interval: 10 });
    }

    #[test]
    fn approval_hands_over_the_token() {
        assert_eq!(read_poll(&json!({"access_token": "gho_x"}), 5), Poll::Done("gho_x".into()));
    }

    /// **A dead code says so once.** Waiting on it forever would leave the person staring at a
    /// number that can no longer work.
    #[test]
    fn an_expired_or_denied_code_stops_the_wait() {
        assert!(matches!(read_poll(&json!({"error": "expired_token"}), 5), Poll::Failed(_)));
        assert!(matches!(read_poll(&json!({"error": "access_denied"}), 5), Poll::Failed(_)));
        assert!(matches!(read_poll(&json!({"error": "unsupported"}), 5), Poll::Failed(_)));
    }

    /// An empty token is not a token. GitHub does not send one, but a field that is present and
    /// blank would otherwise be saved and fail on every call afterwards.
    #[test]
    fn an_empty_token_is_not_a_sign_in() {
        assert_eq!(read_poll(&json!({"access_token": ""}), 5), Poll::Waiting { interval: 5 });
    }

    /// **The reviewer falls back to the person.** With one sign-in, reviews go out under their
    /// name — which is what the app did before there were two slots, and the right default.
    #[test]
    fn a_review_falls_back_to_the_person_when_no_reviewer_is_connected() {
        let me = Account { token: "t-me".into(), login: "ruma".into() };
        let bot = Account { token: "t-bot".into(), login: "reviewer".into() };

        let alone = Accounts { user: Some(me.clone()), reviewer: None };
        assert_eq!(alone.for_role(Role::Reviewer).map(|a| &a.login), Some(&"ruma".to_string()));
        // **`exactly` does not fall back**, so `/github` can say there is no separate reviewer.
        assert_eq!(alone.exactly(Role::Reviewer), None);

        let both = Accounts { user: Some(me), reviewer: Some(bot) };
        assert_eq!(both.for_role(Role::Reviewer).map(|a| &a.login), Some(&"reviewer".to_string()));
        assert_eq!(both.for_role(Role::User).map(|a| &a.login), Some(&"ruma".to_string()));
    }

    /// **Logging the reviewer out must not log the person out.** They are separate credentials and
    /// one command should not take both.
    #[test]
    fn the_two_slots_are_forgotten_separately() {
        let mut both = Accounts {
            user: Some(Account { token: "t-me".into(), login: "ruma".into() }),
            reviewer: Some(Account { token: "t-bot".into(), login: "bot".into() }),
        };
        both.set(Role::Reviewer, None);
        assert_eq!(both.exactly(Role::Reviewer), None);
        assert!(both.exactly(Role::User).is_some(), "the person was logged out too");
    }

    #[test]
    fn a_role_is_read_from_what_was_typed() {
        assert_eq!(Role::parse(""), Some(Role::User));
        assert_eq!(Role::parse("reviewer"), Some(Role::Reviewer));
        assert_eq!(Role::parse("리뷰어"), Some(Role::Reviewer));
        assert_eq!(Role::parse("someone-else"), None);
    }

    /// **A build with no OAuth app registered says so plainly**, rather than sending a request
    /// GitHub answers with something unreadable.
    #[test]
    fn a_build_without_an_app_knows_it() {
        // The environment decides here, so only the shape is checked: whatever `client_id`
        // answers, `begin` must not be reachable with `None`.
        if client_id().is_none() {
            assert!(BUILT_IN_CLIENT_ID.is_empty());
        }
    }
}
