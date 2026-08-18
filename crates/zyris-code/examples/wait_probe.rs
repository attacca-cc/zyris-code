//! **Does `wait` actually wait in live use?**
//!
//! Even with every local test green, this only shows up live — it happened twice in this repo
//! over wire names, and both times local was green. Exactly one thing is measured here:
//! **does the agent wait out a command that runs past the 60s deadline instead of reading it as
//! a failure?**
//!
//! It follows the same rule as `announce_probe` — **the agent's words are not evidence.** We've
//! seen it assert "there is no such tool" four times while the tool was there all along. There
//! are three verdicts and every one of them is a side effect or the clock:
//!
//! 1. **Did the marker file actually appear?** That means the background command ran to the end.
//! 2. Did the turn **stay alive longer than the wire deadline (60s)?** A single node call returns
//!    within 50s, so finishing work that took longer means `until` was called several times.
//! 3. Is there no word for failure in the agent's answer? **This one alone is auxiliary** — if
//!    the first two pass and only this trips, it's a wording problem, not a behavior problem.
//!
//! ```bash
//! timeout 900 cargo run -j2 -p zyris-code --example wait_probe
//! ```

use std::process::ExitCode;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use zyris::runtime::Runner;
use zyris::{Connection, NodeKind};
use zyris_attacca::{AttaccaApi, AttaccaApiClient, ZDeltaKind, ZNewSession, ZTurnFrame};

const CONSUME_WAIT: Duration = Duration::from_secs(5);
/// How long to run in the background. Measuring means something only if it is **comfortably
/// longer than the wire deadline (60s)**.
const SLEEP_SECS: u64 = 90;

/// What to ask. Override with `$ZYRIS_CODE_PROBE_ASK`.
///
/// **Tell it the tool's real wire name.** Calling it just `wait.start` makes the model fail to
/// find that name and give up with "there is no such tool" — that actually happened in
/// `announce_probe`.
fn ask(marker: &std::path::Path) -> String {
    std::env::var("ZYRIS_CODE_PROBE_ASK").unwrap_or_else(|_| {
        format!(
            "Run one long-running job. \
             (1) Call the tool whose name ends in '__wait__start' with \
             command='sleep {SLEEP_SECS}; date > {}'. \
             (2) Then call the tool whose name ends in '__wait__until' with that job id. \
             If done comes back false that is **not a failure, it just has not finished**, so \
             call it again with the same arguments. Repeat until done is true. \
             (3) When it finishes, report the exit code in one line.",
            marker.display()
        )
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zyris=warn".into()),
        )
        .init();

    // Use the same node identity as the TUI. Connecting with a different profile would ask for
    // the enrollment code again.
    if std::env::var_os("ZYRIS_PROFILE").is_none() {
        std::env::set_var("ZYRIS_PROFILE", "zyris-code");
    }

    // Don't let it touch the real repo. One temp directory is this probe's whole world.
    let dir = std::env::temp_dir().join("zyris-code-wait-probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp directory");
    let marker = dir.join("done.txt");
    println!("working directory: {}\n", dir.display());

    // The probe has no screen. With nowhere for the gate to ask, **it's set to auto mode** —
    // this probe's job is to measure the waiting path, not the approval path.
    let bridge = zyris_code::tools::bridge::Bridge::new();
    bridge.sync(zyris_code::mode::Mode::Job, &Default::default(), false);

    let (_api_tx, api_rx) = tokio::sync::watch::channel(None);
    zyris_code::tools::announce(Runner::from_env(), dir, bridge, api_rx)
        .kind(NodeKind::Service)
        .request_scopes(["agents:read", "sessions:write", "events:read"])
        .on_connect(move |conn| {
            let marker = marker.clone();
            async move {
                let ok = match ask_the_agent(&conn, &marker).await {
                    Ok(ok) => ok,
                    Err(e) => {
                        println!("\n### probe failed: {e}\n");
                        false
                    }
                };
                std::process::exit(i32::from(!ok));
            }
        })
        .run()
        .await
}

async fn ask_the_agent(conn: &Connection, marker: &std::path::Path) -> anyhow::Result<bool> {
    let announced = conn.announce().await?;
    println!("announce → accepted={:?} rejected={:?}", announced.accepted, announced.rejected);

    // The server snapshots capabilities within 500ms of the handshake. Wait generously — rule
    // out this spot as the cause first.
    let wait: u64 =
        std::env::var("ZYRIS_CODE_PROBE_WAIT").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    println!("waiting {wait}s");
    tokio::time::sleep(Duration::from_secs(wait)).await;

    let api = conn.wait_capability::<AttaccaApiClient>(CONSUME_WAIT).await?;

    let wanted = std::env::var("ZYRIS_CODE_AGENT").unwrap_or_else(|_| "Main Agent".into());
    let agent = api
        .list_agents()
        .await?
        .into_iter()
        .find(|a| a.name == wanted)
        .ok_or_else(|| anyhow::anyhow!("no agent named '{wanted}'"))?;
    println!("agent: {} ({})", agent.name, agent.id);

    let session = api
        .create_session_with(ZNewSession {
            agent_id: agent.id.clone(),
            title: None,
            project_id: None,
            preamble: None,
        })
        .await?;
    let ask = ask(marker);
    println!("session: {}\nasking: {ask}\n", session.id);

    let at = Instant::now();
    api.send_message(session.id.clone(), ask, Vec::new()).await?;

    let mut stream = api.turn_events(session.id.clone(), None).await?;
    let mut answer = String::new();
    // **Don't mistake a `running:false` before the turn starts for its end.** If the subscription
    // attaches later than the send, the first status arrives as false, and cutting off there
    // makes the answer always look empty.
    let mut started = false;
    while let Some(frame) = stream.items.next().await {
        match frame? {
            ZTurnFrame::Delta { kind: ZDeltaKind::Assistant, text } => {
                started = true;
                print!("{text}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
                answer.push_str(&text);
            }
            ZTurnFrame::Status { running: true } => started = true,
            ZTurnFrame::Status { running: false } if started => break,
            _ => {}
        }
    }
    let took = at.elapsed();

    println!("\n\n=== verdict 1: did the background command run to the end? ===");
    let landed = marker.exists();
    match std::fs::read_to_string(marker) {
        Ok(body) => println!("pass — the marker file appeared: {}", body.trim()),
        Err(e) => println!("fail — no marker file ({e}). The command died partway."),
    }

    println!("\n=== verdict 2: did it stay alive past the deadline? ===");
    // A single node call returns within 50s. Finishing work that took longer means `until` was
    // called several times — which is the whole reason this capability exists.
    let survived = took > Duration::from_secs(60);
    println!(
        "{} — the turn took {}s (background command {SLEEP_SECS}s).",
        if survived { "pass" } else { "fail" },
        took.as_secs()
    );

    println!("\n=== verdict 3: did it avoid reading this as a failure? (auxiliary) ===");
    let lower = answer.to_lowercase();
    // The Korean words stay: the agent answers in the session's language, so an answer that
    // complains in Korean must trip this too. These are needles, not prose.
    let bad = ["fail", "timed out", "timeout", "cancel", "실패", "시간 초과", "취소"];
    let complained: Vec<&str> = bad.into_iter().filter(|w| lower.contains(w)).collect();
    if complained.is_empty() {
        println!("pass — no word for failure in the answer.");
    } else {
        println!("tripped — the answer has {complained:?}. Check the wording:\n{}", answer.trim());
    }

    let ok = landed && survived;
    println!("\n=== verdict: {} ===", if ok { "pass" } else { "fail" });
    Ok(ok)
}
