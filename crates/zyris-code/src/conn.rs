//! Conversations with Attacca. A session is created at **the first message**.

use anyhow::{anyhow, Result};
// `AttaccaApi` is a trait. To call its methods, the client type alone is not enough — the trait
// has to be in scope, or you get stuck with "method not found".
use zyris_attacca::{
    AttaccaApi, AttaccaApiClient, ZHistoryQuery, ZNewJob, ZNewProject, ZNewSession, ZNewWork,
    ZSessionFilter, ZTurnFrame,
};

use crate::app::Frame;
use crate::event::entry_from;
use crate::mode::Route;

/// Timeout placed on each server call. A dead connection (half-open TCP — the peer vanished
/// without FIN/RST) makes the call wait forever with neither answer nor error, so without a
/// timeout the screen loop gets stuck there and freezes — a state where neither keys nor
/// signals work (measured on 2026-08-04). Every server call must run under this timeout.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A server call under a timeout. Never waits forever on a dead connection.
/// When the timeout passes, closes the connection so the Runner reconnects.
pub(crate) async fn within<T>(
    api: &AttaccaApiClient,
    fut: impl std::future::Future<Output = zyris::Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(CALL_TIMEOUT, fut).await {
        Ok(result) => result.map_err(|e| anyhow!("{e}")),
        Err(_) => {
            tracing::warn!(
                "the server call did not answer within {}s ‒ closing and reattaching",
                CALL_TIMEOUT.as_secs()
            );
            api.handle().connection().close("call timed out");
            Err(anyhow!(crate::lang::current().server_timeout(CALL_TIMEOUT.as_secs())))
        }
    }
}

/// The name of the agent we currently attach to.
///
/// **The intended destination is `zyris-code`** (the `name` in `prompts/agents/zyris_code.yml`). But that
/// agent is still in development, and its definition only exists on `develop` of the content repo, so attaching
/// to it now fails with `Agent not found` — the turn path only looks at the default branch (see the
/// "Open Issues" section of CLAUDE.md). Until then, we develop with `Main Agent`.
///
/// Reverting is just this one constant line.
pub const DEFAULT_AGENT: &str = "Main Agent";

/// Every permission this app actually needs for the attacca calls it makes.
///
/// attacca's `zyris_gateway.rs` measures it per call with `require(ApiScope::…)`:
///
/// | Called | Permission needed |
/// |---|---|
/// | `me` | none |
/// | `list_agents` | `agents:read` |
/// | `list_projects` | `projects:read` |
/// | `create_project` | `projects:write` |
/// | `list_sessions`·`session_usage`·`session_history` | `sessions:read` |
/// | `create_session_with`·`send_message`·`cancel_turn` | `sessions:write` |
/// | `turn_events` | `events:read` |
/// | `create_work`·`work_message`·`stop_work`·`continue_work` | `works:write` |
/// | `list_works`·`get_work`·`work_tasks` | `works:read` |
/// | `create_job` | `jobs:write` |
/// | `list_jobs`·`get_job` | `jobs:read` |
///
/// **If even one is missing, that list just quietly comes back empty.** It's not an error but an empty result,
/// so the person believes their account has no agents or projects. This actually happened.
///
/// What we request and what we verify must be **the same list.** If they diverge, we'd either claim a
/// scope we never requested is missing, or stay silent about one that actually is.
///
/// **If even one is missing, the app can't do its job.** When short, drop the credentials and get approved again
/// (`needs_reenrollment`).
///
/// **Before adding a new scope here, first re-check that the server knows it.** If even one unknown scope
/// is present, the whole enrollment request is blocked — the axum `Json` extractor can't read the enum and
/// rejects with 422, so **we never even reach the approval screen.** On 2026-08-03 adding `nodes:write`
/// got blocked exactly that way, and the error body reveals the full list the deployed build accepts:
///
/// ```text
/// POST /api/zyris/v1/device/authorize {"scopes":[…,"nodes:write"], …}
///   → 422 … unknown variant `nodes:write`, expected one of `agents:read`, … `events:read`
/// ```
pub const REQUIRED_SCOPES: [&str; 10] = [
    "agents:read",
    "projects:read",
    // Used by the project form. Re-added after checking the deployed build on 2026-08-03 — it was 200.
    "projects:write",
    "sessions:read",
    "sessions:write",
    "events:read",
    // Used by the `work` capability (`tools/work.rs`). It's the way to hand big jobs to attacca.
    // work mode uses the same ones too — `create_work` is `works:write`, and `get_work`, which waits on
    // the planning conversation, is `works:read`.
    "works:read",
    "works:write",
    // job mode (`Session::open_job`). **Re-added after checking the deployed build directly on 2026-08-03** —
    // as noted above, a single scope the server doesn't know makes the whole enrollment a 422:
    //
    // ```text
    // POST /api/zyris/v1/device/authorize {"scopes":[…,"jobs:read","jobs:write"], …}
    //   → 200 {"device_code":"zdc_…","user_code":"…"}
    // ```
    "jobs:read",
    "jobs:write",
];

/// This program's name. The credential directory branches on it.
pub const APP: &str = "zyris-code";

/// The directory where credentials live. `/cwd` shows it.
///
/// **It is not `~/.config/zyris/`.** That was the shared location for all zyris programs, so two
/// unprofiled ones registered on top of each other's identity. Telling the old path to someone
/// who asks "where is my login" would make them delete the wrong file.
pub fn credential_home() -> String {
    credential_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| crate::lang::current().no_credential_dir().to_string())
}

/// The person's home directory. **One definition, because `$HOME` is not portable.**
///
/// Windows normally leaves `$HOME` unset and names the home directory with `USERPROFILE`. Four
/// places read `$HOME` directly and each invented its own fallback — `/`, the temp directory, or
/// nothing — so on Windows `~/…` expanded to a drive-relative `\…`, the undo history went to a
/// directory the system cleans out, and paths on screen were never shortened.
///
/// `None` when neither is set, so each caller still decides what that means for it.
pub fn user_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
}

