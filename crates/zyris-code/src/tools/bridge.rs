//! Connects the tool-running side and the screen side.
//!
//! The two run on different tasks — tools on the zyris `Runner`, the screen in `app::run`.
//! The screen holds what's needed to decide (mode, the `/config` settings) and everything passing
//! between them is gathered here in one place.
//!
//! **The constraint that `apply` must stay pure shaped this file.** Moving screen state here
//! is done by the I/O site (`run_inner`); the only thing going from here to the screen is `Action`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::app::{Action, AppMsg, Frame};
use crate::config::Config;
use crate::mode::Mode;
use crate::tools::gate::{decide, Call, Decision};

#[derive(Clone, Default)]
pub struct Bridge(Arc<Inner>);

#[derive(Default)]
struct Inner {
    /// The current mode. When the screen changes it, the I/O site carries it over.
    mode: Mutex<Mode>,
    /// The current settings. Same — the screen is the only place they change.
    config: Mutex<Config>,
    /// The working directory. **It's the yardstick for whether something leaves it.**
    root: Mutex<PathBuf>,
    /// What goes to the screen. Empty before the screen is up.
    ///
    /// `None` in `AppMsg` means it came from outside the screen (tools, the bridge) — only the turn stream
    /// carries its own session id as a tag (`app::spawn_stream`).
    to_app: Mutex<Option<mpsc::UnboundedSender<AppMsg>>>,
    /// A signal that wakes when the screen attaches. Used when `main` waits for the screen to attach
    /// before the first enrollment so the enrollment code can reach the screen.
    screen_ready: tokio::sync::Notify,
    /// The skill list to load when creating a session. `tools::announce` decides it and the screen picks it up.
    ///
    /// **Fixed for the session's lifetime** — attacca's `preamble` is set once when the session is created
    /// and can't be changed later. So MCP tools that attach later aren't loaded here.
    preamble: Mutex<Option<String>>,
    /// MCP servers that connected or failed to. `/mcp` reads it.
    ///
    /// **Failures are kept too.** Right now a `Frame::Notice` shows for 6 seconds and disappears, so there's no way to ask later
    /// "why isn't that server here".
    mcp: Mutex<Vec<(String, Result<usize, String>)>>,
    /// What is running in the background. **The tool, the screen, and `/jobs` must see the same
    /// thing** — with two copies, fixing one silently diverges from the other. Same wiring as
    /// `undo` right below.
    jobs: Mutex<Option<crate::tools::jobs::Jobs>>,
    /// The undo log the edit tool uses. `/undo` must use the **same handle** so there's a single lock.
    undo: Mutex<Option<crate::undo::Undo>>,
    /// A handle for dropping credentials and getting re-approved.
    ///
    /// Noticing insufficient permissions is the **screen side after attaching** (the `me()` scope); the credentials to drop
    /// are held by `main.rs` at startup. The place connecting the two is here.
    reauth: Mutex<Option<crate::enroll::Reauth>>,
    next_id: AtomicU64,
}

impl Bridge {
    pub fn new() -> Bridge {
        Bridge::default()
    }

    /// Plugs in its handle once the screen is up. Frames arriving before that have nowhere
    /// to go and are dropped.
    pub fn attach(&self, to_app: mpsc::UnboundedSender<AppMsg>) {
        *self.0.to_app.lock().unwrap() = Some(to_app);
        // The first enrollment can start before the screen is up (`main` launches the app before running the runner).
        // Wake the waiter so the enrollment code can reach the screen.
        self.0.screen_ready.notify_waiters();
    }

    /// Waits for the screen to attach. **If it's already attached, returns immediately.**
    ///
    /// `main` calls this before running the runner — an ordering guarantee so the enrollment window (`Frame::Enroll`)
    /// can reach the screen from the very first connection.
    pub async fn wait_screen(&self) {
        if self.has_screen() {
            return;
        }
        let notified = self.0.screen_ready.notified();
        // Create the waiter, then check again — it may have attached in between.
        if self.has_screen() {
            return;
        }
        notified.await;
    }

    /// Copies over the screen-side decision material (mode + settings). Called whenever the
    /// I/O site touches state.
    pub fn sync(&self, mode: Mode, config: &Config) {
        *self.0.mode.lock().unwrap() = mode;
        *self.0.config.lock().unwrap() = *config;
    }

    /// Records the working directory. `tools::announce` calls it once.
    pub fn set_root(&self, root: PathBuf) {
        *self.0.root.lock().unwrap() = root;
    }

    pub fn root(&self) -> PathBuf {
        self.0.root.lock().unwrap().clone()
    }

    /// Records the background-job registry. `tools::announce` calls it once.
    pub fn set_jobs(&self, jobs: crate::tools::jobs::Jobs) {
        *self.0.jobs.lock().unwrap() = Some(jobs);
    }

    /// `/jobs` and the exit path use it. `None` before announce.
    pub fn jobs(&self) -> Option<crate::tools::jobs::Jobs> {
        self.0.jobs.lock().unwrap().clone()
    }

    pub fn decide(&self, call: &Call) -> Decision {
        let mode = *self.0.mode.lock().unwrap();
        let config = *self.0.config.lock().unwrap();
        decide(mode, &config, call)
    }

    /// Whether the screen is attached.
    ///
    /// Where to draw the enrollment code forks here: with a screen, the screen overlays it;
    /// without one (first run), it's printed as a box on stderr. Doing both would let frames cover the text.
    pub fn has_screen(&self) -> bool {
        self.0.to_app.lock().unwrap().is_some()
    }

