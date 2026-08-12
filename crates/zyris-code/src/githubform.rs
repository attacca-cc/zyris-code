//! The `/github` screen — who this node is signed in as, and how to change it.
//!
//! **Two identities, and they are connected in different ways.** The person signs in through the
//! browser (device flow, `github::auth`) because that is the only way to get a token for an account
//! you are logged into. The reviewer is a token that gets **pasted in**: a fine-grained personal
//! access token can be scoped to one repository and to pull requests alone, which an OAuth token
//! cannot — device flow would hand the reviewer the same `repo` scope the person has, and the whole
//! point of a separate reviewer is that it can do less.
//!
//! `/github login reviewer` still exists for the browser route. This screen is where the safer one
//! lives, because a token has to be pasted somewhere and there was nowhere to paste it.
//!
//! This module is pure. Signing in, saving and asking GitHub who a token belongs to are the I/O
//! site's job (`github_out` in `app.rs`).

use crate::input::Input;

/// A row of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Field {
    /// The person's account. Enter connects it through the browser, or disconnects it.
    #[default]
    User,
    /// The token reviews go out under. Typed or pasted.
    Reviewer,
}

/// What the screen is asking the I/O side to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// Start the browser flow for the person's account.
    LoginUser,
    /// Forget the person's account.
    LogoutUser,
    /// Save this token as the reviewer. **The login is looked up before saving** — a token that
    /// does not work must be caught here, not at the first review.
    SetReviewer(String),
    /// Forget the reviewer.
    ClearReviewer,
}

#[derive(Debug, Clone, Default)]
pub struct Form {
    /// The login the person is connected as, if any.
    pub user: Option<String>,
    /// The login reviews go out under, if a separate one is connected.
    pub reviewer: Option<String>,
    /// What is being typed into the reviewer row.
    pub token: Input,
    pub field: Field,
    /// What the last attempt said — an error, or that it worked. **Cleared on any keystroke that
    /// changes the form**, so a stale answer never sits under a field that has moved on.
    pub note: Option<String>,
    /// Whether something is in flight, so the screen can say so and the keys can stand down.
    pub busy: bool,
    /// A device code waiting to be approved, and where to approve it.
    ///
    /// **Drawn on the screen that asked for it**, because that is where the person is looking.
    /// The sign-in itself runs off the draw loop — waiting on it here froze the app for as long
    /// as the code was good for.
    pub pending: Option<(String, String)>,
}

impl Form {
    pub fn new(user: Option<String>, reviewer: Option<String>) -> Form {
        Form { user, reviewer, ..Form::default() }
    }

    /// Moves down a row, wrapping. With two rows either direction is the other one.
    pub fn next(&mut self) {
        self.field = match self.field {
            Field::User => Field::Reviewer,
            Field::Reviewer => Field::User,
        };
        self.note = None;
    }

    pub fn prev(&mut self) {
        self.next();
    }

    /// The field keystrokes go into. **Only the reviewer row takes text** — the person's row is a
    /// button, and letting it swallow keys would look like a field that never fills.
    pub fn typing(&mut self) -> Option<&mut Input> {
        match self.field {
            Field::User => None,
            Field::Reviewer => Some(&mut self.token),
        }
    }

    /// What Enter means on the row the cursor is on.
    ///
    /// **A row that is already filled offers to empty it.** One key doing "connect" and "disconnect"
    /// depending on the state is how every other list in this app behaves, and a second key for
    /// the second meaning would be one more thing to know.
    pub fn submit(&mut self) -> Option<Ask> {
        if self.busy {
            return None;
        }
        match self.field {
            Field::User => Some(if self.user.is_some() { Ask::LogoutUser } else { Ask::LoginUser }),
            Field::Reviewer => {
                let token = self.token.text.trim().to_string();
                if token.is_empty() {
                    // Nothing typed: Enter on a connected reviewer disconnects it, and on an empty
                    // one does nothing at all rather than sending a blank token to GitHub.
                    return self.reviewer.is_some().then_some(Ask::ClearReviewer);
                }
                Some(Ask::SetReviewer(token))
            }
        }
    }

    /// Records how an attempt turned out and clears what was typed when it worked. **The token is
    /// wiped on success** — it is a credential, and leaving it on screen after it has been stored
    /// serves nothing.
    pub fn settled(&mut self, note: String, worked: bool) {
        self.busy = false;
        self.pending = None;
        self.note = Some(note);
        if worked {
            self.token = Input::new();
        }
    }
}