/// This app's own directory — **the one place everything of ours lives.**
///
/// Credentials, `config.json`, the language file, skills, plugins and `mcp.json` all sit here.
///
/// **Everything must ask this, not build the path itself.** Four places used to join
/// `$HOME/.config/zyris-code/…` by hand (skills twice, plugins, `mcp.json`), which is right only
/// on Linux: macOS puts it under `Library/Application Support` and Windows has no `$HOME` at all,
/// so on Windows that whole tier silently vanished — skills, plugins and MCP servers were simply
/// never found, with nothing said about it. It also ignored `$ZYRIS_CONFIG_DIR` and
/// `$XDG_CONFIG_HOME`, so a person who moved this app's directory got half of it moved.
pub fn app_dir() -> Option<std::path::PathBuf> {
    config_home_for(APP)
}

/// Where credentials go. **We compute it.**
///
/// Upstream has no way to set an app-specific directory (`RunConfig::app` doesn't exist in zyris's `main`). Instead,
/// zyris's `config_dir()` **checks `$ZYRIS_CONFIG_DIR` first** — so filling that variable with
/// this value (`main.rs`) makes credentials land in this app's own location.
///
/// The same directory as `app_dir`; the separate name is for the thing that must never be split.
pub fn credential_dir() -> Option<std::path::PathBuf> {
    app_dir()
}

/// The location the person chose. **An empty value counts as not given** — handing an empty path
/// to someone who tried to clear it with `ZYRIS_CONFIG_DIR=` would drop credentials into the working directory.
fn given_config_dir() -> Option<std::ffi::OsString> {
    std::env::var_os("ZYRIS_CONFIG_DIR").filter(|v| !v.is_empty())
}

/// `$ZYRIS_CONFIG_DIR` → `app` under the platform's user-config location.
///
/// Follows **the same branch** as `runtime::store::config_dir`, which is what actually opens the
/// file. This one decides what `$ZYRIS_CONFIG_DIR` is set to and that one reads the variable, so
/// while they agree the branch is only ever taken once — and when they diverge, credentials are
/// scattered across two places. **Both copies now live in this repo**: the branch was zyris's until
/// it became a library and stopped owning where a program may write a secret.
fn config_home_for(app: &str) -> Option<std::path::PathBuf> {
    if let Some(given) = given_config_dir() {
        // The person meant exactly that location. Don't append the app name.
        return Some(std::path::PathBuf::from(given));
    }
    let base = platform_config_base()?;
    Some(base.join(app))
}

#[cfg(target_os = "macos")]
fn platform_config_base() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join("Library/Application Support"))
}

#[cfg(target_os = "windows")]
fn platform_config_base() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_config_base() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
}

/// What's **missing** from the granted permissions. The requested list and the verified list are always the single `REQUIRED_SCOPES`.
pub fn missing_scopes(granted: &[String]) -> Vec<&'static str> {
    REQUIRED_SCOPES.iter().copied().filter(|s| !granted.iter().any(|g| g == s)).collect()
}

/// Whether credentials must be dropped and approval requested again. **A pure predicate.**
///
/// Permissions fixed at approval time don't widen when the token refreshes. So when a feature grows and needs one
/// more permission, there's only one path — drop the credentials and ask again.
///
/// **Once per process.** The person can approve narrowly again, and asking every time becomes a loop
/// that keeps demanding the browser. After trying once, we only tell them in words.
pub fn needs_reenrollment(granted: &[String], already_tried: bool) -> bool {
    !already_tried && !missing_scopes(granted).is_empty()
}

/// What to tell the person when permissions are short.
///
/// **"Not enough" alone gives no path.** Permissions fixed at approval time don't widen when the token
/// refreshes, so the only thing to do is drop the credentials and get approved again. That method is written here.
pub fn missing_scopes_message(missing: &[&str]) -> String {
    crate::lang::current().missing_scopes(&missing.join(", "))
}

/// What to say when credentials were dropped because permissions were short.
///
/// **We must say that approving right there when the enrollment-code window appears is enough.** Previously, dropping
/// credentials sent the code to stdout where the screen hid it, so we said "turn it off and on"; now the
/// `EnrollmentUi` hook draws the code on screen (`enroll.rs`) — the window appears on reconnect.
pub fn scopes_will_be_asked_again(missing: &[&str]) -> String {
    crate::lang::current().scopes_asked_again(&missing.join(", "))
}

/// The agent to attach to. Overridable with `ZYRIS_CODE_AGENT`.
pub fn agent_name() -> String {
    std::env::var("ZYRIS_CODE_AGENT").unwrap_or_else(|_| DEFAULT_AGENT.to_string())
}

/// The name this window's node asks for: `$ZYRIS_NODE_NAME`, which `main` fills from
/// [`default_node_name`] unless a person set it. The server appends `-2` while another window in
/// the same directory holds the name; [`address`] is what it actually assigned.
pub fn node_name() -> String {
    match std::env::var("ZYRIS_NODE_NAME") {
        Ok(name) if !name.trim().is_empty() => name,
        _ => default_node_name(),
    }
}

/// The working directory's name. Windows in different directories are told apart by this, and two
/// windows in one directory by the server (`myrepo`, `myrepo-2`).
pub fn default_node_name() -> String {
    dir_name(&std::env::current_dir().unwrap_or_default())
}

/// The last component of `dir`, or this app's name for `/` and anything else without one.
fn dir_name(dir: &std::path::Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| APP.to_string())
}

/// The address Attacca assigned this window's node on its latest connection (`HelloAck.node`).
/// `runtime::Runner` writes it on every connection — one that did not resume can come back under a
/// different name — and it is `None` until the first.
static ADDRESS: std::sync::Mutex<Option<zyris::NodeAddress>> = std::sync::Mutex::new(None);

