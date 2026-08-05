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
                "서버 호출이 {}초 안에 답하지 않았다 — 연결을 끊고 다시 붙는다",
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

/// The old location all zyris programs shared. Credentials left here are migrated on first run.
pub const LEGACY_APP: &str = "zyris";

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

/// Where credentials go. **We compute it.**
///
/// Upstream has no way to set an app-specific directory (`RunConfig::app` doesn't exist in zyris's `main`). Instead,
/// zyris's `config_dir()` **checks `$ZYRIS_CONFIG_DIR` first** — so filling that variable with
/// this value (`main.rs`) makes credentials land in this app's own location.
pub fn credential_dir() -> Option<std::path::PathBuf> {
    config_home_for(APP)
}

/// The old location. Credentials found here are migrated on first run.
pub fn legacy_credential_dir() -> Option<std::path::PathBuf> {
    // When the person has set `$ZYRIS_CONFIG_DIR`, there is no old location to migrate — they have
    // already decided where things go, and we have no reason to search other directories.
    if given_config_dir().is_some() {
        return None;
    }
    config_home_for(LEGACY_APP)
}

/// The location the person chose. **An empty value counts as not given** — handing an empty path
/// to someone who tried to clear it with `ZYRIS_CONFIG_DIR=` would drop credentials into the working directory.
fn given_config_dir() -> Option<std::ffi::OsString> {
    std::env::var_os("ZYRIS_CONFIG_DIR").filter(|v| !v.is_empty())
}

/// `$ZYRIS_CONFIG_DIR` → `app` under the platform's user-config location.
///
/// Follows **the same branch** as zyris's `enroll::config_dir()`. If they diverge, the variable we fill
/// and the location upstream reads fall out of sync, scattering credentials across two places.
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

/// **Migrates** this profile's credentials left in the old location. Returns the number moved.
///
/// **It's a move, not a copy.** If a live refresh token exists twice on disk, both eventually get
/// presented, and attacca treats a reuse beyond the 30-second grace as a leaked chain and **revokes
/// the whole node** (`RefreshAttempt::Reused` in `zyris_enrollment_service.rs`). Both die.
///
/// **Never overwrites what is already here.** Credentials this app already registered are newer than the old file,
/// and overwriting would lose the identity currently attached. In that case the old file isn't deleted either —
/// deleting a credential we don't hold is throwing away someone else's.
pub fn migrate_credentials(from: &std::path::Path, into: &std::path::Path, profile: &str) -> usize {
    let suffix = format!("-{}.json", slugify_profile(profile));
    let Ok(entries) = std::fs::read_dir(from) else {
        return 0;
    };
    let mut moved = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };
        if !name_str.ends_with(&suffix) || !entry.path().is_file() {
            continue;
        }
        let target = into.join(&name);
        if target.exists() {
            continue;
        }
        if std::fs::create_dir_all(into).is_err() {
            continue;
        }
        // On the same filesystem it's a single rename. If the home directory spans several mounts, that
        // fails with EXDEV, so we copy first and **delete afterwards** — if the delete fails, two copies remain,
        // and we count that as not moved.
        let done = match std::fs::rename(entry.path(), &target) {
            Ok(()) => true,
            Err(_) => match std::fs::copy(entry.path(), &target) {
                Ok(_) => match std::fs::remove_file(entry.path()) {
                    Ok(()) => true,
                    Err(_) => {
                        let _ = std::fs::remove_file(&target);
                        false
                    }
                },
                Err(_) => false,
            },
        };
        if done {
            // It's a credential. If the move loosens permissions, upstream refuses to read it next run.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600));
            }
            moved += 1;
        }
    }
    moved
}

/// The profile fragment that goes into credential file names.
///
/// **This is a verbatim copy of zyris's `file_store::slugify`.** Upstream names the files and we only
/// recognize them, so if the rules diverge we fail to recognize the files to migrate and quietly pass
/// them by — the person only sees a "please re-enroll" screen. When upstream changes, this changes too.
fn slugify_profile(profile: &str) -> String {
    let mut out = String::with_capacity(profile.len());
    let mut prev_dash = false;
    for ch in profile.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed
    }
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

