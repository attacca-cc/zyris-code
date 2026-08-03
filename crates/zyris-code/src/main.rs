//! zyris-code — Attacca 에이전트와 대화하는 터미널 클라이언트.
//!
//! 이 파일은 얇다. 붙는 일은 `zyris::runtime::Runner`가 하고, 화면은 `app::run`이 한다.

use std::process::ExitCode;
use std::time::Duration;

use zyris::runtime::Runner;
use zyris::NodeKind;
// `AttaccaApi`는 트레이트다. 메서드를 부르려면 클라이언트 타입만으로는 안 되고
// 트레이트가 스코프에 있어야 한다 — 없으면 "method not found"로 막힌다.
use std::sync::Arc;

use tokio::sync::watch;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use zyris_attacca::AttaccaApiClient;
use zyris_code::app;

/// 서버는 핸드셰이크 직후 `attacca_api`를 announce한다. 넉넉한 여유값이다.
const CONSUME_WAIT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> ExitCode {
    // 붙는 동안 셸에 말해 주는 자리. **화면이 뜨기 전에는 여기가 유일한 창구다.**
    let notice = zyris_code::notice::Notice::new();

    // 로그는 파일로 보낸다. 터미널로 보내면 TUI 한가운데 찍히고, 그 자리를 ratatui의
    // 이중 버퍼가 "안 바뀌었다"고 여겨 다시 그리지도 않는다.
    //
    // **다만 연결 실패만은 셸에도 보인다.** 로그가 파일로만 가면 서버가 죽었을 때
    // 사용자가 보는 것은 멈춘 커서 하나뿐이다 — `notice` 층이 그 사유를 주워 둔다.
    let log = std::env::var("ZYRIS_CODE_LOG").unwrap_or_else(|_| "/tmp/zyris-code.log".into());
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        // **`zyris=info`다.** 끊김(`warn`)만 남기면 다시 붙는 과정이 로그에서 사라져,
        // "계속 끊긴다"를 나중에 되짚을 수가 없다.
        .unwrap_or_else(|_| "zyris_code=info,zyris=info".into());
    match std::fs::File::create(&log) {
        Ok(file) => {
            tracing_subscriber::registry()
                .with(tracing_subscriber::fmt::layer().with_writer(file).with_ansi(false))
                .with(filter)
                .with(notice.layer())
                .init();
        }
        // 로그 파일을 못 여는 것이 앱을 못 쓸 이유는 아니다. 셸 알림만은 살려 둔다.
        Err(_) => {
            tracing_subscriber::registry().with(filter).with(notice.layer()).init();
        }
    }

    // 못 붙는 동안 지켜보다 빨간 글씨로 말한다. 붙으면 저절로 조용해지고,
    // **화면이 뜨면 입을 다문다** — 화면이 말한다(등록 코드 창 포함).
    // 다리는 아래에서 만들므로 `watch` 호출도 그 뒤로 옮긴다.
    // 화면은 러너보다 먼저 띄우므로, 첫 등록부터 이 감시자는 조용하다.

    // **화면 말을 먼저 정한다.** 붙기 전에 셸로 나가는 말(`notice`)도 이 값을 쓴다.
    // 순서: `$ZYRIS_CODE_LANG` → 지난번에 고른 것 → 로케일.
    zyris_code::lang::set(zyris_code::lang::startup());

    // 자격은 **이 앱 전용 디렉터리**에 둔다 — `~/.config/zyris-code/`.
    //
    // 예전에는 `~/.config/zyris/`를 zyris를 쓰는 모든 프로그램이 같이 썼다. 거기 파일
    // 이름은 `wss-<서버>-<프로필>.json`뿐이라, 프로필을 안 정한 두 프로그램이 같은
    // 파일을 두고 서로의 신원 위에 등록했다. 프로필로 떼어 놓는 것은 이름 규칙에 기대는
    // 임시방편이었고, 디렉터리를 나누는 쪽이 근본이다.
    //
    // **상류에는 그것을 정하는 길이 없다**(`RunConfig::app`은 zyris `main`에 없다). 대신
    // zyris의 `config_dir()`이 `$ZYRIS_CONFIG_DIR`을 가장 먼저 보므로, 그 변수를 우리가
    // 채운다 — **사람이 준 값이 있으면 그쪽이 이긴다.**
    if std::env::var_os("ZYRIS_CONFIG_DIR").is_none_or(|v| v.is_empty()) {
        if let Some(dir) = zyris_code::conn::credential_dir() {
            std::env::set_var("ZYRIS_CONFIG_DIR", &dir);
            // 옛 자리에 남은 것은 **옮겨 온다**(복사가 아니다). 살아 있는 리프레시 토큰이
            // 디스크에 둘이면 언젠가 둘 다 제시되고, attacca는 그 재사용을 사슬이 샌
            // 것으로 보아 노드를 통째로 revoke한다 — 둘 다 죽는다.
            if let Some(legacy) = zyris_code::conn::legacy_credential_dir() {
                let profile = std::env::var("ZYRIS_PROFILE")
                    .unwrap_or_else(|_| zyris_code::conn::APP.to_string());
                let moved = zyris_code::conn::migrate_credentials(&legacy, &dir, &profile);
                if moved > 0 {
                    tracing::info!(moved, "옛 자격 디렉터리에서 자격을 옮겨 왔다");
                }
            }
        }
    }

    // 프로필은 그 디렉터리 안에서 다시 갈라진다. 안 정하면 `default`로 두면 되고, 이제
    // 그 `default`는 우리 것이다.
    if std::env::var_os("ZYRIS_PROFILE").is_none() {
        std::env::set_var("ZYRIS_PROFILE", "zyris-code");
    }

    // **요청할 스코프는 자격을 만들기 전에 정해져야 한다.** `Enroller`는 만들어질 때
    // `config.scopes`를 복사해 가므로, 그 뒤에 `Runner::request_scopes`로 준 값은 등록
    // 요청에 실리지 않는다 — 그러면 권한을 하나도 요청하지 않은 채 승인 화면이 뜬다.
    //
    // 여기서도 **사람이 준 `$ZYRIS_SCOPES`가 이긴다**(상류 `scopes_pinned`와 같은 뜻이다).
    if std::env::var_os("ZYRIS_SCOPES").is_none() {
        std::env::set_var("ZYRIS_SCOPES", zyris_code::conn::REQUIRED_SCOPES.join(","));
    }

    // 노드 이름도 마찬가지다. **호스트 이름만으로 등록하면 이 머신의 다른 노드와 같은
    // 신원이 된다** — `zyris-daemon`이 같이 돌면 둘 다 `arch`로 붙고, attacca가 한쪽에
    // `-2`를 붙여 떼어 놓는데 어느 쪽이 `arch`를 갖는지는 붙는 순서에 달려 있다.
    // 그러면 에이전트가 읽는 도구 이름(`zyris__arch__…`)이 실행마다 바뀐다.
    if std::env::var_os("ZYRIS_NODE_NAME").is_none() {
        std::env::set_var("ZYRIS_NODE_NAME", zyris_code::conn::node_name());
    }

    // **앱은 러너보다 먼저 띄운다.** 그래야 첫 등록(연결 전)부터 등록 코드 창이
    // 화면에 뜬다 — 예전처럼 stdout 상자가 터미널에 새는 일이 없다. `Runner`는
    // 재연결할 때마다 `on_connect`를 다시 부르지만, 앱은 여기서 한 번만 띄우고
    // 손잡이만 갈아 끼운다(`api_rx`).
    let (api_tx, api_rx) = watch::channel::<Option<Arc<AttaccaApiClient>>>(None);
    let api_tx = Arc::new(api_tx);

    // 도구 쪽과 화면 쪽을 잇는 다리. **화면이 붙기 전에는 어떤 승인도 받을 수 없다.**
    let bridge = zyris_code::tools::bridge::Bridge::new();

    // 못 붙는 동안 지켜보다 셸에 말한다. **화면이 뜨면 입을 다문다** — 화면이
    // 말한다(등록 코드 창 포함). 화면은 러너보다 먼저 뜨므로 첫 등록부터 조용하다.
    notice.watch(bridge.clone());

    // 러너가 치명적으로 끝났을 때 화면에도 닫으라고 보내는 신호. 앱은 이것을 보고
    // 터미널을 되돌린 뒤 끝나고, `main`이 그다음에 사유를 셸에 말한다.
    let (die_tx, die_rx) = watch::channel(false);

    // **화면을 먼저 띄운다.** `on_connect`가 아니라 여기다 — 등록 코드 창이
    // 첫 연결 전에도 도달해야 한다(`enroll::ScreenEnroll`).
    let mut app_task = tokio::spawn(app::run(api_rx.clone(), bridge.clone(), die_rx));
    // 러너가 돌기 전에 화면이 붙기를 기다린다 — 첫 등록 코드가 화면으로 갈 수 있게.
    bridge.wait_screen().await;

    // **여기서부터 이 노드는 에이전트에게 컴퓨터를 내준다.**
    //
    // announce하는 순간 그 계정의 모든 세션이 이 노드를 본다 — 다른 창에서 돌던 세션도
    // 이 컴퓨터를 만질 수 있다. 막는 것은 `tools::guard::Gate` 하나뿐이다.
    let cwd = zyris_code::tools::working_dir();

    // **창을 여럿 띄우는 것은 막지 않는다. 알리기는 한다.**
    //
    // 같은 자격을 쓰는 창이 둘이면 서버 레지스트리는 **나중 연결로 덮어쓴다** — 먼저
    // 뜬 창의 소켓은 살아 있는 채로 도구 호출을 못 받는다. 막는 것은 그 사실을 바꾸지
    // 못하므로(같은 디렉터리라면 어느 창이 받든 바뀌는 파일은 같다) 두 번째 창을
    // 거부하지는 않지만, 조용히 꼬이면 사람은 어느 창이 받는지 알 길이 없다. 그래서
    // 먼저 살아 있는 창이 있으면 그 사실을 활동 줄에 한 번 말해 준다
    // (`conn::another_instance_alive`, 잠금 파일은 `.instance-<프로필>.lock`).
    //
    // 하나만 알고 있으면 된다: 판정(계획 모드·열어 둔 디렉터리)과 승인 창은 **호출을 받은
    // 창의 것**이다. 두 창의 모드가 다르면 어느 쪽 규칙으로 걸릴지는 서버가 정한다.
    if let Some(dir) = zyris_code::conn::credential_dir() {
        let profile = std::env::var("ZYRIS_PROFILE")
            .unwrap_or_else(|_| zyris_code::conn::APP.to_string());
        if zyris_code::conn::another_instance_alive(&dir, &profile) {
            tracing::warn!("다른 zyris-code 창이 같은 자격으로 붙어 있습니다. 도구 호출은 서버가 고른 창으로 갑니다.");
            bridge.frame(zyris_code::app::Frame::Notice(
                "다른 zyris-code 창이 이미 같은 자격으로 붙어 있습니다. 승인 창이 그 창에 떠 있을 수 있습니다."
                    .to_string(),
            ));
        } else {
            // 살아 있는 동안 잠금을 붙들고 간다 — 창이 끝나면 Drop이 지운다.
            let _lock = zyris_code::conn::claim_instance_lock(&dir, &profile);
        }
    }
    //
    // **등록 코드는 상류의 `EnrollmentUi` 훅이 화면으로 보낸다**(`enroll.rs`, 상류 PR #6).
    // 화면이 없을 때만(앱을 못 띄운 극단) stdout 상자로 빠진다. 예전처럼 "화면 뒤
    // 터미널로 새는" 문제는 구조적으로 없다.
    let config = zyris::runtime::RunConfig::from_env();
    let creds: Arc<dyn zyris::runtime::credentials::Credentials> =
        match zyris_code::enroll::source(&config, &bridge) {
            Ok((creds, reauth)) => {
                // 권한이 모자란 것은 붙은 뒤에야 안다(`me()`). 그때 자격을 버릴 수 있게
                // 손잡이를 화면 쪽에 얹어 둔다 — **프로세스당 한 번만 쓴다.**
                if let Some(reauth) = reauth {
                    bridge.set_reauth(reauth);
                }
                creds
            }
            Err(e) => {
                notice.fatal(&e);
                return ExitCode::FAILURE;
            }
        };

    let runner = zyris_code::tools::announce(
        Runner::new(config, creds),
        cwd.clone(),
        bridge.clone(),
        api_rx.clone(),
    )
    .kind(NodeKind::Service)
    // 호출별로 필요한 스코프 (attacca `zyris_gateway.rs`의 `require(ApiScope::…)`):
    //
    //   me                                          없음
    //   list_agents                                 agents:read
    //   create_session_with · send_message
    //     · cancel_turn                             sessions:write
    //   turn_events · session_history               events:read     ← v1에 꼭 필요하다
    //   list_sessions · session_usage               sessions:read   ← v2 피커
    //   list_projects                               projects:read   ← v2 피커
    //
    // **`events:read`를 빠뜨리면 전송은 되는데 답이 안 온다.** 스트림만 조용히
    // ForbiddenScope로 막히고 화면은 아무 일도 없는 것처럼 보인다 — 실제로 한 번 걸렸다.
    //
    // 나머지(agents:write·projects:write·jobs:*·artifacts:*·kanban:*)는 부르는 곳이
    // 없으므로 요청하지 않는다. 쓰지 않는 권한은 토큰이 샜을 때 반경만 넓힌다.
    //
    // **목록은 한 군데다**(`conn::REQUIRED_SCOPES`). 요청하는 것과 붙은 뒤에 확인하는
    // 것이 갈라지면, 요청하지도 않은 것을 없다고 말하거나 없는데도 아무 말을 안 한다.
    .request_scopes(zyris_code::conn::REQUIRED_SCOPES)
    // **창을 가르는 일은 서버가 한다.** 노드가 댈 것이 없다 — `.instance(…)`도,
    // `register_node`로 형제를 찍는 것도 실제 서버에는 없다(2026-08-03 실측). 그래서
    // 여기서는 그냥 붙고, 서버가 노드를 갈라 주기 시작하면 그날부터 저절로 갈린다.
    .on_connect({
        let bridge = bridge.clone();
        let notice = notice.clone();
        move |conn| {
            let api_tx = Arc::clone(&api_tx);
            // `on_connect`는 재연결마다 다시 불린다. 다리와 알림은 손잡이라 사본으로
            // 넘긴다 — 통째로 옮기면 두 번째 연결에서 옮길 것이 없다.
            let bridge = bridge.clone();
            let notice = notice.clone();
            async move {
                // **붙었다.** 화면이 이미 떠 있으므로 셸 알림은 입을 다문다 —
                // ratatui가 그리는 자리에 끼어들면 그 칸은 다시 그려지지 않는다.
                notice.connected();

                // **끊기면 화면과 로그에 남긴다.** 다시 붙는 것은 `Runner`가 하지만,
                // 그동안 화면은 아무 일도 없는 것처럼 보인다 — 조용한 실패가 제일 나쁘다.
                {
                    let watching = conn.clone();
                    let bridge = bridge.clone();
                    tokio::spawn(async move {
                        let reason = watching.closed().await;
                        tracing::warn!(%reason, "연결이 끊겼다. Runner가 다시 붙는다");
                        bridge.frame(app::Frame::Disconnected(reason.to_string()));
                    });
                }

                match conn.wait_capability::<AttaccaApiClient>(CONSUME_WAIT).await {
                    Ok(api) => {
                        // 새 손잡이를 알린다. 이미 도는 앱은 이걸 집어 쓴다.
                        let _ = api_tx.send(Some(Arc::new(api)));
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "서버가 attacca_api를 announce하지 않았다")
                    }
                }
            }
        }
    });

    // **MCP는 배경에서 붙는다.** `npx`로 받아 오는 서버는 몇 초씩 걸리는데, 그동안
    // 화면이 안 뜨면 안 된다. 다 뜨면 `Capabilities::add`가 다시 announce한다.
    zyris_code::tools::start_mcp(runner.capabilities(), cwd, bridge);

    // **조용히 죽지 않는다.** `run()`은 사유를 로그로만 보내고 종료 코드만 남긴다.
    //
    // 러너와 앱 중 **먼저 끝나는 쪽이 종료를 정한다.** 앱이 먼저 끝나면(사용자가
    // Ctrl+C로 껐다) 그대로 성공으로 끝나고, 러너가 먼저 끝나면(치명적 오류) 화면에도
    // 닫으라고 알린 뒤 — 터미널을 되돌리게 — 사유를 셸에 말하고 종료 코드를 남긴다.
    let running = runner.try_run();
    tokio::pin!(running);
    // 두 가지를 모두 `&mut`로 빌린다 — 러너가 먼저 끝나면 앱을 아직 붙들고
    // 터미널을 되돌리게 한 뒤 사유를 말해야 하므로, 그쪽에서 다시 기다린다.
    let outcome = tokio::select! {
        result = &mut running => RunnerEnded::Runner(result),
        app_result = &mut app_task => RunnerEnded::App(app_result),
    };
    match outcome {
        // 러너가 깨끗하게 끝났다(SIGINT).
        RunnerEnded::Runner(Ok(())) => ExitCode::SUCCESS,
        RunnerEnded::Runner(Err(e)) => {
            // 화면이 살아 있으면 먼저 닫는다 — 터미널을 되돌린 뒤에 말해야
            // 사유가 보인다. 못 닫으면(앱이 이미 죽은 경우) 그냥 말한다.
            let _ = die_tx.send(true);
            let _ = tokio::time::timeout(Duration::from_secs(3), app_task).await;
            notice.fatal(&e.to_string());
            e.exit_code()
        }
        // 화면을 닫았으면 프로세스도 끝나야 한다. `Runner::run()`은 훅이 끝나도
        // 연결을 붙들고 계속 돈다 — 앱이 끝났다는 것이 곧 종료 신호다.
        RunnerEnded::App(app_result) => match app_result {
            Ok(Ok(())) => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        },
    }
}

/// 러너와 앱 중 무엇이 먼저 끝났는가. 종료 코드를 정하는 갈래다.
enum RunnerEnded {
    Runner(Result<(), zyris::runtime::RunError>),
    App(Result<Result<(), anyhow::Error>, tokio::task::JoinError>),
}
