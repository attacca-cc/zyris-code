//! The app under a real pseudo-terminal, on every platform.
//!
//! **This is the half the cell tests cannot reach.** `tests/screen.rs` drives a `TestBackend`, so
//! it never finds out whether the app takes a real terminal and gives it back, whether a keystroke
//! arriving as bytes reaches the input, or whether it stops somewhere waiting for an answer the
//! terminal will never send. Those only show up against something that behaves like a terminal.
//!
//! **And it had to be Rust to reach Windows at all.** The smoke scripts under `scripts/` are built
//! on Python's `pty` module, which is unix-only — so the platform whose console differs most from
//! everything else was the one platform none of them could test. `portable-pty` opens a unix pty
//! here and a ConPTY (`CreatePseudoConsole`, Windows 10 1809 and later) there, behind one API, so
//! `cargo test` covers both.
//!
//! **Nothing here touches the network.** A node token from the environment skips enrolment
//! outright (`StaticToken::from_env` wins over everything), and the server address points at a
//! port that refuses at once — so the screen comes up, draws, and takes keys with no account, no
//! credential on disk, and nothing to wait for.
//!
//! **Every read is bounded.** A test that waits for a string the app will never print would hang
//! the suite, and a killed `cargo` leaves the test binary behind eating a core — this repo has
//! lost twenty minutes to exactly that. `read_until` gives up and hands back what it saw.

use std::io::{Read, Write};
use std::sync::{mpsc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize};

/// **One pseudo-terminal at a time.**
///
/// Each of these starts a real app on a real console and drives it by hand. Two at once on a
/// two-core runner starve each other: keystrokes written to one arrive late enough that the app
/// has drawn past them, and which test loses is decided by the scheduler. On Windows they took
/// turns failing run after run, one passing while the other did not and then the other way round
/// — which is what contention looks like and what a real fault does not.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Held for the length of a test. **A panic elsewhere must not close this door** — a poisoned lock
/// still hands over the turn, and the test that poisoned it has already reported its own failure.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How long any one wait may take. Generous enough for a debug build starting on a loaded
/// machine, short enough that a stuck test is still a test result.
const PATIENCE: Duration = Duration::from_secs(20);

