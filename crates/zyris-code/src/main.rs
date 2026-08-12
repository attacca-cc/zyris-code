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
use zyris_code::{app, lang};

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
    //
    // **The default is the platform's temp directory, not `/tmp`.** Windows has no `\tmp` on the
    // current drive, so `File::create` failed there and the `Err` arm below silently dropped the
    // file layer — leaving no logs at all on the one platform where nothing can be reproduced
    // locally. `std::env::temp_dir` reads `%TEMP%` there and `/tmp` here.
    let log = std::env::var("ZYRIS_CODE_LOG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("zyris-code.log"));
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
        }
    }

    // **One credential, one node each.** Every window holds the same device credential — splitting
    // that per window was tried (2026-08-12) and taken out, because identity then depended on what
    // else happened to be running and ordinary use asked for approval again and again. What varies
    // is the *node*: a window past the first dials with one `register_node` minted under this same
    // device (`nodes.rs`), so the server routes its session's tool calls to it rather than handing
    // everything to whichever connection arrived last.
    //
    // **The handle lives as long as `main`** — dropping it removes the lock file, and a window
    // that let go early would look absent to the next one to start.
    let window = zyris_code::conn::credential_dir().map(|dir| {
        let base =
            std::env::var("ZYRIS_PROFILE").unwrap_or_else(|_| zyris_code::conn::APP.to_string());
        zyris_code::conn::claim_window(&dir, &base)
    });

    // The profile splits again inside that directory. Unset just leaves it `default`, and now
    // that `default` is ours.
    if std::env::var_os("ZYRIS_PROFILE").is_none() {
        std::env::set_var("ZYRIS_PROFILE", zyris_code::conn::APP);
    }

    // What's left in the old place is **moved over** (not copied). If two live refresh tokens
    // sit on disk, both will be presented eventually, and attacca reads that reuse as a leaked
    // chain and revokes the whole node — both die.
    //
    // **Only this window's own profile is moved.** A later window's profile never existed back
    // then, so there is nothing of its to find, and reaching for another profile's file would log
    // that program out.
    if let (Some(dir), Some(legacy)) =
        (zyris_code::conn::credential_dir(), zyris_code::conn::legacy_credential_dir())
    {
        let profile =
            std::env::var("ZYRIS_PROFILE").unwrap_or_else(|_| zyris_code::conn::APP.to_string());
        let moved = zyris_code::conn::migrate_credentials(&legacy, &dir, &profile);
        if moved > 0 {
            tracing::info!(moved, "moved credentials from the old credential directory");
        }
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
    //
    // **The slot goes in the name.** Two windows are two nodes, and a node list where both are
    // called `arch zyris-code` tells nobody which is which — `conn::compose_name` puts the number
    // where the slug will still carry it.
    let slot = window.as_ref().map_or(1, |w| w.slot);
    if std::env::var_os("ZYRIS_NODE_NAME").is_none() {
        std::env::set_var("ZYRIS_NODE_NAME", zyris_code::conn::slot_node_name(slot));
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
    // Wait for the screen to attach before the runner runs — so the first enrollment code can
    // reach the screen. **Never wait forever.** If the app task dies before the screen attaches
    // (a terminal that cannot run the TUI, a panic inside `app::run`), the old code sat on the
    // shell's waiting line ("approve in the browser…") with a frozen cursor — exactly what the
    // 2026-08-07 Windows report saw. Say why and exit instead of hanging.
    let died_before_screen = tokio::select! {
        () = bridge.wait_screen() => None,
        ended = &mut app_task => Some(ended),
    };
    if let Some(ended) = died_before_screen {
        // The app may have died mid-raw-mode; restore the terminal so the reason is readable.
        ratatui::restore();
        let why = match ended {
            Ok(Ok(())) => "the screen ended before it attached".to_string(),
            Ok(Err(e)) => e.to_string(),
            Err(e) => e.to_string(),
        };
        notice.fatal_plain(&lang::current().screen_failed(&why));
        return ExitCode::FAILURE;
    }

    // **From here on, this node hands the computer to agents.**
    //
    // The moment it announces, every session on that account sees this node — sessions running in
    // another window can touch this computer too. The only thing blocking is `tools::guard::Gate`.
    let cwd = zyris_code::tools::working_dir();

    // **Every window past the first dials as a node of its own** (`nodes.rs`).
    //
    // It used to be one node for all of them, because identity is the credential and they share
    // one. The server registry is keyed by node id (`insert(node_id, connection)`), so the second
    // window replaced the first's connection and the first was handed no tool calls at all while
    // its socket stayed up. This app could only say so and carry on.
    //
    // **The judgment still belongs to the window that received the call** (plan mode, `/config`
    // dir access). That is now the same window whose session asked, which is what it always
    // should have been.
    //
    // **The handle has to live as long as the window does.** Bound inside a block it was dropped at
    // the closing brace, and `Drop` deletes the very file it had just written — so no window ever
    // left a lock behind and no later window ever found one.
    //
    // **This waits, and it waits where the screen can say so.** Only the first window can register
    // (it is the one holding a connection), so a second window leaves a request and watches for
    // the answer. Failing to get one is not failing to start — it shares, and says so.
    let mut sharing = window.as_ref().is_some_and(|w| w.lock.is_none());
    if slot > 1 && !sharing {
        let dir = zyris_code::conn::credential_dir();
        let name = zyris_code::conn::node_name();
        match &dir {
            Some(dir) => match zyris_code::nodes::obtain(dir, slot, &name).await {
                Some(child) => {
                    tracing::info!(slot, node = %child.node_id, name = %child.name, "this window has a node of its own");
                    std::env::set_var("ZYRIS_NODE_TOKEN", &child.token);
                }
                None => sharing = true,
            },
            None => sharing = true,
        }
        if sharing {
            // Back to the first window's name. Dialling on the shared credential makes this the
            // same node whatever the name says, and a name claiming otherwise only misleads
            // whoever reads the log.
            std::env::set_var("ZYRIS_NODE_NAME", zyris_code::conn::default_node_name());
        }
    }
    // **Told once, and only when it is true.** With one node between them the server keeps the
    // connection that arrived last, so this window has just taken the tool calls from the other
    // one — see `### 창 여럿` in CLAUDE.md.
    if sharing {
        tracing::warn!(
            "another zyris-code window is attached with the same credentials. tool calls go to \
             whichever window the server picked."
        );
        bridge.frame(zyris_code::app::Frame::Notice(
            zyris_code::lang::current().another_window_notice().to_string(),
        ));
    }
    let _instance_lock = window;
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

                // **Hand the connection to the screen side so `/reconnect` can drop it.** Set on
                // every connect, so it is always the live one.
                bridge.set_connection(conn.clone());

                // **On disconnect, leave a mark on the screen and in the log.** `Runner` reconnects,
                // but meanwhile the screen looks like nothing happened — silent failure is the worst.
                {
                    let watching = conn.clone();
                    let bridge = bridge.clone();
                    tokio::spawn(async move {
                        let reason = watching.closed().await;
                        tracing::warn!(%reason, "the connection dropped. Runner reattaches");
                        bridge.frame(app::Frame::Disconnected(reason.to_string()));
                    });
                }

                match conn.wait_capability::<AttaccaApiClient>(CONSUME_WAIT).await {
                    Ok(api) => {
                        // Announce the new handle. An app already running picks it up.
                        let _ = api_tx.send(Some(Arc::new(api)));
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "the server did not announce attacca_api")
                    }
                }
            }
        }
    });

    // **The first window answers the others.** A window past the first cannot register for itself
    // without opening a second connection on the shared credential, which is the very displacement
    // all of this exists to avoid — so it leaves a request file and this loop services it.
    //
    // It runs only while connected, and starts over on every reconnect: the client is handed over
    // through the same watch channel the screen reads.
    if slot == 1 {
        if let Some(dir) = zyris_code::conn::credential_dir() {
            let mut api_rx = api_rx.clone();
            tokio::spawn(async move {
                loop {
                    let client = api_rx.borrow_and_update().clone();
                    if let Some(api) = client {
                        zyris_code::nodes::serve_requests(
                            &api,
                            &dir,
                            &zyris_code::conn::REQUIRED_SCOPES,
                        )
                        .await;
                    }
                    // **Polled, not watched.** A request arrives as a file another process wrote,
                    // and two seconds of waiting is invisible against a window that is still
                    // drawing "connecting…". Watching the directory would mean a platform-specific
                    // dependency for a file that appears at most once per window.
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            });
        }
    }

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