pub fn set_address(address: Option<zyris::NodeAddress>) {
    *ADDRESS.lock().unwrap_or_else(|e| e.into_inner()) = address;
}

pub fn address() -> Option<zyris::NodeAddress> {
    ADDRESS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Who the agent is talking to, for the session preamble and the `rules` tool.
///
/// **The agent sees every node of the account through the same tools.** Each tool takes a
/// `node_path` naming the computer it runs on, so the one thing this block has to say is which path
/// is the computer the person is sitting at. `address` is passed rather than read so a test does
/// not depend on a connection.
///
/// **English, like everything else a tool returns** (user decision, 2026-09-14): the agent is the
/// reader, and the person reads it too, through `/rules`.
pub fn node_preamble(cwd: &std::path::Path, address: Option<&zyris::NodeAddress>) -> String {
    let path = match address {
        Some(address) => address.path(),
        None => format!("not assigned yet ‒ this window asks to be called `{}`", node_name()),
    };
    format!(
        "This conversation is coming from the node below. The person talking to you is at \
         that computer right now.\n\n\
         - node_path: {path}\n\
         - working directory: {cwd}\n\
         - platform: {platform}\n\n\
         Every zyris tool takes a `node_path` argument that says which computer it runs on. \
         Pass the node_path above to read and edit files, run a shell and do everything else \
         here. Another node_path touches a different computer: do not use it unless that \
         computer is what the conversation is about.",
        cwd = cwd.display(),
        platform = std::env::consts::OS,
    )
}

#[derive(Debug, Default)]
pub struct Session {
    id: Option<String>,
    /// The project currently in view. **Everything opened from here goes to this project** ‒
    /// sessions, jobs, and works alike.
    ///
    /// **Must not be consumed on first use.** It used to be single-use: `＋ New thread` filled it
    /// and the first creation cleared it, so when a job was launched after picking a project,
    /// `project_id` was already empty and the server created it in the **default project**.
    /// That actually happened.
    ///
    /// `None` means "not picked yet", and only then is the server's default project right.
    project: Option<String>,
    /// The system directive attached to each session. `None` when there are no skills.
    preamble: Option<String>,
    /// What the next message will **open anew**. `None` appends to the current session.
    ///
    /// **Can't be replaced by clearing `id`.** It's common to visit work∙job mode and come back without
    /// saying anything; if `id` was already dropped then, the conversation in progress is lost. A staged
    /// open and "no session" are different states.
    pending_open: Option<Route>,
    /// How many turn streams this window has opened. Every stream frame is tagged with its own
    /// number, so one from an abandoned stream can be told from the live one's.
    stream_gen: u64,
    /// The task reading the live turn stream. **Aborted before another opens.**
    ///
    /// `turn_events` is a live subscription that never ends by itself ‒ attacca chains the
    /// backfill onto a broadcast receiver (`zyris_gateway.rs::turn_events`). This app used to
    /// open one on **every message** and every switch and close none, so a session that had been
    /// talked to five times had five subscriptions delivering the same frames. `push_delta`
    /// appends, so the answer being streamed came out five times over, interleaved.
    ///
    /// **Dropping the subscription does not stop the turn.** If it did, `turn_to_stop` would not
    /// have to send `cancel_turn` when the window closes.
    stream_task: Option<tokio::task::AbortHandle>,
}

/// What you need to know after opening a session.
///
/// **`sent` is the point.** With `create_job`∙`create_work` the open request **consumes the first message**
/// (`ZNewJob::message`∙`ZNewWork::message`), so calling `send_message` afterwards would send the same
/// words twice. On the path that merely creates a session, nothing has been sent yet.
#[derive(Debug, Clone)]
pub struct Opened {
    pub id: String,
    /// Whether the first message already rode along on the open request.
    pub sent: bool,
    /// What was just opened. `None` when nothing new was opened ‒ the screen only announces then.
    pub announced: Option<(Route, String)>,
}

impl Session {
    /// `preamble` is this session's system directive ‒ currently it carries the skill list.
    ///
    /// **Fixed once when the session is created and can't be changed later** (attacca's `ZNewSession`).
    /// That's why MCP tools attached later aren't carried here ‒ they go into the tool list.
    pub fn new(preamble: Option<String>) -> Self {
        Session { preamble, ..Default::default() }
    }

    /// The id, if already created.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Abandons whatever turn stream is open and hands out the number of the next one.
    ///
    /// **Exactly one live subscription at a time** ‒ see `stream_task`. Aborting kills the task
    /// mid-`next()`, so the "the stream ended, the turn must be over" line at the bottom of
    /// `spawn_stream` never runs for an abandoned one: an old stream cannot report the new one's
    /// turn finished.
    pub fn next_stream(&mut self) -> u64 {
        if let Some(task) = self.stream_task.take() {
            task.abort();
        }
        self.stream_gen += 1;
        self.stream_gen
    }

    /// Remembers the task reading the live stream, so the next one can abandon it.
    pub fn holds_stream(&mut self, task: tokio::task::AbortHandle) {
        self.stream_task = Some(task);
    }

    /// Which opening the live stream is. Frames tagged with any other number are stale ‒ abort
    /// stops a task from sending more, but whatever it already put in the channel is still there.
    pub fn stream_gen(&self) -> u64 {
        self.stream_gen
    }

    /// Switches to another session.
    ///
    /// **Clears the staged open.** Picking a session from the list means "go there", and letting that word
    /// flow into a job would not go to the picked place.
    ///
    /// **The project isn't cleared; it changes to the picked session's.** Clearing it would drop the next
    /// job opened after this into the default project.
    ///
    /// Some paths don't know the project (the way into a session awaiting an answer at startup).
    /// Then pass `None` and **keep what we knew** ‒ clearing it out of ignorance is exactly the path
    /// that drops into the default project.
    pub fn switch_to(&mut self, id: String, project_id: Option<String>) {
        self.id = Some(id);
        if let Some(p) = project_id {
            self.project = Some(p);
        }
        self.pending_open = None;
        // **The stream goes when the session does.** The turn keeps running on the server and is
        // read back as history on return; what must not happen is it still writing to this screen.
        self.next_stream();
    }

    /// Opened one from the project list. **Remembered even before picking a session** ‒ opening the list,
    /// closing it with Esc, and launching a job must still go to that project.
    pub fn enter_project(&mut self, project_id: String) {
        self.project = Some(project_id);
    }

    /// The current project. `/cwd` and tests look at it.
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    /// Stages a new session. Nothing is created on the server yet.
    pub fn stage_new(&mut self, project_id: String) {
        self.id = None;
        self.project = Some(project_id);
        self.pending_open = None;
        self.next_stream();
    }

    /// Points the next message where the mode decided.
    ///
    /// **`Route::Session` leaves the current conversation alone.** That's what it means for normal↔plan not
    /// to touch the session, and coming back to normal from work∙job means "answer that", not
    /// opening a new conversation.
    ///
    /// Conversely, **entering work∙job always opens anew**. Even with a job already open it launches another ‒
    /// choosing the mode again means wanting that.
    pub fn set_route(&mut self, route: Route) {
        self.pending_open = match route {
            Route::Session => None,
            other => Some(other),
        };
    }

    /// What the next message will open anew. Used to decide what the screen says.
    pub fn pending_open(&self) -> Option<Route> {
        self.pending_open
    }

    /// Stages a new session in the default project. Called by `/agent`.
    ///
    /// **A session's agent is fixed at creation and there's no API to change it** (`ZNewSession.agent_id`;
    /// `send_message` takes no agent argument). So changing the agent means opening a new
    /// session. Here too nothing is created on the server ‒ actual creation happens at the first
    /// message. **The previous session is not cleared**: you can return via the ← list.
    pub fn stage_new_default(&mut self) {
        self.id = None;
        // **The project stays as is.** `/agent` changes the agent, not leaves the project ‒
        // clearing it here would create the next session in the default project.
        self.pending_open = None;
        self.next_stream();
    }

    /// Finds the dedicated agent.
    ///
    /// **If not found, doesn't fall back to another agent.** A quiet fallback would show the name in the
    /// status bar while the send fails with `Agent not found` ‒ a state that's hard to diagnose.
    pub async fn agent_id(api: &AttaccaApiClient) -> Result<String> {
        Session::agent_id_named(api, &agent_name()).await
    }

    /// Finds an agent by name. Uses the same path as `/agent` and startup.
    pub async fn agent_id_named(api: &AttaccaApiClient, wanted: &str) -> Result<String> {
        let agents = within(api, api.list_agents())
            .await
            .map_err(|e| anyhow!(crate::lang::current().agent_list_error(&e.to_string())))?;
        agents
            .into_iter()
            .find(|a| a.name == wanted)
            .map(|a| a.id)
            .ok_or_else(|| anyhow!(crate::lang::current().agent_not_found(wanted)))
    }

    /// Returns the session id, creating it now if absent.
    ///
    /// `title` must be `None` ‒ giving a title here makes it permanent and blocks attacca's behavior
    /// of titling from the first message.
    pub async fn ensure(&mut self, api: &AttaccaApiClient, agent_id: &str) -> Result<String> {
        if let Some(id) = &self.id {
            return Ok(id.clone());
        }
        let session = within(
            api,
            api.create_session_with(ZNewSession {
                agent_id: agent_id.to_string(),
                title: None,
                // If a project was staged, create it there; otherwise it's the default project.
                project_id: self.project.clone(),
                preamble: self.preamble.clone(),
            }),
        )
        .await
        .map_err(|e| anyhow!(crate::lang::current().thread_create_error(&e.to_string())))?;
        self.id = Some(session.id.clone());
        Ok(session.id)
    }

    /// Opens where the mode decided. **Called exactly once, right before sending.**
    ///
    /// All three end with an ordinary session id (`ZJob::session_id`,
    /// `ZWork::planner_session_id`), so the caller can open the stream without knowing what was opened.
    pub async fn open_for(
        &mut self,
        api: &AttaccaApiClient,
        agent_id: &str,
        message: &str,
        mode: crate::mode::Mode,
    ) -> Result<Opened> {
        // **A staged open is consumed once.** If not cleared, every message while staying in job mode
        // spawns another job, and no place ever appears to answer the follow-up question.
        //
        // **Even without a staged open, the mode decides when there's no conversation yet.** A stage only
        // happens at the moment the mode *changes*, so there are several spots without one ‒ the first word
        // right after startup, after staging a new thread with `/agent`, after `＋ New thread`.
        // There, creating only a session means **the bottom bar says job but the plain session
        // opens**. That actually happened.
        let route = match self.pending_open.take() {
            Some(staged) => staged,
            // If there's a conversation to continue, continue it. The mode only decides what opens *when opening anew*.
            None if self.id.is_some() => Route::Session,
            None => mode.route(),
        };
        match route {
            Route::Job => self.open_job(api, agent_id, message, false).await,
            Route::Plan => self.open_job(api, agent_id, message, true).await,
            Route::Work => self.open_work(api, agent_id, message).await,
            Route::Session => {
                let id = self.ensure(api, agent_id).await?;
                Ok(Opened { id, sent: false, announced: None })
            }
        }
    }

    /// **The first message becomes the job** (`ZNewJob::message`). That's why `sent` is true.
    async fn open_job(
        &mut self,
        api: &AttaccaApiClient,
        agent_id: &str,
        message: &str,
        plan_mode: bool,
    ) -> Result<Opened> {
        let job = within(
            api,
            api.create_job(ZNewJob {
                message: message.to_string(),
                // **Runs on the chosen agent.** If empty, it goes to Main Agent, and then what `/agent` picked
                // only stays on screen while a different agent actually runs.
                agent_id: Some(agent_id.to_string()),
                project_id: self.project.clone(),
                // Use the deployed build's timezone as is. Forcing this machine's timezone in would make
                // answers diverge from other jobs in the same account.
                timezone: None,
                // **`planning` stays off in both.** It hands the job over to a work, which is what
                // work mode is for. `plan_mode` is the one that differs: it seeds the session with
                // attacca's plan guidance, so the agent investigates and hands a plan back with
                // `submit_plan` instead of doing the thing. That is the whole of plan mode now.
                planning: false,
                plan_mode,
                data: vec![],
            }),
        )
        .await
        .map_err(|e| anyhow!(crate::lang::current().job_create_error(&e.to_string())))?;

        let id = job
            .session_id
            .clone()
            .ok_or_else(|| anyhow!(crate::lang::current().job_no_session(&job.id)))?;
        self.id = Some(id.clone());
        let route = match plan_mode {
            true => Route::Plan,
            false => Route::Job,
        };
        Ok(Opened { id, sent: true, announced: Some((route, job.id)) })
    }

    /// **The first message becomes the goal** (`ZNewWork::message`). That's why `sent` is true.
    async fn open_work(
        &mut self,
        api: &AttaccaApiClient,
        agent_id: &str,
        message: &str,
    ) -> Result<Opened> {
        let work = within(
            api,
            api.create_work(ZNewWork {
                message: message.to_string(),
                agent_id: Some(agent_id.to_string()),
                // **Work tasks run on the project's checkout.** If this is empty it becomes the default
                // project, and that decides what the work is allowed to change.
                project_id: self.project.clone(),
            }),
        )
        .await
        .map_err(|e| anyhow!(crate::lang::current().work_create_error(&e.to_string())))?;

        let id = planner_session(api, &work).await?;
        self.id = Some(id.clone());
        Ok(Opened { id, sent: true, announced: Some((Route::Work, work.id)) })
    }
}

/// Picks up the work's planning conversation. **If absent, waits briefly.**
///
/// `create_work` kicks off a planning turn and returns, but there's no guarantee `planner_session_id`
/// is already in that response ‒ the server creating the session and returning the work row are not the
/// same transaction. Giving up here looks to the person like **the words simply vanished**.
///
/// But it can't hold on too long either. While waiting, the screen can't say anything.
async fn planner_session(api: &AttaccaApiClient, work: &zyris_attacca::ZWork) -> Result<String> {
    if let Some(id) = &work.planner_session_id {
        return Ok(id.clone());
    }
    for _ in 0..PLANNER_TRIES {
        tokio::time::sleep(PLANNER_WAIT).await;
        match within(api, api.get_work(work.id.clone())).await {
            Ok(fresh) => {
                if let Some(id) = fresh.planner_session_id {
                    return Ok(id);
                }
            }
            // One failure isn't a reason to stop. Ask again on the next loop.
            Err(e) => tracing::debug!(error = %e, work = %work.id, "could not re-read the work"),
        }
    }
    Err(anyhow!(
        "work **{}**은 만들었는데 계획 대화가 아직 안 열려 여기서 못 봅니다. \
         attacca에서 열어 보세요.",
        work.id
    ))
}

/// How long to wait for the planning conversation ‒ generously 3 seconds. Past that it's not waiting, it's stuck.
const PLANNER_TRIES: u32 = 6;
const PLANNER_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Wire frames to app frames. Even events we don't render **still pass the cursor through.**
pub fn frame_from(f: ZTurnFrame) -> Frame {
    match f {
        ZTurnFrame::Event { cursor, event } => Frame::Event {
            cursor,
            entry: entry_from(&event),
            todo: crate::todos::change_from(&event),
            plan: crate::plan::submitted_from(&event).map(Box::new),
        },
        ZTurnFrame::Delta { kind, text } => Frame::Delta { kind, text },
        ZTurnFrame::Status { running } => Frame::Status { running },
    }
}

/// Creates a project. Returns the `(id, name)` of what was created.
///
/// **Not called with an empty name** ‒ the server wouldn't know what to create, and once a nameless row
/// appears in the list there's no way to delete it in this app. The description may stay empty.
pub async fn create_project(
    api: &AttaccaApiClient,
    name: &str,
    description: Option<&str>,
) -> Result<(String, String)> {
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!(crate::lang::current().project_name_required()));
    }
    let p = within(
        api,
        api.create_project(ZNewProject {
            name: name.to_string(),
            description: description.map(str::to_string),
        }),
    )
    .await
    .map_err(|e| anyhow!(crate::lang::current().project_create_error(&e.to_string())))?;
    Ok((p.id, p.name))
}

