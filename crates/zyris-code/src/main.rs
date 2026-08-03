//! zyris-code — Attacca 에이전트와 대화하는 터미널 클라이언트.
//!
//! 이 파일은 얇다. 붙는 일은 `zyris::runtime::Runner`가 하고, 화면은 `app::run`이 한다.

use std::process::ExitCode;
use std::time::Duration;

use zyris::runtime::Runner;
use zyris::NodeKind;
// `AttaccaApi`는 트레이트다. 메서드를 부르려면 클라이언트 타입만으로는 안 되고
// 트레이트가 스코프에 있어야 한다 — 없으면 "method not found"로 막힌다.
use std::sync::atomic::{AtomicBool, Ordering};
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

    // 못 붙는 동안 지켜보다 빨간 글씨로 말한다. 붙으면 저절로 조용해진다.
    notice.watch();

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

    // **앱은 한 번만 띄운다.**
    //
    // `Runner`는 재연결할 때마다 `on_connect`를 다시 spawn한다. 훅이 그대로 앱을 띄우면
    // 유휴 상태로 두다 연결이 한 번 끊겼다 붙는 순간 TUI가 둘이 되어 같은 터미널을 두고
    // 싸운다 — 화면이 깨지고 입력이 엉킨다.
    //
    // 그래서 첫 연결만 앱을 띄우고, 이후 연결은 손잡이만 갈아 끼워 준다. 앱은 매번
    // 최신 손잡이를 집어 쓴다.
    let started = Arc::new(AtomicBool::new(false));
    let (api_tx, api_rx) = watch::channel::<Option<Arc<AttaccaApiClient>>>(None);
    let api_tx = Arc::new(api_tx);

    // 도구 쪽과 화면 쪽을 잇는 다리. **화면이 붙기 전에는 어떤 승인도 받을 수 없다.**
    let bridge = zyris_code::tools::bridge::Bridge::new();

    // **여기서부터 이 노드는 에이전트에게 컴퓨터를 내준다.**
    //
    // announce하는 순간 그 계정의 모든 세션이 이 노드를 본다 — 다른 창에서 돌던 세션도
    // 이 컴퓨터를 만질 수 있다. 막는 것은 `tools::guard::Gate` 하나뿐이다.
    let cwd = zyris_code::tools::working_dir();

    // **창을 여럿 띄우는 것에 대해 아무것도 하지 않는다.**
    //
    // 예전에는 두 번째 창을 막았고, 그다음에는 알림을 띄웠다. 둘 다 걷어냈다 — 같은
    // 디렉터리라면 attacca가 어느 창으로 도구 호출을 보내든 **바뀌는 파일은 같다.**
    // 알림은 그 사실을 바꾸지 못하면서 뜰 때마다 화면 위에 한 덩이를 얹을 뿐이다.
    //
    // 하나만 알고 있으면 된다: 판정(계획 모드·열어 둔 디렉터리)과 승인 창은 **호출을 받은
    // 창의 것**이다. 두 창의 모드가 다르면 어느 쪽 규칙으로 걸릴지는 서버가 정한다.
    // **등록 코드는 상류가 stdout에 찍는다.** 값으로 받는 길이 없다.
    //
    // 처음 켤 때는 그래도 괜찮다 — TUI는 `on_connect`에서 뜨므로 그때 화면은 아직 없고,
    // 상류가 찍은 상자가 터미널에 그대로 남는다. 문제는 **화면이 떠 있는데 재등록이
    // 일어날 때**뿐이고, 그때는 `enroll::Watch`가 알아채 화면에 한 줄 남긴다.
    //
    // 한때는 `EnrollmentUi` 훅을 만들어 코드를 값으로 받았는데, 그 훅이 로컬 zyris에만
    // 있고 어디에도 올라가 있지 않아 이 리포가 통째로 안 빌드됐다. **상류에 없는 API에
    // 기대지 않는다.**
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
            let started = Arc::clone(&started);
            let api_tx = Arc::clone(&api_tx);
            let api_rx = api_rx.clone();
            // `on_connect`는 재연결마다 다시 불린다. 다리와 알림은 손잡이라 사본으로
            // 넘긴다 — 통째로 옮기면 두 번째 연결에서 옮길 것이 없다.
            let bridge = bridge.clone();
            let notice = notice.clone();
            async move {
                // **붙었다.** 화면이 뜰 참이므로 셸 알림은 여기서 입을 다문다 —
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
                        // 새 손잡이를 먼저 알린다. 이미 도는 앱은 이걸 집어 쓴다.
                        let _ = api_tx.send(Some(Arc::new(api)));
                        if started.swap(true, Ordering::SeqCst) {
                            tracing::info!("다시 붙었다. 앱은 이미 돌고 있다");
                            return;
                        }
                        let code = match app::run(api_rx, bridge).await {
                            Ok(()) => 0,
                            Err(e) => {
                                tracing::error!(error = %e, "앱이 끝났다");
                                1
                            }
                        };
                        // 화면을 닫았으면 프로세스도 끝나야 한다.
                        //
                        // `Runner::run()`은 훅이 끝나도 연결을 붙들고 계속 돈다. 의도적
                        // 종료로 치는 경로는 SIGINT 하나뿐인데, TUI는 raw 모드라 Ctrl+C가
                        // 시그널이 되지 않고 바이트로 들어온다 — 그래서 여기서 끝낸다.
                        // `conn.close()`는 Runner가 "끊겼다"로 보고 재연결하므로 답이 아니다.
                        //
                        // 터미널은 `app::run`이 이미 되돌린 뒤다.
                        std::process::exit(code);
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
    match runner.try_run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            notice.fatal(&e.to_string());
            e.exit_code()
        }
    }
}
