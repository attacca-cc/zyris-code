//! Noticing a new release, and stepping aside for it.
//!
//! **What replaces the binary is the release's own installer**, fetched from the release being
//! installed rather than from `latest`. Two reasons. It is the script that was tested against that
//! build, so a change in how installing works arrives with the thing it installs; and pinning the
//! tag closes the gap between deciding to update and doing it — `latest` could move in between,
//! and then the version that gets installed is not the version that was checked.
//!
//! **The running process does not do the replacing.** A Windows `.exe` cannot be overwritten while
//! it runs, so a helper waits for this process to end, installs, and starts the new one. Doing it
//! the same way on unix is not the cost it looks like: it is one script instead of two paths, and
//! the relaunch has to wait for the exit regardless.

use std::path::{Path, PathBuf};

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

/// Where the running binary lives, so the installer puts the new one in the same place.
///
/// **Without this the update lands somewhere else.** The installer's default is `~/.local/bin`,
/// and a copy started from anywhere else — `~/.cargo/bin` after `cargo install`, or a checkout —
/// would be left in place while a second, newer copy appeared next to it. Whichever the shell
/// found first would then be the one that ran, and it would not be the one just installed.
pub fn install_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // Follow the symlink `zyris` points through, so the directory is the real one.
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    exe.parent().map(Path::to_path_buf)
}

/// The name the binary was started under, so the relaunch is the same command.
pub fn program() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "zyris-code".into())
}


/// What the newest release is called, or `None` if the question could not be answered.
///
/// **A failure here is silence.** Not reaching GitHub is the ordinary condition of a laptop on a
/// train; saying so on the one line that reports what the agent is doing would be noise about
/// something nobody asked for.
pub async fn latest_tag() -> Option<String> {
    let url = "https://api.github.com/repos/attacca-cc/zyris-code/releases/latest";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        // GitHub refuses a request with no user agent, with a 403 that reads like a rate limit.
        .user_agent(concat!("zyris-code/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let body: serde_json::Value = client.get(url).send().await.ok()?.json().await.ok()?;
    let tag = body.get("tag_name")?.as_str()?.trim().to_string();
    (!tag.is_empty()).then_some(tag)
}

/// The script that outlives this process: wait for it to end, install `tag`, start it again.
///
/// **Written out rather than run inline** because on Windows the file being replaced is the one
/// running, and because either way the new version cannot be started until the old one has let go.
///
/// The installer comes from the release being installed and is given that tag — so the script that
/// replaces the binary is the one tested against that build, and `latest` moving between the check
/// and the install cannot change what lands.
pub fn helper_script(tag: &str, dir: &Path, program: &str, pid: u32) -> String {
    let base = format!("https://github.com/attacca-cc/zyris-code/releases/download/{tag}");
    let dir = dir.display();
    if cfg!(windows) {
        format!(
            "$ErrorActionPreference = 'SilentlyContinue'\r\n\
             while (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ Start-Sleep -Milliseconds 200 }}\r\n\
             $ErrorActionPreference = 'Stop'\r\n\
             $s = Join-Path $env:TEMP 'zyris-code-install.ps1'\r\n\
             Invoke-WebRequest -UseBasicParsing '{base}/install.ps1' -OutFile $s\r\n\
             & $s -Version '{tag}' -Dir '{dir}' -NoModifyPath\r\n\
             Start-Process -FilePath (Join-Path '{dir}' '{program}.exe')\r\n"
        )
    } else {
        format!(
            "#!/bin/sh\n\
             while kill -0 {pid} 2>/dev/null; do sleep 0.2; done\n\
             set -e\n\
             s=\"$(mktemp)\"\n\
             trap 'rm -f \"$s\"' EXIT\n\
             curl -fsSL '{base}/install.sh' -o \"$s\"\n\
             sh \"$s\" --version '{tag}' --dir '{dir}' --no-modify-path\n\
             exec '{dir}/{program}'\n"
        )
    }
}

/// Writes the helper out and starts it, detached. The caller then ends this process.
///
/// **Detached is the whole point.** A child that dies with its parent would be killed by the very
/// exit it is waiting for.
pub fn spawn_helper(tag: &str) -> anyhow::Result<()> {
    let dir = install_dir().ok_or_else(|| anyhow::anyhow!("could not find where this is installed"))?;
    let program = program();
    let pid = std::process::id();
    let script = helper_script(tag, &dir, &program, pid);

    let ext = if cfg!(windows) { "ps1" } else { "sh" };
    let path = std::env::temp_dir().join(format!("zyris-code-update-{pid}.{ext}"));
    std::fs::write(&path, script)?;

    let mut command = if cfg!(windows) {
        let mut c = std::process::Command::new("powershell");
        c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]).arg(&path);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.arg(&path);
        c
    };
    // Nothing of the helper's belongs on this screen, and it outlives the pipes anyway.
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        // Its own process group, so the exit that is about to happen here cannot reach it.
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn()?;
    Ok(())
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

    /// **The tag is pinned everywhere it appears.** Fetching `latest` instead would install
    /// whatever is newest at the moment the helper runs, which is not what was checked, agreed to,
    /// or reported — and between the two there is a window in which a release can appear.
    #[test]
    fn the_helper_installs_the_version_that_was_checked() {
        let script = helper_script("v0.2.0", Path::new("/home/x/.local/bin"), "zyris-code", 4321);
        assert!(script.contains("/download/v0.2.0/"), "the installer is not pinned:\n{script}");
        assert!(!script.contains("releases/latest"), "it falls back to latest:\n{script}");
        assert!(script.contains("v0.2.0"), "the tag is not passed to the installer:\n{script}");
        assert!(script.contains("/home/x/.local/bin"), "it would install somewhere else");
    }

    /// **It waits for this process to end.** On Windows the running `.exe` cannot be replaced at
    /// all until then, and on either platform starting the new one first would leave two.
    #[test]
    fn the_helper_waits_for_the_old_process_to_end() {
        let script = helper_script("v0.2.0", Path::new("/tmp/bin"), "zyris", 4321);
        assert!(script.contains("4321"), "it does not wait for anything:\n{script}");
        let waits = script.contains("kill -0") || script.contains("Get-Process");
        assert!(waits, "no wait loop:\n{script}");
        assert!(script.contains("/tmp/bin/zyris") || script.contains("zyris.exe"), "no relaunch");
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
