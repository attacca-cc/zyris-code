//! Drives the GitHub reads against the real API, with whatever account is signed in here.
//!
//! **Judged by what comes back, not by asking an agent.** A model that says a tool worked is not
//! evidence it did — this repository has been fooled by that before. Every line below is a real
//! request and a real answer.
//!
//! Read-only on purpose. Opening an issue or a pull request to check that opening works leaves
//! something behind on a public repository that somebody then has to close.
//!
//! ```text
//! cargo run -p zyris-code --example github_probe
//! ```

use zyris_code::github::{api, auth, repo_of};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let Some(repo) = repo_of(&cwd) else {
        anyhow::bail!("{} has no GitHub remote", cwd.display());
    };
    println!("repository  {}/{}", repo.owner, repo.name);

    let Some(account) = auth::Accounts::load().for_role(auth::Role::User).cloned() else {
        anyhow::bail!("nobody is signed in ‒ run `/github login` in the app first");
    };
    let client = api::Github::new(account.token)?;

    let who = client.account().await?;
    println!("signed in   {} (id {})", who.login, who.id);
    println!("may         {}", who.scopes.join(", "));
    println!(
        "signing     {}",
        match who.may(zyris_code::github::signing::GPG_SCOPE) {
            true => "this token could put a key on the account",
            false => "this token cannot add a key ‒ /github would ask again",
        }
    );

    let issues = client.issues(&repo, "open", 3).await?;
    println!("open issues {}", issues.as_array().map(Vec::len).unwrap_or(0));

    let pulls = client.pulls(&repo, "all", 3).await?;
    for pull in pulls.as_array().into_iter().flatten() {
        println!("  #{} {} [{}]", pull["number"], pull["title"], pull["state"]);
    }

    // One in full, which is the call that costs three requests and folds the checks.
    if let Some(number) =
        pulls.as_array().and_then(|list| list.first()).and_then(|p| p["number"].as_u64())
    {
        let one = client.pull(&repo, number).await?;
        println!(
            "pull #{number}  +{} -{}  checks={}  files={}",
            one["added"],
            one["removed"],
            one["checks"],
            one["files"].as_array().map(Vec::len).unwrap_or(0),
        );
    }

    // What the strip polls for, on the branch that is checked out.
    let branch = String::from_utf8(
        std::process::Command::new("git").arg("branch").arg("--show-current").output()?.stdout,
    )?;
    let branch = branch.trim();
    match client.branch_pull(&repo, branch).await? {
        Some(p) => println!(
            "strip       #{} +{} -{} {:?} merged={}",
            p.number, p.added, p.removed, p.checks, p.merged
        ),
        None => println!("strip       nothing open for `{branch}`"),
    }
    Ok(())
}
