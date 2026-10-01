//! Speaks to the shell before the screen appears.
//!
//! **The TUI starts inside `on_connect`.** So while the server can't be reached there's no screen at all, and
//! saying nothing then leaves the user staring at a frozen cursor — that actually happened when the server died.
//! Logs go to a file, so there's no knowing what's in them.
//!
//! **Once the screen is up, not a single character goes out.** If something cuts into where ratatui drew, that cell is
//! considered "unchanged" and never redrawn. A disconnect after connecting is announced by the screen
//! (`activity.rs`'s "connecting…").

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};

use crate::tools::bridge::Bridge;

/// When it hasn't connected this long, speak for the first time. Long enough to not cut in between a key press and release.
const FIRST: u64 = 3;
/// After that, repeat only at this interval. Printing every second would be unreadable in its own way.
const REPEAT: u64 = 15;

/// What happened outside while connecting. The watcher reads it and carries it to the shell.
#[derive(Clone, Default)]
pub struct Notice(Arc<Inner>);

#[derive(Default)]
struct Inner {
    /// The last failure reason caught. A single line for a human to read.
    last: Mutex<Option<String>>,
    /// Whether it ever connected. The moment it connects, the watcher falls silent.
    connected: AtomicBool,
    /// Whether someone else is already speaking to the human. The enrollment-code box turns this on.
    hushed: AtomicBool,
}

impl Notice {
    pub fn new() -> Notice {
        Notice::default()
    }

    /// Connected. **From here on, nothing is printed.**
    pub fn connected(&self) {
        self.0.connected.store(true, Ordering::SeqCst);
    }

    /// The connection dropped, so the dial is starting over. **The watcher wakes up again** and
    /// the reasons collected so far are forgotten, so what it reports is about this outage.
    pub fn dropped(&self) {
        *self.0.last.lock().unwrap() = None;
        self.0.connected.store(false, Ordering::SeqCst);
    }

    /// A layer that also routes failures flowing to the log through here.
    ///
    /// **It doesn't select by message text** — if the upstream changes the wording, it would silently not be caught.
    /// It selects only by target and level.
    pub fn layer(&self) -> Watch {
        Watch(self.clone())
    }

    fn remember(&self, why: String) {
        *self.0.last.lock().unwrap() = Some(why);
    }

    /// A spot that ends when there's nothing left to try. **It doesn't die quietly.**
    ///
    /// It prints the last reason along with it. The final wording the upstream produces doesn't always point at the real cause —
    /// when the server actually died, "check this machine's clock" came in, yet
    /// the clock was fine; the real cause was the earlier "couldn't send refresh".
    pub fn fatal(&self, why: &str) {
        let lang = crate::lang::current();
        red(&lang.connect_failed(why));
        if let Some(before) = self.0.last.lock().unwrap().as_deref() {
            if before != why {
                plain(&lang.previous_error(before));
            }
        }
        plain(&lang.log_location(&log_path().to_string_lossy(), std::process::id()));
    }

    /// A spot that ends things but is **not an error**. Red is used sparingly — if everything is red, the real error
    /// gets buried.
    pub fn fatal_plain(&self, what: &str) {
        plain(&format!("\n{what}"));
    }

