//! Print mode, run as the command somebody types.
//!
//! **What is checked here is that it ends.** `-p` is what a script runs, and the failure that
//! matters is not a wrong answer but no answer and no exit — a blank line and a cursor, which in a
//! script is a hang and to a person is indistinguishable from the program being broken. It waited
//! for a first connection for ever until 2026-08-17.
//!
//! Nothing here reaches the network: the server address points at a port that refuses at once.

use std::process::Command;
use std::time::{Duration, Instant};

/// Comfortably more than the connect wait the test asks for, so a slow runner is not a failure.
const PATIENCE: Duration = Duration::from_secs(40);

/// **It cannot connect, and it has to say so and leave.**
///
/// The address refuses immediately, so this is the shape of every unreachable server — a laptop
/// off the network, a VPN that is not up, a machine that was never approved. What it must not do
/// is sit there.
#[test]
fn print_mode_gives_up_instead_of_waiting_for_ever() {
    let dir = std::env::temp_dir().join(format!("zyris-print-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);

    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_zyris-code"))
        .args(["-p", "say hello"])
        // A credential given outright: no enrolment, no browser, nothing written to a real config.
        .env("ZYRIS_NODE_TOKEN", "znt_print_test_not_a_real_token")
        .env("ZYRIS_SERVER_URL", "ws://127.0.0.1:1")
        .env("ZYRIS_CONFIG_DIR", &dir)
        .env("ZYRIS_CODE_CONNECT_WAIT_SECS", "3")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("could not start the app");

    let deadline = Instant::now() + PATIENCE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`-p` never ended — it is waiting for a connection that will not come");
            }
        }
    }
    let out = child.wait_with_output().expect("could not read what it said");

    assert!(!out.status.success(), "it failed to connect and called that success");
    // **Said, not merely exited.** A non-zero exit with nothing on stderr is the worst thing to
    // hand a script: there is no screen here that could have shown the reason.
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!said.trim().is_empty(), "it left without saying anything");
    assert!(said.contains("connect"), "it did not say it could not connect:\n{said}");
    assert!(started.elapsed() < PATIENCE, "it took longer than it was given");

    let _ = std::fs::remove_dir_all(&dir);
}
