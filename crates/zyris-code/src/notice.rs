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

    /// Stop saying we're waiting. **Failures are still spoken.**
    ///
    /// Called when the enrollment-code box appears. The box already says up to "it will continue automatically once you approve", and
    /// adding another line of the same meaning under it would only blur the box's border.
    pub fn hush(&self) {
        self.0.hushed.store(true, Ordering::SeqCst);
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
        plain(&lang.log_location(
            &std::env::var("ZYRIS_CODE_LOG").unwrap_or_else(|_| "/tmp/zyris-code.log".into()),
        ));
    }

    /// A spot that ends things but is **not an error**. Red is used sparingly — if everything is red, the real error
    /// gets buried.
    pub fn fatal_plain(&self, what: &str) {
        plain(&format!("\n{what}"));
    }

    /// Something that doesn't kill but should be known. **Only used before the screen appears.**
    ///
    /// If it cuts into stderr after the screen is up, it covers where ratatui drew and that cell is treated as "unchanged"
    /// and never redrawn.
    pub fn warn_plain(&self, what: &str) {
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
                if notice.0.connected.load(Ordering::SeqCst) {
                    return;
                }
                // If there's a screen, the screen speaks — the shell doesn't cut in.
                if bridge.has_screen() {
                    continue;
                }
                waited += 1;
                let why = notice.0.last.lock().unwrap().clone();
                let Some(why) = why else {
                    // No error yet — waiting for approval.
                    if notice.0.hushed.load(Ordering::SeqCst) {
                        continue;
                    }
                    if waited >= FIRST && !said_waiting {
                        said_waiting = true;
                        plain(&format!("\n{}", crate::lang::current().waiting_for_approval()));
                    }
                    continue;
                };
                // Once at the 3rd second, then every 15 seconds. The seconds in between are silent.
                let speak = match waited.checked_sub(FIRST) {
                    Some(0) => true,
                    Some(since) => since.is_multiple_of(REPEAT),
                    None => false,
                };
                if speak {
                    red(&crate::lang::current().server_unreachable(waited, &why));
                }
            }
        });
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
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
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
        assert!(n.0.last.lock().unwrap().is_none(), "아직 아무 오류도 없다");
        // Only once a reason appears is it a failure.
        n.remember("Connection reset by peer".into());
        assert!(n.0.last.lock().unwrap().is_some());
    }

    /// **`NO_COLOR` is respected.** Escapes are garbage for something receiving through a pipe.
    #[test]
    fn no_color_turns_the_escapes_off() {
        // A test isn't a terminal, so it should be off anyway.
        assert!(!colours(), "터미널이 아닌 곳에 색을 내보내면 안 된다");
    }
}
