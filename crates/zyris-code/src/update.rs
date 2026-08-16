//! Noticing a new release, and installing it before the screen exists.
//!
//! **This runs on the plain terminal, not inside the app.** It used to hand over from the middle of
//! a session: the screen asked for an update, the process wrote a script out, started it detached,
//! and left — the script waited for the exit, installed, and started the new binary. On Windows in
//! `cmd` that last step is `Start-Process`, which opens a **new console window** for a console
//! program, so from the window somebody was looking at the app simply vanished (reported
//! 2026-08-17). There is no way to hand a console back to a process that is about to end, and every
//! fix for that is a fix for a shape that should not exist.
//!
//! So the shape is different now. The check and the install happen **at launch, in the foreground,
//! in the terminal that started us**, printing as they go, and only then does ratatui take the
//! screen. Nothing is detached, nothing waits for an exit, and no window is created.
//!
//! **What replaces the binary is still the release's own installer**, fetched from the release
//! being installed rather than from `latest`. Two reasons. It is the script that was tested against
//! that build, so a change in how installing works arrives with the thing it installs; and pinning
//! the tag closes the gap between deciding to update and doing it — `latest` could move in between,
//! and then the version that gets installed is not the version that was checked.
//!
//! **The running binary can be replaced while it runs.** On unix the file is moved over and this
//! process keeps the inode it already opened; on Windows `install.ps1` renames the running `.exe`
//! aside first, which the filesystem allows even though overwriting it would not be. Either way
//! what is at the path afterwards is the new version, which is what gets started.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::lang;

/// The repository releases come from.
const REPO: &str = "attacca-cc/zyris-code";

/// Set on the process an update started, so a replacement that did not take cannot become a loop.
///
/// **The install can succeed and change nothing that runs.** A copy under `~/.cargo/bin` shadowing
/// the one on `~/.local/bin`, a directory that is not on PATH at all — in either case the new
/// launch is the old version again, finds the same newer release, and installs it again. This
/// marks the launch that came from an update so it does not look a second time.
const RELAUNCH_MARK: &str = "ZYRIS_CODE_UPDATED";

/// How long the launch-time check may take before the screen is allowed to go up.
///
/// **Short, because everybody pays it.** This one sits between typing the command and seeing
/// anything, so a network that is not answering must cost a moment, not the ten seconds an
/// explicit `/update` is allowed to wait for.
const CHECK_TIMEOUT: Duration = Duration::from_secs(4);

/// How long an update asked for by name may take to find out what the newest release is.
const ASK_TIMEOUT: Duration = Duration::from_secs(10);

/// Asked for from the screen, done once the screen is down.
///
/// **A flag rather than a return value.** Installing has to happen after the terminal is given
/// back — the installer prints, and the new version needs the console — and the screen's exit path
/// already fans out into several arms in `main`. This is the same shape as `lang::set` and
/// `theme::set`: one process-wide answer to a question asked in one place and read in one place.
static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Asks for an update to be installed once the screen has been given back.
pub fn request() {
    REQUESTED.store(true, Ordering::Relaxed);
}

/// Whether the screen asked for one before it closed.
pub fn requested() -> bool {
    REQUESTED.load(Ordering::Relaxed)
}

/// What to do when a newer release exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    /// Install it and come back on the new version. The default: a client that hands a machine to
    /// an agent is one where being a version behind is a thing to fix, not a preference.
    #[default]
    Auto,
    /// Say so and wait to be asked (`/update`).
    Notify,
    /// Do not even look.
    Off,
}

impl Policy {
    pub fn parse(text: &str) -> Option<Policy> {
        match text.trim().to_ascii_lowercase().as_str() {
            "auto" | "자동" => Some(Policy::Auto),
            "notify" | "알림" => Some(Policy::Notify),
            "off" | "끔" => Some(Policy::Off),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Policy::Auto => "auto",
            Policy::Notify => "notify",
            Policy::Off => "off",
        }
    }

    /// The values `/config` walks through, in the order it shows them.
    pub const ALL: [Policy; 3] = [Policy::Auto, Policy::Notify, Policy::Off];
}

/// A release version, as far as comparing them needs.
///
/// **Only the three numbers are compared.** A pre-release suffix is dropped rather than ordered,
/// because ordering it wrongly is worse than not having it: `0.2.0-rc1` read as newer than `0.2.0`
/// would install a candidate over a release and keep doing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u32, u32, u32);

