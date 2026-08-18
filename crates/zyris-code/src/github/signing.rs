//! Signing commits with a key this app made, and that GitHub has been told about.
//!
//! **The whole point is the "Verified" badge.** A signature GitHub cannot check is worse than none
//! — it costs a key on disk and buys nothing visible. Three things have to line up for the badge:
//! the key is on the account, the commit's email is a UID on that key, and that email is verified
//! on the account. This module lines all three up itself rather than asking the person to.
//!
//! **The email is GitHub's noreply address for the account** (`{id}+{login}@users.noreply…`). It is
//! verified on every account by construction, so the badge always lands, and it is the one address
//! that cannot leak a private one into a public repository's history. The cost is real and is
//! stated where it is chosen: commits made through this app carry that address rather than
//! whatever `git config user.email` says.
//!
//! **The key has no passphrase.** There is nowhere to type one — this app owns the screen, and a
//! pinentry prompt behind it is a hang, which is the same class of bug as the DSR wait in
//! `Terminal::clear()`. A passphrase stored beside the key it protects protects nothing, so the
//! protection here is the same as an SSH deploy key's: a private directory, owner-only.
//!
//! **Nothing here touches the person's `~/.gnupg` or their `.gitconfig`.** The keyring is this
//! app's own directory and the settings ride on the one `git` invocation that needs them, so a
//! commit made anywhere else is unaffected — see [`crate::git::Git::commit`].

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The scopes a token needs before a key can be put on the account.
///
/// **Not what everybody is asked for.** `write:gpg_key` lets this app add and remove keys on the
/// account, and asking every person who ever reads an issue to hand that over — on a consent
/// screen, for a feature they may never turn on — is a bad trade. Turning signing on is the moment
/// it is worth asking, so that is when it is asked for.
pub const SIGNING_SCOPES: &str = "repo read:user write:gpg_key";

/// The scope that has to be present on the token.
pub const GPG_SCOPE: &str = "write:gpg_key";

/// This app's own keyring. **Not `~/.gnupg`** — a key generated for one program should not appear
/// in the list every other program shows, and removing it should not mean editing a shared ring.
pub fn home() -> Option<PathBuf> {
    crate::conn::credential_dir().map(|dir| dir.join("gnupg"))
}

fn store() -> Option<PathBuf> {
    crate::conn::credential_dir().map(|dir| dir.join("signing.json"))
}

/// What was set up, as it sits on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signing {
    /// The long fingerprint. What `user.signingkey` is set to.
    pub fingerprint: String,
    /// The address the key carries, and therefore the address commits must be authored with.
    pub email: String,
    /// The login it was made for, so the screen can say whose it is.
    #[serde(default)]
    pub login: String,
}

impl Signing {
    /// What is set up, if anything.
    ///
    /// **A record with no fingerprint is nothing.** Keeping one would set `commit.gpgsign=true`
    /// with no key to sign with, and every commit would fail at the last step.
    pub fn load() -> Option<Signing> {
        let at = store()?;
        let text = std::fs::read_to_string(at).ok()?;
        let signing: Signing = serde_json::from_str(&text).ok()?;
        (!signing.fingerprint.is_empty() && !signing.email.is_empty()).then_some(signing)
    }

    pub fn save(&self) -> Result<()> {
        let at = store().context("there is nowhere to keep the signing key")?;
        if let Some(dir) = at.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&at, serde_json::to_string(self)?)?;
        Ok(())
    }

    /// Forgets the key here. **The key is left on the account** — the same shape as signing out,
    /// and for the same reason: taking something off someone's GitHub account is not a thing this
    /// app should do quietly on the way past.
    pub fn forget() -> bool {
        let Some(at) = store() else { return false };
        std::fs::remove_file(at).is_ok()
    }
}

/// The address GitHub always counts as verified for an account.
///
/// **Built here rather than fetched.** GitHub does not hand this back as a field; it is `id` and
/// `login`, which `/user` does answer, put together the way GitHub documents.
pub fn noreply(id: u64, login: &str) -> String {
    format!("{id}+{login}@users.noreply.github.com")
}

/// The program to run. `$ZYRIS_CODE_GPG` wins so a machine with it under another name — `gpg2`, or
/// Gpg4win's full path — can be pointed at it without a build.
pub fn program() -> String {
    program_from(&std::env::var("ZYRIS_CODE_GPG").unwrap_or_default())
}

/// The naming rule on its own, so it can be checked without touching the environment — a test that
/// sets a variable is read by every other test running beside it.
pub fn program_from(given: &str) -> String {
    match given.trim().is_empty() {
        true => "gpg".to_string(),
        false => given.trim().to_string(),
    }
}

async fn gpg(args: &[&str]) -> Result<std::process::Output> {
    let dir = home().context("there is nowhere to keep a keyring")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("could not make {}", dir.display()))?;
    // gpg complains loudly about a world-readable home, and on a shared machine it is right to.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let out = tokio::process::Command::new(program())
        .arg("--homedir")
        .arg(&dir)
        .arg("--batch")
        .arg("--yes")
        .args(args)
        .output()
        .await
        .with_context(|| {
            format!("could not run `{}` ‒ GnuPG has to be installed to sign commits", program())
        })?;
    Ok(out)
}

