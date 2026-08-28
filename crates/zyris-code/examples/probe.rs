//! Diagnostic. Prints the agent list with UUID versions and tries sending one turn to the given agent.
//!
//! A UUID version nibble of 4 means a DB row; 5 means a git agent synthesized from the content repo.
//!
//! ```bash
//! ZYRIS_PROFILE=zyris-code cargo run -p zyris-code --example probe            # list only
//! ZYRIS_PROFILE=zyris-code cargo run -p zyris-code --example probe -- <name>  # send to that agent
//! ```

use std::process::ExitCode;
use std::time::Duration;

use zyris::{Connection, NodeKind};
use zyris_attacca::{AttaccaApi, AttaccaApiClient, ZNewSession};
use zyris_code::runtime::{RunConfig, Runner};

const WAIT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt().with_env_filter("probe=info,zyris=warn").init();

    // **What this probe asks for has to be said before the credential is built.** `request_scopes`
    // used to sit in the builder chain below; the grant is approved once against the list the
    // authorize call carried, so the only place left to say it is ahead of `RunConfig::from_env`.
    // A person who set the variable outranks the probe, same as everywhere else.
    if std::env::var_os("ZYRIS_SCOPES").is_none() {
        std::env::set_var("ZYRIS_SCOPES", "agents:read,sessions:read,sessions:write");
    }

    let config = RunConfig::from_env();
    // The enrollment path wants somewhere to draw a code. This probe has no screen, so it falls to
    // the stdout box — which is the right answer for a diagnostic run from a shell.
    let bridge = zyris_code::tools::bridge::Bridge::new();
    let creds = match zyris_code::enroll::source(&config, &bridge) {
        Ok((creds, _)) => creds,
        Err(e) => {
            println!("could not build credentials: {e}");
            return ExitCode::FAILURE;
        }
    };
    let node = match zyris::Node::builder().name(&config.node_name).kind(NodeKind::Service).build()
    {
        Ok(node) => node,
        Err(e) => {
            println!("could not build the node: {e}");
            return ExitCode::FAILURE;
        }
    };

    Runner::new(config, node, creds)
        .on_connect(|conn| async move {
            probe(&conn).await;
            std::process::exit(0);
        })
        .run()
        .await
}

async fn probe(conn: &Connection) {
    let api = match conn.wait_capability::<AttaccaApiClient>(WAIT).await {
        Ok(api) => api,
        Err(e) => {
            println!("no attacca_api: {e}");
            return;
        }
    };

    let agents = match api.list_agents().await {
        Ok(a) => a,
        Err(e) => {
            println!("list_agents failed: {e}");
            return;
        }
    };

    println!("\n=== {} agents ===", agents.len());
    for a in &agents {
        // The 13th hex character of the UUID is the version: the first character of the third group in 8-4-4-4-12.
        let version = a.id.split('-').nth(2).and_then(|g| g.chars().next()).unwrap_or('?');
        let kind = match version {
            '4' => "DB row",
            '5' => "git synth",
            _ => "?",
        };
        println!("  {:38}  v{}  {:9}  {}", a.id, version, kind, a.name);
    }

    let Some(target) = std::env::args().nth(1) else {
        println!("\n(pass a target name as an argument to try sending one turn)");
        return;
    };

    let Some(agent) = agents.iter().find(|a| a.name == target) else {
        println!("\nno agent named '{target}'");
        return;
    };

    println!("\n=== trying to send to '{}' ===", agent.name);
    let session = match api
        .create_session_with(ZNewSession {
            agent_id: agent.id.clone(),
            title: None,
            project_id: None,
            preamble: None,
        })
        .await
    {
        Ok(s) => {
            println!("  create_session_with ✓  session_id={}", s.id);
            s
        }
        Err(e) => {
            println!("  create_session_with ✗  {e}");
            return;
        }
    };

    match api.send_message(session.id.clone(), "2 더하기 3은?".into(), vec![]).await {
        Ok(()) => println!("  send_message ✓  sent"),
        Err(e) => println!("  send_message ✗  {e}"),
    }
}