    /// Sends a frame and reports **whether it reached**. If it didn't, the caller must speak for itself.
    pub fn reaches_screen(&self, frame: Frame) -> bool {
        self.send(Action::Frame(frame)).is_some()
    }

    /// Records one MCP server's outcome. A tool count means success; a reason means failure.
    pub fn note_mcp(&self, slug: &str, outcome: Result<usize, String>) {
        let mut all = self.0.mcp.lock().unwrap();
        match all.iter_mut().find(|(s, _)| s == slug) {
            Some(slot) => slot.1 = outcome,
            None => all.push((slug.to_string(), outcome)),
        }
    }

    /// A number for showing something on screen and clearing it later.
    pub fn next_id(&self) -> u64 {
        self.0.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// What `/mcp` will show. In the order recorded.
    pub fn mcp_report(&self) -> Vec<(String, Result<usize, String>)> {
        self.0.mcp.lock().unwrap().clone()
    }

    pub fn set_preamble(&self, preamble: Option<String>) {
        *self.0.preamble.lock().unwrap() = preamble;
    }

    pub fn preamble(&self) -> Option<String> {
        self.0.preamble.lock().unwrap().clone()
    }

    /// Also tells the screen side about the undo log the edit tool uses.
    ///
    /// **It must be the same handle.** If each side makes its own with `Undo::for_dir`, the locks play separately and
    /// pressing `/undo` while the agent is editing writes two log lines over each other.
    pub fn set_undo(&self, undo: crate::undo::Undo) {
        *self.0.undo.lock().unwrap() = Some(undo);
    }

    pub fn undo(&self) -> Option<crate::undo::Undo> {
        self.0.undo.lock().unwrap().clone()
    }

    /// Mounts a handle that can drop credentials. **Not present where the human gave the token directly.**
    pub fn set_reauth(&self, reauth: crate::enroll::Reauth) {
        *self.0.reauth.lock().unwrap() = Some(reauth);
    }

    pub fn reauth(&self) -> Option<crate::enroll::Reauth> {
        self.0.reauth.lock().unwrap().clone()
    }

    pub fn frame(&self, frame: Frame) {
        let _ = self.send(Action::Frame(frame));
    }

    fn send(&self, action: Action) -> Option<()> {
        // What leaves the bridge is screen-external — sent without a session tag.
        self.0.to_app.lock().unwrap().as_ref()?.send((None, action)).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::gate::Call;

    fn edit_call() -> Call {
        Call::new("code_edit", "edit", "/tmp/a".into())
    }

    /// The screen decides the mode and the gate sees it. **If it isn't carried over, the screen is in plan mode
    /// while tools still run.**
    #[test]
    fn the_gate_sees_the_mode_the_screen_is_in() {
        let b = Bridge::new();
        assert_eq!(b.decide(&edit_call()), Decision::Run, "the default just runs");
        b.sync(Mode::Plan, &Config::default());
        assert!(matches!(b.decide(&edit_call()), Decision::Refuse(_)));
        b.sync(Mode::Job, &Config::default());
        assert_eq!(b.decide(&edit_call()), Decision::Run);
    }

    /// The directory policy travels the same way — **if the screen's `/config dir allow` never
    /// reached the bridge, the tools keep refusing.**
    #[test]
    fn the_gate_sees_the_directory_policy_the_screen_sets() {
        let b = Bridge::new();
        let leaving = edit_call().leaving(Some(PathBuf::from("/etc/passwd")));
        assert!(matches!(b.decide(&leaving), Decision::Refuse(_)), "the default is deny");
        b.sync(
            Mode::Normal,
            &crate::config::Config {
                dir_access: crate::config::DirAccess::Allow,
                ..Config::default()
            },
        );
        assert_eq!(b.decide(&leaving), Decision::Run, "allow must run it");
    }

    /// Without a screen yet, a frame has nowhere to go. **Dropped, not fatal.**
    #[test]
    fn a_frame_sent_before_the_screen_exists_is_dropped_quietly() {
        Bridge::new().frame(Frame::Notice("아무도 못 본다".into()));
    }

    /// Once the screen is attached, frames go there.
    #[test]
    fn a_frame_reaches_the_screen_once_it_is_attached() {
        let b = Bridge::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        b.attach(tx);
        b.frame(Frame::ShellClosed { id: "p1".into() });
        assert!(rx.try_recv().is_ok(), "it must reach the screen");
    }

    /// Ids must not overlap — you'd confuse what to clear.
    #[test]
    fn every_id_is_new() {
        let b = Bridge::new();
        assert_ne!(b.next_id(), b.next_id());
    }

    /// **A failed MCP server must also remain.** The status line disappears after 6 seconds, leaving nothing to ask later.
    #[test]
    fn a_failed_mcp_server_is_remembered_with_its_reason() {
        let b = Bridge::new();
        b.note_mcp("github", Ok(12));
        b.note_mcp("깨진것", Err("npx를 찾지 못했습니다".into()));
        let report = b.mcp_report();
        assert_eq!(report.len(), 2);
        assert_eq!(report[1].1.as_ref().unwrap_err(), "npx를 찾지 못했습니다");
    }

    /// Recording the same server twice overwrites it — it must not appear twice in the list.
    #[test]
    fn noting_the_same_server_twice_replaces_it() {
        let b = Bridge::new();
        b.note_mcp("github", Err("아직".into()));
        b.note_mcp("github", Ok(3));
        assert_eq!(b.mcp_report(), vec![("github".to_string(), Ok(3))]);
    }
}