    /// Watches until connected, then tells the shell. Once connected, it ends quietly.
    ///
    /// **Waiting and failing are different.** On first launch the upstream prints an enrollment code and
    /// polls until the human approves in the browser — during that time the node obviously isn't connected. It used to
    /// shout "couldn't connect" in red every 15 seconds the whole time. Even before entering the code
    /// it was already calling it a failure, so the human thinks they did something wrong.
    ///
    /// The dividing line now is **whether a collected reason exists**. Without one, it's still waiting, and
    /// then it speaks **once** in a calm color. With one, that is a real failure.
    ///
    /// **Once the screen is up, it falls silent.** Cutting into the shell while the screen is up covers where ratatui drew, and
    /// that cell is treated as "unchanged" and never redrawn — the enrollment-code window is also something the screen
    /// tells (`enroll.rs`). The screen attaches not in `on_connect` but **the moment the app starts**, so
    /// this watcher is quiet from the very first enrollment (before connecting).
    pub fn watch(&self, bridge: Bridge) {
        let notice = self.clone();
        tokio::spawn(async move {
            let mut waited = 0u64;
            let mut said_waiting = false;
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                // **Connected, so there is nothing to say.** The watcher keeps running instead of
                // returning, because a connection that drops later dials again and can fail again
                // (see `dropped`).
                if notice.0.connected.load(Ordering::SeqCst) {
                    waited = 0;
                    said_waiting = false;
                    continue;
                }
                waited += 1;
                let why = notice.0.last.lock().unwrap().clone();
                let screen = bridge.has_screen();
                let Some(why) = why else {
                    // No error yet — waiting for approval.
                    if screen || notice.0.hushed.load(Ordering::SeqCst) {
                        continue;
                    }
                    if waited >= FIRST && !said_waiting {
                        said_waiting = true;
                        plain(&format!("\n{}", crate::lang::current().waiting_for_approval()));
                    }
                    continue;
                };
                if !speak_now(waited) {
                    continue;
                }
                if screen {
                    // **A dial that keeps failing is told to the screen**, as a frame: writing to
                    // the shell would cut into what ratatui drew. It lands on the activity line
                    // beside "Connecting...", with how long it has been going on.
                    bridge.frame(crate::app::Frame::Disconnected(
                        crate::lang::current().still_dialing(waited, &why),
                    ));
                } else {
                    red(&crate::lang::current().server_unreachable(waited, &why));
                }
            }
        });
    }
}

/// Once at the 3rd second, then every 15 seconds. The seconds in between are silent.
fn speak_now(waited: u64) -> bool {
    match waited.checked_sub(FIRST) {
        Some(0) => true,
        Some(since) => since.is_multiple_of(REPEAT),
        None => false,
    }
}

/// Where the log is written.
///
/// **One place, because two of them drifted.** This said `/tmp/zyris-code.log` outright while the
/// side that opens the file asked the platform — so on Windows the app pointed somebody at a path
/// that does not exist there, in the one message whose whole job is saying where to look (reported
/// 2026-08-18). `std::env::temp_dir` reads `%TEMP%` on Windows and `/tmp` here.
pub fn log_path() -> std::path::PathBuf {
    std::env::var("ZYRIS_CODE_LOG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("zyris-code.log"))
}

/// Past this size the log is emptied when the app starts, rather than grown for ever.
const LOG_KEEP_BYTES: u64 = 10 * 1024 * 1024;

/// The log file, opened for appending, **with every line marked by the process that wrote it.**
///
/// **Several windows share one log.** It used to be emptied on every start, so opening a second
/// window wiped the first one's record, and the two then wrote into the same file with nothing to
/// tell their lines apart. Appending keeps both; the `[pid]` prefix says whose each line is.
///
/// ponytail: the size check runs only at start, so a second window starting while the log is over
/// the limit empties it under the first; rotate by date if that ever loses something that matters.
pub struct LogFile {
    file: std::fs::File,
    prefix: String,
}

impl LogFile {
    pub fn open(path: &std::path::Path) -> std::io::Result<LogFile> {
        Self::open_keeping(path, LOG_KEEP_BYTES)
    }

    fn open_keeping(path: &std::path::Path, keep: u64) -> std::io::Result<LogFile> {
        if std::fs::metadata(path).is_ok_and(|m| m.len() > keep) {
            std::fs::File::create(path)?;
        }
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        Ok(LogFile { file, prefix: format!("[{}] ", std::process::id()) })
    }
}

/// One formatted event, written as one append.
///
/// `tracing-subscriber`'s formatter hands each event over in a single `write_all`, so prefixing
/// every `write` marks every line; building the whole line first keeps two processes' lines from
/// interleaving inside one another.
pub struct LogLine<'a>(&'a LogFile);