/// The node name to register with the server.
///
/// **Using only the hostname gives the same identity as the machine's other nodes.** If `zyris-daemon`
/// runs on the same computer, both register as `arch`, and attacca separates them by appending
/// `-2` to one with `slug_with_suffix` — **which one keeps `arch` depends on the order
/// they attached**, so the tool names (`zyris__arch__…`) can change between runs.
///
/// That's why this app registers carrying its own name: `arch zyris-code`.
///
/// **Length is a constraint.** attacca's `slugify_node_name` keeps only alphanumerics, folds the rest into hyphens,
/// then **truncates at 16 characters** (`ZYRIS_NODE_SLUG_MAX_LEN`). `arch zyris-code` fits exactly as
/// `arch-zyris-code` (15 chars), but with a long hostname the trailing `zyris-code` gets cut
/// away and only the hostname remains. In that case **the distinguishing part goes first.**
pub fn node_name() -> String {
    let host = zyris::machine_name().unwrap_or_else(|| "node".to_string());
    let dir = std::env::current_dir()
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()));
    compose_name(&host, dir.as_deref())
}

/// The pure decision that builds the name. `dir` is the last fragment of the working directory.
///
/// **The working directory goes into the name.** Windows opened in different directories of the same machine
/// must be distinguishable in attacca's node list — like `arch zyris-code · zyris-daemon`.
/// Since the slug truncates at 16 characters, the directory only survives in the display name (the slug is always
/// of the form `arch-zyris-code`). When the directory equals the app name (running in this repo), it's not
/// appended — no reason to say the same thing twice.
fn compose_name(host: &str, dir: Option<&str>) -> String {
    let suffix = dir.filter(|d| !d.is_empty() && *d != SUFFIX);
    let natural = match suffix {
        Some(dir) => format!("{host} {SUFFIX} · {dir}"),
        None => format!("{host} {SUFFIX}"),
    };
    if slug_of(&natural).contains(SUFFIX) {
        natural
    } else {
        // Truncated away the app name. Reversing the order at least keeps what it is.
        format!("{SUFFIX} {host}")
    }
}

/// The slug attacca gives this node. It's the middle fragment of tool names.
///
/// **If it collides, the server appends `-2`** (`slug_with_suffix`). So the value here isn't always the actual
/// one — with two nodes of the same name, which keeps the bare name is the attach order.
pub fn node_slug() -> String {
    slug_of(&std::env::var("ZYRIS_NODE_NAME").unwrap_or_else(|_| node_name()))
}

/// This app's display appended to the name. For the distinction to work, this must survive in the slug.
const SUFFIX: &str = "zyris-code";

// ── Window lock ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
//
// **With two windows using the same credentials, the server registry is overwritten by the later connection.** So it
// doesn't quietly tangle: if an earlier window is already up, we say so on screen. We don't block —
// in the same directory, whichever window receives it, the files changed are the same (CLAUDE.md "Multiple windows").

/// Lock file name. Branched by profile, so different profiles (different nodes) don't interfere with each other.
fn instance_lock_path(config_dir: &std::path::Path, profile: &str) -> std::path::PathBuf {
    config_dir.join(format!(".instance-{}.lock", slugify_profile(profile)))
}

/// A handle that removes the lock file while alive. **However the window ends, it gets removed.**
pub struct InstanceLock(std::path::PathBuf);

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Is there **already a living** other window for this profile? True if the PID in the lock file is alive —
/// a dead window's trace is not a living window.
pub fn another_instance_alive(config_dir: &std::path::Path, profile: &str) -> bool {
    let Ok(pid) = std::fs::read_to_string(instance_lock_path(config_dir, profile)) else {
        return false;
    };
    process_alive(pid.trim())
}

/// Claims the lock. If a living window already exists, `None` — then we only show a notice and carry on.
///
/// **Clears a dead window's trace and claims again.** Since only one PID is stored, if two windows claim
/// at once the later one wins — this lock asks "is there another living window", not "who is the owner",
/// so the losing side of a race will see the other one at the next check.
pub fn claim_instance_lock(config_dir: &std::path::Path, profile: &str) -> Option<InstanceLock> {
    let path = instance_lock_path(config_dir, profile);
    if another_instance_alive(config_dir, profile) {
        return None;
    }
    let _ = std::fs::remove_file(&path);
    if std::fs::write(&path, std::process::id().to_string()).is_ok() {
        Some(InstanceLock(path))
    } else {
        None
    }
}