/// What a token looks like on screen.
///
/// **Never the token itself.** It is a credential; the screen is shared over SSH, screenshotted and
/// scrolled back through. The length is shown so a paste that silently truncated is still visible.
pub fn masked(token: &str) -> String {
    let n = token.chars().count();
    if n == 0 {
        return String::new();
    }
    // The first few characters are the token's type (`github_pat_`, `ghp_`), which says whether
    // the right kind of thing was pasted and gives nothing away.
    let head: String = token.chars().take(hint_len(token)).collect();
    format!("{head}{} ({n})", "•".repeat(n.saturating_sub(head.chars().count()).min(24)))
}

/// How much of the front is safe to show — the type prefix and no more.
fn hint_len(token: &str) -> usize {
    for prefix in ["github_pat_", "ghp_", "gho_", "ghs_"] {
        if token.starts_with(prefix) {
            return prefix.chars().count();
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Form {
        Form::new(Some("ruma".into()), None)
    }

    /// **A connected row offers to disconnect.** One key for both meanings is how every other list
    /// here behaves.
    #[test]
    fn enter_connects_what_is_empty_and_disconnects_what_is_not() {
        let mut f = Form::new(None, None);
        assert_eq!(f.submit(), Some(Ask::LoginUser));
        f.user = Some("ruma".into());
        assert_eq!(f.submit(), Some(Ask::LogoutUser));
    }

    /// **A pasted token is what the reviewer row submits.** Device flow cannot produce a
    /// fine-grained token, and a fine-grained token is the only kind that can be narrowed to
    /// pull requests on one repository.
    #[test]
    fn the_reviewer_row_submits_what_was_pasted() {
        let mut f = form();
        f.next();
        f.typing().expect("the reviewer row must take text").insert_str("  github_pat_abc  ");
        assert_eq!(f.submit(), Some(Ask::SetReviewer("github_pat_abc".into())));
    }

    /// **The person's row is a button, not a field.** Letting it take keys would look like a field
    /// that never fills.
    #[test]
    fn the_persons_row_does_not_take_typing() {
        let mut f = form();
        assert!(f.typing().is_none());
    }

    /// Enter on an empty reviewer row with nothing connected does nothing — sending a blank token
    /// to GitHub would only produce an error nobody asked for.
    #[test]
    fn an_empty_reviewer_row_with_nothing_connected_asks_for_nothing() {
        let mut f = form();
        f.next();
        assert_eq!(f.submit(), None);
        f.reviewer = Some("bot".into());
        assert_eq!(f.submit(), Some(Ask::ClearReviewer));
    }

    /// **The token is wiped once it is stored.** It is a credential and leaving it on screen after
    /// it has been saved serves nothing.
    #[test]
    fn a_token_that_was_accepted_leaves_the_screen() {
        let mut f = form();
        f.next();
        f.typing().unwrap().insert_str("github_pat_abc");
        f.busy = true;
        f.settled("done".into(), true);
        assert!(f.token.text.is_empty(), "{:?}", f.token.text);
        assert!(!f.busy);
    }

    /// A token that was refused stays put, so it can be corrected rather than retyped.
    #[test]
    fn a_token_that_was_refused_stays_where_it_was_typed() {
        let mut f = form();
        f.next();
        f.typing().unwrap().insert_str("nope");
        f.settled("GitHub refused it".into(), false);
        assert_eq!(f.token.text, "nope");
        assert_eq!(f.note.as_deref(), Some("GitHub refused it"));
    }

    /// **The token is never drawn.** This screen is shared over SSH, screenshotted, and scrolled
    /// back through; the length is shown so a paste that truncated is still visible.
    #[test]
    fn a_token_is_never_shown_in_full() {
        let shown = masked("github_pat_11ABCDE_secretsecretsecret");
        assert!(shown.starts_with("github_pat_"), "{shown}");
        assert!(!shown.contains("secret"), "the token was shown: {shown}");
        assert!(shown.contains("37"), "the length is missing: {shown}");
        // A token of an unknown shape gives nothing away at all.
        assert!(!masked("abcdefghijklmnop").contains("abc"));
        assert_eq!(masked(""), "");
    }
}