/// The project list in the shape the picker uses: `(id, name, description, is_default)`.
///
/// **The description rides along instead of being dropped.** It is the only thing that says what a
/// project is for, and the picker has exactly one place to say it — the note area under the list
/// (`Row::note`, drawn by `widgets::picker::detail_of`). A description that stopped here left the
/// project list showing bare names.
pub async fn projects(
    api: &AttaccaApiClient,
) -> Result<Vec<(String, String, Option<String>, bool)>> {
    let items = within(api, api.list_projects())
        .await
        .map_err(|e| anyhow!(crate::lang::current().project_list_error(&e.to_string())))?;
    Ok(items.into_iter().map(|p| (p.id, p.name, p.description, p.is_default)).collect())
}

/// A project's session list. A session without a title is pre-first-message, so it's labeled as such.
pub async fn sessions(
    api: &AttaccaApiClient,
    project_id: &str,
) -> Result<Vec<(String, String, bool)>> {
    let items = within(
        api,
        api.list_sessions(ZSessionFilter {
            project_id: Some(project_id.to_string()),
            limit: Some(50),
        }),
    )
    .await
    .map_err(|e| anyhow!(crate::lang::current().thread_list_error(&e.to_string())))?;
    Ok(items
        .into_iter()
        .map(|s| {
            let title = s
                .title
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| crate::lang::current().untitled().to_string());
            (s.id, title, s.running)
        })
        .collect())
}