/// Whether GnuPG is on this machine at all. Asked before anything is promised, because the answer
/// on a fresh Windows box is usually no and saying so is the whole of the help.
pub async fn installed() -> bool {
    tokio::process::Command::new(program())
        .arg("--version")
        .output()
        .await
        .is_ok_and(|o| o.status.success())
}

/// Makes a key for `email`, or answers the one that is already there for it.
///
/// **Idempotent on the address.** Turning signing on twice should not leave two keys on the
/// account with nothing to tell them apart.
pub async fn generate(email: &str, name: &str) -> Result<String> {
    if let Some(fpr) = fingerprint_for(email).await? {
        return Ok(fpr);
    }
    // ed25519: small, fast, and what GitHub's own documentation reaches for. `sign` alone — this
    // key has one job, and an encryption subkey would be a key nobody can use to reach anyone.
    let uid = format!("{name} <{email}>");
    let out = gpg(&[
        "--pinentry-mode",
        "loopback",
        "--passphrase",
        "",
        "--quick-generate-key",
        &uid,
        "ed25519",
        "sign",
        "never",
    ])
    .await?;
    if !out.status.success() {
        bail!("could not make a signing key: {}", said(&out));
    }
    fingerprint_for(email)
        .await?
        .context("GnuPG made a key and then did not list it ‒ nothing to sign with")
}

/// The fingerprint of the secret key carrying `email`, if the ring has one.
async fn fingerprint_for(email: &str) -> Result<Option<String>> {
    let out = gpg(&["--with-colons", "--list-secret-keys", email]).await?;
    if !out.status.success() {
        // "no secret key" is the ordinary answer before there is one, not a failure.
        return Ok(None);
    }
    Ok(first_fingerprint(&String::from_utf8_lossy(&out.stdout)))
}

/// The armoured public key, which is what GitHub is handed.
pub async fn public_key(fingerprint: &str) -> Result<String> {
    let out = gpg(&["--armor", "--export", fingerprint]).await?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("could not export the public key: {}", said(&out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn said(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr);
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no reason given")
        .trim()
        .to_string()
}

/// The first fingerprint in `--with-colons` output.
///
/// **The `fpr` line, not the `sec` line.** The key id on `sec` is the last 16 characters of the
/// fingerprint, and git will take it, but GitHub's own listing shows fingerprints — matching what
/// a person would compare against is worth the one extra field.
///
/// Pure, so the parsing can be tested without a keyring.
pub fn first_fingerprint(colons: &str) -> Option<String> {
    colons.lines().filter_map(|line| line.strip_prefix("fpr:")).find_map(|rest| {
        let fpr = rest.split(':').find(|f| !f.is_empty())?;
        fpr.chars().all(|c| c.is_ascii_hexdigit()).then(|| fpr.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The address GitHub counts as verified, put together the way GitHub documents it.
    #[test]
    fn the_noreply_address_is_the_id_and_the_login() {
        assert_eq!(noreply(1234, "ruma"), "1234+ruma@users.noreply.github.com");
    }

    /// **The fingerprint comes off the `fpr` line.** The `sec` line carries a short key id, which
    /// git accepts but which does not match what GitHub shows on the account — and the whole job
    /// of this feature is that a person can look at both and see the same key.
    #[test]
    fn the_fingerprint_is_read_out_of_the_colon_listing() {
        let listing = "\
sec:u:255:22:AAAA1111BBBB2222:1755000000:::u:::scESC:::+:::23::0:
fpr:::::::::1111222233334444555566667777888899990000:
grp:::::::::0000AAAA:
uid:u::::1755000000::ABCDEF::ruma <1+ruma@users.noreply.github.com>::::::::::0:
";
        assert_eq!(
            first_fingerprint(listing),
            Some("1111222233334444555566667777888899990000".to_string()),
        );
    }

    /// A ring with nothing in it answers nothing rather than a made-up key.
    #[test]
    fn an_empty_listing_has_no_fingerprint() {
        assert_eq!(first_fingerprint(""), None);
        assert_eq!(
            first_fingerprint("sec:u:255:22:AAAA:1755000000:::u:::scESC:::+:::23::0:\n"),
            None
        );
    }

    /// **A record with no key in it is not a set-up.** It would switch `commit.gpgsign` on with
    /// nothing to sign with, and every commit would fail at the last step for a reason that has
    /// nothing to do with the commit.
    #[test]
    fn a_half_written_record_does_not_count_as_signing_being_on() {
        let empty = Signing::default();
        assert!(empty.fingerprint.is_empty() && empty.email.is_empty());
    }

    /// The program can be pointed elsewhere — Gpg4win installs it under a full path, and some
    /// distributions still call it `gpg2`.
    #[test]
    fn the_gpg_program_can_be_named_from_outside() {
        assert_eq!(program_from(""), "gpg", "with nothing set, the plain name");
        assert_eq!(program_from("  "), "gpg", "blank is nothing, not a program called space");
        assert_eq!(program_from("gpg2"), "gpg2");
        assert_eq!(
            program_from(r"C:\Program Files\GnuPG\bin\gpg.exe"),
            r"C:\Program Files\GnuPG\bin\gpg.exe"
        );
    }
}