impl Version {
    /// Reads `v0.1.2`, `0.1.2`, `0.1.2-rc1`. `None` when it is not a version at all — a tag we
    /// cannot read must not be treated as newer, or every launch tries to install it.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim().trim_start_matches(['v', 'V']);
        let core = text.split(['-', '+']).next()?;
        let mut parts = core.split('.');
        let mut next = || parts.next().and_then(|p| p.parse::<u32>().ok());
        let (major, minor) = (next()?, next().unwrap_or(0));
        let patch = next().unwrap_or(0);
        // A fourth number means this is not the versioning we know how to compare.
        if parts.next().is_some() {
            return None;
        }
        Some(Version(major, minor, patch))
    }
}

/// Whether `candidate` is a release worth moving to from `current`.
///
/// **Unreadable on either side means no.** Left to a guess, a tag like `nightly` would look newer
/// on every launch and the app would install it again every time.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Some(new), Some(now)) => new > now,
        _ => false,
    }
}

/// What a launch-time look leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Carry on: nothing newer, nothing readable, or nobody asked.
    Stay,
    /// Install it now, before the screen goes up.
    Install(String),
    /// Say it exists and carry on.
    Tell(String),
}

/// The decision, on its own so it can be read without a network.
///
/// **The policy is the whole of it.** `auto` replaces a binary on somebody's machine without being
/// asked, which is only acceptable because it is the setting they left in place; getting this
/// backwards would either install without consent or never install at all, and neither shows until
/// it has already happened.
pub fn step(policy: Policy, found: Option<&str>, current: &str) -> Step {
    let Some(tag) = found.filter(|tag| is_newer(tag, current)) else {
        return Step::Stay;
    };
    match policy {
        Policy::Auto => Step::Install(tag.to_string()),
        Policy::Notify => Step::Tell(tag.to_string()),
        Policy::Off => Step::Stay,
    }
}

/// Whether this launch should ask GitHub anything at all.
///
/// **A launch that came from an update does not look again.** The install can succeed and change
/// nothing that runs — another copy earlier on PATH, a directory that is not on it — and then the
/// new launch is the old version, finds the same release, and installs it again, for ever.
pub fn should_look(policy: Policy, relaunched: bool) -> bool {
    policy != Policy::Off && !relaunched
}

/// Where the running binary lives, so the installer puts the new one in the same place.
///
/// **Without this the update lands somewhere else.** The installer's default is `~/.local/bin`,
/// and a copy started from anywhere else — `~/.cargo/bin` after `cargo install`, or a checkout —
/// would be left in place while a second, newer copy appeared next to it. Whichever the shell
/// found first would then be the one that ran, and it would not be the one just installed.
pub fn install_dir() -> Option<PathBuf> {
    exe_path().and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// The file this process is running, resolved through the `zyris` symlink.
///
/// **Read before anything is installed, and kept.** Once the file has been replaced, Linux reports
/// `/proc/self/exe` as the old path with ` (deleted)` on the end, and starting that goes nowhere.
fn exe_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// The name the binary was started under, so the relaunch is the same command.
pub fn program() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "zyris-code".into())
}

fn client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        // GitHub refuses a request with no user agent, with a 403 that reads like a rate limit.
        .user_agent(concat!("zyris-code/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()
}

/// What the newest release is called, or `None` if the question could not be answered.
///
/// **A failure here is silence.** Not reaching GitHub is the ordinary condition of a laptop on a
/// train; saying so would be noise about something nobody asked for.
pub async fn latest_tag(within: Duration) -> Option<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body: serde_json::Value =
        client()?.get(url).timeout(within).send().await.ok()?.json().await.ok()?;
    let tag = body.get("tag_name")?.as_str()?.trim().to_string();
    (!tag.is_empty()).then_some(tag)
}

/// What the newest release is called, with a way to answer that without publishing one.
///
/// **`$ZYRIS_CODE_UPDATE_TAG` is here so this path can be reached at all.** Everything below it —
/// installing, and handing this terminal over to the new version — only happens when a newer
/// release exists, which is not a state a test can arrange, and it is exactly the step that broke
/// on Windows. Together with `$ZYRIS_CODE_UPDATE_SCRIPT` it lets the whole handover run against a
/// stand-in installer, on a real console, without a release and without replacing anything.
async fn newest(within: Duration) -> Option<String> {
    match std::env::var("ZYRIS_CODE_UPDATE_TAG") {
        Ok(tag) if !tag.trim().is_empty() => Some(tag.trim().to_string()),
        _ => latest_tag(within).await,
    }
}