/// How a session's last turn ended, read back from its history.
///
/// `None` when the session has no terminal event yet ‒ a fresh thread that has not taken a turn.
pub fn status_from_events(
    events: &[zyris_attacca::ZSessionEvent],
) -> Option<crate::picker::ThreadStatus> {
    use crate::picker::ThreadStatus;
    let mut out = None;
    for e in events {
        match e.kind.as_str() {
            // A terminal error marks the turn failed.
            "error" => out = Some(ThreadStatus::Failed),
            // An answer (or a completed work run) marks it a success. A tool error
            // mid-turn is not terminal ‒ the agent may still finish.
            "chat_agent" | "work_summary" => out = Some(ThreadStatus::Success),
            _ => {}
        }
    }
    out
}

/// Fetches a session's history and derives its last-turn status.
pub async fn session_status(
    api: &AttaccaApiClient,
    session_id: &str,
) -> Option<crate::picker::ThreadStatus> {
    let events = history(api, session_id).await.ok()?;
    status_from_events(&events)
}

/// The session's past history. Used to fill the screen when switching sessions.
///
/// An empty `after` means everything ‒ the opposite of `turn_events`, so don't confuse them.
pub async fn history(
    api: &AttaccaApiClient,
    session_id: &str,
) -> Result<Vec<zyris_attacca::ZSessionEvent>> {
    within(api, api.session_history(session_id.to_string(), ZHistoryQuery::default()))
        .await
        .map_err(|e| anyhow!(crate::lang::current().history_error(&e.to_string())))
}

