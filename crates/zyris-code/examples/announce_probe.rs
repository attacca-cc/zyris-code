//! **The only live verification that runs automatically.**
//!
//! No human hands needed. Since it consumes `attacca_api`, it opens a session itself, tells the agent
//! to use tools, and runs one short turn to completion. Two verdicts:
//!
//! 1. **Did a file on this computer actually change?**
//! 2. Do the events the server returned become a **green/red diff** in the screen pipeline?
//!
//! **The agent's words are not evidence.** We've seen it claim "there is no such tool" four times
//! even when the tool exists — only side effects are evidence (spec §13.3).
//!
//! ```bash
//! cargo run -j2 -p zyris-code --example announce_probe
//! ```

use std::process::ExitCode;
use std::time::Duration;

use futures_util::StreamExt;
use zyris::runtime::Runner;
use zyris::{Connection, NodeKind};
use zyris_attacca::{
    AttaccaApi, AttaccaApiClient, ZDeltaKind, ZHistoryQuery, ZNewSession, ZTurnFrame,
};

const CONSUME_WAIT: Duration = Duration::from_secs(5);
/// **Actually makes it happen.** Asking the model to list tools in prose is something it's bad at,
/// so a "there is none" answer can't be trusted — only whether the file actually changed is evidence.
/// **Tell it the tool's real name.** The wire name is `zyris__{node}__{capability}__{tool}`, so
/// calling it `file_io.read` makes the model fail to find it and give up with "no such tool". This
/// actually happened once.
/// What to ask. Override with `$ZYRIS_CODE_PROBE_ASK` — no reason to edit the example to test something else.
fn ask() -> String {
    std::env::var("ZYRIS_CODE_PROBE_ASK").unwrap_or_else(|_| ASK.to_string())
}

const ASK: &str = "두 가지를 하라. \
                   (1) 이름이 '__code_probe__ping'으로 끝나는 도구를 say='PROBE-OK'로 호출하라. \
                   (2) 이름이 '__code_edit__edit'으로 끝나는 도구를 \
                   path='note.txt', old_string='BEFORE', new_string='AFTER'로 호출하라. \
                   각각에 대해 도구가 목록에 있었는지 없었는지 한 줄로 알려라.";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Pong {
    pub said: String,
}

/// A known good. The simplest tool the agent has actually called before.
#[zyris::capability(name = "code_probe", version = 1)]
pub trait CodeProbe {
    /// Echoes back what it was told.
    async fn ping(&self, say: String) -> zyris::Result<Pong>;
}

struct Probe;

#[async_trait::async_trait]
impl CodeProbe for Probe {
    async fn ping(&self, say: String) -> zyris::Result<Pong> {
        println!("\n>>> code_probe.ping이 불렸다: {say}\n");
        Ok(Pong { said: say })
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zyris=warn".into()),
        )
        .init();

    // Use the same node identity as the TUI. Connecting with a different profile would ask for the enrollment code again.
    if std::env::var_os("ZYRIS_PROFILE").is_none() {
        std::env::set_var("ZYRIS_PROFILE", "zyris-code");
    }