/// Where the installer for `tag` is fetched from.
///
/// **Pinned to the release being installed**, never `latest`: between the check and the install a
/// release can appear, and then what lands is not what was reported.
pub fn install_url(tag: &str) -> String {
    let file = if cfg!(windows) { "install.ps1" } else { "install.sh" };
    format!("https://github.com/{REPO}/releases/download/{tag}/{file}")
}

/// How the installer is run: the program, and its arguments.
///
/// **`--no-modify-path` is not an optimisation.** An update nobody asked to have opinions about
/// must not rewrite shell configuration; the lines the installer leaves in `.zshrc` belong to the
/// one time somebody ran it themselves.
pub fn installer_command(script: &Path, tag: &str, dir: &Path) -> (String, Vec<String>) {
    let (script, dir) = (script.display().to_string(), dir.display().to_string());
    if cfg!(windows) {
        let args = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", &script]
            .into_iter()
            .map(str::to_string)
            .chain(["-Version".into(), tag.to_string(), "-Dir".into(), dir, "-NoModifyPath".into()])
            .collect();
        ("powershell".to_string(), args)
    } else {
        let args = [script, "--version".into(), tag.to_string(), "--dir".into(), dir]
            .into_iter()
            .chain(["--no-modify-path".to_string()])
            .collect();
        ("sh".to_string(), args)
    }
}

/// Fetches the release's installer and runs it, with its output going to this terminal.
///
/// **Inherited, not captured.** Downloading and checking a binary takes long enough that silence
/// reads as a hang, and the installer already says what it is doing at each step — capturing that
/// to print a summary afterwards would replace a live account with a delayed one.
pub async fn install(tag: &str) -> anyhow::Result<()> {
    let dir =
        install_dir().ok_or_else(|| anyhow::anyhow!("could not find where this is installed"))?;

    // A stand-in installer, given rather than fetched — see `newest`. Everything after this point
    // is the same, which is the only reason running against it proves anything.
    let given = std::env::var_os("ZYRIS_CODE_UPDATE_SCRIPT").map(PathBuf::from);
    let path = match &given {
        Some(path) => path.clone(),
        None => {
            let client =
                client().ok_or_else(|| anyhow::anyhow!("could not build an HTTP client"))?;
            let script =
                client.get(install_url(tag)).send().await?.error_for_status()?.text().await?;
            let ext = if cfg!(windows) { "ps1" } else { "sh" };
            let path = std::env::temp_dir()
                .join(format!("zyris-code-install-{}.{ext}", std::process::id()));
            std::fs::write(&path, script)?;
            path
        }
    };

    let (program, args) = installer_command(&path, tag, &dir);
    let status = tokio::process::Command::new(program).args(args).status().await;
    // What we fetched has done its work or failed; either way it is not worth keeping around. What
    // somebody handed us is theirs.
    if given.is_none() {
        let _ = std::fs::remove_file(&path);
    }

    match status? {
        s if s.success() => Ok(()),
        s => anyhow::bail!("the installer exited with {s}"),
    }
}

/// Starts the newly installed binary in this same terminal, and does not come back.
///
/// **The same terminal is the whole point.** The old shape started the new version with
/// `Start-Process`, which gives a console program a console of its own — in `cmd` that reads as the
/// app having quit, because from that window it did.
///
/// On unix this replaces the process image, so nothing is left behind. On Windows there is no
/// `exec`, so this one waits: a parent that exited instead would hand the shell its prompt back
/// while the new version was still drawing into the same window.
pub fn relaunch(tag: &str) -> anyhow::Result<std::convert::Infallible> {
    let exe = exe_path().ok_or_else(|| anyhow::anyhow!("could not find this program"))?;
    let mut command = std::process::Command::new(&exe);
    // The same invocation, so `-p …` survives an update that happens on the way to answering it.
    command.args(std::env::args_os().skip(1)).env(RELAUNCH_MARK, tag);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Only returns on failure.
        Err(command.exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        std::process::exit(status.code().unwrap_or(0));
    }
}