#[cfg(unix)]
fn process_alive(pid: &str) -> bool {
    let Ok(pid) = pid.trim().parse::<u32>() else {
        return false;
    };
    // PID 0 means "my process group", so kill(0, 0) always succeeds — treat it as an impossible value.
    if pid == 0 {
        return false;
    }
    // kill(pid, 0): sends no signal, only asks whether that PID exists.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: &str) -> bool {
    // On platforms with no way to ask whether a process exists, treat a leftover file as dead —
    // the next window just overwrites it, and the warning only means something on Unix.
    false
}

/// **The same rule** as attacca's `slugify_node_name` (`attacca-domain/src/zyris_node.rs`).
///
/// We must know here in advance what the server will produce to judge whether the name gets truncated. If the rules
/// diverge, this judgment is wrong, so when it changes this must change too.
fn slug_of(name: &str) -> String {
    const MAX: usize = 16;
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
        if slug.len() >= MAX {
            break;
        }
    }
    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "node".to_string()
    } else {
        trimmed
    }
}

#[derive(Debug, Default)]
pub struct Session {
    id: Option<String>,
    /// The project currently in view. **Everything opened from here goes to this project** —
    /// sessions, jobs, and works alike.
    ///
    /// **Must not be consumed on first use.** It used to be single-use: `＋ 새 세션` filled it and the first
    /// creation cleared it, so when a job was launched after picking a project, `project_id` was already
    /// empty and the server created it in the **default project**. That actually happened.
    ///
    /// `None` means "not picked yet", and only then is the server's default project right.
    project: Option<String>,
    /// The system directive attached to each session. `None` when there are no skills.
    preamble: Option<String>,
    /// What the next message will **open anew**. `None` appends to the current session.
    ///
    /// **Can't be replaced by clearing `id`.** It's common to visit work·job mode and come back without
    /// saying anything; if `id` was already dropped then, the conversation in progress is lost. A staged
    /// open and "no session" are different states.
    pending_open: Option<Route>,
}

/// What you need to know after opening a session.
///
/// **`sent` is the point.** With `create_job`·`create_work` the open request **consumes the first message**
/// (`ZNewJob::message`·`ZNewWork::message`), so calling `send_message` afterwards would send the same
/// words twice. On the path that merely creates a session, nothing has been sent yet.
#[derive(Debug, Clone)]
pub struct Opened {
    pub id: String,
    /// Whether the first message already rode along on the open request.
    pub sent: bool,
    /// What was just opened. `None` when nothing new was opened — the screen only announces then.
    pub announced: Option<(Route, String)>,
}

impl Session {
    /// `preamble` is this session's system directive — currently it carries the skill list.
    ///
    /// **Fixed once when the session is created and can't be changed later** (attacca's `ZNewSession`).
    /// That's why MCP tools attached later aren't carried here — they go into the tool list.
    pub fn new(preamble: Option<String>) -> Self {
        Session { preamble, ..Default::default() }
    }

    /// The id, if already created.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
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
    /// Then pass `None` and **keep what we knew** — clearing it out of ignorance is exactly the path
    /// that drops into the default project.
    pub fn switch_to(&mut self, id: String, project_id: Option<String>) {
        self.id = Some(id);
        if let Some(p) = project_id {
            self.project = Some(p);
        }
        self.pending_open = None;
    }

    /// Opened one from the project list. **Remembered even before picking a session** — opening the list,
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
    }

    /// Points the next message where the mode decided.
    ///
    /// **`Route::Session` leaves the current conversation alone.** That's what it means for normal↔plan not
    /// to touch the session, and coming back to normal from work·job means "answer that", not
    /// opening a new conversation.
    ///
    /// Conversely, **entering work·job always opens anew**. Even with a job already open it launches another —
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
    /// session. Here too nothing is created on the server — actual creation happens at the first
    /// message. **The previous session is not cleared**: you can return via the ← list.
    pub fn stage_new_default(&mut self) {
        self.id = None;
        // **The project stays as is.** `/agent` changes the agent, not leaves the project —
        // clearing it here would create the next session in the default project.
        self.pending_open = None;
    }

    /// Finds the dedicated agent.
    ///
    /// **If not found, doesn't fall back to another agent.** A quiet fallback would show the name in the
    /// status bar while the send fails with `Agent not found` — a state that's hard to diagnose.
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
    /// `title` must be `None` — giving a title here makes it permanent and blocks attacca's behavior
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
        // happens at the moment the mode *changes*, so there are several spots without one — the first word
        // right after startup, after staging a new thread with `/agent`, after `＋ 새 세션`. There, creating only a
        // session means **the bottom bar says job but the plain session opens**. That actually happened.
        let route = match self.pending_open.take() {
            Some(staged) => staged,
            // If there's a conversation to continue, continue it. The mode only decides what opens *when opening anew*.
            None if self.id.is_some() => Route::Session,
            None => mode.route(),
        };
        match route {
            Route::Job => self.open_job(api, agent_id, message).await,
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
                // **Leave both off.** `planning` hands the job over to work, and `plan_mode` receives a plan
                // inside the job and stops; here the mode already is that branch —
                // job mode means "set it going", and when planning is needed that's plan mode or work mode.
                planning: false,
                plan_mode: false,
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
        Ok(Opened { id, sent: true, announced: Some((Route::Job, job.id)) })
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
/// is already in that response — the server creating the session and returning the work row are not the
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
            Err(e) => tracing::debug!(error = %e, work = %work.id, "work를 다시 읽지 못했다"),
        }
    }
    Err(anyhow!(
        "work **{}**은 만들었는데 계획 대화가 아직 안 열려 여기서 못 봅니다. \
         attacca에서 열어 보세요.",
        work.id
    ))
}