    // Don't let it touch the real repo. One temp directory is this probe's whole world.
    let dir = std::env::temp_dir().join("zyris-code-probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("임시 디렉터리");
    let note = dir.join("note.txt");
    std::fs::write(&note, "첫 줄\nBEFORE\n끝 줄\n").expect("note.txt");
    println!("작업 디렉터리: {}\n", dir.display());

    // **Hand out a control too.** `code_probe` is a known good the agent has actually called.
    // If it shows up but `code_edit` doesn't, the cause is code_edit's schema.
    // The probe has no screen. With nowhere for the gate to ask, **it's set to auto mode** —
    // this probe's job is to measure the tool path, not the approval path.
    let bridge = zyris_code::tools::bridge::Bridge::new();
    bridge.sync(zyris_code::mode::Mode::Job, &Default::default());

    // The probe doesn't use the attacca handle — the `work` tool just needs to appear in the list.
    let (_api_tx, api_rx) = tokio::sync::watch::channel(None);
    zyris_code::tools::announce(Runner::from_env(), dir, bridge, api_rx)
        .capability(CodeProbeServer(Probe))
        .kind(NodeKind::Service)
        .request_scopes(["agents:read", "sessions:write", "events:read"])
        .on_connect(move |conn| {
            let note = note.clone();
            async move {
                if let Err(e) = ask_the_agent(&conn, &note).await {
                    println!("\n### 프로브 실패: {e}\n");
                }
                std::process::exit(0);
            }
        })
        .run()
        .await
}

async fn ask_the_agent(conn: &Connection, note: &std::path::Path) -> anyhow::Result<()> {
    // See directly what the server accepted. If this is rejected, that's the whole reason tools don't show up.
    let announced = conn.announce().await?;
    println!("announce → accepted={:?} rejected={:?}", announced.accepted, announced.rejected);

    // The server snapshots capabilities into the node row within 500ms of the handshake, then
    // re-syncs every REFRESH_TICK (attacca-server/src/routes/zyris.rs:124, :215).
    // Wait generously — rule out this spot as the cause first.
    let wait: u64 =
        std::env::var("ZYRIS_CODE_PROBE_WAIT").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    println!("{wait}초 기다린다");
    tokio::time::sleep(Duration::from_secs(wait)).await;

    let api = conn.wait_capability::<AttaccaApiClient>(CONSUME_WAIT).await?;

    let wanted = std::env::var("ZYRIS_CODE_AGENT").unwrap_or_else(|_| "Main Agent".into());
    let agents = api.list_agents().await?;
    let agent = agents
        .into_iter()
        .find(|a| a.name == wanted)
        .ok_or_else(|| anyhow::anyhow!("'{wanted}' 에이전트가 없다"))?;
    println!("에이전트: {} ({})", agent.name, agent.id);

    let session = api
        .create_session_with(ZNewSession {
            agent_id: agent.id.clone(),
            title: None,
            project_id: None,
            preamble: None,
        })
        .await?;
    let ask = ask();
    println!("세션: {}\n묻는다: {ask}\n", session.id);

    api.send_message(session.id.clone(), ask.clone(), Vec::new()).await?;

    let mut stream = api.turn_events(session.id.clone(), None).await?;
    let mut answer = String::new();
    // **Don't mistake a `running:false` before the turn starts for its end.** If the subscription
    // attaches later than the send, the first status arrives as false, and cutting off there makes the answer always look empty.
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

    println!("\n\n=== 판정 1: 파일이 실제로 바뀌었는가 ===");
    let body = std::fs::read_to_string(note).unwrap_or_default();
    if body.contains("AFTER") && !body.contains("BEFORE") {
        println!("통과 — 에이전트가 이 컴퓨터의 파일을 실제로 고쳤다.");
        println!("파일 내용:\n{body}");
    } else {
        println!("실패 — 파일이 안 바뀌었다.");
        println!("파일 내용: {body:?}");
        println!("에이전트의 답: {}", answer.trim());
    }

    println!("\n=== 판정 2: 그 이벤트가 화면에서 초록/빨강이 되는가 ===");
    let events = api.session_history(session.id, ZHistoryQuery::default()).await?;
    println!("이벤트 {}개를 다시 읽었다", events.len());
    check_the_diff_paints(&events);
    Ok(())
}

/// Feeds **the events the server actually returned** straight through the screen pipeline.
///
/// Screen tests use a hand-built `Diff`, so a mismatched field name in the tool result, or attacca
/// wrapping the result, would go unnoticed. Here we only look at a real round trip of that exchange.
fn check_the_diff_paints(events: &[zyris_attacca::ZSessionEvent]) {
    use zyris_code::rows::{rows, Fold, Folds};
    use zyris_code::timeline::Timeline;

    let mut timeline = Timeline::new();
    let mut folds = Folds::new();
    for event in events {
        if let Some(entry) = zyris_code::event::entry_from(event) {
            // Open them all, as they would be at the moment the tool was used.
            folds.insert(entry.seq, Fold { open: true });
            timeline.upsert(entry);
        }
    }
    let drawn = rows(timeline.items(), 100, &folds, zyris_code::lang::Lang::En);
    let coloured = |want: ratatui::style::Color| -> Vec<String> {
        drawn
            .lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| s.style.fg == Some(want)))
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    };
    let added = coloured(zyris_code::theme::DIFF_ADD);
    let removed = coloured(zyris_code::theme::DIFF_DEL);
    if added.is_empty() || removed.is_empty() {
        println!("실패 — diff가 화면 줄이 되지 않았다.");
        println!("초록 줄: {added:?}\n빨강 줄: {removed:?}");
        return;
    }
    println!("통과 — 초록/빨강으로 그려진다.");
    for line in added.iter().chain(&removed) {
        println!("  {}", line.trim_end());
    }
}