impl Write for LogLine<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut line = Vec::with_capacity(self.0.prefix.len() + buf.len());
        line.extend_from_slice(self.0.prefix.as_bytes());
        line.extend_from_slice(buf);
        (&self.0.file).write_all(&line)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        (&self.0.file).flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogFile {
    type Writer = LogLine<'a>;

    fn make_writer(&'a self) -> LogLine<'a> {
        LogLine(self)
    }
}

/// A single red line. **No color unless it's a terminal** — for something receiving through a pipe,
/// the escapes are just garbage characters. `NO_COLOR` is respected too.
fn red(text: &str) {
    let mut err = std::io::stderr();
    let _ =
        if colours() { writeln!(err, "\x1b[1;31m{text}\x1b[0m") } else { writeln!(err, "{text}") };
    let _ = err.flush();
}

fn plain(text: &str) {
    let mut err = std::io::stderr();
    let _ = writeln!(err, "{text}");
    let _ = err.flush();
}

fn colours() -> bool {
    colours_with(std::env::var_os("NO_COLOR").as_deref(), std::io::stderr().is_terminal())
}

/// **The decision itself**, over the two things it reads.
///
/// **Split out because the ambient answer cannot be tested.** The test below used to assert
/// `!colours()` on the reasoning that "a test isn't a terminal" — but `is_terminal()` asks the real
/// fd 2, and libtest's capture only redirects the printing macros. So a plain `cargo test` typed
/// into a terminal leaves stderr a terminal, and the suite went red **on a program that was
/// behaving correctly**; run through a pipe (CI, a tool, `cargo test > log`) the same suite was
/// green, which is how a test like that survives. Here the rule can be driven directly.
fn colours_with(no_color: Option<&std::ffi::OsStr>, stderr_is_a_terminal: bool) -> bool {
    no_color.is_none() && stderr_is_a_terminal
}

/// A tracing layer that collects failure reasons.
pub struct Watch(Notice);

impl<S: tracing::Subscriber> Layer<S> for Watch {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // **Only what the `zyris` crate emits** is watched. Connecting isn't done only by `runtime` —
        // `enroll` does it too — the real cause ("couldn't send refresh") actually came from `enroll::http`,
        // and watching only `runtime` missed it.
        //
        // Our crate's target is `zyris_code::…` (underscore), so it doesn't match here. Piping those warnings
        // to the shell would say "connection failed" and then show an unrelated situation.
        if !meta.target().starts_with("zyris::") {
            return;
        }
        if *meta.level() > tracing::Level::WARN {
            return;
        }
        let mut grab = Grab::default();
        event.record(&mut grab);
        if let Some(why) = grab.take() {
            self.0.remember(why);
        }
    }
}

/// Pulls out the value of `error = %e`. If absent, the message will do.
#[derive(Default)]
struct Grab {
    error: Option<String>,
    message: Option<String>,
}

impl Grab {
    fn take(self) -> Option<String> {
        self.error.or(self.message)
    }
}

