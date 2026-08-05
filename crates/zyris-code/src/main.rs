//! zyris-code — a terminal client that talks to an Attacca agent.
//!
//! This file is thin. `zyris::runtime::Runner` does the connecting, and `app::run` does the screen.

use std::process::ExitCode;
use std::time::Duration;

use zyris::runtime::Runner;
use zyris::NodeKind;
// `AttaccaApi` is a trait. Calling its methods requires the trait to be in scope,
// not just the client type — otherwise you get stuck with "method not found".
use std::sync::Arc;

use tokio::sync::watch;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use zyris_attacca::AttaccaApiClient;
use zyris_code::app;

/// The server announces `attacca_api` right after the handshake. A generous margin.
const CONSUME_WAIT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> ExitCode {
    // Where we talk to the shell while connecting. **Before the screen appears this is the only window.**
    let notice = zyris_code::notice::Notice::new();

    // Logs go to a file. Sent to the terminal, they'd land in the middle of the TUI, and ratatui's
    // double buffer would treat that spot as "unchanged" and never redraw it.
    //
    // **Only connection failures also reach the shell.** If logs only went to a file, when the server
    // dies the user sees nothing but a frozen cursor — the `notice` layer collects the reason.
    let log = std::env::var("ZYRIS_CODE_LOG").unwrap_or_else(|_| "/tmp/zyris-code.log".into());
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        // **It's `zyris=info`.** If only disconnects (`warn`) were kept, the reconnecting would vanish
        // from the log and "keeps disconnecting" couldn't be traced later.
        .unwrap_or_else(|_| "zyris_code=info,zyris=info".into());
    match std::fs::File::create(&log) {
        Ok(file) => {
            tracing_subscriber::registry()
                .with(tracing_subscriber::fmt::layer().with_writer(file).with_ansi(false))
                .with(filter)
                .with(notice.layer())
                .init();
        }
        // Not being able to open the log file is no reason to break the app. Keep at least the shell notice.
        Err(_) => {
            tracing_subscriber::registry().with(filter).with(notice.layer()).init();
        }
    }

    // While it can't connect, it watches and talks in red. Once connected it quiets down by itself,
    // and **once the screen is up it falls silent** — the screen does the talking (including the
    // enrollment code window). The bridge is built below, so the `watch` call moves after that too.
    // The screen is raised before the runner, so this watcher is quiet from the very first enrollment.

    // **Decide the screen language first.** Even the words that go to the shell before connecting (`notice`) use this value.
    // Order: `$ZYRIS_CODE_LANG` → last choice → locale.
    zyris_code::lang::set(zyris_code::lang::startup());

    // Credentials live in **an app-specific directory** — `~/.config/zyris-code/`.
    //
    // Previously, every program using zyris shared `~/.config/zyris/`. There, file names were only
    // `wss-<server>-<profile>.json`, so two programs that didn't set a profile registered over each
    // other's identity on the same file. Separating by profile leaned on a naming rule and was a
    // stopgap; splitting the directory is the fundamental fix.
    //
    // **Upstream offers no way to set that** (`RunConfig::app` isn't in zyris `main`). Instead, since
    // zyris's `config_dir()` checks `$ZYRIS_CONFIG_DIR` first, we fill that variable — **a value the
    // person gave wins.**
    if std::env::var_os("ZYRIS_CONFIG_DIR").is_none_or(|v| v.is_empty()) {
        if let Some(dir) = zyris_code::conn::credential_dir() {
            std::env::set_var("ZYRIS_CONFIG_DIR", &dir);
            // What's left in the old place is **moved over** (not copied). If two live refresh tokens
            // sit on disk, both will be presented eventually, and attacca reads that reuse as a leaked
            // chain and revokes the whole node — both die.
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

    // The profile splits again inside that directory. Unset just leaves it `default`, and now
    // that `default` is ours.
    if std::env::var_os("ZYRIS_PROFILE").is_none() {
        std::env::set_var("ZYRIS_PROFILE", "zyris-code");
    }

    // **The scopes to request must be decided before credentials are made.** Since `Enroller` copies
    // `config.scopes` when it's built, values given later via `Runner::request_scopes` don't ride on
    // the enrollment request — the approval screen would then appear without requesting any scope.
    //
    // Here too **the person's `$ZYRIS_SCOPES` wins** (same meaning as upstream `scopes_pinned`).
    if std::env::var_os("ZYRIS_SCOPES").is_none() {
        std::env::set_var("ZYRIS_SCOPES", zyris_code::conn::REQUIRED_SCOPES.join(","));
    }

    // Same for the node name. **Registering with just the hostname makes you the same identity as the
    // machine's other nodes** — if `zyris-daemon` runs alongside, both attach as `arch`, and attacca
    // separates them by appending `-2` to one, but which one keeps `arch` depends on attach order.
    // Then the tool names the agent reads (`zyris__arch__…`) change from run to run.
    if std::env::var_os("ZYRIS_NODE_NAME").is_none() {
        std::env::set_var("ZYRIS_NODE_NAME", zyris_code::conn::node_name());
    }

    // **The app is raised before the runner.** That way the enrollment code window is on screen from
    // the first enrollment (before connecting) — no more stdout box leaking into the terminal like
    // before. `Runner` calls `on_connect` again on every reconnect, but the app is started once here
    // and only the handle is swapped (`api_rx`).
    let (api_tx, api_rx) = watch::channel::<Option<Arc<AttaccaApiClient>>>(None);
    let api_tx = Arc::new(api_tx);

    // The bridge between the tools side and the screen side. **No approval can be received before the screen attaches.**
    let bridge = zyris_code::tools::bridge::Bridge::new();

    // While it can't connect, it watches and talks to the shell. **Once the screen is up it falls
    // silent** — the screen talks (enrollment code window included), and since it comes up before the runner, it's quiet from the first enrollment.
    notice.watch(bridge.clone());

    // The signal that tells the screen to close when the runner ends fatally. The app sees this,
    // restores the terminal, and exits; `main` then tells the shell the reason.
    let (die_tx, die_rx) = watch::channel(false);

    // **The screen is raised first.** Here, not in `on_connect` — the enrollment code window must
    // reach it before the first connection too (`enroll::ScreenEnroll`).
    let mut app_task = tokio::spawn(app::run(api_rx.clone(), bridge.clone(), die_rx));
    // Wait for the screen to attach before the runner runs — so the first enrollment code can reach the screen.
    bridge.wait_screen().await;

    // **From here on, this node hands the computer to agents.**
    //
    // The moment it announces, every session on that account sees this node — sessions running in
    // another window can touch this computer too. The only thing blocking is `tools::guard::Gate`.
    let cwd = zyris_code::tools::working_dir();

    // **Multiple windows are not blocked. They are announced.**
    //
    // With two windows using the same credentials, the server registry **overwrites with the later
    // connection** — the first window's socket stays alive but stops receiving tool calls. Blocking
    // wouldn't change that (with the same directory, whichever window receives, the files changed are
    // the same), so the second window isn't refused; but if it tangles quietly, the person has no way
    // to know which window is receiving. So when an earlier window is alive, that fact is said once
    // in the activity line (`conn::another_instance_alive`; lock file `.instance-<profile>.lock`).
    //
    // You only need to know one thing: judgment (plan mode · open directory) and the approval window
    // belong to **the window that received the call**; if the two windows are in different modes, the server decides which rules apply.
    if let Some(dir) = zyris_code::conn::credential_dir() {
        let profile =
            std::env::var("ZYRIS_PROFILE").unwrap_or_else(|_| zyris_code::conn::APP.to_string());
        if zyris_code::conn::another_instance_alive(&dir, &profile) {
            tracing::warn!("다른 zyris-code 창이 같은 자격으로 붙어 있습니다. 도구 호출은 서버가 고른 창으로 갑니다.");
            bridge.frame(zyris_code::app::Frame::Notice(
                zyris_code::lang::current().another_window_notice().to_string(),
            ));
        } else {
            // Hold the lock for as long as we live — Drop removes it when the window ends.
            let _lock = zyris_code::conn::claim_instance_lock(&dir, &profile);
        }
    }
    //
    // **The enrollment code is sent to the screen by upstream's `EnrollmentUi` hook** (`enroll.rs`, upstream PR #6).
    // Only when there's no screen (the extreme where the app couldn't start) does it fall to a stdout box. The old
    // "leaking into the terminal behind the screen" problem is structurally gone.
    let config = zyris::runtime::RunConfig::from_env();
    let creds: Arc<dyn zyris::runtime::credentials::Credentials> =
        match zyris_code::enroll::source(&config, &bridge) {
            Ok((creds, reauth)) => {
                // You only learn permissions are short after connecting (`me()`). To be able to drop
                // the credentials then, a handle is set on the screen side — **used once per process.**
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
    // Scope needed per call (attacca `zyris_gateway.rs`'s `require(ApiScope::…)`):
    //
    //   me                                          none
    //   list_agents                                 agents:read
    //   create_session_with · send_message
    //     · cancel_turn                             sessions:write
    //   turn_events · session_history               events:read     ← essential for v1
    //   list_sessions · session_usage               sessions:read   ← v2 picker
    //   list_projects                               projects:read   ← v2 picker
    //
    // **If `events:read` is missing, sending works but the answer never comes.** Only the stream is
    // quietly blocked with ForbiddenScope and the screen looks like nothing happened — it caught us once.
    //
    // The rest (agents:write·projects:write·jobs:*·artifacts:*·kanban:*) has no caller,
    // so it isn't requested. Unused permissions only widen the blast radius if a token leaks.
    //
    // **The list lives in one place** (`conn::REQUIRED_SCOPES`). If what's requested and what's
    // checked after attaching diverge, you'd either deny something you never asked for or stay silent about something missing.
    .request_scopes(zyris_code::conn::REQUIRED_SCOPES)
    // **Splitting windows is the server's job.** The node has nothing to offer — neither `.instance(…)`
    // nor registering siblings via `register_node` exists on the real server (measured 2026-08-03). So
    // here it just attaches, and the day the server starts splitting nodes, it happens by itself.
    .on_connect({
        let bridge = bridge.clone();
        let notice = notice.clone();
        move |conn| {
            let api_tx = Arc::clone(&api_tx);
            // `on_connect` is called again on every reconnect. The bridge and notice are handles, so
            // clones are passed — moving them outright would leave nothing to move on the second connect.
            let bridge = bridge.clone();
            let notice = notice.clone();
            async move {
                // **Attached.** The screen is already up, so the shell notice falls silent —
                // cutting into what ratatui is drawing means that cell never gets redrawn.
                notice.connected();

                // **On disconnect, leave a mark on the screen and in the log.** `Runner` reconnects,
                // but meanwhile the screen looks like nothing happened — silent failure is the worst.
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
                        // Announce the new handle. An app already running picks it up.
                        let _ = api_tx.send(Some(Arc::new(api)));
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "서버가 attacca_api를 announce하지 않았다")
                    }
                }
            }
        }
    });

    // **MCP attaches in the background.** Servers fetched with `npx` take seconds, and the screen
    // must not wait for that. Once they're up, `Capabilities::add` announces again.
    zyris_code::tools::start_mcp(runner.capabilities(), cwd, bridge);

    // **It doesn't die quietly.** `run()` sends the reason only to the log and leaves an exit code.
    //
    // **Whichever of runner and app ends first decides the exit.** If the app ends first (the user
    // quit with Ctrl+C), it just ends as success; if the runner ends first (fatal error), it tells
    // the screen to close — to restore the terminal — then tells the shell the reason and leaves an exit code.
    let running = runner.try_run();
    tokio::pin!(running);
    // Both are borrowed as `&mut` — if the runner ends first, the app is still held to restore the
    // terminal before stating the reason, so it waits again on that side.
    let outcome = tokio::select! {
        result = &mut running => RunnerEnded::Runner(result),
        app_result = &mut app_task => RunnerEnded::App(app_result),
    };
    match outcome {
        // The runner ended cleanly (SIGINT).
        RunnerEnded::Runner(Ok(())) => ExitCode::SUCCESS,
        RunnerEnded::Runner(Err(e)) => {
            // Close the screen first if it's alive — the reason is visible only after the terminal
            // is restored. If it can't be closed (app already dead), just say it.
            let _ = die_tx.send(true);
            let _ = tokio::time::timeout(Duration::from_secs(3), app_task).await;
            notice.fatal(&e.to_string());
            e.exit_code()
        }
        // If the screen closed, the process must end too. `Runner::run()` keeps running, holding the
        // connection even after hooks finish — the app ending is itself the exit signal.
        RunnerEnded::App(app_result) => match app_result {
            Ok(Ok(())) => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        },
    }
}

/// Which of runner and app ended first. The branch that decides the exit code.
enum RunnerEnded {
    Runner(Result<(), zyris::runtime::RunError>),
    App(Result<Result<(), anyhow::Error>, tokio::task::JoinError>),
}