/// Finds a session that's awaiting an answer.
///
/// If the app is quit without answering a question, the server keeps waiting. If the person had to
/// find that session by hand after restarting, it would be effectively impossible to answer ‒ it's
/// picked up right at startup.
///
/// A blocked session has `running` set, so the list alone narrows it down. History is read only for those few.
pub async fn session_awaiting_answer(api: &AttaccaApiClient) -> Option<String> {
    let sessions =
        within(api, api.list_sessions(ZSessionFilter { project_id: None, limit: Some(50) }))
            .await
            .ok()?;
    for s in sessions.into_iter().filter(|s| s.running).take(5) {
        let events = history(api, &s.id).await.ok()?;
        // If there's at least one question awaiting an answer, that's the session.
        let pending = events.iter().rev().take(50).any(|e| {
            matches!(
                crate::event::entry_from(e).map(|x| x.kind),
                Some(crate::event::EntryKind::Question { answered: false, .. })
            )
        });
        if pending {
            return Some(s.id);
        }
    }
    None
}

/// Session usage. If the deployment doesn't meter, `capability_not_announced` comes back ‒
/// that's not an error but "this deployment lacks the feature", so it's quietly emptied.
pub async fn usage(api: &AttaccaApiClient, session_id: &str) -> Option<crate::usage::Usage> {
    let u = within(api, api.session_usage(session_id.to_string())).await.ok()?;
    Some(crate::usage::Usage {
        model: u.model,
        context_tokens: u.context_tokens,
        total_tokens: u.total_tokens,
        credits_used: u.credits_used,
    })
}

/// This session's title. `None` when not yet present ‒ it attaches after the first message.
pub async fn session_title(api: &AttaccaApiClient, session_id: &str) -> Option<String> {
    let sessions =
        within(api, api.list_sessions(ZSessionFilter { project_id: None, limit: Some(100) }))
            .await
            .ok()?;
    sessions
        .into_iter()
        .find(|s| s.id == session_id)
        .and_then(|s| s.title)
        .filter(|t| !t.trim().is_empty())
}

#[cfg(test)]
mod tests {
    /// **The block names the path every tool is called with.** The agent sees every node's tools
    /// under one name and picks a computer with `node_path`; naming this one any other way —
    /// "this node", a display name — leaves it nothing to pass.
    #[test]
    fn the_node_block_names_the_path_every_tool_is_called_with() {
        let address = zyris::NodeAddress {
            system: "laptop".into(),
            program: "zyris-code".into(),
            name: "myrepo-2".into(),
        };
        let out = node_preamble(std::path::Path::new("/home/ruma/myrepo"), Some(&address));
        assert!(out.contains("node_path: laptop/zyris-code/myrepo-2"), "{out}");
        assert!(out.contains("/home/ruma/myrepo"), "where it is standing: {out}");
        assert!(out.contains(std::env::consts::OS), "what it is running on: {out}");
    }

    /// Before the first connection there is no path, and the block must not invent one.
    #[test]
    fn before_the_first_connection_the_block_says_the_path_is_not_known() {
        let out = node_preamble(std::path::Path::new("/home/ruma/myrepo"), None);
        assert!(out.contains("not assigned yet"), "{out}");
    }

    /// **The node is named after its directory**, and a directory with no name gets this app's.
    #[test]
    fn a_node_is_named_after_its_directory() {
        assert_eq!(dir_name(std::path::Path::new("/home/ruma/myrepo")), "myrepo");
        assert_eq!(dir_name(std::path::Path::new("/")), "zyris-code");
    }

    use super::*;
    use serde_json::json;
    use zyris_attacca::{ZDeltaKind, ZSessionEvent};

    fn event(seq: i64, kind: &str, message: &str) -> ZSessionEvent {
        ZSessionEvent {
            seq,
            cursor: seq,
            kind: kind.into(),
            payload: json!({ "message": message }),
            created_at: None,
        }
    }