impl Visit for Grab {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let text = format!("{value:?}");
        match field.name() {
            "error" => self.error = Some(text.trim_matches('"').to_string()),
            "message" => self.message = Some(text),
            _ => {}
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "error" => self.error = Some(value.to_string()),
            "message" => self.message = Some(value.to_string()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The path it names has to be the path it writes.** These were worked out in two places and
    /// one of them said `/tmp/zyris-code.log` outright, so on Windows the app pointed somebody at a
    /// directory that does not exist there — in the one line whose whole job is saying where to
    /// look (reported 2026-08-18).
    #[test]
    fn the_log_it_names_is_the_log_it_writes() {
        let named = log_path();
        assert!(named.ends_with("zyris-code.log"), "{named:?}");
        assert_eq!(
            named.parent(),
            Some(std::env::temp_dir().as_path()),
            "the log was named somewhere other than where this platform puts temporary files",
        );
        // On Windows that is `%TEMP%`, never `/tmp` — which is what the old literal said.
        if cfg!(windows) {
            assert!(!named.starts_with("/tmp"), "a unix path was named on Windows: {named:?}");
        }
    }

    /// The watcher speaks at 3 s and then every 15 s while a dial keeps failing.
    #[test]
    fn a_failing_dial_is_repeated_not_dropped() {
        let at: Vec<u64> = (0..=50).filter(|w| speak_now(*w)).collect();
        assert_eq!(at, [3, 18, 33, 48]);
    }

    /// A drop re-arms the watcher and forgets the old reasons.
    #[test]
    fn a_drop_rearms_the_watcher() {
        let n = Notice::new();
        n.remember("old".into());
        n.connected();
        n.dropped();
        assert!(!n.0.connected.load(Ordering::SeqCst));
        assert!(n.0.last.lock().unwrap().is_none());
    }

    /// Once connected, the watcher falls silent. Otherwise text gets printed over the screen.
    #[test]
    fn connecting_silences_the_watcher() {
        let n = Notice::new();
        assert!(!n.0.connected.load(Ordering::SeqCst));
        n.connected();
        assert!(n.0.connected.load(Ordering::SeqCst));
    }

    /// It must hold the failure reason so the shell can say why.
    #[test]
    fn the_reason_is_kept_for_the_message() {
        let n = Notice::new();
        n.remember("Connection reset by peer".into());
        assert_eq!(n.0.last.lock().unwrap().as_deref(), Some("Connection reset by peer"));
    }

    /// If the `error` field exists, it wins — what a human reads is the reason, not the log wording.
    #[test]
    fn the_error_field_wins_over_the_log_message() {
        let grab = Grab {
            error: Some("Connection reset by peer".into()),
            message: Some("connect failed".into()),
        };
        assert_eq!(grab.take().as_deref(), Some("Connection reset by peer"));
    }

    /// Without a reason, at least show the message. Better than saying nothing.
    #[test]
    fn without_a_reason_the_message_is_used() {
        let grab = Grab { error: None, message: Some("connect failed".into()) };
        assert_eq!(grab.take().as_deref(), Some("connect failed"));
    }

    /// On exit it must also say the previous reason — the final wording is sometimes not the real cause.
    #[test]
    fn the_last_transient_reason_is_kept_for_the_fatal_message() {
        let n = Notice::new();
        n.remember("refresh를 보내지 못했다".into());
        // It prints to stderr, so here we only check what it holds. The wording is locked by the test above.
        assert_eq!(n.0.last.lock().unwrap().as_deref(), Some("refresh를 보내지 못했다"));
    }

    /// **Without an error, it isn't a failure.** On first launch it isn't connected until the enrollment code is entered, and
    /// calling that time a failure makes the human think they did something wrong.
    #[test]
    fn waiting_is_not_the_same_as_failing() {
        let n = Notice::new();
        assert!(n.0.last.lock().unwrap().is_none(), "there is no error yet");
        // Only once a reason appears is it a failure.
        n.remember("Connection reset by peer".into());
        assert!(n.0.last.lock().unwrap().is_some());
    }

    /// **`NO_COLOR` is respected, and a pipe gets no escapes.** Driven directly rather than asked
    /// of the process: the ambient answer depends on where the suite was started from, which is
    /// how this test was red for anybody running `cargo test` in a terminal while the program was
    /// right (see `colours_with`).
    #[test]
    fn no_color_turns_the_escapes_off() {
        use std::ffi::OsStr;
        assert!(
            !colours_with(None, false),
            "colour must not go out to somewhere that isn't a terminal"
        );
        assert!(!colours_with(Some(OsStr::new("1")), true), "NO_COLOR was not respected");
        assert!(colours_with(None, true), "a terminal with no NO_COLOR should get colour");
    }

    /// **A second window adds to the log instead of emptying it**, and each line says whose it is.
    #[test]
    fn opening_the_log_again_keeps_what_is_there_and_marks_each_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        std::fs::write(&path, "an earlier window\n").unwrap();
        let log = LogFile::open(&path).unwrap();
        LogLine(&log).write_all(b"this window\n").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, format!("an earlier window\n[{}] this window\n", std::process::id()));
    }

    /// Past the limit it is emptied at start, so it does not grow for ever.
    #[test]
    fn a_log_past_the_limit_is_emptied_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        std::fs::write(&path, "x".repeat(100)).unwrap();
        LogFile::open_keeping(&path, 50).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        std::fs::write(&path, "x".repeat(10)).unwrap();
        LogFile::open_keeping(&path, 50).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().len(), 10);
    }

    /// **The way to read the log names this run.** Pasted to an agent, the whole shared log would
    /// cost it every other window's lines; the command it is handed filters to this process.
    #[test]
    fn the_log_line_says_how_to_read_only_this_run() {
        let line = crate::lang::Lang::En.log_location("/tmp/zyris-code.log", 4242);
        assert!(line.contains("[4242] "), "{line}");
        assert!(line.contains("/tmp/zyris-code.log"), "{line}");
        // The prefix the filter looks for is the one `LogFile` writes.
        let dir = tempfile::tempdir().unwrap();
        let log = LogFile::open(&dir.path().join("log")).unwrap();
        assert_eq!(log.prefix, format!("[{}] ", std::process::id()));
    }
}
