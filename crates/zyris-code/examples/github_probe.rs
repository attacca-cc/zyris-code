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