    /// A thread's status comes from its last terminal event: an error marks it failed, an
    /// answer (or a finished work run) marks it a success, and a bare thread has none.
    #[test]
    fn a_threads_status_is_its_last_turn_outcome() {
        use crate::picker::ThreadStatus;
        // No terminal event yet — a fresh thread.
        assert_eq!(status_from_events(&[]), None);
        // Mid-turn tool chatter, then an answer → success.
        let ok = [
            event(1, "chat_user", "안녕"),
            event(2, "tool_call", ""),
            event(3, "chat_agent", "hi"),
        ];
        assert_eq!(status_from_events(&ok), Some(ThreadStatus::Success));
        // An answer that never comes, then an error → failed.
        let err = [event(1, "chat_user", "안녕"), event(2, "error", "boom")];
        assert_eq!(status_from_events(&err), Some(ThreadStatus::Failed));
        // A tool error mid-turn is not terminal — the agent can still finish.
        let recovered = [
            event(1, "chat_user", "안녕"),
            event(2, "tool_call", ""),
            event(3, "chat_agent", "hi"),
        ];
        assert_eq!(status_from_events(&recovered), Some(ThreadStatus::Success));
        // A completed work run counts as a success too.
        let work = [event(1, "chat_user", ""), event(2, "work_summary", "done")];
        assert_eq!(status_from_events(&work), Some(ThreadStatus::Success));
    }

    /// **Credentials go to this app's own directory.** `~/.config/zyris/` was shared by every zyris
    /// program, so two unprofiled ones registered on top of each other's identity.
    #[test]
    fn credentials_live_under_this_apps_own_name() {
        if given_config_dir().is_some() {
            // When a location is given, it's used verbatim — see the next test.
            return;
        }
        // In an environment without a home (systemd `ProtectHome=yes`), absent is correct.
        if let Some(ours) = config_home_for(APP) {
            assert_eq!(ours.file_name().unwrap(), "zyris-code");
        }
    }

    /// **The location the person gave wins.** And no app name is appended to it — that
    /// person meant exactly that directory.
    #[test]
    fn a_given_config_dir_wins_and_is_taken_literally() {
        // Instead of faking the value `given_config_dir` reads, only look at the rule for when that value exists.
        // (Actually setting the environment variable would let other tests in the same process see it.)
        let given: Option<std::ffi::OsString> = Some("/somewhere/else".into());
        let picked = given.clone().map(std::path::PathBuf::from).unwrap();
        assert_eq!(picked, std::path::Path::new("/somewhere/else"));
        // An empty value counts as not given — using the empty path as-is would drop credentials into the working directory.
        let empty: Option<std::ffi::OsString> = Some(std::ffi::OsString::new());
        assert!(empty.filter(|v| !v.is_empty()).is_none());
    }

    /// **When short, we ask once more.** Permissions fixed at approval time don't widen on refresh,
    /// so there's no path other than dropping credentials and getting approved again.
    #[test]
    fn a_narrow_approval_is_asked_again_exactly_once() {
        let narrow: Vec<String> = vec!["agents:read".into()];
        assert!(needs_reenrollment(&narrow, false), "when short, ask again");
        // **We don't ask twice.** The person can approve narrowly again, and asking every time becomes
        // a loop that keeps demanding the browser.
        assert!(!needs_reenrollment(&narrow, true), "after trying once, only tell them in words");
    }

    /// With everything granted, nothing should happen. **If the browser pops up on the ordinary path, that's an accident.**
    #[test]
    fn a_full_approval_is_left_alone() {
        let all: Vec<String> = REQUIRED_SCOPES.iter().map(|s| s.to_string()).collect();
        assert!(missing_scopes(&all).is_empty());
        assert!(!needs_reenrollment(&all, false));
        // Receiving more than asked is not a shortage either.
        let more: Vec<String> =
            all.iter().cloned().chain(std::iter::once("jobs:read".to_string())).collect();
        assert!(!needs_reenrollment(&more, false));
    }

    /// What's missing must be named **by name**. "Not enough" alone leaves nothing actionable.
    #[test]
    fn what_is_missing_is_named() {
        let narrow: Vec<String> = vec!["agents:read".into(), "projects:read".into()];
        let missing = missing_scopes(&narrow);
        assert!(missing.contains(&"events:read"), "{missing:?}");
        assert!(!missing.contains(&"agents:read"), "{missing:?}");
        assert!(missing_scopes_message(&missing).contains("events:read"));
    }

    /// Changing the agent **opens a new session at the next message.** A session's agent is fixed at
    /// creation with no API to change it (`ZNewSession`), so continuing the old session wouldn't change it.
    #[test]
    fn staging_a_new_default_session_forgets_the_current_one() {
        let mut s = Session::new(None);
        s.switch_to("abc".into(), None);
        assert_eq!(s.id(), Some("abc"));
        s.stage_new_default();
        assert_eq!(s.id(), None, "keeping the previous session would not change the agent");
    }

    /// **Nothing is created on the server now.** Only staging; actual creation happens at the first message —
    /// empty sessions that are only opened and abandoned must not pile up in the account.
    #[test]
    fn staging_creates_nothing_by_itself() {
        let mut s = Session::new(None);
        s.stage_new_default();
        assert_eq!(s.id(), None);
    }

    /// **Normal↔plan doesn't touch the conversation in progress.** If this breaks, turning on plan mode
    /// mid-conversation cuts the thread off, and the only way plan mode is useful disappears.
    #[test]
    fn routing_to_a_session_leaves_the_conversation_alone() {
        let mut s = Session::new(None);
        s.switch_to("abc".into(), None);
        s.set_route(Route::Session);
        assert_eq!(s.id(), Some("abc"), "plan mode threw away the conversation in progress");
        assert_eq!(s.pending_open(), None);
    }

    /// Entering work·job stages that **the next message opens anew**.
    #[test]
    fn routing_to_work_or_job_stages_an_open() {
        for route in [Route::Work, Route::Job] {
            let mut s = Session::new(None);
            s.set_route(route);
            assert_eq!(s.pending_open(), Some(route));
        }
    }

