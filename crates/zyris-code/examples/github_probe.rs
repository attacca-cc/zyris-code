//! Asks GitHub for a device code, and stops there.
//!
//! **Proves the OAuth app is real and has device flow switched on** — the two things that cannot
//! be checked from a unit test, and the two that fail in ways nobody would guess from the message.
//! An app that exists but has device flow off answers `device_flow_disabled`, which is the whole
//! reason this probe is separate from signing in.
//!
//! It stops before the wait, so **nothing is approved and no token is written.** The code it prints
//! expires by itself in a few minutes.
//!
//! ```sh
//! cargo run -p zyris-code --example github_probe
//! ```

#[tokio::main]
async fn main() {
    // **Signed in already? Then read something, and stop.** Asking for a device code when there is
    // a working token would only produce a code nobody needs.
    let accounts = zyris_code::github::auth::Accounts::load();
    if accounts.exactly(zyris_code::github::auth::Role::User).is_some() {
        return read_something(&accounts).await;
    }

    let Some(id) = zyris_code::github::auth::client_id() else {
        eprintln!("no OAuth app is configured for this build");
        std::process::exit(1);
    };
    println!("client id: {id}");
    match zyris_code::github::auth::begin().await {
        Ok(pending) => {
            println!("device flow is on.");
            println!("  open {} and enter: {}", pending.verification_uri, pending.user_code);
            println!("  it expires in {}s. nothing was saved.", pending.expires_in);
        }
        Err(why) => {
            eprintln!("GitHub refused: {why}");
            eprintln!("if it says device_flow_disabled, tick 'Enable Device Flow' on the app.");
            std::process::exit(1);
        }
    }
}

/// **Reads, never writes.** A probe that opened an issue to prove it could would leave the proof
/// behind on a real repository.
async fn read_something(accounts: &zyris_code::github::auth::Accounts) {
    use zyris_code::github::auth::Role;

    for role in [Role::User, Role::Reviewer] {
        match accounts.exactly(role) {
            Some(a) => println!("{:9} {}", role.code(), a.login),
            None => println!("{:9} — not connected", role.code()),
        }
    }
    let token = accounts.for_role(Role::User).expect("checked above").token.clone();
    let client = match zyris_code::github::api::Github::new(token) {
        Ok(c) => c,
        Err(e) => return eprintln!("could not build the client: {e}"),
    };
    match client.me().await {
        Ok(login) => println!("\nGitHub answers to this token as: {login}"),
        Err(e) => return eprintln!("\nthe token does not work: {e}"),
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let Some(repo) = zyris_code::github::repo_of(&cwd) else {
        return println!("no GitHub remote here, so there is nothing to read");
    };
    println!("repository here: {}/{}", repo.owner, repo.name);

    match client.pulls(&repo, "all", 3).await {
        Ok(pulls) => {
            let n = pulls.as_array().map_or(0, Vec::len);
            println!("\npull requests ({n} read):");
            println!("{}", serde_json::to_string_pretty(&pulls).unwrap_or_default());
        }
        Err(e) => println!("\npull requests: {e}"),
    }
    match client.issues(&repo, "open", 3).await {
        Ok(issues) => println!("\nopen issues: {}", issues.as_array().map_or(0, Vec::len)),
        Err(e) => println!("\nissues: {e}"),
    }
}