/// The app, running on a pseudo-terminal, with everything it says collected as it arrives.
struct Session {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// **Shared, because `take_writer` gives it up once.** On Windows the reader thread has to
    /// write back (see `start`), and asking the master for a second writer fails outright.
    writer: std::sync::Arc<std::sync::Mutex<Box<dyn Write + Send>>>,
    /// Filled by a reader thread. The pty read blocks, so it cannot live on this thread.
    output: mpsc::Receiver<Vec<u8>>,
    seen: Vec<u8>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl Session {
    fn start() -> Session {
        Session::start_with(&[])
    }

    /// The same, with more of the environment set — for the paths that only exist when something
    /// outside the app is true.
    fn start_with(extra: &[(&str, String)]) -> Session {
        let pty = portable_pty::native_pty_system()
            .openpty(PtySize { rows: 30, cols: 100, pixel_width: 0, pixel_height: 0 })
            .expect("could not open a pseudo-terminal");

        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_zyris-code"));
        for (name, value) in extra {
            cmd.env(name, value);
        }
        // A credential given outright: no enrolment window, nothing written to disk, no browser.
        cmd.env("ZYRIS_NODE_TOKEN", "znt_pty_test_not_a_real_token");
        // Refused at once rather than left hanging, so the screen settles quickly.
        cmd.env("ZYRIS_SERVER_URL", "ws://127.0.0.1:1");
        // Somewhere empty, so a real credential on this machine is never read or written.
        let dir = std::env::temp_dir().join(format!("zyris-pty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        cmd.env("ZYRIS_CONFIG_DIR", &dir);
        // Off: the self-heal reprints the whole screen on a timer, which would make "what arrived
        // after the keystroke" impossible to read.
        cmd.env("ZYRIS_CODE_HEAL_MS", "0");
        // Off: `git status` on a large checkout would land in the middle of what we are reading.
        cmd.env("ZYRIS_CODE_GIT_MS", "0");
        cmd.env("ZYRIS_CODE_LOG", dir.join("log").to_string_lossy().to_string());

        let child = pty.slave.spawn_command(cmd).expect("could not start the app");
        // **The slave is dropped here on purpose.** Held open, the master never sees end-of-file
        // when the app exits and the reader thread waits for ever.
        drop(pty.slave);

        let mut reader = pty.master.try_clone_reader().expect("could not read the pty");
        let writer = std::sync::Arc::new(std::sync::Mutex::new(
            pty.master.take_writer().expect("could not write to the pty"),
        ));

        // **On Windows the cursor-position question has to be answered, and on unix it must not
        // be.** The two are not inconsistent: they are questions from different askers.
        //
        // On unix nothing here answers `ESC[6n`, on purpose — this pty stands in for a remote
        // terminal that never replies, which is how `Terminal::clear()` blocking the whole app
        // was caught, and answering would retire that guard.
        //
        // On Windows the asker is not the app. Console work there goes through the Win32 API, and
        // crossterm only ever writes `ESC[6n` from its unix path; what emits it is ConPTY, which
        // is translating for a terminal it assumes exists. Left unanswered it draws nothing at
        // all — the first run of this suite on Windows sat for forty seconds with `ESC[6n` as the
        // entire screen. So here we are that terminal, and we answer.
        #[cfg(windows)]
        let answer = std::sync::Arc::clone(&writer);

        let (tx, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                #[cfg(windows)]
                if buf[..n].windows(4).any(|w| w == b"\x1b[6n".as_slice()) {
                    // Row 1, column 1. Where the cursor actually is does not matter to anything
                    // being tested; that the question gets an answer does.
                    if let Ok(mut w) = answer.lock() {
                        let _ = w.write_all(b"\x1b[1;1R");
                        let _ = w.flush();
                    }
                }
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });

        Session { child, writer, output, seen: Vec::new(), _master: pty.master }
    }

    /// Everything the app has said so far, escapes and all.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.seen).into_owned()
    }

    /// The same, with the escape sequences taken out.
    ///
    /// **Typed text never arrives as one string.** ratatui writes only the cells that changed, so
    /// five letters go out as five runs of `move · colour · letter` and `hello` is never on the
    /// wire in one piece. Dropping the sequences puts the letters back next to each other.
    fn plain(&self) -> String {
        strip_escapes(&self.text())
    }

    /// Waits until `needle` shows up once the escapes are stripped.
    fn wait_for_text(&mut self, needle: &str) -> bool {
        self.wait_until(|s| s.plain().contains(needle))
    }

    /// Waits until `needle` has been said, or gives up.
    ///
    /// **What already arrived counts.** Once the app has drawn and gone quiet no more bytes come,
    /// so a check that only looks at newly-read output would wait out the clock for something it
    /// was already holding — the Python scripts learned this the hard way and five of them failed
    /// against a perfectly healthy app.
    fn wait_for(&mut self, needle: &str) -> bool {
        self.wait_until(|s| s.text().contains(needle))
    }

    fn wait_until(&mut self, done: impl Fn(&Session) -> bool) -> bool {
        self.wait_until_by(PATIENCE, done)
    }

    fn wait_until_by(&mut self, patience: Duration, done: impl Fn(&Session) -> bool) -> bool {
        let deadline = Instant::now() + patience;
        loop {
            if done(self) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            match self.output.recv_timeout(left.min(Duration::from_millis(250))) {
                Ok(chunk) => self.seen.extend_from_slice(&chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return done(self),
            }
        }
    }

    /// Waits until the screen has settled enough to take keys — the bottom bar is the last thing
    /// the first frame draws. Sending before that races the app's own start-up.
    /// **Judged by what was drawn, not by how.** Taking the alternate screen is an escape sequence
    /// on unix and a console API call on Windows, so waiting for `CSI ? 1049 h` would be waiting
    /// for something that platform never puts on the wire. The bottom bar is text either way.
    fn wait_until_ready(&mut self) {
        let bar = self.wait_for_text("normal") || self.wait_for_text("일반");
        assert!(bar, "the bottom bar never appeared — the app drew nothing:\n{}", self.text());
    }

    /// Types `text` and waits for it to come back on the screen, asking for a full repaint
    /// between tries.
    ///
    /// **Only the cells that changed go out.** Several letters typed at once can therefore be
    /// split over frames — `h` in one, `ello` in the next, with a screenful of unrelated cells
    /// between them: every letter present, in order, and never adjacent. Windows showed this the
    /// first time the suite ran there. Ctrl+L redraws everything at once, which is how the smoke
    /// scripts under `scripts/` read the screen too.
    ///
    /// Repainting is repeated because the first one can be served before the last letter has been
    /// consumed, and would then faithfully redraw a half-typed line.
    fn type_and_see(&mut self, text: &str) -> bool {
        self.send(text.as_bytes());
        // Twenty tries at a second each, so the whole loop is bounded by `PATIENCE` even on a
        // runner that is being slow rather than broken.
        for _ in 0..20 {
            let looking_for = text.to_string();
            if self.wait_until_by(Duration::from_secs(1), move |s| s.plain().contains(&looking_for))
            {
                return true;
            }
            self.send(b"\x0c");
        }
        false
    }

    fn send(&mut self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    /// Waits for the app to end. `None` if it outlived our patience.
    fn wait_for_exit(&mut self) -> Option<u32> {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status.exit_code()),
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => return None,
            }
        }
        None
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // **Never leave it running.** An orphaned app holds a core at 100% and outlives the test
        // run that started it.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// **Only what is used is asked for.** crossterm's `EnableMouseCapture` turns on five modes at
/// once, two of which this app has no use for: `1003` reports the pointer whenever it moves at all
/// and nothing here reads a bare hover, and `1015` is the coordinate encoding `1006` replaced.
/// Over SSH the first is bytes on the wire for every twitch of the mouse.
///
/// Unix only, for the reason on `quitting_gives_the_terminal_back`.
#[cfg(unix)]
#[test]
fn only_the_mouse_modes_this_app_reads_are_switched_on() {
    let _turn = one_at_a_time();
    let mut app = Session::start();
    app.wait_until_ready();
    let out = app.text();

    for (what, seq) in
        [("buttons", "\x1b[?1000h"), ("drag", "\x1b[?1002h"), ("SGR coordinates", "\x1b[?1006h")]
    {
        assert!(out.contains(seq), "{what} tracking was never switched on ({seq:?})");
    }
    for (what, seq) in [("any-motion", "\x1b[?1003h"), ("urxvt coordinates", "\x1b[?1015h")] {
        assert!(!out.contains(seq), "{what} tracking was switched on and is never read ({seq:?})");
    }
}

/// Drops ANSI escape sequences, leaving what a person would see.
///
/// CSI (`ESC [ … final`), OSC (`ESC ] … BEL` or `ST`), and the two-character escapes. Enough for
/// reading a screen back; it is not a terminal emulator and does not try to be.
fn strip_escapes(raw: &str) -> String {
    let bytes: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != '\u{1b}' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            Some('[') => {
                i += 2;
                // Parameters and intermediates, then one final byte in `@`..=`~`.
                while i < bytes.len() && !('\u{40}'..='\u{7e}').contains(&bytes[i]) {
                    i += 1;
                }
                i += 1;
            }
            Some(']') => {
                i += 2;
                // Runs to BEL or ST (`ESC \`).
                while i < bytes.len() {
                    if bytes[i] == '\u{7}' {
                        i += 1;
                        break;
                    }
                    if bytes[i] == '\u{1b}' && bytes.get(i + 1) == Some(&'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            // `ESC >`, `ESC =`, and the like.
            Some(_) => i += 2,
            None => i += 1,
        }
    }
    out
}

/// **The screen comes up on a real terminal and keys reach the input.**
///
/// Everything else here builds on those two: a screen that never draws makes every other check
/// meaningless, and a keystroke that arrives as bytes rather than as a crossterm event is the
/// shape most terminal differences take.
#[test]
fn the_app_draws_on_a_pseudo_terminal_and_takes_a_keystroke() {
    let _turn = one_at_a_time();
    let mut app = Session::start();
    app.wait_until_ready();

    assert!(app.type_and_see("hello"), "a keystroke never reached the input box:\n{}", app.text());
}

/// **The app ends when it is told to.** Ctrl+C arms the quit and the second one takes it; an app
/// that cannot be closed from the keyboard is one a person has to kill from another window.
#[test]
fn ctrl_c_twice_ends_the_app() {
    let _turn = one_at_a_time();
    let mut app = Session::start();
    app.wait_until_ready();
    app.send(b"\x03");
    std::thread::sleep(Duration::from_millis(300));
    app.send(b"\x03");
    assert!(app.wait_for_exit().is_some(), "the app did not end after Ctrl+C:\n{}", app.text());
}

/// **The terminal is handed back.** Leaving the alternate screen, mouse tracking or line wrapping
/// switched on leaves the shell behind it unusable in a way that outlives the process — and it is
/// invisible from inside the app, so only a test that watches the bytes on the way out can see it.
///
/// **Unix only, because only here is it on the wire.** On Windows crossterm drives the console
/// through its API rather than escape sequences, so there is nothing passing through the pty to
/// check; `ctrl_c_twice_ends_the_app` covers what can be seen there.
#[cfg(unix)]
#[test]
fn quitting_gives_the_terminal_back() {
    let _turn = one_at_a_time();
    let mut app = Session::start();
    app.wait_until_ready();

    // Ctrl+C twice: the first arms the quit, the second takes it.
    app.send(b"\x03");
    std::thread::sleep(Duration::from_millis(300));
    app.send(b"\x03");

    // **Waiting on the restore is what reads it.** Waiting only for the process to end leaves the
    // bytes it wrote on its way out sitting unread in the channel, and the check then fails
    // against an app that did everything right.
    for (what, seq) in [
        ("the alternate screen", "\x1b[?1049l"),
        ("mouse tracking", "\x1b[?1000l"),
        ("line wrapping", "\x1b[?7h"),
    ] {
        assert!(app.wait_for(seq), "{what} was left switched on ({seq:?}):\n{}", app.text());
    }

    assert!(app.wait_for_exit().is_some(), "the app did not end after Ctrl+C:\n{}", app.text());
}

/// **The app must not stop waiting for an answer this terminal will never give.**
///
/// Asking the terminal something and blocking on the reply is what `Terminal::clear()`'s cursor
/// query used to do, and on a terminal that does not answer it took the whole app with it. This
/// pty answers nothing, which is exactly that terminal — so a screen that draws and then keeps
/// taking keys is the proof it does not happen any more.
///
/// **On Windows this measures less**, and says so rather than pretending otherwise: the harness
/// answers ConPTY's own cursor query there (see `start`), so the terminal is not silent. It is
/// still not silence the app depends on — console work on Windows goes through Win32 and crossterm
/// writes that query only from its unix path — so what this guards is the unix build.
#[test]
fn a_terminal_that_answers_nothing_does_not_stall_the_app() {
    let _turn = one_at_a_time();
    let mut app = Session::start();
    app.wait_until_ready();

    // Ctrl+L asks for a full repaint — the path that used to go through the cursor query.
    app.send(b"\x0c");
    assert!(
        app.type_and_see("still awake"),
        "the app stopped answering after a repaint:\n{}",
        app.text()
    );
}

/// **An update keeps the terminal it was started in.**
///
/// This is the whole of the Windows report (2026-08-17): in `cmd`, updating made the app vanish.
/// The old shape asked for the update from inside the screen, wrote a script out, started it
/// detached and exited; the script waited for that exit, installed, and started the new binary with
/// `Start-Process` — which gives a console program **a console of its own**. From the window
/// somebody was watching, the app had quit, and it had.
///
/// So what has to be checked is not that an installer runs. It is that after one runs, the app is
/// drawing **into the same pseudo-terminal it started in** — which is what the harness is holding.
///
/// **The installer is a stand-in, and nothing is replaced.** `$ZYRIS_CODE_UPDATE_TAG` says a newer
/// release exists without one existing and `$ZYRIS_CODE_UPDATE_SCRIPT` puts a script of ours where
/// the release's would be. Everything after that point — running it, and handing this console to
/// the new version — is the real path, and the version it hands over to is this same binary.
#[test]
fn an_update_keeps_the_terminal_it_was_started_in() {
    let _turn = one_at_a_time();

    let dir = std::env::temp_dir().join(format!("zyris-update-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let marker = dir.join("ran");
    let _ = std::fs::remove_file(&marker);
    let script = dir.join(if cfg!(windows) { "stand-in.ps1" } else { "stand-in.sh" });

    // It is handed the same arguments as the real installer, so it has to accept them — a script
    // that refused them would fail before writing anything and the test would read as the app's
    // fault. On Windows that means declaring the parameters; on unix it means ignoring them.
    let text = if cfg!(windows) {
        format!(
            "param([string] $Version, [string] $Dir, [switch] $NoModifyPath)\r\n\
             Set-Content -Path '{}' -Value $Version\r\n",
            marker.display()
        )
    } else {
        format!("#!/bin/sh\nprintf '%s' \"$2\" > '{}'\n", marker.display())
    };
    std::fs::write(&script, text).expect("could not write the stand-in installer");

    let mut app = Session::start_with(&[
        ("ZYRIS_CODE_UPDATE_TAG", "v99.0.0".into()),
        ("ZYRIS_CODE_UPDATE_SCRIPT", script.display().to_string()),
        // So what it says can be read for what it is, whatever the machine's language.
        ("ZYRIS_CODE_LANG", "en".into()),
    ]);

    // **Said on the terminal, before the screen exists.** Somebody who typed a command and got
    // seconds of nothing reaches for Ctrl+C; this is the line that stops that.
    assert!(app.wait_for("v99.0.0"), "the update was never mentioned:\n{}", app.text());

    // **Judged by what happened on disk, not by what was printed.** A line saying it is installing
    // costs nothing to print and proves nothing.
    let ran = app.wait_until(|_| marker.exists());
    assert!(ran, "the installer never ran:\n{}", app.text());
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap_or_default().trim(),
        "v99.0.0",
        "the installer was not told which release to install"
    );

    // **And here is the report.** The screen is up again, on this same pseudo-terminal, after the
    // handover — no new console, nothing detached, nothing left for a shell prompt to come back to.
    app.wait_until_ready();
    assert!(
        app.type_and_see("still here"),
        "the app came back somewhere else — this terminal is not taking keys:\n{}",
        app.text()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **Cutting and putting back, on a real console.**
///
/// `Ctrl+U` takes the draft, `Ctrl+Y` puts it back — the one pair where losing the second key costs
/// work, because this input box has no undo and what `Ctrl+U` took is nowhere else. It was reported
/// not to work on Windows (2026-08-17), and nothing here could say whether the key was even
/// arriving: the handling is shared with `Ctrl+U`, which does work, so the suspicion was that the
/// console never delivers it. A pseudo-terminal is where that question has an answer — and on
/// Windows this one is a ConPTY, the same path a console takes.
///
/// **Judged by the two words ending up joined**, never by one of them going away. What is read here
/// is everything the app has ever written, so text that has been erased is still in it — an
/// assertion that something is absent would pass on a screen that still shows it. `betaalpha` can
/// only be there if the second key put back what the first one took.
///
/// The bytes are the control codes themselves (`0x15`, `0x19`), because that is what a terminal
/// sends and what a console turns back into a key event.
#[test]
fn what_ctrl_u_takes_ctrl_y_puts_back() {
    let _turn = one_at_a_time();
    let mut app = Session::start();
    app.wait_until_ready();

    assert!(app.type_and_see("alpha"), "the draft never arrived:\n{}", app.text());
    app.send(b"\x15"); // Ctrl+U — with the cursor at the end, the whole draft.
    assert!(app.type_and_see("beta"), "nothing could be typed after the cut:\n{}", app.text());
    app.send(b"\x19"); // Ctrl+Y — `alpha` comes back, after `beta`.

    // Only changed cells go out, so the two words are unlikely to be sent as one run. Ctrl+L asks
    // for the whole screen again, which is what `type_and_see` does for the same reason.
    let mut back = false;
    for _ in 0..20 {
        if app.wait_until_by(Duration::from_secs(1), |s| s.plain().contains("betaalpha")) {
            back = true;
            break;
        }
        app.send(b"\x0c");
    }
    assert!(back, "Ctrl+Y did not put back what Ctrl+U took:\n{}", app.text());
}