/// The launch-time step: look once, install if that is the policy, come back on the new version.
///
/// Returns a tag worth mentioning on screen, which is only ever the `notify` case — `auto` does not
/// come back from here, and `off` never looked.
///
/// **Print mode does not call this.** `-p` is what a script runs; an install writing to its stdout
/// would end up in whatever it was piped into, and a caller waiting on one answer should not have a
/// download appear in front of it.
pub async fn at_launch(policy: Policy) -> Option<String> {
    if !should_look(policy, std::env::var_os(RELAUNCH_MARK).is_some()) {
        return None;
    }
    let found = newest(CHECK_TIMEOUT).await;
    match step(policy, found.as_deref(), env!("CARGO_PKG_VERSION")) {
        Step::Stay => None,
        Step::Tell(tag) => Some(tag),
        Step::Install(tag) => match carry_out(&tag).await {
            // Unreachable on success: `relaunch` does not return. Failing, it falls through to the
            // screen with the tag, so `/update` can be tried by hand instead of nothing being said.
            Ok(()) => None,
            Err(_) => Some(tag),
        },
    }
}

/// The update asked for by name, once the screen has been given back.
///
/// **The tag is looked up again here**, because the one the screen was holding was found when it
/// opened and a session outlives that by hours.
pub async fn install_now() {
    let lang = lang::current();
    // **A process an update started does not update again.** `--update` relaunches with the same
    // arguments it was given, so this comes back around — and if the install changed nothing that
    // runs (a copy earlier on PATH, a directory that is not on it), the version is the same, it is
    // newer than itself, and it installs and restarts without end. `at_launch` is guarded the same
    // way; this is the arm somebody reaches by name.
    if std::env::var_os(RELAUNCH_MARK).is_some() {
        println!("{}", lang.update_now_on(env!("CARGO_PKG_VERSION")));
        return;
    }
    let Some(tag) = newest(ASK_TIMEOUT).await else {
        eprintln!("{}", lang.update_failed(lang.update_no_answer()));
        return;
    };
    if !is_newer(&tag, env!("CARGO_PKG_VERSION")) {
        println!("{}", lang.update_current());
        return;
    }
    let _ = carry_out(&tag).await;
}

