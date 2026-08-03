//! **자동으로 도는 유일한 라이브 검증 수단.**
//!
//! 사람 손을 빌리지 않는다. `attacca_api`를 소비하므로 스스로 세션을 열고, 에이전트에게
//! 도구를 시키고, 짧은 턴 하나를 끝까지 돌린다. 판정은 둘이다:
//!
//! 1. **이 컴퓨터의 파일이 실제로 바뀌었는가.**
//! 2. 서버가 돌려준 그 이벤트가 화면 파이프라인에서 **초록/빨강 diff가 되는가.**
//!
//! **에이전트의 말은 근거로 쓰지 않는다.** 도구가 있는데도 "없습니다"라고 단언하는 것을
//! 네 번 겪었다 — 부작용만이 증거다(스펙 13절 3번).
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
/// **시켜 본다.** 도구 목록을 글로 나열하라고 하면 모델이 잘 못하는 일이라 "없다"는 답이
/// 진짜 없다는 뜻인지 알 수 없다 — 실제로 파일이 바뀌었는지만이 근거다.
/// **도구 이름을 실제 모양으로 말해 준다.** 와이어 이름은 `zyris__{노드}__{캐퍼빌리티}__{도구}`라
/// `file_io.read`라고 부르면 모델이 그 이름을 못 찾고 "도구가 없다"로 포기한다. 실제로 한 번
/// 그랬다.
/// 시킬 말. `$ZYRIS_CODE_PROBE_ASK`로 바꾼다 — 다른 것을 재 보려고 예제를 고칠 이유가 없다.
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

/// 알려진 정상. 에이전트가 실제로 호출한 적이 있는 가장 단순한 도구다.
#[zyris::capability(name = "code_probe", version = 1)]
pub trait CodeProbe {
    /// 받은 말을 그대로 돌려준다.
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

    // TUI와 같은 노드 신원을 쓴다. 다른 프로필로 붙으면 등록 코드를 다시 요구한다.
    if std::env::var_os("ZYRIS_PROFILE").is_none() {
        std::env::set_var("ZYRIS_PROFILE", "zyris-code");
    }

    // 진짜 리포를 만지게 두지 않는다. 임시 디렉터리 하나가 이 프로브의 세계다.
    let dir = std::env::temp_dir().join("zyris-code-probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("임시 디렉터리");
    let note = dir.join("note.txt");
    std::fs::write(&note, "첫 줄\nBEFORE\n끝 줄\n").expect("note.txt");
    println!("작업 디렉터리: {}\n", dir.display());

    // **대조군을 같이 내준다.** `code_probe`는 에이전트가 실제로 호출한 적이 있는 알려진
    // 정상이다. 그것은 보이는데 `code_edit`이 안 보이면 원인은 code_edit의 스키마다.
    // 프로브에는 화면이 없다. 게이트가 물을 곳이 없으므로 **자동 모드로 맞춰 둔다** —
    // 승인 경로가 아니라 도구 경로를 재는 것이 이 프로브의 일이다.
    let bridge = zyris_code::tools::bridge::Bridge::new();
    bridge.sync(zyris_code::mode::Mode::Job, &Default::default());

    // 프로브는 attacca 손잡이를 안 쓴다 — `work` 도구가 목록에 뜨기만 하면 된다.
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
    // 서버가 무엇을 받아들였는지 직접 본다. 여기서 rejected면 도구가 안 뜨는 이유가 끝난다.
    let announced = conn.announce().await?;
    println!("announce → accepted={:?} rejected={:?}", announced.accepted, announced.rejected);

    // 서버는 handshake 뒤 500ms 안에 capability를 스냅숏해 노드 행에 쓰고, 그 뒤로는
    // REFRESH_TICK마다 맞춘다(attacca-server/src/routes/zyris.rs:124, :215).
    // 넉넉히 기다린다 — 여기가 원인인지부터 배제한다.
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
    // **턴이 시작하기 전의 `running:false`를 끝으로 오해하면 안 된다.** 구독이 전송보다
    // 늦게 붙으면 첫 상태가 false로 오고, 거기서 끊으면 답이 늘 비어 보인다.
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

/// **서버가 실제로 돌려준 이벤트**를 화면 파이프라인에 그대로 통과시킨다.
///
/// 화면 테스트는 손으로 만든 `Diff`를 쓰므로 도구 결과의 필드 이름이 어긋나거나 attacca가
/// 결과를 감싸 보내면 못 본다. 여기는 그 왕복을 진짜로 한 바퀴 돌린 것만 본다.
fn check_the_diff_paints(events: &[zyris_attacca::ZSessionEvent]) {
    use zyris_code::rows::{rows, Fold, Folds};
    use zyris_code::timeline::Timeline;

    let mut timeline = Timeline::new();
    let mut folds = Folds::new();
    for event in events {
        if let Some(entry) = zyris_code::event::entry_from(event) {
            // 도구를 쓴 그 순간처럼 전부 펴 둔다.
            folds.insert(entry.seq, Fold { open: true });
            timeline.upsert(entry);
        }
    }
    let drawn = rows(timeline.items(), 100, &folds);
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