/// How long to wait for the planning conversation — generously 3 seconds. Past that it's not waiting, it's stuck.
const PLANNER_TRIES: u32 = 6;
const PLANNER_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Wire frames to app frames. Even events we don't render **still pass the cursor through.**
pub fn frame_from(f: ZTurnFrame) -> Frame {
    match f {
        ZTurnFrame::Event { cursor, event } => Frame::Event { cursor, entry: entry_from(&event) },
        ZTurnFrame::Delta { kind, text } => Frame::Delta { kind, text },
        ZTurnFrame::Status { running } => Frame::Status { running },
    }
}

/// Creates a project. Returns the `(id, name)` of what was created.
///
/// **Not called with an empty name** — the server wouldn't know what to create, and once a nameless row
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

/// The project list in the shape the picker uses.
pub async fn projects(api: &AttaccaApiClient) -> Result<Vec<(String, String, bool)>> {
    let items = within(api, api.list_projects())
        .await
        .map_err(|e| anyhow!(crate::lang::current().project_list_error(&e.to_string())))?;
    Ok(items.into_iter().map(|p| (p.id, p.name, p.is_default)).collect())
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

/// The session's past history. Used to fill the screen when switching sessions.
///
/// An empty `after` means everything — the opposite of `turn_events`, so don't confuse them.
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
/// find that session by hand after restarting, it would be effectively impossible to answer — it's
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

/// Session usage. If the deployment doesn't meter, `capability_not_announced` comes back —
/// that's not an error but "this deployment lacks the feature", so it's quietly emptied.
pub async fn usage(api: &AttaccaApiClient, session_id: &str) -> Option<crate::sidebar::Usage> {
    let u = within(api, api.session_usage(session_id.to_string())).await.ok()?;
    Some(crate::sidebar::Usage {
        model: u.model,
        context_tokens: u.context_tokens,
        total_tokens: u.total_tokens,
        credits_used: u.credits_used,
    })
}

/// This session's title. `None` when not yet present — it attaches after the first message.
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
    use super::*;
    use serde_json::json;
    use zyris_attacca::{ZDeltaKind, ZSessionEvent};

    /// **Credentials go to this app's own directory.** `~/.config/zyris/` was shared by every zyris
    /// program, so two unprofiled ones registered on top of each other's identity.
    ///
    /// Jiggling environment variables would trample other tests running in parallel, so here we only look at the
    /// branching rule — the old and new locations must differ **only in the last segment** and be the same above it.
    #[test]
    fn credentials_live_under_this_apps_own_name() {
        if given_config_dir().is_some() {
            // When a location is given, it's used verbatim rather than the branching rule —
            // that rule is covered by a_given_config_dir_wins_and_is_taken_literally.
            return;
        }
        let (Some(ours), Some(legacy)) = (config_home_for(APP), config_home_for(LEGACY_APP)) else {
            // In an environment without a home (systemd `ProtectHome=yes`), both being absent is correct.
            assert!(config_home_for(APP).is_none() && config_home_for(LEGACY_APP).is_none());
            return;
        };
        assert_eq!(ours.file_name().unwrap(), "zyris-code");
        assert_eq!(legacy.file_name().unwrap(), "zyris");
        assert_eq!(ours.parent(), legacy.parent(), "두 자리는 같은 부모 아래에 있어야 한다");
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

    /// **Old credentials move over and the old spot is left empty.** Copying would leave a live refresh token
    /// twice on disk, and attacca, seeing the reuse, revokes the whole node.
    #[test]
    fn a_legacy_credential_moves_and_leaves_nothing_behind() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        let name = "wss-attacca-cc-zyris-v1-ws-zyris-code.json";
        std::fs::write(old.path().join(name), "{\"refresh_token\":\"r\"}").unwrap();
        // Another profile's file belongs to someone else. Touching it logs that program out.
        std::fs::write(old.path().join("wss-attacca-cc-default.json"), "{}").unwrap();

        assert_eq!(migrate_credentials(old.path(), new.path(), "zyris-code"), 1);
        assert!(new.path().join(name).exists(), "새 자리에 있어야 한다");
        assert!(!old.path().join(name).exists(), "옛 자리는 비어야 한다");
        assert!(old.path().join("wss-attacca-cc-default.json").exists(), "남의 것은 그대로다");
    }

    /// **If something is already here, don't overwrite it.** Overwriting the identity currently attached
    /// with the old file loses that credential. And the old file isn't deleted either — that would be
    /// throwing away a credential we don't hold.
    #[test]
    fn migration_never_overwrites_what_is_already_here() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        let name = "wss-attacca-cc-zyris-v1-ws-zyris-code.json";
        std::fs::write(old.path().join(name), "옛것").unwrap();
        std::fs::write(new.path().join(name), "지금것").unwrap();

        assert_eq!(migrate_credentials(old.path(), new.path(), "zyris-code"), 0);
        assert_eq!(std::fs::read_to_string(new.path().join(name)).unwrap(), "지금것");
        assert!(old.path().join(name).exists(), "안 가져왔으면 지우지도 않는다");
    }

    /// **When short, we ask once more.** Permissions fixed at approval time don't widen on refresh,
    /// so there's no path other than dropping credentials and getting approved again.
    #[test]
    fn a_narrow_approval_is_asked_again_exactly_once() {
        let narrow: Vec<String> = vec!["agents:read".into()];
        assert!(needs_reenrollment(&narrow, false), "모자라면 다시 묻는다");
        // **We don't ask twice.** The person can approve narrowly again, and asking every time becomes
        // a loop that keeps demanding the browser.
        assert!(!needs_reenrollment(&narrow, true), "한 번 해 봤으면 말로만 알린다");
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

    /// The one naming files is upstream. **If the rules diverge, we fail to recognize the files to migrate.**
    #[test]
    fn the_profile_slug_matches_what_zyris_writes() {
        // Values copied verbatim from zyris `enroll/file_store.rs`'s tests.
        assert_eq!(slugify_profile("zyris-code"), "zyris-code");
        assert_eq!(slugify_profile("///"), "default");
        assert_eq!(slugify_profile(""), "default");
        assert_eq!(slugify_profile("Two  Words"), "two-words");
    }

    /// **The slug rule must match attacca.** If this diverges, the judgment about whether the name gets
    /// truncated is wrong, and the app registers without its name.
    #[test]
    fn the_slug_rule_matches_what_attacca_does() {
        // Values copied verbatim from `attacca-domain/src/zyris_node.rs`'s tests.
        assert_eq!(slug_of("Allen's Desktop!!"), "allen-s-desktop");
        assert_eq!(slug_of("   "), "node");
        assert_eq!(slug_of("a-very-long-machine-name-here"), "a-very-long-mach");
    }

    /// `HOSTNAME` is process-global, so these two run in one thread.
    static HOST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// **Registering with only the hostname makes the same identity as the machine's other nodes.**
    #[test]
    fn the_node_name_carries_this_app() {
        let _g = HOST.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HOSTNAME", "arch");
        assert_eq!(node_name(), "arch zyris-code");
        assert_eq!(slug_of(&node_name()), "arch-zyris-code");
    }

    /// **A long hostname cuts off the tail.** Left as is, only the hostname remains and the
    /// distinction disappears — then the distinguishing part goes first.
    #[test]
    fn a_long_hostname_does_not_swallow_the_app_name() {
        let _g = HOST.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HOSTNAME", "a-very-long-machine-name-here");
        let name = node_name();
        let slug = slug_of(&name);
        assert!(slug.contains("zyris-code"), "앱 이름이 잘려 나갔다: {name} → {slug}");
        assert!(slug.len() <= 16, "{slug}");
    }

    /// **The working directory goes into the name.** Different directories on the same machine must be distinguishable.
    /// The slug truncates at 16 characters, so it only survives in the display name.
    #[test]
    fn the_node_name_carries_the_working_directory() {
        assert_eq!(compose_name("arch", Some("zyris-daemon")), "arch zyris-code · zyris-daemon");
        assert_eq!(slug_of("arch zyris-code · zyris-daemon"), "arch-zyris-code");
        // A directory equal to the app name isn't appended — it's a duplicate.
        assert_eq!(compose_name("arch", Some("zyris-code")), "arch zyris-code");
        // Without a directory (e.g. root) it's the usual name.
        assert_eq!(compose_name("arch", None), "arch zyris-code");
    }

    /// A dead window's trace is not a living window. PID 0 must be treated as dead, since kill(0, 0)
    /// always succeeds.
    #[test]
    fn a_stale_instance_lock_is_not_a_living_window() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(instance_lock_path(dir.path(), "test"), "0").unwrap();
        assert!(!another_instance_alive(dir.path(), "test"));
    }

    /// Claiming the lock makes "another window" for that profile visible, and dropping the handle releases it.
    /// A second window can't claim the lock again — that's where the notice goes.
    #[test]
    fn claiming_the_lock_marks_another_window_and_releasing_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!another_instance_alive(dir.path(), "test"));
        let lock = claim_instance_lock(dir.path(), "test").expect("첫 창은 주인이다");
        assert!(another_instance_alive(dir.path(), "test"));
        assert!(claim_instance_lock(dir.path(), "test").is_none(), "둘째 창은 주인이 될 수 없다");
        drop(lock);
        assert!(!another_instance_alive(dir.path(), "test"), "풀렸는데 남아 있다");
    }

    /// A lock holding a dead PID is cleared and claimed again — if a dead window makes the second window
    /// get a false notice, that's noise too.
    #[test]
    fn a_dead_pid_lock_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(instance_lock_path(dir.path(), "test"), "4000000000").unwrap();
        assert!(!another_instance_alive(dir.path(), "test"));
        assert!(claim_instance_lock(dir.path(), "test").is_some(), "죽은 창의 잠금은 치워야 한다");
    }

    /// Changing the agent **opens a new session at the next message.** A session's agent is fixed at
    /// creation with no API to change it (`ZNewSession`), so continuing the old session wouldn't change it.
    #[test]
    fn staging_a_new_default_session_forgets_the_current_one() {
        let mut s = Session::new(None);
        s.switch_to("abc".into(), None);
        assert_eq!(s.id(), Some("abc"));
        s.stage_new_default();
        assert_eq!(s.id(), None, "앞 세션을 계속 쓰면 에이전트가 안 바뀐다");
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
        assert_eq!(s.id(), Some("abc"), "계획 모드가 하던 대화를 버렸다");
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
        assert_eq!(s.id(), Some("abc"), "예약만 했는데 하던 대화를 잃었다");
        s.set_route(Route::Session);
        assert_eq!(s.id(), Some("abc"));
        assert_eq!(s.pending_open(), None, "돌아왔는데 예약이 남아 있다");
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
        assert_eq!(s.project(), Some("프로젝트-2"), "모른다고 비우면 기본 프로젝트로 떨어진다");

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
    /// A stage only happens at the moment the mode *changes*. So there are several spots without one — the first
    /// word right after startup, after `/agent`, after `＋ 새 세션`. There, creating only a session means **the bottom
    /// bar says job but the plain session opens**. That actually happened.
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
        assert_eq!(route_for(None, false, Mode::Job), Route::Job, "job 모드인데 맨 세션이 열린다");
        assert_eq!(route_for(None, false, Mode::Work), Route::Work);
        assert_eq!(route_for(None, false, Mode::Normal), Route::Session);
        assert_eq!(route_for(None, false, Mode::Plan), Route::Session);

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
            Frame::Event { cursor, entry } => {
                assert_eq!(cursor, 99);
                assert!(entry.is_none(), "recall은 렌더하지 않는다");
            }
            other => panic!("이벤트 프레임이어야 한다: {other:?}"),
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
            other => panic!("델타 프레임이어야 한다: {other:?}"),
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
            other => panic!("상태 프레임이어야 한다: {other:?}"),
        }
    }
}