/// Say what is happening, install, and hand the terminal to the new version.
///
/// **It says so before it starts.** Fetching and checking a release is seconds of nothing, and a
/// terminal that went quiet right after a command is one somebody reaches for Ctrl+C in.
async fn carry_out(tag: &str) -> anyhow::Result<()> {
    let lang = lang::current();
    println!("{}", lang.update_installing_from(env!("CARGO_PKG_VERSION"), tag));
    if let Err(e) = install(tag).await {
        eprintln!("{}", lang.update_failed(&e.to_string()));
        return Err(e);
    }
    println!("{}", lang.update_restarting(tag));
    match relaunch(tag) {
        Ok(never) => match never {},
        Err(e) => {
            // Installed, but this process could not become the new one. Saying so is the whole
            // difference between "run it again" and a version number that never changes.
            eprintln!("{}", lang.update_installed_not_started(&e.to_string()));
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_however_the_tag_is_written() {
        assert_eq!(Version::parse("v0.1.2"), Some(Version(0, 1, 2)));
        assert_eq!(Version::parse("0.1.2"), Some(Version(0, 1, 2)));
        assert_eq!(Version::parse("  V1.0.0  "), Some(Version(1, 0, 0)));
        assert_eq!(Version::parse("2.3"), Some(Version(2, 3, 0)), "a missing patch is zero");
    }

    /// **A tag we cannot read is not newer.** Guessed at, `nightly` would look newer on every
    /// launch and the app would install it again, and again.
    #[test]
    fn a_tag_that_is_not_a_version_is_never_newer() {
        assert_eq!(Version::parse("nightly"), None);
        assert_eq!(Version::parse(""), None);
        assert_eq!(Version::parse("v1.2.3.4"), None, "four numbers is not this scheme");
        assert!(!is_newer("nightly", "0.1.1"));
        assert!(!is_newer("0.2.0", "not-a-version"));
    }

    #[test]
    fn newer_is_newer_and_nothing_else_is() {
        assert!(is_newer("0.1.2", "0.1.1"));
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.1", "0.1.1"), "the same version is not an update");
        assert!(!is_newer("0.1.0", "0.1.1"), "older is not an update");
    }

    /// **A pre-release is not ordered, it is trimmed.** Read as newer than the release it precedes,
    /// a candidate would be installed over a finished version and stay there.
    #[test]
    fn a_pre_release_never_outranks_the_release_it_precedes() {
        assert_eq!(Version::parse("0.2.0-rc1"), Some(Version(0, 2, 0)));
        assert!(!is_newer("0.2.0-rc1", "0.2.0"), "a candidate must not replace the release");
        assert!(is_newer("0.2.0-rc1", "0.1.9"), "but it is still newer than what came before");
    }

    /// **Only `auto` replaces a binary without being asked.** This is the one decision in the file
    /// that acts on somebody's machine on its own, and it is invisible until it has happened.
    #[test]
    fn only_auto_installs_by_itself() {
        assert_eq!(step(Policy::Auto, Some("v0.3.0"), "0.2.0"), Step::Install("v0.3.0".into()));
        assert_eq!(step(Policy::Notify, Some("v0.3.0"), "0.2.0"), Step::Tell("v0.3.0".into()));
        assert_eq!(step(Policy::Off, Some("v0.3.0"), "0.2.0"), Step::Stay, "off looked anyway");
    }

    /// Nothing newer, and nothing readable, are both "carry on" — including for `auto`, which would
    /// otherwise reinstall the version it is already running on every launch.
    #[test]
    fn there_is_nothing_to_do_when_there_is_nothing_newer() {
        for policy in Policy::ALL {
            assert_eq!(step(policy, None, "0.2.0"), Step::Stay, "{policy:?} acted on no answer");
            assert_eq!(step(policy, Some("v0.2.0"), "0.2.0"), Step::Stay, "{policy:?} reinstalled");
            assert_eq!(step(policy, Some("v0.1.0"), "0.2.0"), Step::Stay, "{policy:?} went back");
            assert_eq!(step(policy, Some("nightly"), "0.2.0"), Step::Stay, "{policy:?} guessed");
        }
    }

    /// **A launch that came from an update does not look again.** An install that lands somewhere
    /// PATH does not reach succeeds while changing nothing that runs, and without this the next
    /// launch finds the same release and installs it again, every time.
    #[test]
    fn a_launch_that_came_from_an_update_does_not_look_again() {
        assert!(should_look(Policy::Auto, false));
        assert!(should_look(Policy::Notify, false));
        assert!(!should_look(Policy::Auto, true), "an update that did not take became a loop");
        assert!(!should_look(Policy::Off, false), "off looked");
    }

    /// **The tag is pinned.** Fetching `latest` instead would install whatever is newest at the
    /// moment the installer runs, which is not what was checked, agreed to, or reported — and
    /// between the two there is a window in which a release can appear.
    #[test]
    fn the_installer_comes_from_the_release_that_was_checked() {
        let url = install_url("v0.2.0");
        assert!(url.contains("/download/v0.2.0/"), "the installer is not pinned: {url}");
        assert!(!url.contains("releases/latest"), "it falls back to latest: {url}");
    }

    /// **Where this copy runs from, and no shell configuration.** Installing to the default
    /// directory would leave a newer binary beside the one PATH actually finds, and an update
    /// nobody asked to have opinions about must not rewrite `.zshrc`.
    #[test]
    fn the_installer_is_told_where_this_copy_lives_and_to_touch_nothing_else() {
        let dir = Path::new("/home/x/.local/bin");
        let (program, args) = installer_command(Path::new("/tmp/i.sh"), "v0.2.0", dir);
        let line = format!("{program} {}", args.join(" "));
        assert!(line.contains("/home/x/.local/bin"), "it would install somewhere else: {line}");
        assert!(line.contains("v0.2.0"), "the tag is not passed to the installer: {line}");
        assert!(line.contains("/tmp/i.sh"), "the script is not run: {line}");
        let quiet = line.contains("--no-modify-path") || line.contains("-NoModifyPath");
        assert!(quiet, "an update would rewrite shell configuration: {line}");
    }

    #[test]
    fn the_policy_reads_both_languages_and_survives_a_round_trip() {
        assert_eq!(Policy::parse("auto"), Some(Policy::Auto));
        assert_eq!(Policy::parse("자동"), Some(Policy::Auto));
        assert_eq!(Policy::parse("OFF"), Some(Policy::Off));
        assert_eq!(Policy::parse("무엇"), None, "an unknown value must not be taken as a policy");
        for p in Policy::ALL {
            assert_eq!(Policy::parse(p.as_str()), Some(p));
        }
        assert_eq!(Policy::default(), Policy::Auto);
    }
}