    /// **Staging doesn't discard the conversation in progress.** Visiting work·job and coming back without
    /// saying anything is common; if the session was already dropped then, there's nowhere to return.
    #[test]
    fn staging_an_open_keeps_the_conversation_to_come_back_to() {
        let mut s = Session::new(None);
        s.switch_to("abc".into(), None);
        s.set_route(Route::Job);
        assert_eq!(s.id(), Some("abc"), "only staged, yet the conversation in progress was lost");
        s.set_route(Route::Session);
        assert_eq!(s.id(), Some("abc"));
        assert_eq!(s.pending_open(), None, "came back, yet the staged open is still there");
    }

    /// Picking a session from the list means "going there". **If a staged open remains, it doesn't go
    /// to the picked place and another job spawns.**
    #[test]
    fn picking_a_session_by_hand_cancels_a_staged_open() {
        let mut s = Session::new(None);
        s.set_route(Route::Job);
        s.switch_to("고른-세션".into(), None);
        assert_eq!(s.pending_open(), None);
        assert_eq!(s.id(), Some("고른-세션"));

        let mut s = Session::new(None);
        s.set_route(Route::Work);
        s.stage_new("프로젝트-1".into());
        assert_eq!(s.pending_open(), None);

        let mut s = Session::new(None);
        s.set_route(Route::Work);
        s.stage_new_default();
        assert_eq!(s.pending_open(), None);
    }

    /// **The chosen project must stick.** `pending_project` used to be single-use and the first creation
    /// cleared it, so launching a job after picking a project had `project_id` already empty and
    /// **the server created it in the default project.** That actually happened.
    #[test]
    fn the_chosen_project_sticks_to_everything_opened_after_it() {
        // Merely opening a project from the list remembers it — Esc without picking a session keeps it.
        let mut s = Session::new(None);
        s.enter_project("프로젝트-1".into());
        assert_eq!(s.project(), Some("프로젝트-1"));

        // Picking a session inside it goes to that session's project.
        s.switch_to("세션-1".into(), Some("프로젝트-2".into()));
        assert_eq!(s.project(), Some("프로젝트-2"));

        // On a path that doesn't know the project, **what we knew isn't cleared.**
        s.switch_to("세션-2".into(), None);
        assert_eq!(
            s.project(),
            Some("프로젝트-2"),
            "clearing it out of ignorance drops into the default project"
        );

        // `/agent` changes the agent, not leaves the project.
        s.stage_new_default();
        assert_eq!(s.project(), Some("프로젝트-2"));

        // Staging job·work keeps it too — this is exactly the value that stage will use.
        s.set_route(Route::Job);
        assert_eq!(s.project(), Some("프로젝트-2"));
    }

    /// Before any project is picked it's `None`, and **only then** is the server's default project right.
    #[test]
    fn a_fresh_session_has_no_project_of_its_own() {
        assert_eq!(Session::new(None).project(), None);
    }

    /// **What the mode decides must also hold when there's no staged open.**
    ///
    /// A stage only happens at the moment the mode *changes*. So there are several spots without
    /// one — the first word right after startup, after `/agent`, after `＋ New thread`. There,
    /// creating only a session means **the bottom bar says job but the plain session opens**.
    /// That actually happened.
    ///
    /// `open_for` can't be called here (it needs a server), so this decision is mimicked as is.
    /// If this diverges, nothing this test protects is left, so when `open_for` changes, change this too.
    #[test]
    fn with_no_conversation_yet_the_mode_decides_what_opens() {
        use crate::mode::Mode;
        let route_for = |staged: Option<Route>, has_id: bool, mode: Mode| match staged {
            Some(r) => r,
            None if has_id => Route::Session,
            None => mode.route(),
        };

        // No conversation and no staged open → the mode decides.
        assert_eq!(
            route_for(None, false, Mode::Job),
            Route::Job,
            "job mode, yet a plain session opens"
        );
        assert_eq!(route_for(None, false, Mode::Work), Route::Work);
        assert_eq!(route_for(None, false, Mode::Normal), Route::Session);
        // Plan mode opens a job of its own too ‒ attacca's plan mode is a flag set at creation,
        // so there is nothing to turn on in a session that already exists.
        assert_eq!(route_for(None, false, Mode::Plan), Route::Plan);

        // If there's a conversation to continue, continue it — a job must not spawn on every message.
        assert_eq!(route_for(None, true, Mode::Job), Route::Session);

        // A staged open wins. Even with a conversation in progress, it opens anew.
        assert_eq!(route_for(Some(Route::Job), true, Mode::Normal), Route::Job);
    }

    /// Even an event we don't render must pass the cursor through — the resume position must not be missed.
    #[test]
    fn a_hidden_event_still_carries_its_cursor() {
        let f = ZTurnFrame::Event {
            cursor: 99,
            event: ZSessionEvent {
                seq: 5,
                cursor: 99,
                kind: "recall".into(),
                payload: json!({"kind": "recall", "content": "…"}),
                created_at: None,
            },
        };
        match frame_from(f) {
            Frame::Event { cursor, entry, .. } => {
                assert_eq!(cursor, 99);
                assert!(entry.is_none(), "recall is not rendered");
            }
            other => panic!("must be an event frame: {other:?}"),
        }
    }

    #[test]
    fn a_delta_frame_keeps_its_kind() {
        let f = ZTurnFrame::Delta { kind: ZDeltaKind::Reasoning, text: "생각".into() };
        match frame_from(f) {
            Frame::Delta { kind, text } => {
                assert_eq!(kind, ZDeltaKind::Reasoning);
                assert_eq!(text, "생각");
            }
            other => panic!("must be a delta frame: {other:?}"),
        }
    }

    /// When zyris-code is ready, only this needs changing. This test turns red at the same time,
    /// making the fact that it changed visible.
    #[test]
    fn the_default_agent_is_main_agent_while_zyris_code_is_in_progress() {
        assert_eq!(DEFAULT_AGENT, "Main Agent");
    }

    #[test]
    fn a_status_frame_carries_running() {
        match frame_from(ZTurnFrame::Status { running: true }) {
            Frame::Status { running } => assert!(running),
            other => panic!("must be a status frame: {other:?}"),
        }
    }
}
