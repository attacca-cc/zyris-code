//! Updating, run as the command somebody types.
//!
//! **No screen and no pseudo-terminal here.** `--update` builds nothing: no credential, no node,
//! no ratatui. What it does is run an installer and hand this process over to what was installed,
//! and both of those are visible from the outside — a file on disk, and a process that ends.
//!
//! **Nothing touches the network.** `$ZYRIS_CODE_UPDATE_TAG` says a newer release exists without
//! one existing, and `$ZYRIS_CODE_UPDATE_SCRIPT` puts a stand-in where the release's installer
//! would be. Everything between those two points is the real path.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Long enough for a debug binary to start twice on a loaded runner, short enough that a process
/// which will never end is still a test result rather than a hung suite.
const PATIENCE: Duration = Duration::from_secs(60);

/// A directory of its own per test, so two of these never read each other's marks.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zyris-update-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("could not make a scratch directory");
    dir
}

/// An installer that records that it ran, and what it was told to install.
///
/// **It appends.** Whether this ran once or ten times is the whole question in
/// `an_update_asked_for_by_name_installs_once`, and a file that is overwritten cannot answer it.
///
/// It is handed the same arguments as the real installer and has to accept them — one that refused
/// would fail before writing anything, and the test would read that as the app's fault.
fn stand_in(dir: &Path) -> (PathBuf, PathBuf) {
    let marker = dir.join("ran");
    let script = dir.join(if cfg!(windows) { "stand-in.ps1" } else { "stand-in.sh" });
    let text = if cfg!(windows) {
        format!(
            "param([string] $Version, [string] $Dir, [switch] $NoModifyPath)\r\n\
             Add-Content -Path '{}' -Value $Version\r\n",
            marker.display()
        )
    } else {
        format!("#!/bin/sh\nprintf '%s\\n' \"$2\" >> '{}'\n", marker.display())
    };
    std::fs::write(&script, text).expect("could not write the stand-in installer");
    (script, marker)
}

/// Runs the app until it ends, killing it rather than letting it hold the suite.
fn run(args: &[&str], env: &[(&str, String)]) -> Option<std::process::Output> {
    run_by(args, env, PATIENCE)
}

fn run_by(
    args: &[&str],
    env: &[(&str, String)],
    patience: Duration,
) -> Option<std::process::Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zyris-code"));
    command.args(args);
    for (name, value) in env {
        command.env(name, value);
    }
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("could not start the app");

    let deadline = Instant::now() + patience;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }
    child.wait_with_output().ok()
}

/// **Installing once is the whole of it.**
///
/// `--update` restarts with the arguments it was given, which means it comes back around to this
/// same code. If the install changed nothing that runs — a copy earlier on PATH, a directory that
/// is not on it at all — then the version is what it was, it is newer than itself, and it installs
/// and restarts again, and again, with nothing on screen to say why. The mark the relaunch leaves
/// in the environment is what stops that, and this is what proves it stops.
#[test]
fn an_update_asked_for_by_name_installs_once() {
    let dir = scratch("once");
    let (script, marker) = stand_in(&dir);

    let output = run(
        &["--update"],
        &[
            ("ZYRIS_CODE_UPDATE_TAG", "v99.0.0".into()),
            ("ZYRIS_CODE_UPDATE_SCRIPT", script.display().to_string()),
            ("ZYRIS_CODE_LANG", "en".into()),
        ],
    );
    let output = output.expect("`--update` never ended — it is installing in a loop");

    let ran = std::fs::read_to_string(&marker).unwrap_or_default();
    let times = ran.lines().filter(|line| !line.trim().is_empty()).count();
    assert_eq!(times, 1, "the installer ran {times} times, not once:\n{ran}");
    assert_eq!(ran.lines().next().unwrap_or_default().trim(), "v99.0.0", "the wrong release");

    // And the process that came back says where it ended up rather than claiming to be current —
    // if the install went somewhere PATH cannot reach, "already the newest" would be a lie told by
    // the thing that failed.
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(said.contains("Now on"), "it did not say what it is running now:\n{said}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// **Print mode does not look for a release.** `-p` is what a script runs; an installer writing to
/// its stdout would land in whatever it was piped into, and a caller waiting on one answer should
/// not have a download appear in front of it.
#[test]
fn print_mode_never_updates() {
    let dir = scratch("print");
    let (script, marker) = stand_in(&dir);

    // **Given a few seconds and then stopped, rather than waited out.** Print mode with nowhere to
    // connect to keeps trying, so waiting for it to end would be waiting for the clock. The update
    // step is the first thing that happens — before the credential, before the node — so if it were
    // going to install, it would have by now, and the answer is on disk either way.
    run_by(
        &["-p", "hello"],
        &[
            ("ZYRIS_CODE_UPDATE_TAG", "v99.0.0".into()),
            ("ZYRIS_CODE_UPDATE_SCRIPT", script.display().to_string()),
            ("ZYRIS_NODE_TOKEN", "znt_update_test_not_a_real_token".into()),
            ("ZYRIS_SERVER_URL", "ws://127.0.0.1:1".into()),
            ("ZYRIS_CONFIG_DIR", dir.display().to_string()),
        ],
        Duration::from_secs(8),
    );

    assert!(!marker.exists(), "print mode installed a release on its way to answering");

    let _ = std::fs::remove_dir_all(&dir);
}
