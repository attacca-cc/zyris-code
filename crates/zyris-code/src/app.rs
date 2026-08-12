//! App state and key handling.
//!
//! `on_key` and `apply` are pure — that property is what lets the whole key binding
//! surface be tested like a table. I/O lives in exactly one place, `run()` (Task 10).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use zyris_attacca::ZDeltaKind;

use crate::event::{Entry, EntryKind};
use crate::input::Input;
use crate::mode::Mode;
use crate::rows::Folds;
use crate::scroll::Scroll;
use crate::timeline::Timeline;

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// An `entry` of `None` is an event we do not render. The cursor still advances.
    Event {
        cursor: i64,
        entry: Option<Entry>,
        /// What this event did to the session's todo list, if anything. It rides along with the
        /// entry rather than arriving on its own so that **one place feeds the list** — history
        /// replay goes back through here, so it is fed exactly like the live stream.
        todo: Option<crate::todos::Change>,
    },
    Delta {
        kind: ZDeltaKind,
        text: String,
    },
    Status {
        running: bool,
    },
    /// The agent opened a shell. **Not saying so leaves a ghost shell running.**
    ShellOpened {
        id: String,
        name: String,
    },
    ShellClosed {
        id: String,
    },
    /// A command started running. **`exec` only reports once, on completion** — saying
    /// nothing in the meantime leaves the user waiting blind for up to 55 seconds.
    ExecStart {
        id: u64,
        command: String,
    },
    ExecDone {
        id: u64,
    },
    /// A background job started. **If it is invisible the user quits the app unaware and
    /// the build dies with it** — what cannot be seen is as dangerous as what is not there.
    JobStart {
        id: String,
        label: String,
    },
    /// It finished. `ok` is whether the exit code was 0.
    JobEnded {
        id: String,
        ok: bool,
        secs: u64,
    },
    /// **The result of background polling.** Usage and title need the network, and having
    /// the draw loop wait on that traps the loop on a dead connection — so polling runs in
    /// a task outside the loop and only the result comes back in as a frame. It carries the
    /// session id as a tag so stale results from before a switch get dropped
    /// (`frame_is_current`).
    Poll {
        usage: Option<crate::usage::Usage>,
        title: Option<String>,
    },
    /// A project or thread list that finished loading — the first fill and every later
    /// refresh both arrive this way.
    ///
    /// **Every list is fetched off the loop.** Awaiting one here holds keys and drawing for as
    /// long as the server takes, and on a big account that is seconds of a frozen window.
    /// The map rides along only when this frame knows better than the state does; `None`
    /// leaves it alone, so a project list cannot wipe the thread cache.
    Picker {
        picker: crate::picker::Picker,
        thread_was_running: Option<std::collections::HashMap<String, bool>>,
    },
    /// One thread's outcome dot, as it lands. **The dots stream in one by one** — deriving
    /// each from that thread's history is a request apiece, and waiting for all of them
    /// before showing anything is what made opening a busy project look like a hang.
    ThreadStatus {
        id: String,
        status: crate::picker::ThreadStatus,
    },
    /// A list could not be fetched. The list closes and the reason is said once.
    PickerFailed(String),
    /// A session's history, replayed into the screen. Sent by the task that fetched it, so
    /// switching threads never blocks the loop. Tagged with the session id, so a switch made
    /// while an older one was still loading drops the loser (`frame_is_current`).
    History {
        /// `(cursor, what to draw, what it did to the todo list)` — the three `Frame::Event`
        /// carries, converted off the loop and replayed back through it.
        entries: Vec<(i64, Option<crate::event::Entry>, Option<crate::todos::Change>)>,
    },
    /// **What git says about the working directory.** Same reason as `Poll`: reading it needs
    /// a process, and awaiting that on the draw loop would stall keys and drawing. The
    /// background arm sends the answer in and the strip above the input picks it up. `None`
    /// means there is nothing to say — no repository, no git, or it timed out.
    Git(Option<crate::repo::Repo>),
    /// **The socket dropped.** The zyris `Runner` reconnects on its own, but meanwhile the
    /// screen looks like nothing happened — silent failure is the worst kind. Carries the
    /// reason verbatim.
    Disconnected(String),
    /// Tells the user once about something that happened off-screen (an MCP server that did
    /// not come up, authentication dropping and re-enrollment starting, and the like).
    /// Fades on its own after `STATUS_WINDOW`.
    Notice(String),
    /// An enrollment code was issued. **From this moment the screen owns showing the code** —
    /// the upstream stdout box goes quiet (`enroll::ScreenEnroll`).
    Enroll(EnrollView),
    /// The code lapsed (`Lapsed`) or was denied in the browser (`Denied`). The window stays;
    /// only what it says changes.
    EnrollPhase(EnrollPhase),
    /// Approved, and the credential was stored. Close the window.
    EnrollDone,
}

/// What to draw in the enrollment code window.
#[derive(Debug, Clone, PartialEq)]
pub struct EnrollView {
    pub code: String,
    pub uri: String,
    /// When the code lapses. The drawing side measures the time left from this.
    pub expires_at: std::time::Instant,
    pub phase: EnrollPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollPhase {
    /// Waiting for approval.
    Waiting,
    /// The code lapsed. A new one is being requested, and `Frame::Enroll` comes again
    /// when it arrives.
    Lapsed,
    /// Denied in the browser.
    Denied,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Insert(char),
    /// One paste, as a single chunk. Newlines inside go in verbatim.
    Paste(String),
    Backspace,
    Delete,
    DeleteWord,
    Left,
    Right,
    Home,
    End,
    Submit(String),
    Wheel(i32),
    /// Keyboard page scroll. Same sign as `Wheel` — positive goes up. The wheel is
    /// the only other way to scroll, and not every terminal delivers wheel events
    /// (mobile SSH, tmux without mouse) — history must be reachable by keyboard alone.
    Page(i32),
    ToggleFold,
    /// Unfold or fold the todo list under the activity line (Ctrl+T, or a click on that line).
    ToggleTodos,
    /// Where the mouse was pressed. Screen coordinates.
    Press(u16, u16),
    /// A Ctrl+click landed on a link. The URL is opened by the OS (I/O side).
    OpenLink(String),
    /// Where it moved to while held.
    DragTo(u16, u16),
    /// The moment it was released. If it never moved, count it as a click.
    Release,
    ClearSelection,
    /// Redraw the whole screen. Changes no state at all — only the I/O side handles it.
    Repaint,
    /// Operating the question screen.
    AskUp,
    AskDown,
    AskToggle,
    AskConfirm,
    AskCancel,
    /// Opening/operating the list.
    OpenPicker,
    PickUp,
    PickDown,
    /// Choose the current row. What that becomes is the I/O side's job.
    PickConfirm,
    /// Back. From the session level to the project list; at the project level, close.
    PickBack,
    /// The new-project form. Next field / previous field / create / close.
    FormNext,
    FormPrev,
    FormConfirm,
    FormCancel,
    /// Close the popup panel (Esc or Enter).
    PanelClose,
    /// Scroll the popup panel. Positive scrolls toward the top.
    PanelScroll(i32),
    /// Move focus between the panel body and its button (Tab). No-op without one.
    PanelFocus,
    /// Activate the focused panel button (Enter/Space while it is focused).
    /// `apply` turns it into the same path as the matching slash command.
    PanelActivate,
    /// Move the settings form's cursor between rows (↑↓). Positive is up.
    ConfigMove(i32),
    /// Walk the value under the settings form's cursor (←→). Positive is right.
    ConfigShift(i32),
    /// Take the settings form's draft and close it (Enter).
    ///
    /// **Esc is `PanelClose`, which throws the draft away** — that is the whole reason the
    /// form edits a draft instead of the live settings.
    ConfigSave,
    CycleMode,
    /// Wipe everything typed.
    ClearInput,
    /// Walk one step back through what was sent and bring it back.
    RecallOlder,
    /// Walk one step forward out of the recall. Past the bottom the input clears.
    RecallNewer,
    /// The answer to an approval window. **There is no limit on how long it waits** —
    /// the user may come back to it minutes later, so the window must not vanish on its own.
    /// One Ctrl+C. It only arms the quit; the second press is what actually quits.
    ArmQuit,
    Cancel,
    Quit,
    /// Close the enrollment code window. **Esc is the only key that closes it** — if any
    /// other key clears it away, the approval step goes by without the code ever being seen.
    EnrollClose,
    Frame(Frame),
}

pub struct State {
    pub timeline: Timeline,
    /// This session's todo list, rebuilt from its todo tool calls (`todos.rs`).
    pub todos: crate::todos::Todos,
    /// Whether the todo list is unfolded under the activity line. **Closed by default and only
    /// the person opens it** — the same rule the reasoning chips follow. Ctrl+T, or a click on
    /// the activity line.
    pub todos_open: bool,
    pub folds: Folds,
    pub input: Input,
    pub scroll: Scroll,
    pub running: bool,
    pub connected: bool,
    /// What to say and when it was said. **It fades on its own once time passes.**
    ///
    /// Holding on to a past circumstance ("Zyris로는 아직 만들 수 없습니다") means that
    /// spot can no longer say what is happening now. Keep it long enough to read, then clear.
    status: Option<(String, Instant, Severity)>,
    /// Permission mode. Not sent to the server yet — only shown and cycled.
    pub mode: Mode,
    /// Name of the currently attached agent, for the bottom bar.
    pub agent: String,
    /// When Ctrl+C was pressed once. Pressing again within this window quits.
    pub quit_armed_at: Option<Instant>,
    /// Set by `/quit`. The I/O side sees it and breaks out of the loop.
    ///
    /// `apply` is pure, so it cannot quit here — same trick as `submit_now`/`flush_queue`.
    pub quitting: bool,
    /// Have we already asked for this turn to stop?
    ///
    /// **Without this there is no way to close the window when the server hangs.** Ctrl+C
    /// goes to cancel while a turn runs, and if the cancel does not take so `running` stays
    /// true, every press just sends the same request again. Once asked, the next Ctrl+C is
    /// handed over to quitting.
    pub stopping: bool,
    /// Text selected in the transcript area. **It goes to the system clipboard on release.**
    pub selection: Option<String>,
    /// What was sent in this session. ↑ brings it back. Newest is last.
    pub sent: Vec<String>,
    /// Typed while a turn was running. **It has not gone to the server yet.** Sent in order
    /// once the turn ends.
    ///
    /// Sending mid-work can make the agent lose track of what it was doing, and above all
    /// **once sent it cannot be edited.** Held here, ↑ pulls it back out to fix.
    pub queued: Vec<String>,
    /// Should the queue be flushed now? The I/O side sees it and takes it.
    pub flush_queue: bool,
    /// Where the recall currently sits (index into `sent`). Editing the text releases it.
    recall: Option<usize>,
    /// The position to hand to `turn_events(after:)` on reconnect.
    pub last_cursor: Option<i64>,
    /// Line count and height of the transcript area as last drawn.
    ///
    /// `apply` has to stay pure, so it cannot know the viewport size by itself. The widget
    /// writes it here every frame and wheel handling reads that value. 0 means nothing has
    /// been drawn yet, so the wheel does nothing — before the first frame there is nothing
    /// to scroll either.
    pub view_total: usize,
    pub view_height: usize,
    /// Top-left screen coordinate of the transcript area. Used to map mouse coordinates to
    /// row/column.
    pub view_origin: (u16, u16),
    /// Index of the line currently at the top of the screen.
    pub view_top: usize,
    /// The transcript lines we built. **Only changed items are drawn again.**
    ///
    /// This used to be rebuilt in full every frame, which got linearly heavier as the
    /// conversation grew and blew the frame budget — that is what made glyphs overprint
    /// each other and the app lag.
    pub rows_cache: crate::rows::Cache,
    /// Row index → seq of the card that pressing that row folds and unfolds.
    pub view_cards: std::collections::HashMap<usize, i64>,
    /// The **effective** open state of each foldable node, as the last frame drew it.
    ///
    /// A click toggles from what is on screen, not from what is stored: a card with no fold state
    /// draws open while its stored `Fold` is `open: false`, so flipping the stored value there set
    /// `open: true` and the card did not move.
    pub view_open: std::collections::HashMap<i64, bool>,
    /// The links on the visible transcript lines, in screen coordinates' line order.
    /// `transcript::draw` fills it from the rows cache; `widgets::draw` wraps those cells
    /// in OSC 8 so the terminal makes them Ctrl+clickable.
    pub view_links: Vec<Vec<crate::markdown::Link>>,
    /// Links drawn by whatever is laid **over** the conversation — the enrolment window and any
    /// other overlay with a URL in it.
    ///
    /// **In absolute screen cells**, unlike `view_links`, because an overlay is centred on the
    /// screen rather than anchored to the transcript. It is rebuilt every frame by the drawing
    /// side, the same way `view_total` and `activity_row` are, because `apply` is pure and cannot
    /// know where anything landed.
    pub screen_links: Vec<ScreenLink>,
    /// The selected range, in **screen** coordinates. **It survives releasing the mouse** — if
    /// it vanished on release there would be no moment to press Ctrl+C. Scrolling drops it
    /// (`Action::Wheel`): it is anchored to the screen, so the text under it would no longer
    /// be the text that was copied.
    pub drag: Option<crate::selection::Drag>,
    /// Is the button held down right now? The range only grows while it is.
    pub dragging: bool,
    /// The visible text of the last drawn frame, one `String` per screen row. Mouse selection
    /// reads from this — a drag anywhere on the screen extracts what it covers.
    pub screen: Vec<String>,
    /// The question being answered right now (its seq and state).
    ///
    /// A question lands here on its own when it arrives — the turn is blocked waiting for
    /// the answer, so the user should not have to open it separately.
    pub asking: Option<(i64, crate::question::Answering)>,
    /// The area the question screen occupies. Used to map a click to a row.
    pub ask_area: Option<ratatui::layout::Rect>,
    /// Which screen row the activity line was drawn on. Clicking it opens the todo list, and
    /// `apply` is pure — so the drawing side writes it down here, the way `view_total` is.
    pub activity_row: Option<u16>,
    /// Should the answer filled in by submitting the question be sent right away? The I/O
    /// side sees it and clears it.
    pub submit_now: bool,
    /// The slash commands the plugins add (`plugin::commands`). **Read once at startup** — they
    /// are files on disk, and re-reading them per keystroke would put disk access in the draw loop.
    pub plugin_commands: Vec<crate::plugin::PluginCommand>,
    /// The open project/session list.
    pub picker: Option<crate::picker::Picker>,
    /// Cached last-turn outcome per session id, so the picker's real-time refresh does not
    /// refetch every thread's history on each poll. `Unknown` = nothing to cache yet, so the
    /// next refresh derives it again.
    pub thread_status: std::collections::HashMap<String, crate::picker::ThreadStatus>,
    /// Whether a session was running on the last refresh. A running→idle transition means a
    /// turn just finished, so its cached outcome must be re-derived.
    pub thread_was_running: std::collections::HashMap<String, bool>,
    /// What the project we are in is called. `Session` carries only its id, and an id on the
    /// bottom bar says nothing — so the name is kept here, taken wherever one is entered.
    /// `None` until a project has been chosen.
    pub project_name: Option<String>,
    /// A thread's history is on its way. **The old thread stays on screen until it lands** —
    /// blanking the transcript first would leave an empty window for however long the fetch
    /// takes, and the activity line says what is going on instead.
    pub loading_history: bool,
    /// The new-project form. Opens when "＋ 새 프로젝트" is chosen from the ← list.
    /// **The list stays underneath**, so closing with Esc returns right to that spot.
    pub new_project: Option<crate::newproject::Form>,
    /// The `/github` screen. **Where a reviewer token gets pasted** — device flow cannot produce a
    /// fine-grained token, and a fine-grained one is the only kind that can be narrowed to pull
    /// requests on one repository.
    pub github_form: Option<crate::githubform::Form>,
    /// What that screen asked the I/O side to do. Cleared once taken, the way `project_out` is.
    pub github_out: Option<crate::githubform::Ask>,
    /// The popup panel `/mode`·`/mcp`·`/skills`·`/plugin`·`/account`·`/status` open.
    /// `None` is the ordinary state; Esc or Enter closes it.
    pub panel: Option<crate::panel::Panel>,
    /// Filled when the form takes Enter — (name, description). The I/O side does the
    /// creating.
    pub project_out: Option<(String, String)>,
    /// Session usage — credits, context, tokens. Polled off the draw loop (`Frame::Poll`) and
    /// shown on the bottom bar's right edge.
    pub usage: crate::usage::Usage,
    /// What to use as the terminal window title. Changes once the session gets a title.
    pub title: String,
    /// Frames drawn. Blinking indicators take their phase from this.
    ///
    /// It is a frame count rather than a clock so that **tests do not have to wait on
    /// time.** It keeps the drawing side pure.
    pub tick: u64,
    /// What the tools resolve relative paths against. The screen has to show it, or there is
    /// no telling which repo the `src/app.rs` on a tool line belongs to.
    pub cwd: std::path::PathBuf,
    /// What git says about `cwd` right now. `None` is the ordinary case on a machine without
    /// git, and the strip then shows the path with nothing after it.
    pub repo: Option<crate::repo::Repo>,
    /// The home directory, for shortening the path to `~/…`. Read once — the strip is drawn
    /// every frame and must not touch the environment.
    pub home: Option<std::path::PathBuf>,
    /// PTYs currently open. **If they are invisible a ghost shell runs** — a shell the agent
    /// opened stays there without the user knowing.
    pub shells: Vec<Shell>,
    /// Jobs running in the background right now. They drop out when they end — what the user
    /// wants to know is **what is running now**, and the agent reads finished output with
    /// `wait.logs`.
    pub jobs: Vec<JobRow>,
    /// A slash command the user typed. `run()` picks it up and runs it — same trick as
    /// `submit_now`.
    pub command_out: Option<String>,
    /// The enrollment code window. **It lands here on its own when re-enrollment starts.**
    ///
    /// `None` is the normal state. If the credential is revoked or refresh fails for good,
    /// upstream calls `EnrollmentUi::show` and that fills this slot (`enroll::ScreenEnroll`).
    /// Closing with Esc puts it back to `None` — enrollment itself keeps running in the
    /// background, and `EnrollDone` closes it once approved.
    pub enroll: Option<EnrollView>,
    /// The command running right now — (id, command, when it started). The activity line
    /// shows this.
    pub running_exec: Option<(u64, String, Instant)>,
    /// Set by the self-healing tick. The next draw **forces every cell out again** —
    /// the `AlwaysUpdate` flag bypasses the diff and overwrites. It does not clear, so it
    /// does not flicker.
    pub force_update: bool,
    /// Self-heal while a turn is running. **Only blank cells are forced out again** —
    /// residue only ever hides on blank cells, and a space is safe to overlap with
    /// anything, so on a slow SSH link it can never show the same word twice.
    pub force_update_blank: bool,
    /// The screen language. `/config lang` changes it and it moves together with `lang::current()`.
    pub lang: crate::lang::Lang,
    /// The settings `/config` shows and changes. The gate reads `dir_access` through the
    /// bridge; `default_mode` decides the mode the next launch opens in.
    pub config: crate::config::Config,
    /// Set while a `/reconnect` is on its way. **The drop that follows is not a failure.**
    ///
    /// Without it the deliberate reconnect reports itself in red as "the connection was lost" —
    /// true, but it reads as something having gone wrong when it is the thing that was asked for.
    pub reconnecting: bool,
    /// Raised when the settings changed and the disk and the gate have yet to hear about it.
    ///
    /// **`apply` is pure, so it cannot save.** The I/O loop takes this down the same way it
    /// takes `command_out` — and forgetting to carry a setting to `bridge.sync` is exactly
    /// how `/mode` once left the gate on the old mode.
    pub config_out: bool,
}

/// One row for a job running in the background.
#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: String,
    pub label: String,
    /// The time is not carried in the frame but **stamped where it is received** — same way
    /// as `running_exec`.
    pub since: Instant,
}

/// One shell the agent left open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    /// The PTY identifier `terminal.open` returned. Used to find it when closing.
    pub id: String,
    /// The human-readable name. Usually the shell executable's name.
    pub name: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            timeline: Timeline::new(),
            todos: crate::todos::Todos::new(),
            todos_open: false,
            folds: Folds::new(),
            input: Input::new(),
            scroll: Scroll::new(),
            running: false,
            connected: false,
            status: None,
            mode: Mode::default(),
            agent: String::new(),
            quit_armed_at: None,
            quitting: false,
            stopping: false,
            selection: None,
            sent: Vec::new(),
            queued: Vec::new(),
            flush_queue: false,
            recall: None,
            last_cursor: None,
            view_total: 0,
            view_height: 0,
            view_origin: (0, 0),
            view_top: 0,
            rows_cache: crate::rows::Cache::new(),
            view_cards: std::collections::HashMap::new(),
            view_open: std::collections::HashMap::new(),
            view_links: Vec::new(),
            screen_links: Vec::new(),
            drag: None,
            dragging: false,
            screen: Vec::new(),
            asking: None,
            ask_area: None,
            activity_row: None,
            submit_now: false,
            plugin_commands: Vec::new(),
            picker: None,
            thread_status: std::collections::HashMap::new(),
            thread_was_running: std::collections::HashMap::new(),
            project_name: None,
            loading_history: false,
            new_project: None,
            github_form: None,
            github_out: None,
            panel: None,
            project_out: None,
            usage: crate::usage::Usage::default(),
            title: "Zyris Code".into(),
            tick: 0,
            // Must be **the same place** the tools use. `tools::working_dir` is the one
            // definition.
            cwd: crate::tools::working_dir(),
            repo: None,
            home: crate::conn::user_home(),
            shells: Vec::new(),
            jobs: Vec::new(),
            command_out: None,
            enroll: None,
            running_exec: None,
            force_update: false,
            force_update_blank: false,
            lang: crate::lang::current(),
            config: crate::config::Config::default(),
            reconnecting: false,
            config_out: false,
        }
    }
}

/// After one Ctrl+C, pressing again within this window quits.
pub const QUIT_WINDOW: Duration = Duration::from_millis(1500);

/// The gap that marks a paste burst. Terminals without bracketed paste let a paste through
/// as keys arriving a few ms apart — a speed no human can type at.
const PASTE_BURST: Duration = Duration::from_millis(25);

/// How long a notice stays on screen. Plenty to read one sentence.
pub const STATUS_WINDOW: Duration = Duration::from_secs(6);

/// How long we wait, while quitting, for an answer to "stop the turn".
///
/// One round trip is all it takes, so keep it short. **The window closes even past it** —
/// holding on to someone who wants out is worse than one leftover turn.
pub const STOP_WAIT: Duration = Duration::from_secs(3);
/// If the app has not ended this long after a shutdown signal, restore the screen and force
/// the exit. A safety net for the case where the loop is stuck and never sees the signal.
const SHUTDOWN_FORCE: Duration = Duration::from_secs(5);

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set what to say. Visible only for `STATUS_WINDOW` from this moment.
    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now(), Severity::Notice));
    }

    /// Say that something went wrong. **The activity line paints this differently.**
    ///
    /// Every notice used to be the same colour, errors included — so a failure looked exactly
    /// like "connected", on the one line whose whole job is to say what is happening.
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now(), Severity::Error));
    }

    /// Take the notice down now rather than waiting for it to fade. Used when leaving a
    /// conversation — what it had to say stops being true the moment another is on screen.
    pub fn clear_status(&mut self) {
        self.status = None;
    }

    /// The notice to show right now. `None` once the time has passed.
    pub fn status(&self) -> Option<&str> {
        self.status_at(Instant::now())
    }

    /// How bad the current notice is. `Notice` when there is none.
    pub fn status_severity(&self) -> Severity {
        self.status_severity_at(Instant::now())
    }

    pub fn status_severity_at(&self, now: Instant) -> Severity {
        self.status
            .as_ref()
            .filter(|(_, at, _)| now.duration_since(*at) < STATUS_WINDOW)
            .map(|(_, _, s)| *s)
            .unwrap_or(Severity::Notice)
    }

    /// The variant that takes the clock. Split out so tests can fake time.
    pub fn status_at(&self, now: Instant) -> Option<&str> {
        self.status
            .as_ref()
            .filter(|(_, at, _)| now.duration_since(*at) < STATUS_WINDOW)
            .map(|(s, _, _)| s.as_str())
    }

    /// Remember it as something sent. The same message twice in a row is kept once —
    /// walking past the same line twice makes recall look broken.
    pub fn remember_sent(&mut self, text: &str) {
        if self.sent.last().map(String::as_str) != Some(text) {
            self.sent.push(text.to_string());
        }
    }

    /// Are we sitting on a recalled message right now? The ↑↓ arms look at this.
    pub fn recalling(&self) -> bool {
        self.recall.is_some()
    }

    /// Is a quit armed? It releases on its own once time passes.
    pub fn quit_pending(&self) -> bool {
        self.quit_pending_at(Instant::now())
    }

    /// The variant that takes the clock. Split out so tests can fake time.
    pub fn quit_pending_at(&self, now: Instant) -> bool {
        self.quit_armed_at.is_some_and(|t| now.duration_since(t) < QUIT_WINDOW)
    }

    /// The input a character goes into right now.
    ///
    /// While typing a question's free-text answer it is that one; otherwise the input at the
    /// bottom. Without this arm, characters typed as a direct answer land in the bottom
    /// input instead.
    pub fn editor(&mut self) -> &mut Input {
        match &mut self.asking {
            Some((_, a)) if a.typing => &mut a.input,
            _ => &mut self.input,
        }
    }

    /// Maps a screen coordinate to (row, column) in the transcript content. `None` outside
    /// the transcript area.
    ///
    /// The scroll offset has to be added — the first line on screen is not the first line of
    /// the content.
    pub fn content_at(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        let (ox, oy) = self.view_origin;
        if x < ox || y < oy {
            return None;
        }
        let row = (y - oy) as usize;
        if row >= self.view_height {
            return None;
        }
        Some((self.view_top + row, (x - ox) as usize))
    }

    /// The URL of the link under the given screen coordinate, if any. `None` outside the
    /// transcript or on a cell with no link.
    ///
    /// `view_links` is indexed the same way as the transcript's visible lines (line 0 is the
    /// one at `view_origin`), and each `Link`'s columns are in that line's display columns —
    /// so the only mapping needed is the `view_origin` offset, exactly like `inject_links`.
    pub fn link_at(&self, x: u16, y: u16) -> Option<String> {
        // **What is drawn on top is what gets clicked.** An overlay covers the transcript, so a
        // cell it painted belongs to it — checking the transcript first would open whatever URL
        // happens to be hidden underneath.
        if let Some(found) = self
            .screen_links
            .iter()
            .find(|l| l.row == y && x >= l.start && x < l.end)
            .map(|l| l.url.clone())
        {
            return Some(found);
        }
        let (ox, oy) = self.view_origin;
        if x < ox || y < oy {
            return None;
        }
        let line = (y - oy) as usize;
        let col = (x - ox) as usize;
        self.view_links
            .get(line)?
            .iter()
            .find(|l| col >= l.start && col < l.end)
            .map(|l| l.url.clone())
    }
}

/// A clickable URL somewhere on the screen, in absolute cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenLink {
    pub row: u16,
    /// First column, inclusive.
    pub start: u16,
    /// One past the last column.
    pub end: u16,
    pub url: String,
}

pub fn on_key(state: &State, key: KeyEvent) -> Vec<Action> {
    // **Windows sends a KeyEvent for both press and release.** Without filtering on kind,
    // one press types twice — the bug where `/exit` comes out as `//eexxitit` (ratatui
    // issue #347). macOS/Linux have no release event, so this only shows up on Windows.
    // Repeat from holding a key stays — that is not a duplicate.
    if key.kind == KeyEventKind::Release {
        return vec![];
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // **The enrollment code window is topmost.** No other key may do something unexpected
    // while the code is up — Esc closes it, and only Ctrl+C (quit) passes through.
    // Enrollment itself keeps running in the background, so closing with Esc does not
    // interrupt it.
    if state.enroll.is_some() && !(ctrl && matches!(key.code, KeyCode::Char('c'))) {
        return match key.code {
            KeyCode::Esc => vec![Action::EnrollClose],
            _ => vec![],
        };
    }

    // With a question open, keys go there. The turn is blocked waiting for the answer, so
    // that is the one thing to do right now. Only quitting always works.
    if let Some((_, a)) = &state.asking {
        if !(ctrl && matches!(key.code, KeyCode::Char('c'))) {
            return ask_key(a, key, ctrl);
        }
    }

    // **The GitHub screen takes the keys the same way the new-project form does.** Ctrl+C is the
    // one exception everywhere: stopping or quitting must never be trapped behind a screen.
    if state.github_form.is_some() && !(ctrl && matches!(key.code, KeyCode::Char('c'))) {
        return match key.code {
            KeyCode::Enter => vec![Action::FormConfirm],
            KeyCode::Esc => vec![Action::FormCancel],
            KeyCode::Tab | KeyCode::Down => vec![Action::FormNext],
            KeyCode::BackTab | KeyCode::Up => vec![Action::FormPrev],
            KeyCode::Backspace => vec![Action::Backspace],
            KeyCode::Delete => vec![Action::Delete],
            KeyCode::Left => vec![Action::Left],
            KeyCode::Right => vec![Action::Right],
            KeyCode::Home => vec![Action::Home],
            KeyCode::End => vec![Action::End],
            // **Ctrl+U clears the token.** Retyping a pasted token that went in wrong is not a
            // thing anyone should have to do character by character.
            KeyCode::Char('u') if ctrl => vec![Action::ClearInput],
            KeyCode::Char(c) if !ctrl => vec![Action::Insert(c)],
            _ => vec![],
        };
    }

    // **The new-project form sits on top of the list.** The list stays open underneath, so
    // closing with Esc returns right to that spot. Characters go to the form's active field.
    if state.new_project.is_some() && !(ctrl && matches!(key.code, KeyCode::Char('c'))) {
        return match key.code {
            KeyCode::Enter => vec![Action::FormConfirm],
            KeyCode::Esc => vec![Action::FormCancel],
            KeyCode::Tab | KeyCode::Down => vec![Action::FormNext],
            KeyCode::BackTab | KeyCode::Up => vec![Action::FormPrev],
            KeyCode::Backspace => vec![Action::Backspace],
            KeyCode::Delete => vec![Action::Delete],
            KeyCode::Left => vec![Action::Left],
            KeyCode::Right => vec![Action::Right],
            KeyCode::Home => vec![Action::Home],
            KeyCode::End => vec![Action::End],
            KeyCode::Char(c) if !ctrl => vec![Action::Insert(c)],
            _ => vec![],
        };
    }

    // **The popup panel is modal.** While it is up, keys only scroll, focus its
    // button (Tab) or close it — typing must not leak into the input behind it.
    // Esc closes; Enter activates the button when it is focused, otherwise closes.
    // ↑↓ · j·k scroll by one row, PageUp·PageDown by a page. Only quitting always works.
    if state.panel.is_some() && !(ctrl && matches!(key.code, KeyCode::Char('c'))) {
        let has_button = state.panel.as_ref().is_some_and(|p| p.button.is_some());
        let button_focused = state.panel.as_ref().is_some_and(|p| p.button_focused);
        // **A form is edited, not scrolled.** ↑↓ pick the row, ←→ pick the value, Enter
        // saves and closes, Esc closes and throws the draft away. It never scrolls, because
        // it is built to always fit (`panel::form_lines`).
        if state.panel.as_ref().is_some_and(|p| p.form.is_some()) {
            return match key.code {
                KeyCode::Esc => vec![Action::PanelClose],
                KeyCode::Enter => vec![Action::ConfigSave],
                KeyCode::Up | KeyCode::Char('k') => vec![Action::ConfigMove(1)],
                KeyCode::Down | KeyCode::Char('j') => vec![Action::ConfigMove(-1)],
                KeyCode::Left | KeyCode::Char('h') => vec![Action::ConfigShift(-1)],
                KeyCode::Right | KeyCode::Char('l') => vec![Action::ConfigShift(1)],
                _ => vec![],
            };
        }
        return match key.code {
            KeyCode::Esc => vec![Action::PanelClose],
            KeyCode::Tab | KeyCode::BackTab if has_button => vec![Action::PanelFocus],
            KeyCode::Enter if button_focused => vec![Action::PanelActivate],
            KeyCode::Char(' ') if button_focused => vec![Action::PanelActivate],
            KeyCode::Enter => vec![Action::PanelClose],
            KeyCode::Up | KeyCode::Char('k') => vec![Action::PanelScroll(1)],
            KeyCode::Down | KeyCode::Char('j') => vec![Action::PanelScroll(-1)],
            KeyCode::PageUp => vec![Action::PanelScroll(10)],
            KeyCode::PageDown => vec![Action::PanelScroll(-10)],
            _ => vec![],
        };
    }

    if state.picker.is_some() && !(ctrl && matches!(key.code, KeyCode::Char('c'))) {
        // **The command list is chosen by typing.** If characters were taken as movement
        // keys (k/j), `/skills` could not be typed — here a character is plain input and the
        // list narrows.
        let typing =
            matches!(state.picker.as_ref().map(|p| &p.level), Some(crate::picker::Level::Commands));
        return match key.code {
            KeyCode::Up => vec![Action::PickUp],
            KeyCode::Down => vec![Action::PickDown],
            KeyCode::Char('k') if !typing => vec![Action::PickUp],
            KeyCode::Char('j') if !typing => vec![Action::PickDown],
            // **A fully typed command must run on the first Enter.** If Enter only ever
            // meant "pick" because the list is up, typing `/rules` to the end and pressing
            // it would just rewrite the same text into the input and do nothing — this
            // actually happened.
            KeyCode::Enter if typing && typed_a_whole_command(state) => {
                vec![Action::Submit(state.input.text.trim().to_string())]
            }
            KeyCode::Enter => vec![Action::PickConfirm],
            KeyCode::Char(c) if typing && !ctrl => vec![Action::Insert(c)],
            KeyCode::Backspace if typing => vec![Action::Backspace],
            // ← is back. At the project level there is nothing behind, so it closes.
            // In the command list ← moves the cursor, so only Esc closes.
            KeyCode::Left if !typing => vec![Action::PickBack],
            KeyCode::Esc => vec![Action::PickBack],
            KeyCode::Left if typing => vec![Action::Left],
            KeyCode::Right if typing => vec![Action::Right],
            // → does nothing. Enter is the only way to confirm.
            _ => vec![],
        };
    }

    match key.code {
        // **Ctrl+C is the key that stops or quits.** Copy is not here — with three meanings
        // overlapping there is no telling what happens when it matters. Selected text goes
        // to the clipboard on release.
        KeyCode::Char('c') if ctrl => {
            // **Once it cancels, after that it quits.** There is a case where the cancel
            // does not take and `running` stays true — when the server hangs. The window
            // must still be closable then.
            if state.running && !state.stopping {
                vec![Action::Cancel]
            } else if state.quit_pending() {
                vec![Action::Quit]
            } else {
                vec![Action::ArmQuit]
            }
        }
        // Ctrl+L means "redraw the screen" in shells, vim and less alike. It is the key
        // people press reflexively when the screen breaks, so it gets no other meaning.
        KeyCode::Char('l') if ctrl => vec![Action::Repaint],
        KeyCode::Char('o') if ctrl => vec![Action::ToggleFold],
        // **t for tasks.** Nothing else claims Ctrl+T here, and the terminal sends it through
        // untouched — it is not one of the bytes a tty reserves.
        KeyCode::Char('t') if ctrl => vec![Action::ToggleTodos],
        KeyCode::BackTab => vec![Action::CycleMode],
        KeyCode::Char('w') if ctrl => vec![Action::DeleteWord],
        // **Wipe everything typed.** `Ctrl+U` is the canonical one — readline, bash and zsh
        // all do it, and the terminal just sends one 0x15 byte, so **it arrives everywhere.**
        //
        // `Ctrl+Backspace` is taken too. That one only comes when the terminal reports it —
        // many terminals send the same byte as plain Backspace, leaving no way to tell them
        // apart. It is a bonus.
        KeyCode::Char('u') if ctrl => vec![Action::ClearInput],
        KeyCode::Backspace if ctrl => vec![Action::ClearInput],
        // readline convention. Moving has to work where there are no arrow keys.
        KeyCode::Char('a') if ctrl => vec![Action::Home],
        KeyCode::Char('e') if ctrl => vec![Action::End],
        // With a selection up, Esc clears it. This comes before cancelling a running turn —
        // what is in front of you comes first.
        KeyCode::Esc if state.selection.is_some() => vec![Action::ClearSelection],
        KeyCode::Esc if state.running => vec![Action::Cancel],
        // **Shift+Enter and Alt+Enter are newlines.** With the kitty keyboard protocol on
        // (`PushKeyboardEnhancementFlags` in `run()` below) Shift+Enter arrives separately as
        // Enter+SHIFT. Alt+Enter (ESC+\r) is the fallback for terminals without the
        // protocol — it has to come before the submit arm.
        KeyCode::Enter if alt || shift => vec![Action::Insert('\n')],
        KeyCode::Enter if !state.input.text.is_empty() => {
            vec![Action::Submit(state.input.text.clone())]
        }
        KeyCode::Backspace => vec![Action::Backspace],
        KeyCode::Delete => vec![Action::Delete],
        // **↑ brings back what was sent.** It starts when the input is empty, and once
        // inside it keeps walking further back — stopping at the second ↑ makes recall
        // useless.
        KeyCode::Up if state.input.text.is_empty() || state.recalling() => {
            vec![Action::RecallOlder]
        }
        KeyCode::Down if state.recalling() => vec![Action::RecallNewer],
        // **This arm has to come before the Left below.** match picks from the top, so with
        // the order reversed the list would never open.
        //
        // It only opens when the input is empty. With text there, moving the cursor comes
        // first.
        KeyCode::Left if state.input.text.is_empty() => vec![Action::OpenPicker],
        KeyCode::Left => vec![Action::Left],
        KeyCode::Right => vec![Action::Right],
        KeyCode::Home => vec![Action::Home],
        KeyCode::End => vec![Action::End],
        // **PageUp/PageDown scroll the conversation by a page.** The wheel is the only
        // other way and not every terminal delivers wheel events (mobile SSH, tmux
        // without mouse) — history must be reachable by keyboard alone.
        KeyCode::PageUp => vec![Action::Page(1)],
        KeyCode::PageDown => vec![Action::Page(-1)],
        KeyCode::Char(c) if !ctrl => vec![Action::Insert(c)],
        _ => vec![],
    }
}

/// Keys for the question screen. While typing free text, characters go to the input.
fn ask_key(a: &crate::question::Answering, key: KeyEvent, ctrl: bool) -> Vec<Action> {
    if a.typing {
        return match key.code {
            KeyCode::Enter => vec![Action::AskConfirm],
            KeyCode::Esc => vec![Action::AskCancel],
            KeyCode::Backspace => vec![Action::Backspace],
            KeyCode::Left => vec![Action::Left],
            KeyCode::Right => vec![Action::Right],
            KeyCode::Char(c) if !ctrl => vec![Action::Insert(c)],
            _ => vec![],
        };
    }
    match key.code {
        KeyCode::Up => vec![Action::AskUp],
        KeyCode::Down => vec![Action::AskDown],
        // Enter alone both chooses and acts. On an action row (back/next/submit) it does
        // that instead.
        KeyCode::Enter | KeyCode::Char(' ') => vec![Action::AskConfirm],
        KeyCode::Esc => vec![Action::AskCancel],
        _ => vec![],
    }
}

/// Whether an Enter that arrived mid-burst may be turned into a newline.
///
/// **Only when Enter really means "send" at that spot.** Looking only at the burst
/// check would swallow the confirm key of a list, form or question and drop a
/// newline into the invisible input behind it — the case where a quick
/// ↓→Enter on the command list inserted a newline into "/m" instead of picking.
///
/// While typing free text in a question, that input is the send target — pasting
/// several lines in a burst must not submit on the first line, so the newline stands.
fn enter_becomes_newline(state: &State, key: &KeyEvent, in_burst: bool) -> bool {
    // Windows sends a press and a release separately. Turning the release into a
    // newline would double it on a plain Enter — the same filtering `on_key` does.
    if key.kind == KeyEventKind::Release {
        return false;
    }
    if !(in_burst && key.code == KeyCode::Enter && key.modifiers.is_empty()) {
        return false;
    }
    if let Some((_, a)) = &state.asking {
        return a.typing && !a.input.text.is_empty();
    }
    state.enroll.is_none()
        && state.new_project.is_none()
        && state.picker.is_none()
        && !state.input.text.is_empty()
}

pub fn apply(state: &mut State, action: &Action) {
    // Editing a character leaves the recall right away. Otherwise one ↓ loses the edit —
    // and editing a recalled message before sending it is the whole point of the feature.
    if matches!(
        action,
        Action::Insert(_)
            | Action::Paste(_)
            | Action::Backspace
            | Action::Delete
            | Action::DeleteWord
    ) {
        state.recall = None;
    }

    // **Any other input drops the mouse selection.** The highlight is anchored to the screen,
    // so once the person types, moves the cursor, sends a line, opens a list or does anything
    // else, it points at stale text and has served its purpose. Only the mouse's own gestures
    // (press, drag, release), a plain redraw and clearing itself keep it alive — so it never
    // outlives the moment it was made. The wheel and page scroll are left to their own arms,
    // which already drop the highlight while keeping the copied text.
    if !matches!(
        action,
        Action::Press(..)
            | Action::DragTo(..)
            | Action::Release
            | Action::OpenLink(_)
            | Action::Wheel(_)
            | Action::Page(_)
            | Action::Repaint
            | Action::ClearSelection
    ) && (state.drag.is_some() || state.selection.is_some())
    {
        state.drag = None;
        state.selection = None;
    }

    // **With the GitHub screen open, keys belong to it.** Only the reviewer row takes text — the
    // person's row is a button, so a keystroke there is not swallowed into an invisible field.
    if let Some(form) = state.github_form.as_mut() {
        match action {
            Action::Insert(c) => {
                if let Some(field) = form.typing() {
                    field.insert(*c);
                }
            }
            // **A pasted token arrives whole** (`EnableBracketedPaste`), so a token with a newline
            // on the end does not submit halfway through.
            Action::Paste(text) => {
                if let Some(field) = form.typing() {
                    field.insert_str(text.trim());
                }
            }
            Action::Backspace => {
                if let Some(field) = form.typing() {
                    field.backspace();
                }
            }
            Action::Delete => {
                if let Some(field) = form.typing() {
                    field.delete();
                }
            }
            Action::DeleteWord => {
                if let Some(field) = form.typing() {
                    field.delete_word();
                }
            }
            Action::Left => {
                if let Some(field) = form.typing() {
                    field.left();
                }
            }
            Action::Right => {
                if let Some(field) = form.typing() {
                    field.right();
                }
            }
            Action::Home => {
                if let Some(field) = form.typing() {
                    field.home();
                }
            }
            Action::End => {
                if let Some(field) = form.typing() {
                    field.end();
                }
            }
            Action::ClearInput => {
                if let Some(field) = form.typing() {
                    field.take();
                }
            }
            Action::FormNext => form.next(),
            Action::FormPrev => form.prev(),
            Action::FormConfirm => {
                if let Some(ask) = form.submit() {
                    form.busy = true;
                    form.note = None;
                    state.github_out = Some(ask);
                }
            }
            Action::FormCancel => state.github_form = None,
            _ => {}
        }
        return;
    }

    // **With the new-project form open, character keys go to the form's active field.**
    // They must not leak into the input below — the form is a different place. Creating does
    // not call the server here; it only fills `project_out` — the I/O side actually creates.
    if state.new_project.is_some() {
        match action {
            Action::Insert(c) => {
                state.new_project.as_mut().expect("just checked it").active().insert(*c)
            }
            Action::Paste(text) => {
                state.new_project.as_mut().expect("just checked it").active().insert_str(text)
            }
            Action::Backspace => {
                state.new_project.as_mut().expect("just checked it").active().backspace()
            }
            Action::Delete => {
                state.new_project.as_mut().expect("just checked it").active().delete()
            }
            Action::DeleteWord => {
                state.new_project.as_mut().expect("just checked it").active().delete_word()
            }
            Action::Left => state.new_project.as_mut().expect("just checked it").active().left(),
            Action::Right => state.new_project.as_mut().expect("just checked it").active().right(),
            Action::Home => state.new_project.as_mut().expect("just checked it").active().home(),
            Action::End => state.new_project.as_mut().expect("just checked it").active().end(),
            Action::FormNext => state.new_project.as_mut().expect("just checked it").next(),
            Action::FormPrev => state.new_project.as_mut().expect("just checked it").prev(),
            Action::FormConfirm => {
                let done = state.new_project.as_mut().and_then(|form| form.submit(state.lang));
                if let Some((name, description)) = done {
                    state.project_out = Some((name, description));
                }
            }
            Action::FormCancel => state.new_project = None,
            _ => {}
        }
        return;
    }
    match action {
        Action::Insert(c) => {
            state.editor().insert(*c);
            follow_the_slash(state);
        }
        Action::Paste(text) => {
            // Newlines inside go in verbatim. The slash command list does not open — a
            // paste must not change the mode. The matches! above releases the recall.
            state.editor().insert_str(text);
        }
        Action::Backspace => {
            state.editor().backspace();
            follow_the_slash(state);
        }
        Action::Delete => state.editor().delete(),
        Action::DeleteWord => state.editor().delete_word(),
        Action::Left => state.editor().left(),
        Action::Right => state.editor().right(),
        Action::Home => state.input.home(),
        Action::End => state.input.end(),
        Action::Submit(text) => {
            state.input.take();
            state.recall = None;
            // Once sent, the list has done its job. Left open it keeps covering the screen.
            state.picker = None;
            // **Slash commands never reach the server.** One typo must not spend credits,
            // and there is no reason to queue them just because a turn is running —
            // changing the mode or looking at a list is not something to wait a turn for.
            if crate::command::is_command(text) {
                state.remember_sent(text);
                state.command_out = Some(text.clone());
                return;
            }
            // **While work is running, hold it instead of sending.** It does not go into
            // the sent history yet either — only adding it after it really goes out keeps
            // "what was sent" from being a lie.
            if state.running {
                state.queued.push(text.clone());
                return;
            }
            // **The words go up the moment they are submitted.** The only other source of
            // `Item::User` is the server's `chat_user`, so without this a person's own line is
            // missing for the whole round trip — and in 일/작업 mode the first message never
            // goes through `send_message` at all (it rides inside `ZNewJob::message`), so it
            // may never appear. `Timeline::upsert` retires the echo when the server's copy lands.
            //
            // **One arm covers all four modes** — the mode is consulted later, in
            // `Session::open_for`. It sits below both early returns on purpose: slash commands
            // never reach the conversation, and a held message is echoed when it really goes out.
            state.timeline.echo(text.as_str());
            state.remember_sent(text);
        }
        Action::Wheel(notches) => {
            // **With a panel open, the wheel scrolls the panel.** The transcript is
            // hidden behind it, so scrolling that instead would move text the user
            // cannot see.
            if let Some(p) = &mut state.panel {
                if *notches > 0 {
                    p.scroll_up(*notches as usize);
                } else {
                    p.scroll_down((-(*notches)) as usize);
                }
                return;
            }
            let (total, height) = (state.view_total, state.view_height);
            state.scroll.wheel(*notches, total, height);
            // The selection is anchored to the screen; scrolling moves the text out from under
            // it, so the highlight would point at text different from what was copied.
            state.drag = None;
        }
        Action::Page(dir) => {
            // PageUp/PageDown with a panel open never reach here — `on_key` routes them
            // to the panel's own scroll first. So this is always the transcript.
            let (total, height) = (state.view_total, state.view_height);
            state.scroll.page(*dir, total, height);
            // Same reason as the wheel: the selection is anchored to the screen, and
            // scrolling moves the text out from under it.
            state.drag = None;
        }
        Action::ToggleFold => {
            // **The key only ever reaches the last work card's head** — the whole stretch of
            // working, opened or folded in one press. Reasoning chips and tool rows are opened by
            // clicking them: there is no way to say *which* one from the keyboard, and walking a
            // cursor through them would be a second selection to keep in mind.
            //
            // The person's choice is remembered from here on, so the run no longer opens or folds
            // it. Like a click, it flips **what is on screen** — a running card draws open while
            // its stored fold says `open: false`, and flipping the stored one there does nothing.
            let key = state.timeline.items().iter().rev().find_map(|item| match item {
                crate::timeline::Item::Work { seq, .. } => Some(*seq),
                _ => None,
            });
            if let Some(key) = key {
                let shown = state.view_open.get(&key).copied();
                let fold = state.folds.entry(key).or_default();
                fold.open = !shown.unwrap_or(fold.open);
                fold.user_touched = true;
                let now = fold.open;
                state.view_open.insert(key, now);
            }
        }
        Action::ToggleTodos => state.todos_open = !state.todos_open,
        Action::Press(x, y) => {
            // Pressing on the question screen picks that row.
            if let (Some(area), Some((_, a))) = (state.ask_area, state.asking.as_ref()) {
                if *y >= area.y && *y < area.y + area.height {
                    if let Some(i) = crate::widgets::ask_row_at(a, area, *y) {
                        if let Some((_, a)) = &mut state.asking {
                            a.cursor = i;
                        }
                        apply(state, &Action::AskConfirm);
                    }
                    return;
                }
            }
            // A new press discards the previous selection.
            state.selection = None;
            // **The whole screen is selectable — blank space included.** A drag that starts
            // on empty cells still works; it just selects nothing until it covers text.
            state.drag = Some(crate::selection::Drag::new((*y as usize, *x as usize)));
            state.dragging = true;
        }
        Action::DragTo(x, y) => {
            if !state.dragging {
                return;
            }
            if let Some(drag) = state.drag.as_mut() {
                drag.to = (*y as usize, *x as usize);
            }
            if let Some(drag) = state.drag {
                if !drag.is_click() {
                    // Plain text is built **only here.** Building it every frame gets
                    // heavier in proportion to the screen — a drag runs at hand speed,
                    // so building it then is enough. The text comes from the last drawn
                    // frame, so any visible text — even the enrollment code — is copyable.
                    let text = crate::selection::extract(&state.screen, &drag);
                    state.selection = (!text.trim().is_empty()).then_some(text);
                }
            }
        }
        Action::Release => {
            state.dragging = false;
            // **The range stays.** It has to be possible to see how far the selection went.
            // Exporting to the clipboard is I/O and does not happen here — `run` does it.
            let Some(drag) = state.drag else { return };
            if drag.is_click() {
                // **A click on the activity line opens or folds the todo list.** Only when there
                // is one to show — on every other line that row is ordinary text, and taking the
                // click would cost the ability to select it.
                if !state.todos.is_empty() && state.activity_row == Some(drag.from.0 as u16) {
                    state.drag = None;
                    state.todos_open = !state.todos_open;
                    return;
                }
                // No movement means a click — if that row is a foldable node head (a topic,
                // subtopic or tool), fold or unfold it. The drag holds screen coordinates, so the
                // row is mapped back to a transcript content row first. The person's choice is
                // remembered: from now on auto-open/auto-fold leaves this node alone.
                state.drag = None;
                let content = state.content_at(drag.from.1 as u16, drag.from.0 as u16);
                if let Some(&seq) = content.and_then(|(r, _)| state.view_cards.get(&r)) {
                    let shown = state.view_open.get(&seq).copied();
                    let fold = state.folds.entry(seq).or_default();
                    fold.open = !shown.unwrap_or(fold.open);
                    fold.user_touched = true;
                    // **Written back at once, not left to the next frame.** Two clicks landing
                    // before a repaint would otherwise both read the same stale state and fold
                    // twice.
                    let now = fold.open;
                    state.view_open.insert(seq, now);
                }
            }
        }
        // **Opening a link is I/O — `run` does it.** The drag never started, so there is
        // no selection to clear and no fold to toggle.
        Action::OpenLink(_) => {}
        Action::AskUp => {
            if let Some((_, a)) = &mut state.asking {
                a.up();
            }
        }
        Action::AskDown => {
            if let Some((_, a)) = &mut state.asking {
                a.down();
            }
        }
        Action::AskToggle => {
            if let Some((_, a)) = &mut state.asking {
                a.toggle();
            }
        }
        Action::AskConfirm => {
            use crate::question::{Act, RowKind};

            let Some((_, a)) = &mut state.asking else {
                return;
            };
            if a.typing {
                // Finishing the typing comes first.
                a.confirm();
                return;
            }
            match a.row_at(a.cursor) {
                Some(RowKind::Option(_)) | Some(RowKind::Free) => a.toggle(),
                Some(RowKind::Action(Act::Back)) => a.back(),
                Some(RowKind::Action(Act::Next)) | Some(RowKind::Action(Act::Skip)) => a.advance(),
                Some(RowKind::Action(Act::Edit)) => a.to_edit(),
                // Say we will not answer. Closing quietly leaves the other side waiting.
                Some(RowKind::Action(Act::Reject)) => {
                    state.asking = None;
                    state.input = Input::new();
                    state.input.insert_str(state.lang.question_refused());
                    state.submit_now = true;
                }
                // Submitting is immediate. Put the answer in the input and the I/O side
                // sends it as-is.
                Some(RowKind::Action(Act::Submit)) => {
                    if let Some((_, a)) = state.asking.take() {
                        state.input = Input::new();
                        let text = a.answer_text(state.lang);
                        state.input.insert_str(if text.is_empty() {
                            state.lang.all_skipped()
                        } else {
                            &text
                        });
                        state.submit_now = true;
                    }
                }
                None => {}
            }
        }
        Action::AskCancel => {
            // While typing, only stop the typing. Otherwise put the question away — an
            // answer can just be a plain message too.
            let typing = state.asking.as_ref().is_some_and(|(_, a)| a.typing);
            if typing {
                if let Some((_, a)) = &mut state.asking {
                    a.typing = false;
                    a.input = Input::new();
                }
            } else {
                state.asking = None;
            }
        }
        Action::ClearSelection => {
            state.selection = None;
            state.drag = None;
        }
        // Repainting is the screen's business alone. No state changes, so there is nothing
        // to do here.
        Action::Repaint => {}
        // **The box goes up empty and the rows arrive later.** The I/O side only starts the
        // fetch; it no longer waits for it, so putting the placeholder here is safe (`apply`
        // runs after that arm) and it is what makes ← respond the instant it is pressed.
        Action::OpenPicker => state.picker = Some(crate::picker::Picker::loading_projects()),
        Action::PickUp => {
            if let Some(p) = &mut state.picker {
                p.up();
            }
        }
        Action::PickDown => {
            if let Some(p) = &mut state.picker {
                p.down();
            }
        }
        Action::PanelClose => state.panel = None,
        Action::PanelFocus => {
            if let Some(p) = &mut state.panel {
                if p.button.is_some() {
                    p.button_focused = !p.button_focused;
                }
            }
        }
        // **The button's work is I/O, so `apply` only notes it down.** It closes the
        // panel and queues the same command the button stands for — the loop below
        // runs `finish_command`, which is where the credentials actually drop.
        Action::PanelActivate => {
            let logout = state
                .panel
                .as_ref()
                .is_some_and(|p| p.button == Some(crate::panel::PanelButton::Logout));
            state.panel = None;
            if logout {
                state.command_out = Some("/account logout".to_string());
            }
        }
        Action::PanelScroll(by) => {
            if let Some(p) = &mut state.panel {
                if *by > 0 {
                    p.scroll_up(*by as usize);
                } else {
                    p.scroll_down((-(*by)) as usize);
                }
            }
        }
        // **The form redraws itself after every key.** The lines are what the widget draws,
        // so a moved cursor that did not refresh would leave the mark on the old row.
        Action::ConfigMove(by) => {
            if let Some(p) = &mut state.panel {
                if let Some(form) = &mut p.form {
                    form.move_cursor(*by);
                }
                p.refresh();
            }
        }
        Action::ConfigShift(by) => {
            if let Some(p) = &mut state.panel {
                if let Some(form) = &mut p.form {
                    form.shift(*by);
                }
                p.refresh();
            }
        }
        // **Only the draft crosses over here.** Writing the file, telling the global language
        // and carrying the policy to the gate are all I/O, so they happen where `config_out`
        // is picked up — the same split `command_out` uses.
        Action::ConfigSave => {
            if let Some(form) = state.panel.as_ref().and_then(|p| p.form) {
                state.config = form.draft;
                state.lang = form.lang;
                state.config_out = true;
            }
            state.panel = None;
        }
        // Acting on the choice (moving in the list, switching sessions, creating) is the
        // I/O side's job.
        Action::PickConfirm => {}
        // **Do not touch the picker here.** `apply` runs after the I/O handling, so looking
        // again at what I/O just moved back from sessions to projects turns this into "we
        // are at the project level, close it" — and back becomes plain close. This actually
        // happened.
        Action::PickBack => {}
        Action::CycleMode => state.mode = state.mode.next(),
        Action::ClearInput => {
            state.editor().take();
            state.recall = None;
        }
        // Recall. **A queued message comes first** — it is the only one still editable.
        Action::RecallOlder => {
            if let Some(text) = state.queued.pop() {
                state.input.text = text;
                state.input.end();
                // Pulling it out took it off the queue. Walking further back would lose
                // what is being edited, so stop here — that is what not setting `recall`
                // means.
                state.recall = None;
                return;
            }
            if state.sent.is_empty() {
                return;
            }
            let next = match state.recall {
                Some(i) => i.saturating_sub(1),
                None => state.sent.len() - 1,
            };
            state.recall = Some(next);
            let text = state.sent[next].clone();
            state.input.text = text;
            state.input.end();
        }
        Action::RecallNewer => {
            let Some(i) = state.recall else { return };
            match state.sent.get(i + 1) {
                Some(text) => {
                    state.recall = Some(i + 1);
                    state.input.text = text.clone();
                    state.input.end();
                }
                // Past the bottom the recall ends. It amounts to coming back to where the
                // typing was.
                None => {
                    state.recall = None;
                    state.input.take();
                }
            }
        }
        Action::ArmQuit => state.quit_armed_at = Some(Instant::now()),
        // Sending is the I/O side's job. Here we only note that we asked — the activity
        // line shows it, and whether the next Ctrl+C goes to quitting is decided by it too.
        Action::Cancel => state.stopping = true,
        Action::Quit => {}
        // **Enrollment itself is not cut off here.** Only the window closes; the upstream
        // polling keeps running in the background — `EnrollDone` closes it again once
        // approved, and on lapse the window comes back with a new code.
        Action::EnrollClose => state.enroll = None,
        // **With the form closed no Form* reaches this far** — while open, the guard above
        // intercepts them. They still have to be listed to be exhaustive.
        Action::FormNext | Action::FormPrev | Action::FormConfirm | Action::FormCancel => {}
        Action::Frame(frame) => apply_frame(state, frame),
    }
}

fn apply_frame(state: &mut State, frame: &Frame) {
    match frame {
        Frame::Event { cursor, entry, todo } => {
            // The cursor advances even for an event we do not render — the resume position
            // must not be lost.
            state.last_cursor = Some(*cursor);
            // **Keyed by the event's `seq`, so an in-place update replaces.** The seq comes off
            // the entry, which is always there for a todo change: every one of them is a
            // `tool_call`, and those always draw.
            if let (Some(change), Some(entry)) = (todo, entry) {
                state.todos.note(entry.seq, change.clone());
            }
            let Some(entry) = entry else { return };
            if let EntryKind::WorkStart(_) = entry.kind {
                state.folds.entry(entry.seq).or_default();
            }
            // A question awaiting an answer puts us straight into answering mode. The turn
            // is blocked, so there is no reason to make the user open it. A question that
            // was already answered does not reopen.
            if let EntryKind::Question { steps, answered } = &entry.kind {
                if *answered {
                    if state.asking.as_ref().is_some_and(|(q, _)| *q == entry.seq) {
                        state.asking = None;
                    }
                } else if state.asking.is_none() {
                    state.asking =
                        Some((entry.seq, crate::question::Answering::new(steps.clone())));
                }
            }
            state.timeline.upsert(entry.clone());
        }
        // **A card never folds or unfolds by itself.**
        //
        // This used to keep it open while reasoning streamed and fold it once the answer
        // started. But then the screen being read moves on its own, and exceptions like
        // "late reasoning must not reopen it" keep piling on. Folding is left as the one
        // thing the user decides — Ctrl+O.
        Frame::Delta { kind, text } => state.timeline.push_delta(*kind, text),
        Frame::Status { running } => {
            // **The end of a turn is when the queue gets flushed.** Sending is I/O and
            // cannot happen here — only raise the flag and `run` picks it up. Same trick as
            // `submit_now`.
            if state.running && !*running && !state.queued.is_empty() {
                state.flush_queue = true;
            }
            // The request to stop lasts only for that turn. **Release it only on a change** —
            // the same state arrives many times while running, and releasing every time
            // would make Ctrl+C repeat the cancel forever.
            if state.running != *running {
                state.stopping = false;
            }
            state.running = *running;
        }
        Frame::ShellOpened { id, name } => {
            // The same PTY arriving twice would put two copies in the list.
            if !state.shells.iter().any(|s| s.id == *id) {
                state.shells.push(Shell { id: id.clone(), name: name.clone() });
            }
        }
        Frame::ShellClosed { id } => state.shells.retain(|s| s.id != *id),
        // The background polling result. No value means the server did not answer — pass
        // over it quietly.
        Frame::Poll { usage, title } => {
            if let Some(u) = usage {
                if u != &state.usage {
                    state.usage = u.clone();
                }
            }
            if let Some(t) = title {
                if !t.trim().is_empty() {
                    state.title = t.clone();
                }
            }
        }
        // Replace, never merge. Leaving a repository behind has to clear the strip, and the
        // background arm only sends this when the value actually changed.
        Frame::Git(got) => state.repo = got.clone(),
        // A list that finished loading. **The cursor is preserved** — a refresh landing while
        // someone is choosing must not yank their selection out from under them.
        //
        // **It is dropped if the list has moved on.** A slow project list arriving after the
        // person already went into a project would throw them back out.
        Frame::Picker { picker, thread_was_running } => {
            let same = state.picker.as_ref().is_some_and(|cur| {
                std::mem::discriminant(&cur.level) == std::mem::discriminant(&picker.level)
            });
            if same {
                let (cursor, top) =
                    state.picker.as_ref().map(|cur| (cur.cursor, cur.top)).unwrap_or((0, 0));
                let mut p = picker.clone();
                p.cursor = cursor.min(p.rows.len().saturating_sub(1));
                // The scroll position too — a refresh that snapped the list back to the top
                // would be the same yank as losing the cursor.
                p.top = top.min(p.rows.len().saturating_sub(1));
                // **A dot already on screen is kept.** A refresh only knows the outcomes it
                // had cached when it started, so a rebuild landing while the rest are still
                // streaming would blank them — and with a refresh every few seconds they
                // would blink out and back for as long as the derivation took.
                if let Some(old) = &state.picker {
                    let unknown = Some(crate::picker::ThreadStatus::Unknown);
                    for row in p.rows.iter_mut().filter(|r| r.status == unknown) {
                        let was = old.rows.iter().find(|o| o.id.is_some() && o.id == row.id);
                        // Only a dot that says something replaces this one; the old row may be
                        // waiting on its own derivation too.
                        if let Some(was) = was.filter(|o| o.status != unknown) {
                            row.status = was.status;
                        }
                    }
                }
                state.picker = Some(p);
            }
            if let Some(map) = thread_was_running {
                state.thread_was_running = map.clone();
            }
        }
        // One thread's dot. **A running dot is not overwritten** — running is what is
        // happening now, and this is only the last outcome. Neither is a settled dot replaced
        // by `Unknown`: a derivation that came back empty knows less than the row already does.
        Frame::ThreadStatus { id, status } => {
            state.thread_status.insert(id.clone(), *status);
            if let Some(row) = state
                .picker
                .as_mut()
                .and_then(|p| p.rows.iter_mut().find(|r| r.id.as_deref() == Some(id.as_str())))
            {
                use crate::picker::ThreadStatus;
                if row.status != Some(ThreadStatus::Running) && *status != ThreadStatus::Unknown {
                    row.status = Some(*status);
                }
            }
        }
        // **Say it and close.** A list left open and forever empty reads as a hang, and so
        // does an activity line stuck on "loading…".
        Frame::PickerFailed(why) => {
            state.picker = None;
            state.loading_history = false;
            state.set_error(why.clone());
        }
        // A session's history. It arrives whole, so the screen it replaces is torn down here
        // rather than at the moment of the click — until this lands, the previous thread is
        // still what is on screen and still what the person can read.
        Frame::History { entries } => {
            clear_conversation(state);
            for (cursor, entry, todo) in entries {
                let frame =
                    Frame::Event { cursor: *cursor, entry: entry.clone(), todo: todo.clone() };
                apply(state, &Action::Frame(frame));
            }
            state.loading_history = false;
        }
        // **The screen says when it dropped.** The activity line turns to "connecting…" and
        // the reason goes by once as a notice. Reconnecting is the Runner's job, and
        // `api_rx` tells us once it is back.
        Frame::Disconnected(why) => {
            state.connected = false;
            // **Losing the connection is a failure, not news** — silent failure is the worst
            // kind, and this is the one line that gets to say it. Unless we asked for it.
            if std::mem::take(&mut state.reconnecting) {
                state.set_status(state.lang.reconnecting());
            } else {
                state.set_error(state.lang.disconnected(why));
            }
        }
        // The time is not carried in the frame but stamped where it is received — same way
        // as `status_at`.
        Frame::ExecStart { id, command } => {
            state.running_exec = Some((*id, command.clone(), Instant::now()));
        }
        // **Only clear the one that finished.** With overlapping runs, a later one clearing
        // an earlier one makes the screen lie.
        Frame::ExecDone { id } => {
            if state.running_exec.as_ref().is_some_and(|(at, _, _)| at == id) {
                state.running_exec = None;
            }
        }
        Frame::JobStart { id, label } => {
            // The same id arriving twice would put two rows in the list.
            if !state.jobs.iter().any(|j| j.id == *id) {
                state.jobs.push(JobRow {
                    id: id.clone(),
                    label: label.clone(),
                    since: Instant::now(),
                });
            }
        }
        // A finished job leaves the list and **is announced once, then gone.** Success is
        // announced too — not knowing it finished leaves the user waiting.
        Frame::JobEnded { id, ok, secs } => {
            state.jobs.retain(|j| j.id != *id);
            // A job that finished is news; a job that failed is not.
            let said = state.lang.job_ended(id, *ok, *secs);
            if *ok {
                state.set_status(said);
            } else {
                state.set_error(said);
            }
        }
        // The enrollment code window. It comes up on its own when re-enrollment starts. When
        // a new code arrives after a lapse only the contents are swapped — the window stays
        // unless the user closes it with Esc.
        Frame::Enroll(view) => state.enroll = Some(view.clone()),
        Frame::EnrollPhase(phase) => {
            if let Some(view) = &mut state.enroll {
                view.phase = *phase;
            }
        }
        // Approved, and the credential was stored. Close the window.
        Frame::EnrollDone => state.enroll = None,
        Frame::Notice(text) => state.set_status(text.clone()),
    }
}

/// Is what was typed already a whole command name? If so there is no reason to pick from
/// the list again.
fn typed_a_whole_command(state: &State) -> bool {
    let typed = state.input.text.trim();
    state.picker.as_ref().is_some_and(|p| p.rows.iter().any(|r| r.label == typed))
}

/// When the input starts with `/`, put up the command list and narrow it as typing goes on.
///
/// **If the list does not come up nobody uses it.** With no way to know what commands exist,
/// slash commands become a feature only their author uses.
///
/// With a question or an approval open, leave it alone — something else already owns that
/// spot.
fn follow_the_slash(state: &mut State) {
    if state.asking.is_some() {
        return;
    }
    let typed = state.input.text.clone();
    let opening = typed.starts_with('/') && !typed.contains(char::is_whitespace);
    let showing =
        matches!(state.picker.as_ref().map(|p| &p.level), Some(crate::picker::Level::Commands));
    match (opening, showing) {
        (true, _) => {
            let mut p = crate::picker::Picker::commands(state.lang, &state.plugin_commands);
            p.narrow(&typed, state.lang, &state.plugin_commands);
            // If narrowing leaves nothing, close the list — an empty window looks broken,
            // and it only covers the screen while typing a non-command like `/home/...`.
            state.picker = (!p.rows.is_empty()).then_some(p);
        }
        // Either erased or an argument started. The list has done its job.
        (false, true) => state.picker = None,
        (false, false) => {}
    }
}

/// Runs the slash commands **that only need to touch state**.
///
/// The ones that need the server or the disk (`/agent`, `/mcp`, `/plugin`, `/undo`, …) are
/// left untouched and handed back — the I/O side takes them and finishes. This function has
/// to stay pure for tests to attach to it.
pub fn run_command(state: &mut State, text: &str) -> Option<crate::command::Command> {
    use crate::command::Command;
    let cmd = crate::command::parse(text)?;
    match &cmd {
        Command::Help => state.timeline.say(crate::command::help_text(state.lang)),
        Command::Mode(None) => {
            // **The panel is the answer now** — a wall of text in the conversation was
            // the whole complaint. The four modes with the current one marked read
            // better than one sentence.
            state.panel = Some(crate::panel::mode(state.lang, state.mode));
        }
        Command::Mode(Some(mode)) => {
            state.mode = *mode;
            let said = state.lang.mode_changed(mode.label(state.lang));
            state.timeline.say(said);
        }
        Command::Cwd => {
            // **Say the node name too.** With two machines sharing a hostname (`arch` is
            // common) this is the only way to tell them apart in the server's node list.
            // **No leading spaces on a line.** Markdown reads a four-space-indented line as
            // a code block — folding the string for readability turns it into a box on
            // screen.
            state.timeline.say(state.lang.cwd_text(
                &state.cwd,
                &crate::conn::node_name(),
                &crate::conn::node_slug(),
                &crate::conn::credential_home(),
            ));
        }
        Command::Clear => {
            state.timeline.clear();
            // **The server's record is untouched.** Clearing must not read as the session
            // being gone.
            state.timeline.say(state.lang.clear_done());
        }
        // **Logs are not dumped on screen** — those are for the agent to read, and covering the
        // transcript hides the conversation itself. Here we only give what is running and how to
        // stop it.
        Command::Jobs(None) => state.timeline.say(jobs_text(&state.jobs, state.lang)),
        Command::Jobs(Some(_)) => {}
        // Shutting the screen down is I/O. Only raise the flag.
        Command::Quit => state.quitting = true,
        // **The settings panel** — every value marked with the current one, changed by
        // `option value` below or by the matching slash command (`/config dir …` etc.).
        Command::Config(None) => {
            state.panel = Some(crate::panel::config(state.lang, state.config));
        }
        // **Purely I/O** — dropping the socket happens in `finish_command`.
        Command::Reconnect => {}
        Command::Config(Some(action)) => match action {
            crate::command::ConfigAction::Dir(access) => {
                state.config.dir_access = *access;
                state.timeline.say(state.lang.config_dir_changed(*access));
            }
            crate::command::ConfigAction::Lang(lang) => {
                state.lang = *lang;
                state.timeline.say(lang.lang_changed().to_string());
            }
            crate::command::ConfigAction::Theme(theme) => {
                state.config.theme = *theme;
                state.timeline.say(state.lang.config_theme_changed(*theme));
            }
            crate::command::ConfigAction::Mode(mode) => {
                state.config.default_mode = *mode;
                state.timeline.say(state.lang.config_mode_changed(*mode));
            }
        },
        // Letting an unknown one pass quietly means getting it wrong again next time. Say
        // what does exist alongside.
        Command::Unknown(what) => {
            // **A plugin's command is looked for before it is called unknown.** The parser knows
            // only the built-in table — anything a plugin adds arrives here, and this is where it
            // stops being a typo and starts being a command.
            //
            // What it does is send its prompt. A command is a prompt (`plugin::PluginCommand`), so
            // running one is typing what the plugin author wrote and pressing Enter — reusing the
            // ordinary send path rather than inventing a second way for text to reach the server.
            if let Some(found) = state.plugin_commands.iter().find(|c| c.name == *what) {
                let prompt = found.prompt.clone();
                if prompt.is_empty() {
                    state.timeline.say(state.lang.plugin_command_empty(what));
                    return Some(cmd);
                }
                state.input.take();
                state.input.insert_str(&prompt);
                state.submit_now = true;
                return Some(cmd);
            }
            state
                .timeline
                .say(state.lang.unknown_command(what, &crate::command::help_text(state.lang)));
        }
        // The ones that cannot be done here. The I/O side takes them.
        Command::Mcp(_)
        | Command::Skills
        | Command::Rules
        | Command::Agent(_)
        // Needs the server — `finish_command` finishes it.
        | Command::Plugin(_)
        | Command::Account(_)
        | Command::Github(_)
        | Command::Changes
        | Command::Undo
        // `/status` only touches the session, which lives on the I/O side (`finish_command`).
        | Command::Status => {}
    }
    Some(cmd)
}

/// What `/jobs` shows.
fn jobs_text(jobs: &[JobRow], lang: crate::lang::Lang) -> String {
    if jobs.is_empty() {
        return lang.jobs_none().to_string();
    }
    let now = Instant::now();
    let mut out = String::from(lang.jobs_header());
    for job in jobs {
        let secs = now.saturating_duration_since(job.since).as_secs();
        out.push_str(&lang.jobs_row(&job.id, &job.label, secs));
    }
    out.push_str(lang.jobs_hint());
    out
}

/// How bad a notice is. **The activity line paints the two differently.**
///
/// Without it, every notice was one colour — including errors — so a failure looked exactly like
/// "connected" on the one line whose whole job is to say what is happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Severity {
    /// Something worth mentioning that is not wrong.
    #[default]
    Notice,
    /// Something went wrong.
    Error,
}

/// The pieces `/status` shows, gathered once so the text and the panel tell the
/// same story.
fn status_info<'a>(state: &'a State, session: &'a Session) -> crate::lang::StatusInfo<'a> {
    crate::lang::StatusInfo {
        session_id: session.id(),
        project: session.project(),
        agent: &state.agent,
        mode: state.mode.label(state.lang),
        cwd: &state.cwd,
        usage: &state.usage,
        pending: session.pending_open(),
    }
}

// ---------------------------------------------------------------------------
// From here on is the one and only I/O place. Everything above is pure.
// ---------------------------------------------------------------------------

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossterm::event::{Event as TermEvent, EventStream, MouseButton, MouseEventKind};
use crossterm::execute;
use futures_util::StreamExt;
use tokio::sync::mpsc;
// The trait has to be in scope to call its methods.
use zyris_attacca::{AttaccaApi, AttaccaApiClient};

use crate::conn::{frame_from, Session};
use crate::widgets;

/// Minimum interval between draws. Even with deltas pouring in character by character, they
/// are merged and drawn per frame.
///
/// **On a slow link like ssh this value is exactly what breaks the screen.** Drawing at 16ms
/// (60fps) means the link cannot carry every frame while an answer streams fast, and the next
/// frame overprints a half-arrived screen and tears it. 20fps is smooth enough for the eye,
/// so give the slack to the link.
///
/// `ZYRIS_CODE_FPS` changes it — raise it for a smoother look on a local terminal.
fn frame_interval() -> Duration {
    let fps: u64 = std::env::var("ZYRIS_CODE_FPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|f| (1..=120).contains(f))
        .unwrap_or(20);
    Duration::from_millis(1000 / fps)
}

/// Minimum interval between redraws while a turn is running.
///
/// Measured, the bytes one frame puts on the wire differ **by a factor of a thousand
/// depending on what changed** (`tests/perf.rs::measure_bytes_on_the_wire`).
///
/// | What | Bytes out |
/// |---|---|
/// | Nothing changed | 32 B |
/// | One character typed | 64 B |
/// | One streaming chunk | 3.4 KB |
/// | Full redraw | 21 KB |
///
/// So **we do not slow everything down.** Key input draws right where it was pressed
/// (64 B), and only the inflow of an answer is batched at this interval. Streaming is far
/// faster than a human hand, so batching here halves the volume without being noticed.
const STREAM_MIN_GAP: Duration = Duration::from_millis(100);

/// How often usage and title are asked for again. Asking every frame would hammer the server.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// How long after focus returns a left-press is taken to be the window-activating click
/// rather than a click meant for the transcript.
///
/// The activating click arrives in the same input batch as the focus event, so this only has
/// to cover that gap — nobody focuses a window and deliberately hits a fold inside a quarter
/// of a second.
const FOCUS_CLICK_GRACE: Duration = Duration::from_millis(250);

/// App loop watchdog. The loop bumps a progress counter at this interval, and if nothing
/// comes up this long after the last progress the screen is restored and the process ends —
/// structurally removing the "uncontrollable" state where the loop is stuck on a block and
/// cannot even receive a signal.
const LOOP_WATCHDOG_INTERVAL: Duration = Duration::from_secs(10);
const LOOP_WATCHDOG_STALL: Duration = Duration::from_secs(60);

/// How often (in milliseconds) the screen is redrawn whole to **heal itself**. 0 turns it
/// off; the default is 2 seconds.
///
/// ratatui **writes nothing into the cell behind a wide character.** Measured, this is what
/// goes on the wire (`tests/perf.rs::measure_what_goes_out_behind_a_wide_character`):
///
/// ```text
/// ESC[1;1H한  ESC[1;3H글  ESC[1;5H……
///          └ It skips to column 2. Column 1 never gets anything at all.
/// ```
///
/// It is an optimization resting on the belief that the terminal **paints a wide character
/// across two cells.** On a terminal that does not honour that belief — the kind that
/// reserves two cells but draws the glyph in only one — the old character in the trailing
/// cell shows through. That is the structural reason for the leftover glyph crumbs on an SSH
/// screen. Worse, that cell counts as "unchanged" in our buffer, so **it is never drawn
/// again.**
///
/// Within this model the only fix is **to clear and draw from scratch.** So we do it now and
/// then. Even someone who does not know Ctrl+L gets a clean screen a few seconds later. If
/// nothing on screen changed there is nothing new to break, so it is skipped. At 21KB a time
/// it is not a burden even done often — if the crumbs bother you lower it
/// (`ZYRIS_CODE_HEAL_MS=300`), and if it looks like flicker raise it.
fn heal_interval() -> Option<Duration> {
    // **The default is 2 seconds.** With a policy that lets the terminal use its own
    // background (`theme::page_bg`), the ratatui diff's protection that erases the trailing
    // cell behind a wide character (`previous.bg != Reset`) never fires — so that cell stays
    // "unchanged" in our buffer and is never erased. Periodically **overwriting** the whole
    // screen (`force_update`) clears that residue.
    //
    // This used to **clear** and redraw every 2 seconds, which looked like periodic flicker
    // on a slow SSH link. `AlwaysUpdate` does not clear; it just puts the same contents out
    // again, so it does not flicker — it uses the terminal "overwriting the same cell" as
    // the residue removal. If it looks like flicker raise it
    // (`ZYRIS_CODE_HEAL_MS=4000`), and to turn it off give 0.
    let ms: u64 = std::env::var("ZYRIS_CODE_HEAL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|ms| *ms <= 3_600_000)
        .unwrap_or(2000);
    // Set too short, the gap between clearing and redrawing widens and looks like flicker.
    (ms > 0).then(|| Duration::from_millis(ms.max(50)))
}

/// The sequence that changes the terminal window title.
fn set_terminal_title(title: &str) {
    use std::io::Write;
    let mut out = io::stdout();
    let _ = write!(out, "\x1b]2;{}\x07", title_for_osc(title));
    let _ = out.flush();
}

/// A title that is safe to put inside an OSC.
///
/// The title is written by the server — that is, **text we did not write.** One control
/// character mixed in cuts the sequence short there and dumps the rest onto the screen as
/// literal text. The length is cut too.
fn title_for_osc(title: &str) -> String {
    title.chars().filter(|c| !c.is_control()).take(120).collect()
}

/// Opens a URL in the OS's default browser, spawned so it never blocks the draw loop.
///
/// This is the emulator-independent half of link opening: the transcript also wraps link
/// cells in OSC 8, but with mouse capture on some terminals (Alacritty) forward the
/// Ctrl+click to the app instead of opening the hyperlink themselves (alacritty#8129). So
/// the app opens it itself, and Ctrl+click behaves the same in every emulator.
fn open_url(url: &str) {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, url, "could not open link");
    }
}

/// How long we wait for the answer to the kitty keyboard protocol support question.
///
/// A terminal that does not know the question never answers it, so silence for this long
/// means no support. If the answer arrives later than this on a slow SSH link, crossterm
/// quietly skips the leftover bytes (the public `Event` has no keyboard-flags variant) —
/// the app does not break.
const KITTY_PROBE_TIMEOUT: Duration = Duration::from_millis(150);

/// Asks **at startup** whether the terminal supports the kitty keyboard protocol.
///
/// Shift+Enter only arrives distinguishable from Enter (CSI-u) on a terminal with the
/// protocol on. A terminal without it sends Shift+Enter as the very same single `\r` byte
/// as Enter, so there is no way to tell them apart from the bytes the app receives. Send
/// `CSI ? u`, and a `CSI ? <flags> u` answer means a supporting terminal — only a terminal
/// that knows the protocol can answer that question.
///
/// **This reads stdin directly rather than going through crossterm.** At this point the
/// event source is not open yet (`EventStream` is created in `run_inner`), so there is no
/// competitor. If the answer arrives late, crossterm quietly skips it, so it is safe.
fn probe_kitty_keyboard() -> bool {
    #[cfg(unix)]
    {
        use std::io::{Read, Write};
        use std::os::fd::AsRawFd;

        let mut out = io::stdout();
        if write!(out, "\x1b[?u").is_err() || out.flush().is_err() {
            return false;
        }

        let fd = io::stdin().as_raw_fd();
        let deadline = Instant::now() + KITTY_PROBE_TIMEOUT;
        let mut resp = Vec::with_capacity(16);
        let mut byte = [0u8; 1];
        while Instant::now() < deadline && resp.len() < 32 {
            let left = deadline.saturating_duration_since(Instant::now());
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // 0 = timed out, <0 = error. Either way it means "not a supporting terminal".
            if unsafe { libc::poll(&mut pfd, 1, left.as_millis() as libc::c_int) } <= 0 {
                break;
            }
            match io::stdin().read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    resp.push(byte[0]);
                    // The answer ends with `u`. Even read in pieces, the last byte decides.
                    if resp.ends_with(b"u") {
                        break;
                    }
                }
            }
        }
        kitty_probe_ok(&resp)
    }
    #[cfg(not(unix))]
    {
        // The Windows console carries modifiers in the input record as-is, so Shift+Enter
        // arrives as Enter+SHIFT even without the protocol — count it as supported. (We do
        // not send the question, since the answer could leak in as input.)
        true
    }
}

/// Is the answer to `CSI ? u` one from a supporting terminal? The answer format is
/// `CSI ? <flags> u`.
///
/// The flags value itself is not examined — we just pushed flag 1 in, and the answer returns
/// that current state, so accepting on format alone yields no false positives. Even off the
/// format check, "an answer came at all" is itself evidence of support. (A terminal that
/// does not know it never answers.)
fn kitty_probe_ok(resp: &[u8]) -> bool {
    // It is found even with characters the user typed mixed in front — typing right at
    // startup is not rare.
    let i = resp.windows(3).position(|w| w == b"\x1b[?");
    i.is_some_and(|i| {
        let tail = &resp[i + 3..];
        !tail.is_empty()
            && tail.ends_with(b"u")
            && tail[..tail.len() - 1].iter().all(|b| b.is_ascii_digit() || *b == b';' || *b == b':')
    })
}

/// The one and only I/O place.
///
/// `ratatui::init()` takes raw mode and the alternate screen together — do not take them
/// again by hand. Only mouse capture is turned on separately. However it ends, not restoring
/// leaves the shell broken, so the body lives in its own function and recovery happens
/// regardless of the result.
/// Turn one terminal feature on or off. **A failure must not kill the screen.**
///
/// Features are a bonus — when one fails, only that feature is lost and the app
/// continues. The reason is left in the log only, so it can be found later why the
/// screen came up without that feature.
///
/// **Why one at a time:** on Windows, crossterm 0.29's kitty keyboard protocol
/// commands (`PushKeyboardEnhancementFlags`·`PopKeyboardEnhancementFlags`) are blocked
/// on the ANSI path (`is_ansi_code_supported() == false`) and die with `Unsupported`.
/// Bundled in one `execute!`, that single command **kills the whole screen** — with no
/// screen, instead of the first enrollment's code window the shell's waiting line
/// ("approve in the browser and it continues…") is all you see. Restoring in one chunk
/// would also leave a trailing `EnableLineWrap` un-sent, so the shell's line wrap stays
/// off.
fn terminal_feature(label: &'static str, command: impl crossterm::Command) {
    if let Err(e) = execute!(io::stdout(), command) {
        tracing::warn!(label, error = %e, "terminal feature change failed — continuing without that feature");
    }
}

/// The only place that talks to the terminal.
///
/// `ratatui::init()` takes raw mode and the alternate screen together — we don't take them again.
pub async fn run(
    api_rx: ApiRx,
    bridge: crate::tools::bridge::Bridge,
    die: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    // Turn terminal features on **one at a time** — if any one fails, the screen still
    // comes up (see `terminal_feature`). On Windows the kitty keyboard protocol always
    // fails, so the line-wrap-off below must still be reached.
    terminal_feature("mouse capture", crossterm::event::EnableMouseCapture);
    // Coming back from another window, the terminal sometimes does not restore the
    // screen for us. We have to know focus came back to redraw the whole thing.
    terminal_feature("focus change", crossterm::event::EnableFocusChange);
    // **Receive a paste as one chunk.** With it off, the terminal lets a paste through
    // as if it were typed, the newlines in the content get read as Enter, and the first
    // line of a multi-line prompt gets sent as-is. With it on, the paste is wrapped in
    // ESC[200~…ESC[201~ and arrives as one `Event::Paste`.
    terminal_feature("bracketed paste", crossterm::event::EnableBracketedPaste);
    // **Makes Shift+Enter distinguishable from Enter.** With the kitty keyboard protocol
    // on, modified keys arrive separately as CSI-u sequences. A terminal that does not
    // know it simply ignores this, so turning it on does no harm. On Windows crossterm
    // blocks the command and it fails (see the `terminal_feature` comment) — then
    // Shift+Enter just sends instead of newlining, and Alt+Enter newline and paste-burst
    // detection cover that spot.
    terminal_feature(
        "kitty keyboard protocol",
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
        ),
    );
    // **Turn off line wrapping.** If the width we counted and the width the terminal
    // draws differ by even one cell (glyphs like `●`, `·`, `─` become two cells
    // depending on terminal settings) the end of the line overflows and **spills onto
    // the line below**, pushing everything under it down. With it off, the overflowing
    // glyph is merely cut at that line, keeping the damage to one line.
    terminal_feature("line wrap off", crossterm::terminal::DisableLineWrap);

    // **Find out now whether this terminal takes Shift+Enter as a newline.** On a terminal
    // without support we point at Alt+Enter from the start — otherwise the user does not
    // know how to make a newline and just fires the message off.
    let kitty = probe_kitty_keyboard();

    let for_exit = bridge.clone();
    let result = run_inner(&mut terminal, api_rx, bridge, die, kitty).await;

    // **Leave no orphans.** When the app ends, background jobs end with it — left alive, a
    // cargo on this machine keeps eating RAM. `/quit` and Ctrl+C both funnel into the same
    // `break 'app`, so the way out is this one place.
    if let Some(jobs) = for_exit.jobs() {
        jobs.stop_all();
    }

    // Turn off one by one. Restoring also tolerates failure — if one command dies
    // (on Windows `PopKeyboardEnhancementFlags` fails), the ones after it must still go
    // out, or line wrap stays off in the shell and long lines look cut.
    terminal_feature("mouse capture off", crossterm::event::DisableMouseCapture);
    terminal_feature("focus change off", crossterm::event::DisableFocusChange);
    terminal_feature("bracketed paste off", crossterm::event::DisableBracketedPaste);
    terminal_feature("kitty keyboard protocol off", crossterm::event::PopKeyboardEnhancementFlags);
    // Line wrapping is something the shell uses. Not restoring it makes long commands
    // look cut off in the shell.
    terminal_feature("line wrap on", crossterm::terminal::EnableLineWrap);
    ratatui::restore();
    result
}

/// The channel carrying the handle to the currently live connection.
///
/// When the connection drops and comes back, the handle changes. The app **takes the latest
/// one** from here every time — holding on to the first means every call after a reconnect
/// goes out on a dead connection.
pub type ApiRx = tokio::sync::watch::Receiver<Option<Arc<AttaccaApiClient>>>;

/// What rides the screen channel. `Some(session id)` is a frame from that session's turn
/// stream; `None` is something from off-screen (tools, the bridge).
///
/// **Frames from a stale session are dropped by the receiver.** Leave work running and
/// switch to another session, and the previous session's turn keeps running on the server
/// with its stream still sending frames — without the tag, the previous session's messages
/// mix into the timeline of the session being viewed. That actually happened.
pub type AppMsg = (Option<Origin>, Action);

/// Where a frame came from. `None` in `AppMsg` means off-screen work that belongs to the window
/// rather than to a conversation — git, the tool bridge — and always passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The session it is about.
    pub session: String,
    /// Which opening of that session's turn stream it came from. `None` for a one-shot answer
    /// (history, a title) — those are replies to a request, not a subscription that can pile up.
    pub stream: Option<u64>,
}

impl Origin {
    /// A one-shot answer about a session.
    pub fn asked(session: impl Into<String>) -> Origin {
        Origin { session: session.into(), stream: None }
    }
}

/// Is this frame from the conversation on screen, and from its live stream?
///
/// **Two ways to be stale, not one.** The session can have been left, and — because
/// `turn_events` is a subscription that never ends on its own — an older stream on the *same*
/// session can still be talking. Aborting stops a task sending more, but what it already put in
/// the channel is still queued behind this, and `push_delta` appends: a doubled frame doubles the
/// words being streamed.
fn frame_is_current(from: &Option<Origin>, current: Option<&str>, gen: u64) -> bool {
    match from {
        None => true,
        Some(o) => current == Some(o.session.as_str()) && o.stream.is_none_or(|s| s == gen),
    }
}

/// The current handle. `None` if not attached yet.
fn api_of(rx: &ApiRx) -> Option<Arc<AttaccaApiClient>> {
    rx.borrow().clone()
}

/// Clears the screen and makes **the next frame draw in full.**
///
/// **We do not use `Terminal::clear()`.** That one asks the terminal where the cursor is
/// (DSR `ESC[6n`) before clearing and **waits synchronously** for the answer. On a terminal
/// that does not answer it stops right there, and even when it does answer it steals those
/// bytes from our key input stream. A remote terminal is exactly the place where the answer
/// can be late or absent — the pty smoke test is precisely that situation, and the app
/// really did hang.
///
/// `resize` does the same thing (clear + full redraw on the next frame) without asking
/// anything.
fn repaint(terminal: &mut ratatui::DefaultTerminal) {
    use ratatui::backend::Backend;

    if let Ok(size) = terminal.backend().size() {
        let _ = terminal.resize(ratatui::layout::Rect::new(0, 0, size.width, size.height));
    }
}

/// Kicks off one `git status` in the background and sends the answer back as a frame.
///
/// **Both loops in `run_inner` go through this one definition.** The pre-connection loop draws a
/// screen too, so the strip has to be right from the first frame; two copies of "how to read
/// git" would drift the way `/mode` once drifted from Shift+Tab.
///
/// Returns without doing anything if a read is already out. **Runs must not overlap** — on a
/// slow checkout the previous call may still be running when the next tick lands.
fn spawn_git(
    cwd: &std::path::Path,
    tx: &mpsc::UnboundedSender<AppMsg>,
    busy: &Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    if busy.swap(true, Ordering::SeqCst) {
        return;
    }
    let cwd = cwd.to_path_buf();
    let tx = tx.clone();
    let busy = Arc::clone(busy);
    tokio::spawn(async move {
        let got = crate::repo::read(&cwd).await;
        busy.store(false, Ordering::SeqCst);
        // No session tag — this is about the machine, not the conversation, so it must survive
        // a session switch (`frame_is_current`).
        let _ = tx.send((None, Action::Frame(Frame::Git(got))));
    });
}

async fn run_inner(
    terminal: &mut ratatui::DefaultTerminal,
    mut api_rx: ApiRx,
    bridge: crate::tools::bridge::Bridge,
    mut die: tokio::sync::watch::Receiver<bool>,
    kitty: bool,
) -> anyhow::Result<()> {
    let mut state = State::new();
    // **The saved settings come in here.** `State::default` keeps the built-in defaults so
    // tests stay deterministic; this is the one place the disk is read.
    state.config = crate::config::Config::load();
    // **Before the first frame.** The palette decides every colour drawn from here on, and a
    // frame drawn in the other one flashes as it is corrected.
    crate::theme::set(state.config.theme.resolve());
    if let Some(mode) = state.config.default_mode {
        state.mode = mode;
    }
    // **Read once, here.** The `/` list is built on every keystroke, and reading the plugin
    // directories from the draw loop would put disk access on the typing path.
    state.plugin_commands = crate::plugin::commands(&crate::plugin::discover(&state.cwd));

    // **Attach the screen first.** The enrollment code window has to reach it even before
    // the first connection — `enroll::ScreenEnroll` sends `Frame::Enroll` here.
    let (tx, mut rx) = mpsc::unbounded_channel::<AppMsg>();

    bridge.attach(tx.clone());
    bridge.sync(state.mode, &state.config);

    // Off means the branch is simply never polled — the strip then shows the path alone.
    let git_every = crate::repo::poll_interval();
    let git_on = git_every.is_some();
    // **Runs must not overlap.** On a slow checkout the previous `git status` may still be out
    // when the next tick lands, and stacking them would put several gits on a four-thread
    // machine at once.
    let git_busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // **Ask once before the wait loop.** That loop draws a screen too — first enrollment sits
    // in it for as long as it takes someone to walk to a browser — and a strip that only fills
    // in after connecting would read as "this machine has no git".
    if git_on {
        spawn_git(&state.cwd, &tx, &git_busy);
    }

    let mut keys = EventStream::new();
    let mut ticker = tokio::time::interval(frame_interval());
    let mut last_size: Option<(u16, u16)> = None;
    let mut dirty = true;

    // **Wait for the first handle while drawing the screen.** This stretch is the first
    // enrollment — the enrollment code window comes up over "connecting…". `on_connect`
    // sends the handle.
    let api = loop {
        if let Some(api) = api_of(&api_rx) {
            break api;
        }
        if *die.borrow() {
            anyhow::bail!(crate::lang::current().connection_lost());
        }
        tokio::select! {
            result = api_rx.changed() => {
                // The sender is gone — the runner ended. Stopping on a path other than
                // `die` too is what makes it close quietly.
                if result.is_err() {
                    anyhow::bail!(crate::lang::current().connection_lost());
                }
            }
            _ = die.changed() => {}
            Some((sid, action)) = rx.recv() => {
                // There is no session yet (first enrollment). A stream frame arriving now
                // is a stale one.
                if !frame_is_current(&sid, None, 0) {
                    continue;
                }
                apply(&mut state, &action);
                dirty = true;
            }
            Some(Ok(ev)) = keys.next() => {
                let mut quit = false;
                match ev {
                    TermEvent::Key(k) => {
                        for action in on_key(&state, k) {
                            if matches!(action, Action::Quit) {
                                quit = true;
                                break;
                            }
                            apply(&mut state, &action);
                        }
                    }
                    TermEvent::Paste(text) => {
                        apply(&mut state, &Action::Paste(text));
                    }
                    TermEvent::Resize(w, h) if last_size != Some((w, h)) => {
                        last_size = Some((w, h));
                        repaint(terminal);
                    }
                    _ => {}
                }
                if quit {
                    // There is no session and no turn yet — just close.
                    return Ok(());
                }
                // **The gate has to hear about this loop's keys too.** First enrollment sits
                // here for as long as it takes someone to walk to a browser, and Shift+Tab
                // works the whole time — but `last_mode` is only seeded after this loop ends,
                // so the main loop's edge check can never fire for a mode changed in here.
                // The bottom bar would say one mode while the gate went on using another, and
                // no later keypress would ever reconcile them.
                //
                // The main loop's full block also stages work/job sessions; there is nothing to
                // stage yet, so carrying the decision material is the whole job here.
                if std::mem::take(&mut state.config_out) {
                    state.config.save();
                    crate::lang::set(state.lang);
                    crate::lang::save(state.lang);
                    crate::theme::set(state.config.theme.resolve());
                }
                bridge.sync(state.mode, &state.config);
                dirty = true;
            }
            _ = ticker.tick() => {
                state.tick = state.tick.wrapping_add(1);
                // With the enrollment code window up, the time left is ticking down, so
                // keep drawing.
                if state.enroll.is_some() {
                    dirty = true;
                }
            }
        }
        if dirty {
            terminal.draw(|f| widgets::draw(f, &mut state))?;
            dirty = false;
        }
    };

    let mut api = api;
    state.connected = true;
    // **Keep "connecting…" from freezing on the screen.** Always redraw once attached —
    // the last frame the wait loop drew is still pre-connection, and with `dirty` left off
    // "connecting…" stays until some key is pressed (it really did stay).
    // "connected" shows briefly, then drops to `idle` after 6 seconds. On a terminal that
    // cannot receive Shift+Enter we announce the alternative instead of the connection
    // notice — letting it go quietly leaves the user firing off a message with no idea why
    // a newline does not work.
    dirty = true;
    state.set_status(if kitty {
        state.lang.connected()
    } else {
        state.lang.kitty_shift_enter_hint()
    });

    // The person approving may have given less than was requested. **Then lists come back
    // quietly empty** — not an error but an empty result, so the user thinks their account
    // has no agents.
    //
    // The scopes fixed at approval never widen later. No amount of token refreshing changes
    // them. So what to say here is not "these are missing" but **what to do about it.**
    // **Do not wait forever on a dead connection.** If `me()` passes the deadline we treat
    // the server as dead and skip the credential check — the screen has to come up anyway.
    if let Ok(me) = crate::conn::within(&api, api.me()).await {
        // Look at everything we request. This used to look at only three, so when
        // `agents:read` or `projects:read` was missing the list was just empty and nothing
        // was said.
        let missing = crate::conn::missing_scopes(&me.scopes);
        if !missing.is_empty() {
            // **Ask again once.** Upstream has no `renew_when_scopes_missing`, so we do it:
            // discarding the credential means the next launch shows a clean enrollment code
            // with no screen in the way. Where the user supplied the token by hand there is
            // no credential to discard, so we only say so.
            let reauth = bridge.reauth();
            let tried = reauth.as_ref().is_none_or(|r| r.spent());
            let asked_again = crate::conn::needs_reenrollment(&me.scopes, tried)
                && match &reauth {
                    Some(r) => r.discard_once().await,
                    None => false,
                };
            let said = if asked_again {
                crate::conn::scopes_will_be_asked_again(&missing)
            } else {
                crate::conn::missing_scopes_message(&missing)
            };
            state.set_status(said.clone());
            state.timeline.say(said);
        }
    }

    state.agent = crate::conn::agent_name();
    let mut agent_id = match Session::agent_id(&api).await {
        Ok(id) => id,
        Err(e) => {
            state.set_error(e.to_string());
            String::new()
        }
    };
    // The skill list was decided by `tools::announce` and put on the bridge.
    let mut session = Session::new(bridge.preamble());
    // The previous value, held to see whether the mode **changed**. Looking at `state.mode`
    // alone is true for as long as we stay in the mode, which revives the work/job staging
    // on every message.
    let mut last_mode = state.mode;

    // If a question is still awaiting an answer, go into that session. If it was not
    // answered before quitting, the server is still waiting, and having to hunt for it by
    // hand is as good as having no way to answer.
    if let Some(id) = crate::conn::session_awaiting_answer(&api).await {
        switch(&mut state, &mut session, id, None, &api, &tx);
    }
    // **Exactly one event stream is alive at a time.** This used to build a second one that
    // merely *shadowed* the pre-connection loop's — and shadowing does not drop the first, so
    // both stayed alive with a crossterm reader thread each, competing for the same terminal
    // input. Keystrokes then land in whichever one wins the race and the loser's are read by
    // nobody: that is what a key arriving late, or not at all, looks like.
    //
    // The old one is dropped explicitly rather than reused, because the two loops are separate
    // `select!`s — handing a stream from one to the other leaves its in-flight read to be
    // cancelled and re-polled by a different waker.
    drop(keys);
    let mut keys = EventStream::new();
    let mut ticker = tokio::time::interval(frame_interval());
    let mut poll = tokio::time::interval(POLL_INTERVAL);
    let mut git = tokio::time::interval(git_every.unwrap_or(Duration::from_secs(86400)));
    // When the last key event happened — the basis for detecting a paste burst on a
    // terminal without bracketed paste.
    let mut last_key_at: Option<Instant> = None;
    // When focus last came back. The click that restored it must not act on the transcript.
    let mut focus_back_at: Option<Instant> = None;
    // When off, leave it as a timer that never fires. The point is not to add another
    // `select!` arm.
    let mut heal = tokio::time::interval(heal_interval().unwrap_or(Duration::from_secs(86400)));
    let healing = heal_interval().is_some();
    // Has the screen been touched since the last self-heal? Not having drawn means nothing
    // new can have broken.
    let mut drew_since_heal = false;
    let mut last_draw = Instant::now();
    set_terminal_title(&state.title);
    let mut shown_title = state.title.clone();
    // An armed quit releases on its own once time passes. But with no input there is nothing
    // to redraw, so the notice text stays on screen — catch the frame where it releases and
    // draw once more.
    let mut last_quit_pending = false;
    let mut last_had_status = false;
    let mut shutdown = shutdown_signals();
    // **If the loop stalls, nobody finds out.** An await on a dead connection is released by
    // its deadline, but other blocking (a stuck terminal write, say) can remain. Then keys
    // and signals alike need a live loop to reach anything, and with the loop dead nothing
    // works at all. A separate task watches the progress counter and, if it has been stalled
    // a long time, restores the screen and ends the process — so an "unkillable" state
    // cannot arise structurally.
    let progress = Arc::new(AtomicU64::new(0));
    let watchdog_progress = Arc::clone(&progress);
    let watchdog = tokio::spawn(async move {
        loop {
            tokio::time::sleep(LOOP_WATCHDOG_INTERVAL).await;
            let seen = watchdog_progress.load(Ordering::Relaxed);
            tokio::time::sleep(LOOP_WATCHDOG_STALL).await;
            if watchdog_progress.load(Ordering::Relaxed) == seen {
                tracing::error!(
                    "the app loop stalled for {}s — restoring the screen and ending",
                    LOOP_WATCHDOG_STALL.as_secs()
                );
                ratatui::restore();
                std::process::exit(1);
            }
        }
    });

    'app: loop {
        progress.fetch_add(1, Ordering::Relaxed);
        tokio::select! {
            Some(Ok(ev)) = keys.next() => {
                let actions = match ev {
                    TermEvent::Key(k) => {
                        // **Rescue a paste burst as newlines.** A terminal without
                        // bracketed paste (mobile Termius and the like) lets a paste
                        // through as keys arriving in rapid succession. When keys arrive
                        // at an interval no human can type at (< PASTE_BURST), an Enter
                        // among them is a newline, not a submit — otherwise the first line
                        // of a multi-line prompt goes out on its own.
                        let now = Instant::now();
                        let in_burst = last_key_at
                            .is_some_and(|t| now.duration_since(t) < PASTE_BURST);
                        last_key_at = Some(now);
                        if enter_becomes_newline(&state, &k, in_burst) {
                            // An Enter that arrived mid-burst is a newline — and the
                            // decision is `enter_becomes_newline`'s, so an approval,
                            // list, form or question keeps its confirm key.
                            vec![Action::Insert('\n')]
                        } else {
                            on_key(&state, k)
                        }
                    }
                    // A paste is not a key — it goes in as one chunk, not split on Enter.
                    TermEvent::Paste(text) => vec![Action::Paste(text)],
                    // **The click that gave the window focus back is not a click in the app.**
                    // On Windows the activating click is delivered to us as well, and a press
                    // with no movement toggles the fold of whatever card head it lands on and
                    // drops the selection — so alt-tabbing back could silently unfold a card
                    // above the viewport and slide old text into view. Swallowing the press
                    // makes the release a no-op too, since it needs a drag to act on.
                    TermEvent::Mouse(m)
                        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
                            && focus_back_at
                                .is_some_and(|t: Instant| t.elapsed() < FOCUS_CLICK_GRACE) =>
                    {
                        focus_back_at = None;
                        vec![]
                    }
                    TermEvent::Mouse(m) => match m.kind {
                        MouseEventKind::ScrollUp => vec![Action::Wheel(1)],
                        MouseEventKind::ScrollDown => vec![Action::Wheel(-1)],
                        MouseEventKind::Down(MouseButton::Left) => {
                            // **A Ctrl+click opens a link instead of starting a drag.** The
                            // terminal is expected to open OSC 8 hyperlinks on Ctrl+click —
                            // but with mouse capture on, Alacritty forwards the click to the
                            // app rather than opening it (alacritty#8129). Opening it here
                            // makes the feature work in every emulator identically.
                            if m.modifiers.contains(KeyModifiers::CONTROL) {
                                if let Some(url) = state.link_at(m.column, m.row) {
                                    vec![Action::OpenLink(url)]
                                } else {
                                    vec![Action::Press(m.column, m.row)]
                                }
                            } else {
                                vec![Action::Press(m.column, m.row)]
                            }
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            vec![Action::DragTo(m.column, m.row)]
                        }
                        MouseEventKind::Up(MouseButton::Left) => vec![Action::Release],
                        _ => vec![],
                    },
                    // On regaining focus, redraw but **do not clear.** The focus event
                    // arrives every time the keyboard opens and closes on mobile SSH
                    // (Termius) — clearing everything each time makes the screen flash.
                    //
                    // **Overwrite instead.** `force_update` puts every cell out again without
                    // clearing first, so a terminal that did not restore our screen while we
                    // were away gets it back with no flash. That is the same trick the
                    // periodic self-heal uses.
                    TermEvent::FocusGained => {
                        focus_back_at = Some(Instant::now());
                        state.force_update = true;
                        dirty = true;
                        vec![]
                    }
                    // Clear and redraw whole only when the size actually changed. With
                    // same-size resizes arriving back to back, clearing everything each
                    // time flickers.
                    //
                    // **The payload is not the geometry on Windows.** crossterm reports the
                    // console *screen-buffer* size there, while ratatui lays out with the
                    // *window* rect — two different quantities that drift apart. Deduping on
                    // the payload therefore skips repaints that were needed and performs ones
                    // that were not. Ask the backend what it is about to draw into instead.
                    TermEvent::Resize(..) => {
                        use ratatui::backend::Backend;
                        let now = terminal.backend().size().ok().map(|s| (s.width, s.height));
                        if now.is_some() && last_size != now {
                            last_size = now;
                            repaint(terminal);
                        }
                        dirty = true;
                        vec![]
                    }
                    _ => vec![],
                };
                // Whether this event asked for anything at all. The draw at the end of the arm
                // is keyed on it — see the comment there.
                let acted = !actions.is_empty();
                for action in actions {
                    match &action {
                        // `run` restores the screen, and **the turn running on the server is
                        // stopped below this.**
                        Action::Quit => break 'app,
                        // **A Ctrl+click on a link opens it in the OS browser.** Opening is
                        // I/O, so it happens here — `apply` ignores the action. Spawned so
                        // the loop is not blocked on the browser.
                        Action::OpenLink(url) => {
                            open_url(url);
                        }
                        // **While work is running we do not send here.** `apply` puts it on
                        // the queue and `flush_queue` below sends them in order when the
                        // turn ends. `apply` runs after this match, so the `running` seen
                        // here is still the old value.
                        // Slash commands never go to the server. `apply` puts them in
                        // `command_out` and we run them below this match.
                        Action::Submit(text) if crate::command::is_command(text) => {}
                        Action::Submit(_) if state.running => {}
                        Action::Submit(text) => {
                            if agent_id.is_empty() {
                                state.set_error(state.lang.agent_cannot_send());
                            } else {
                                send_and_tell(&api, &mut state, &mut session, &agent_id, text, &tx)
                                    .await;
                            }
                        }
                        // **The list opens empty and fills itself.** Fetching it here would
                        // hold keys and drawing until the server answered, so the box goes up
                        // saying "loading…" and a task sends the rows in.
                        Action::OpenPicker => spawn_projects(&api, state.lang, &tx),
                        // All of "back" is decided here — from the session level, go back to
                        // the project list; at the project level, close.
                        Action::PickBack => {
                            let in_sessions = matches!(
                                state.picker.as_ref().map(|p| &p.level),
                                Some(crate::picker::Level::Sessions { .. })
                            );
                            if in_sessions {
                                state.picker = Some(crate::picker::Picker::loading_projects());
                                spawn_projects(&api, state.lang, &tx);
                            } else {
                                state.picker = None;
                            }
                        }
                        Action::PickConfirm => {
                            pick(&api, &mut state, &mut session, &mut agent_id, &tx).await;
                        }
                        Action::Cancel => {
                            if let Some(id) = session.id() {
                                let _ = crate::conn::within(&api, api.cancel_turn(id.to_string()))
                                    .await;
                            }
                        }
                        // Only clears the screen. It is redrawn just below.
                        Action::Repaint => {
                            repaint(terminal);
                        }
                        // **Scrolling is drawn by the diff.** This used to clear and redraw
                        // whole to wipe wide-character crumbs, but now every cell has a
                        // background (`theme::bg()`) so crumbs cannot arise structurally and
                        // there is nothing to clear — clearing makes the screen flash.
                        Action::Wheel(_) => {}
                        _ => {}
                    }
                    apply(&mut state, &action);

                    // **The turn stream starts once the history is in.** It resumes from just
                    // past what was re-read, and that cursor exists only after the replay —
                    // opening it before would either re-deliver the whole thread or skip the
                    // events that arrived while it was loading.
                    if matches!(action, Action::Frame(Frame::History { .. })) {
                        if let Some(id) = session.id().map(str::to_string) {
                            spawn_stream(
                                Arc::clone(&api),
                                &mut session,
                                id,
                                state.last_cursor,
                                tx.clone(),
                            );
                        }
                    }

                    // **Selected text goes to the clipboard the moment the mouse is
                    // released.** There is no key to press — leaving Ctrl+C as the one stop
                    // key is less confusing when it matters. `apply` sets the range, so
                    // this has to come after it. Exporting is I/O, hence here. A terminal
                    // that does not know OSC 52 ignores it quietly, at no cost.
                    if matches!(action, Action::Release) {
                        if let Some(text) = &state.selection {
                            crate::clipboard::export(text);
                        }
                    }

                    // Slash commands. `run_command` finishes the pure part, and only what
                    // needs the server or the disk is finished here.
                    if let Some(text) = state.command_out.take() {
                        finish_command(
                            &api, &bridge, &mut state, &mut session, &mut agent_id, &text, &tx,
                        )
                            .await;
                        // `/quit`. The way out is the same as Ctrl+C — a running turn is
                        // stopped below.
                        if state.quitting {
                            break 'app;
                        }
                    }

                    // **The settings form's Enter reaches the disk and the gate here.**
                    // The exact three things `/config …` does in `finish_command` — one
                    // stopping short of any of them is how a setting changes on screen and
                    // nowhere else.
                    if std::mem::take(&mut state.config_out) {
                        state.config.save();
                        crate::lang::set(state.lang);
                        crate::lang::save(state.lang);
                        // The palette applies to the very next frame — the same promise the
                        // directory policy makes to the gate.
                        crate::theme::set(state.config.theme.resolve());
                        bridge.sync(state.mode, &state.config);
                    }

                    // **Creating a new project.** When the form takes Enter, it is created
                    // here — it is I/O, so `apply` cannot. On failure the reason is left on
                    // the form and the form stays — it has to be fixable and retryable.
                    if let Some((name, description)) = state.project_out.take() {
                        match crate::conn::create_project(&api, &name, Some(&description)).await {
                            Ok((id, name)) => {
                                // **Create it and go inside.** Having to pick it again after
                                // creating is doing the work twice, and it invites the
                                // accident of starting work somewhere other than what was
                                // just created.
                                session.enter_project(id);
                                state.project_name = Some(name.clone());
                                session.stage_new_default();
                                // Clear the previous session's turn state — moving to a new
                                // project must not leave "working" behind.
                                leave_session(&mut state);
                                state.new_project = None;
                                state.picker = None;
                                let said = state.lang.project_created(&name);
                                state.timeline.say(said);
                            }
                            Err(e) => {
                                if let Some(form) = &mut state.new_project {
                                    form.error = Some(e.to_string());
                                }
                            }
                        }
                    }

                    // **What the GitHub screen asked for.** Same shape as the project form: the
                    // pure side records the ask, this side does it, and the answer goes back onto
                    // the screen so it can be corrected and retried without reopening anything.
                    if let Some(ask) = state.github_out.take() {
                        run_github_ask(&mut state, ask).await;
                    }

                    // **Everything to do after a mode change is gathered here in one place.**
                    // There are two ways in (Shift+Tab through `apply`, `/mode` through the
                    // `finish_command` just above), and fixing them separately means one day
                    // only one gets fixed — `/mode` really was failing to reach the gate.
                    //
                    // **It only runs on the edge.** Running for as long as we stay in the
                    // mode would create one job per message in job mode, leaving no place to
                    // answer a follow-up question.
                    if state.mode != last_mode {
                        last_mode = state.mode;
                        bridge.sync(state.mode, &state.config);
                        restage(&state, &mut session);
                    }

                    // Pressing submit on a question sends right away — putting the answer in
                    // the input and making the user press Enter once more turns a submission
                    // into a draft.
                    if std::mem::take(&mut state.submit_now) {
                        let text = state.input.take();
                        if !text.is_empty() {
                            // **The third way a message leaves.** Answering a question card never
                            // builds an `Action::Submit`, so it misses the echo in `apply` — and
                            // an answer that vanishes on Enter is the same complaint.
                            state.timeline.echo(text.as_str());
                            send_and_tell(&api, &mut state, &mut session, &agent_id, &text, &tx)
                                .await;
                        }
                    }
                    // **Draw right where the key was pressed.** One frame is 64 bytes so
                    // there is nothing to save, and waiting for a tick makes the hand feel
                    // sluggish by exactly that much.
                    //
                    // **But only when the event asked for something.** On Windows mouse motion
                    // cannot be switched off — `EnableMouseCapture` turns on the whole console
                    // mouse mode — so merely sliding the pointer across the window delivers a
                    // `Moved` event per sample, each mapping to no action at all. Drawing for
                    // every one of them is a full frame per mouse sample, and that is what the
                    // flicker and the sluggishness are made of. `FocusGained` and `Resize`
                    // still redraw: they set `dirty` themselves.
                    if dirty || acted {
                        terminal.draw(|f| widgets::draw(f, &mut state))?;
                        dirty = false;
                        drew_since_heal = true;
                        last_draw = Instant::now();
                    }
                }
            }
            Some((sid, action)) = rx.recv() => {
                // **Drop frames from a stale session.** After switching the screen, the
                // previous session's messages must not keep coming up — leave work running
                // and talk in another session, and that work's events pour in here. The
                // turn keeps running on the server, and going back to that session shows it
                // all as history on reopen.
                if !frame_is_current(&sid, session.id(), session.stream_gen()) {
                    continue;
                }
                // **A poll that found nothing new must not wake the screen.** git is read every
                // few seconds; marking the frame dirty each time would redraw an idle terminal
                // forever for a value that did not move.
                if let Action::Frame(Frame::Git(got)) = &action {
                    if got == &state.repo {
                        continue;
                    }
                }
                apply(&mut state, &action);
                // If background polling changed the title, the window title follows. `switch`
                // changes state.title too, so both are watched here in one place.
                if state.title != shown_title {
                    set_terminal_title(&state.title);
                    shown_title = state.title.clone();
                }
                // The frame ending a turn arrives here — so this is also where the queue is
                // flushed.
                flush_queue(&api, &mut session, &agent_id, &mut state, &tx).await;
                dirty = true;
            }
            // Reconnect. Swap the handle in and reopen the stream for the session in view.
            Ok(()) = api_rx.changed() => {
                if let Some(fresh) = api_of(&api_rx) {
                    api = fresh;
                    state.connected = true;
                    // A drop and reattach shows on screen too — connecting → connected →
                    // idle.
                    state.set_status(state.lang.connected());
                    if let Some(id) = session.id().map(str::to_string) {
                        spawn_stream(
                            Arc::clone(&api),
                            &mut session,
                            id,
                            state.last_cursor,
                            tx.clone(),
                        );
                    }
                    dirty = true;
                }
            }
            // Usage and title do not arrive as events, so ask for them periodically.
            //
            // **The network runs outside the loop.** An await on a dead connection holds the
            // loop until the deadline (`within`) releases it, and keys and drawing stop
            // meanwhile — that is exactly where the screen stutters on a flaky network. Here
            // we only spawn a task and the result comes back as `Frame::Poll`. That is when
            // `dirty = true` is set and it gets drawn.
            // **git runs outside the loop, exactly like `poll`.** `read` shells out to a
            // process; awaiting that here would stall keys and drawing for as long as it takes.
            _ = git.tick(), if git_on => spawn_git(&state.cwd, &tx, &git_busy),
            _ = poll.tick() => {
                if let Some(id) = session.id().map(str::to_string) {
                    let api = Arc::clone(&api);
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let usage = crate::conn::usage(&api, &id).await;
                        let title = crate::conn::session_title(&api, &id).await;
                        let _ = tx.send((
                            Some(Origin::asked(id)),
                            Action::Frame(Frame::Poll { usage, title }),
                        ));
                    });
                }
                // While the thread list is open, keep its statuses live: a thread that goes
                // idle (or a new one that appears) shows up without closing and reopening.
                // Runs off the loop like poll/git so a slow server cannot stall keys.
                if let Some(p) = &state.picker {
                    if let crate::picker::Level::Sessions { project_id, project_name } = &p.level {
                        spawn_sessions(
                            &api,
                            &tx,
                            project_id.clone(),
                            project_name.clone(),
                            state.lang,
                            state.thread_status.clone(),
                            state.thread_was_running.clone(),
                        );
                    }
                }
                if state.title != shown_title {
                    set_terminal_title(&state.title);
                    shown_title = state.title.clone();
                }
            }
            _ = ticker.tick() => {
                state.tick = state.tick.wrapping_add(1);
                let pending = state.quit_pending();
                // We have to redraw at the moment a notice fades too. Without it the erased
                // text stays on screen — same reason as the armed quit.
                let has_status = state.status().is_some();
                if has_status != last_had_status {
                    last_had_status = has_status;
                    dirty = true;
                }
                if pending != last_quit_pending {
                    last_quit_pending = pending;
                    if !pending {
                        // The arming released, so clear the state too. Left set, the next
                        // check catches on it again.
                        state.quit_armed_at = None;
                    }
                    dirty = true;
                }
                // While working the dot has to blink, so keep redrawing. One frame is around
                // 0.2ms, so it is no burden — before, this was not possible.
                if state.running {
                    dirty = true;
                }
                // A running thread's status dot in the picker blinks too, so the list must be
                // redrawn each frame while one is on screen.
                if state
                    .picker
                    .as_ref()
                    .is_some_and(|p| p.rows.iter().any(|r| r.status == Some(crate::picker::ThreadStatus::Running)))
                {
                    dirty = true;
                }
                // With the enrollment code window up, the time left is ticking down, so keep
                // drawing.
                if state.enroll.is_some() {
                    dirty = true;
                }
                // While a turn runs, batch the drawing. One streaming chunk is 3.4KB, so
                // twenty a second is the heaviest thing for a remote terminal — ten or
                // twenty looks the same to the eye but halves the volume out.
                let held = state.running && last_draw.elapsed() < STREAM_MIN_GAP;
                if dirty && !held {
                    terminal.draw(|f| widgets::draw(f, &mut state))?;
                    dirty = false;
                    drew_since_heal = true;
                    last_draw = Instant::now();
                }
            }
            // **Being told to quit from outside quits the same way.** `kill` or closing the
            // terminal window (SIGHUP) arrives here — dying by that path would leave a turn
            // running on the server.
            Some(()) = shutdown.recv() => break 'app,
            // **The runner ended.** When `main` signals this channel (a fatal error) the
            // screen closes quietly too — `main` says the reason after the terminal is
            // restored.
            _ = die.changed(), if *die.borrow() => break 'app,
            // Self-healing. Only done if something was drawn since the last heal — a screen
            // sitting still cannot break, and an idle session has no reason to keep pushing
            // bytes over SSH.
            _ = heal.tick(), if healing => {
                // **While a turn runs, heal only blank cells.** Streaming already redraws
                // the screen every frame, but its diff never touches blank cells — when a
                // wide character turns into a narrow one, the trailing cell stays "blank on
                // both buffers" and is never redrawn, leaving a glyph crumb on SSH screens.
                // A full overwrite (~21KB) fixes it but overlaps a streaming frame on slow
                // links and shows **the same word twice** (measured on Termius). Writing
                // only blanks costs a few KB, and a space can never corrupt or double
                // content. The diff skips the cell right after a wide character, so the
                // blank pass never writes under one.
                if drew_since_heal {
                    if state.running {
                        state.force_update_blank = true;
                    } else {
                        state.force_update = true;
                    }
                    // **Force cells out again without clearing.** clear is what causes
                    // the flicker — `AlwaysUpdate` bypasses the diff and overwrites,
                    // wiping the residue in the trailing cell behind a wide character.
                    // The next draw goes back to the normal diff.
                    terminal.draw(|f| widgets::draw(f, &mut state))?;
                    dirty = false;
                    drew_since_heal = false;
                }
            }
        }
    }

    // This is the normal exit path. A watchdog task left running could end the process on a
    // false alarm 60 seconds later, so cut it here.
    watchdog.abort();

    // **Closing the window stops it on the server too.**
    //
    // Turns run on the server — even once this side is gone, that side keeps thinking and
    // fails looking for a node that is not there on every tool call. Credits keep going out
    // meanwhile. Someone closing means "stop", not "keep it working while I stop watching".
    if let Some(id) = turn_to_stop(&state, &session) {
        // One last frame. If the network is sluggish this looks like a brief freeze, and
        // with nothing said it reads as refusing to quit. The screen is still ours — `run`
        // is what restores it.
        //
        // Disarm the quit first. The activity line puts that above everything
        // (`activity.rs`), so leaving it armed lets the notice from the Ctrl+C just pressed
        // cover this last line.
        state.quit_armed_at = None;
        state.set_status(state.lang.stopping_turn());
        let _ = terminal.draw(|f| widgets::draw(f, &mut state));
        // **The window closes even if it cannot be stopped.** Waiting here indefinitely for
        // a server that does not answer leaves someone who wanted out in front of a screen
        // they cannot close.
        match tokio::time::timeout(STOP_WAIT, api.cancel_turn(id)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "could not stop the turn while quitting"),
            Err(_) => tracing::warn!("asked to stop the turn while quitting, but got no answer"),
        }
    }
    Ok(())
}

/// The session to ask to stop when quitting. `None` if no turn is running.
///
/// Split out pure — with the decision mixed into the I/O place, tests would have to stand up
/// a server.
fn turn_to_stop(state: &State, session: &Session) -> Option<String> {
    if !state.running {
        return None;
    }
    session.id().map(str::to_string)
}

/// Gathers the shutdown signals sent from outside into one channel.
///
/// Ctrl+C does not come here — raw mode makes it arrive as a byte rather than a signal, and
/// `on_key` receives it. This side is `kill` (SIGTERM) and the terminal window closing
/// (SIGHUP).
///
/// Off unix there is no sender, so `recv()` is immediately `None` and `select!` disables
/// that arm — quieter than removing the arm with cfg.
fn shutdown_signals() -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel(1);
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        for kind in [SignalKind::terminate(), SignalKind::hangup()] {
            let Ok(mut sig) = signal(kind) else { continue };
            let tx = tx.clone();
            tokio::spawn(async move {
                sig.recv().await;
                let _ = tx.send(()).await;
                // **If the loop is stuck somewhere, the signal above is never handled.**
                // (The loop has to come around to see `select!`'s shutdown arm.) If it has
                // still not ended a few seconds later, restore the screen and force the
                // exit — so `kill` always works. A normal exit finishes before that, so
                // this line never runs first.
                tokio::time::sleep(SHUTDOWN_FORCE).await;
                // Even on a forced exit, bracketed paste and the kitty keyboard protocol
                // are restored on the way out — otherwise the shell keeps wrapping pastes
                // in one chunk and Shift+Enter stays CSI-u. Restoring also tolerates
                // failure (Windows' kitty commands always fail — see `terminal_feature`).
                terminal_feature("bracketed paste off", crossterm::event::DisableBracketedPaste);
                terminal_feature(
                    "kitty keyboard protocol off",
                    crossterm::event::PopKeyboardEnhancementFlags,
                );
                ratatui::restore();
                std::process::exit(0);
            });
        }
    }
    rx
}

/// How many thread outcomes are derived at once.
///
/// Each is a history read of its own, so all of them at once would open a socket per thread on
/// a busy project; one at a time would take as long as the sum. This is the middle.
const STATUS_AT_ONCE: usize = 6;

/// Fetches the project list off the loop.
///
/// **Nothing here may be awaited on the draw loop.** The list is one request, but on a big
/// account it is not a fast one, and while it runs neither keys nor drawing happen — which is
/// exactly what "the window freezes when I press ←" was.
fn spawn_projects(
    api: &Arc<AttaccaApiClient>,
    lang: crate::lang::Lang,
    tx: &mpsc::UnboundedSender<AppMsg>,
) {
    let (api, tx) = (Arc::clone(api), tx.clone());
    tokio::spawn(async move {
        let frame = match crate::conn::projects(&api).await {
            Ok(items) => Frame::Picker {
                picker: crate::picker::Picker::projects(items, lang),
                thread_was_running: None,
            },
            Err(e) => Frame::PickerFailed(e.to_string()),
        };
        let _ = tx.send((None, Action::Frame(frame)));
    });
}

/// Fetches a project's thread list off the loop, then **streams each thread's outcome dot in
/// as it lands.**
///
/// The rows go up the moment the one list request returns; the dots follow. Deriving an
/// outcome means reading that thread's history, so waiting for every one of them before
/// showing anything is what made opening a busy project look like a hang.
///
/// A thread that is running shows as `Running` at once. A finished one reuses the cached
/// outcome unless it was running on the last refresh — running→idle means a turn just ended,
/// so that one is re-derived.
fn spawn_sessions(
    api: &Arc<AttaccaApiClient>,
    tx: &mpsc::UnboundedSender<AppMsg>,
    project_id: String,
    project_name: String,
    lang: crate::lang::Lang,
    cached: std::collections::HashMap<String, crate::picker::ThreadStatus>,
    was_running: std::collections::HashMap<String, bool>,
) {
    use crate::picker::ThreadStatus;
    let (api, tx) = (Arc::clone(api), tx.clone());
    tokio::spawn(async move {
        let items = match crate::conn::sessions(&api, &project_id).await {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.send((None, Action::Frame(Frame::PickerFailed(e.to_string()))));
                return;
            }
        };
        let mut rows = Vec::with_capacity(items.len());
        let mut derive: Vec<String> = Vec::new();
        let mut now_running = std::collections::HashMap::with_capacity(items.len());
        for (id, title, running) in items {
            let was = was_running.get(&id).copied().unwrap_or(false);
            let status = if running {
                ThreadStatus::Running
            } else {
                let known = cached.get(&id).copied().unwrap_or(ThreadStatus::Unknown);
                if known != ThreadStatus::Unknown && !was {
                    known
                } else {
                    // Not known yet — the row goes up with a grey dot and the real one
                    // arrives on its own.
                    derive.push(id.clone());
                    ThreadStatus::Unknown
                }
            };
            now_running.insert(id.clone(), running);
            rows.push((id, title, status));
        }
        let _ = tx.send((
            None,
            Action::Frame(Frame::Picker {
                picker: crate::picker::Picker::sessions(project_id, project_name, rows, lang),
                thread_was_running: Some(now_running),
            }),
        ));

        let room = Arc::new(tokio::sync::Semaphore::new(STATUS_AT_ONCE));
        for id in derive {
            let (api, tx, room) = (Arc::clone(&api), tx.clone(), Arc::clone(&room));
            tokio::spawn(async move {
                let Ok(_permit) = room.acquire().await else { return };
                // A thread whose history says nothing keeps the grey dot it went up with.
                let status = crate::conn::session_status(&api, &id)
                    .await
                    .unwrap_or(crate::picker::ThreadStatus::Unknown);
                let _ = tx.send((None, Action::Frame(Frame::ThreadStatus { id, status })));
            });
        }
    });
}

/// Fetches the agent list off the loop. Same reason as `spawn_projects`.
fn spawn_agents(api: &Arc<AttaccaApiClient>, tx: &mpsc::UnboundedSender<AppMsg>) {
    let (api, tx) = (Arc::clone(api), tx.clone());
    tokio::spawn(async move {
        let frame = match crate::conn::within(&api, api.list_agents()).await {
            Ok(agents) => {
                let rows = agents
                    .into_iter()
                    .map(|a| crate::picker::Row {
                        id: Some(a.name.clone()),
                        label: a.name,
                        note: None,
                        enabled: true,
                        status: None,
                    })
                    .collect();
                Frame::Picker {
                    picker: crate::picker::Picker::agents(rows),
                    thread_was_running: None,
                }
            }
            Err(e) => Frame::PickerFailed(crate::lang::current().agent_list_error(&e.to_string())),
        };
        let _ = tx.send((None, Action::Frame(frame)));
    });
}

/// Fetches a thread's history off the loop and sends it in as one frame.
///
/// **Tagged with the session id.** Switching again while this is in flight leaves an answer
/// nobody wants any more, and `frame_is_current` drops it — otherwise the loser would land on
/// top of the thread the person actually chose.
fn spawn_history(api: &Arc<AttaccaApiClient>, tx: &mpsc::UnboundedSender<AppMsg>, id: String) {
    let (api, tx) = (Arc::clone(api), tx.clone());
    tokio::spawn(async move {
        let frame = match crate::conn::history(&api, &id).await {
            Ok(events) => Frame::History {
                entries: events
                    .iter()
                    .map(|e| (e.cursor, crate::event::entry_from(e), crate::todos::change_from(e)))
                    .collect(),
            },
            Err(e) => Frame::PickerFailed(e.to_string()),
        };
        let _ = tx.send((Some(Origin::asked(id)), Action::Frame(frame)));
    });
}

/// Actually acts on what was chosen from the list.
async fn pick(
    api: &Arc<AttaccaApiClient>,
    state: &mut State,
    session: &mut Session,
    agent_id: &mut String,
    tx: &mpsc::UnboundedSender<AppMsg>,
) {
    use crate::picker::{Pick, Picker};

    let Some(chosen) = state.picker.as_ref().and_then(Picker::pick) else {
        return;
    };
    // **Every way out of a thread list stays in that project** — opening a thread, making one,
    // or closing with Esc. Taken here once rather than in each arm, so a new arm cannot forget
    // it and leave the bottom bar naming a project we already left.
    if let Some(crate::picker::Level::Sessions { project_name, .. }) =
        state.picker.as_ref().map(|p| &p.level)
    {
        state.project_name = Some(project_name.clone());
    }
    match chosen {
        Pick::Unavailable(why) => state.set_error(why),
        Pick::OpenProject { id, name } => {
            // **Remember it the moment we enter.** Even if it closes with Esc without a
            // session being chosen, jobs and works opened from here must belong to this
            // project.
            session.enter_project(id.clone());
            state.project_name = Some(name.clone());
            state.picker = Some(Picker::loading_sessions(id.clone(), name.clone()));
            spawn_sessions(
                api,
                tx,
                id,
                name,
                state.lang,
                state.thread_status.clone(),
                state.thread_was_running.clone(),
            );
        }
        Pick::OpenSession { id, project_id } => {
            switch(state, session, id, Some(project_id), api, tx);
        }
        Pick::NewSession { project_id } => {
            // **Do not create it on the server now.** It is created when the first message
            // is sent — empty sessions that were only opened and looked at must not pile up
            // on the account. Here we only remember which project to create it in.
            session.stage_new(project_id);
            // The previous session's turn does not belong to this screen — clear the status
            // line and the queue.
            leave_session(state);
            clear_conversation(state);
            // A new session has no title yet.
            state.title = "Zyris Code".into();
            state.usage.clear();
            state.picker = None;
            let _ = agent_id;
        }
        // **Fetching is the moment somebody else's code lands on this machine**, so it happens
        // only once the person has said where — never as a side effect of typing the address.
        Pick::InstallPlugin { source, project } => {
            state.picker = None;
            let into = if project {
                state.cwd.join(".zyris-code/plugins")
            } else {
                crate::plugin::install_dir()
            };
            let said = match crate::plugin::install_into(&into, &source).await {
                Ok(p) => {
                    let contents = state.lang.plugin_contents_text(&p);
                    state.lang.plugin_added(&p, &contents)
                }
                Err(why) => why,
            };
            state.timeline.say(said);
        }
        Pick::UseAgent { name } => {
            state.picker = None;
            switch_agent(api, state, session, agent_id, &name).await;
        }
        // **Do not create it right away.** A name and a description have to be taken — the
        // form owns that job.
        Pick::NewProject => {
            // **Leave the list as it is.** The form goes on top of it, and closing with Esc
            // returns right to that spot — no need to reopen and refetch the list.
            state.new_project = Some(crate::newproject::Form::new());
        }
        // **Do not run it right away.** Some take arguments, like `/mode`, so it has to be
        // possible to keep typing after choosing. A trailing space lets the argument be
        // typed immediately.
        Pick::TypeCommand { text } => {
            state.picker = None;
            state.input.take();
            state.input.insert_str(&format!("{text} "));
        }
    }
}

/// Called where the screen leaves this session. Clears the previous session's turn state.
///
/// The turn itself keeps running on the server — cutting it here would amount to killing the
/// server's work. Only the screen stops belonging to that session, so we clear that turn's
/// indicators (`running`, `stopping`) and the queue. **Without clearing, the status line
/// freezes on "working"**: a new session has no stream yet so nobody sets `running` back to
/// false, and frames from the stale session are dropped by `frame_is_current` so it does not
/// come from there either. Left as it is, the queue also fires the held messages off at the
/// end of this or the next session's turn.
fn leave_session(state: &mut State) {
    state.running = false;
    state.stopping = false;
    state.queued.clear();
    state.flush_queue = false;
}

/// Tears the conversation down, so what the next session draws is only its own.
///
/// **One place, because everything here belongs to a session.** Both the folds and the todo list
/// are keyed by that session's event `seq`s, so leaving either behind puts the last thread's
/// state on the next thread's rows. There are two ways out of a conversation — replacing it with
/// a fetched history, and staging a brand new one — and when they each cleared their own list the
/// next field added was always going to be forgotten by one of them.
fn clear_conversation(state: &mut State) {
    leave_session(state);
    // **News about a conversation goes with it.** "could not send" from the thread just left,
    // sitting on the line that is supposed to say what is happening here, reads as this thread
    // failing.
    state.clear_status();
    state.timeline = Timeline::new();
    state.todos = crate::todos::Todos::new();
    state.folds = Folds::new();
    state.asking = None;
    state.last_cursor = None;
    state.scroll = Scroll::new(); // Start from the bottom.
}

/// Switches to another session. Re-reads the past record to fill the screen and reopens the
/// live stream.
fn switch(
    state: &mut State,
    session: &mut Session,
    id: String,
    project_id: Option<String>,
    api: &Arc<AttaccaApiClient>,
    tx: &mpsc::UnboundedSender<AppMsg>,
) {
    // **The screen is not torn down here.** History is fetched off the loop, and a long thread
    // takes a while; blanking now would leave an empty window for all of it. `Frame::History`
    // does the clearing when it lands, so until then the previous thread stays readable.
    session.switch_to(id.clone(), project_id);
    state.usage.clear();
    state.picker = None;
    // **Cleared at the click, not when the history lands.** A notice outranks "loading…" on the
    // activity line, so the thread being left would keep talking for the whole fetch.
    state.clear_status();
    state.loading_history = true;
    // A title is asked for separately so it is not held up behind the history — a window
    // title still naming the previous thread makes it unclear which conversation is in view.
    state.title = "Zyris Code".into();
    spawn_history(api, tx, id.clone());
    let (api, tx, sid) = (Arc::clone(api), tx.clone(), id);
    tokio::spawn(async move {
        let title = crate::conn::session_title(&api, &sid).await;
        let _ =
            tx.send((Some(Origin::asked(sid)), Action::Frame(Frame::Poll { usage: None, title })));
    });
}

/// Secures a session (creating one if there is none), sends the message, and opens the turn
/// stream. Once the turn ends, sends the held messages in order.
///
/// **If one fails, stop there and leave the rest.** They are retried at the end of the next
/// turn — quietly dropping what could not be sent leaves the user believing it went out.
/// Finishes the slash commands **that need the server or the disk**.
///
/// `run_command` already finished the pure part. Only four kinds reach here.
async fn finish_command(
    api: &Arc<AttaccaApiClient>,
    bridge: &crate::tools::bridge::Bridge,
    state: &mut State,
    session: &mut Session,
    agent_id: &mut String,
    text: &str,
    tx: &mpsc::UnboundedSender<AppMsg>,
) {
    use crate::command::{AccountAction, Command};
    let Some(cmd) = run_command(state, text) else { return };
    match cmd {
        // **Read off disk at the moment it is asked for.** Another client may have added a server
        // since this window started, and a list that only knew about launch time would keep saying
        // it is not there.
        Command::Mcp(None) => {
            let allowed = crate::mcp::discovery::Allowed::load();
            let found: Vec<(String, String, bool)> = crate::mcp::discovery::found(&state.cwd)
                .into_iter()
                .map(|f| (f.spec.slug.clone(), f.source, allowed.allows(&f.spec.slug)))
                .collect();
            state.panel = Some(crate::panel::mcp(state.lang, &bridge.mcp_report(), &found));
        }
        Command::Mcp(Some(switch)) => {
            use crate::command::McpSwitch;
            let (slug, on) = match &switch {
                McpSwitch::On(slug) => (slug.clone(), true),
                McpSwitch::Off(slug) => (slug.clone(), false),
                McpSwitch::Unknown(what) => {
                    let help = crate::command::help_text(state.lang);
                    state.timeline.say(state.lang.unknown_command(what, &help));
                    return;
                }
            };
            // **Only what was discovered can be switched.** A server written down for this app
            // always runs, so saying "off" about one would be a promise this cannot keep.
            let known =
                crate::mcp::discovery::found(&state.cwd).into_iter().any(|f| f.spec.slug == slug);
            if !known {
                state.timeline.say(state.lang.mcp_not_found(&slug));
                return;
            }
            let mut allowed = crate::mcp::discovery::Allowed::load();
            let said = if allowed.set(&slug, on) {
                allowed.save();
                state.lang.mcp_switched(&slug, on)
            } else {
                state.lang.mcp_already(&slug, on)
            };
            state.timeline.say(said);
        }
        // Stopping is I/O that touches the registry. The listing was already said by `run_command`.
        // **We don't drop it from the list** — that it died is what the reaper's `JobEnded` says.
        Command::Jobs(Some(id)) => {
            let stopped = bridge.jobs().is_some_and(|jobs| jobs.stop(&id));
            state.timeline.say(if stopped {
                state.lang.jobs_stopped(&id)
            } else {
                state.lang.jobs_unknown(&id)
            });
        }
        Command::Skills => {
            let skills = crate::tools::skill::Skills::discover(&state.cwd);
            state.panel = Some(crate::panel::skills(state.lang, &skills.list()));
        }
        // **The preamble is an invisible place.** This is the only way to find out when
        // something quietly failed to load into it.
        Command::Rules => {
            let found = crate::instructions::collect(&state.cwd);
            state.timeline.say(state.lang.rules_text(&found));
        }
        // Off the loop, like every other list — see `spawn_projects`.
        Command::Agent(None) => {
            state.picker = Some(crate::picker::Picker::loading_agents());
            spawn_agents(api, tx);
        }
        Command::Agent(Some(name)) => switch_agent(api, state, session, agent_id, &name).await,
        Command::Plugin(what) => {
            use crate::command::Plugin as P;
            match what {
                // The listing is a panel now — one row per plugin reads better than
                // a paragraph of bullets.
                P::List => {
                    state.panel = Some(crate::panel::plugins(
                        state.lang,
                        &crate::plugin::discover(&state.cwd),
                    ));
                }
                other => {
                    let said = run_plugin(state, other).await;
                    // **An empty answer means the command opened something instead of finishing.**
                    // `/plugin add` puts up the where-to list; saying nothing there would add a
                    // blank line to the transcript under the box.
                    if !said.is_empty() {
                        state.timeline.say(said);
                    }
                }
            }
        }
        // **A setting change reaches the disk and the gate.** `run_command` only touched
        // the state; `save` writes the file and `bridge.sync` carries the new policy to the
        // tools (same lesson as `/mode` forgetting `bridge.sync`).
        // **Drop the socket and let the runner redial.** That redial re-announces, which is the
        // only way to get back into attacca's registry once another window displaced us —
        // nothing else here can even detect that state, let alone leave it.
        Command::Reconnect => match bridge.connection() {
            Some(conn) => {
                state.reconnecting = true;
                state.set_status(state.lang.reconnecting());
                conn.close("reconnect requested from /reconnect");
            }
            None => state.set_error(state.lang.reconnect_not_attached()),
        },
        Command::Config(Some(action)) => {
            state.config.save();
            // The palette applies to the very next frame — the same promise the directory
            // policy makes to the gate. Missing it is how a setting changes on screen and
            // nowhere else.
            crate::theme::set(state.config.theme.resolve());
            bridge.sync(state.mode, &state.config);
            if let crate::command::ConfigAction::Lang(lang) = action {
                crate::lang::set(lang);
                crate::lang::save(lang);
            }
        }
        Command::Config(None) => {}
        Command::Changes => {
            let said = match bridge.undo() {
                Some(undo) => state.lang.changes_text(&undo.changed(), &state.cwd),
                None => state.lang.undo_log_not_ready().to_string(),
            };
            state.timeline.say(said);
        }
        // **Needs no server call** — but the session (id, project) only lives on the I/O side.
        Command::Status => {
            state.panel = Some(crate::panel::status(state.lang, &status_info(state, session)));
        }
        Command::Undo => {
            let said = match bridge.undo() {
                Some(undo) => match undo.revert_last() {
                    Ok(path) => {
                        let shown = path.strip_prefix(&state.cwd).unwrap_or(&path);
                        // **Do not tell the agent.** Slipping "what you did was reverted"
                        // into the middle of a running turn makes it retry the same edit.
                        // Reading the file next turn shows the change anyway.
                        state.lang.reverted(&shown.display().to_string())
                    }
                    Err(why) => why,
                },
                None => state.lang.undo_log_not_ready().to_string(),
            };
            state.timeline.say(said);
        }
        // **Who this node is attached as.** `me()` needs no scope of its own, so it answers
        // even for a grant that came back short — and that is exactly the case where the
        // account view must still work.
        Command::Account(None) => match crate::conn::within(api, api.me()).await {
            Ok(me) => {
                let name =
                    if me.display_name.trim().is_empty() { &me.email } else { &me.display_name };
                state.panel = Some(crate::panel::account(
                    state.lang,
                    name,
                    &me.email,
                    &me.user_id,
                    me.plan.as_deref(),
                    me.credits.as_deref(),
                    &me.scopes,
                ));
            }
            Err(e) => state.timeline.say(state.lang.account_error(&e.to_string())),
        },
        // **Logging out drops the credentials and then drops the connection**, so the enrolment
        // window comes up on the spot.
        //
        // It used to clear the file and say "restart the app". That was written when the
        // enrolment code went to stdout, where it would have been buried under the running TUI —
        // it now draws on screen (`enroll::ScreenEnroll`), so there is nothing left to protect
        // against and nothing to explain: closing the socket makes the runner redial, the redial
        // finds no credential, and the code appears exactly as it does on a first launch.
        //
        // It also went through `discard_once`, which allows one discard per process for the
        // automatic scope check. Once that allowance was spent, pressing logout **cleared nothing
        // and reported failure** while the credentials stayed on disk and kept working.
        Command::Account(Some(AccountAction::Logout(_))) => {
            let said = match bridge.reauth() {
                // Where a token was given directly there is nothing to discard.
                None => state.lang.account_logout_nothing().to_string(),
                Some(reauth) => {
                    if reauth.discard().await {
                        // **Only after the credentials are gone.** Cutting first would let the
                        // redial succeed with the credential still there and reconnect as if
                        // nothing had been asked for.
                        // Not attached means there is nothing to cut — the next attempt already
                        // has no credential to use.
                        if let Some(conn) = bridge.connection() {
                            state.reconnecting = true;
                            conn.close("logged out from /account");
                        }
                        state.panel = None;
                        state.lang.account_logged_out().to_string()
                    } else {
                        state.lang.account_logout_failed().to_string()
                    }
                }
            };
            state.timeline.say(said);
        }
        // **Signing in is the person's to do, not the agent's.** The tools refuse until it has
        // happened and say so, which is the only honest way round — a node cannot open a browser
        // on somebody's behalf and should not try.
        Command::Github(action) => {
            let said = run_github(state, action.clone()).await;
            if !said.is_empty() {
                state.timeline.say(said);
            }
        }
        _ => {}
    }
}

/// `/github`. **The wait is the whole of it** — the person has to approve a code in a browser, so
/// this polls until they have, and says what is happening while it does.
async fn run_github(state: &mut State, action: Option<crate::command::AccountAction>) -> String {
    use crate::github::auth;

    match action {
        // Both slots at once. **Saying only the user's would hide which account reviews go out
        // under**, which is the one thing the two-slot arrangement exists to make visible.
        // **`/github` on its own opens the screen.** A wall of text saying who is connected is
        // not something anyone can act on; the screen is, and it is the only place a reviewer
        // token can be pasted.
        None => {
            let accounts = auth::Accounts::load();
            state.github_form = Some(crate::githubform::Form::new(
                accounts.exactly(auth::Role::User).map(|a| a.login.clone()),
                accounts.exactly(auth::Role::Reviewer).map(|a| a.login.clone()),
            ));
            String::new()
        }
        Some(crate::command::AccountAction::Logout(role)) => {
            if auth::Accounts::forget(role) {
                state.lang.github_logged_out(role)
            } else {
                state.lang.github_nothing_to_log_out().to_string()
            }
        }
        Some(crate::command::AccountAction::Login(role)) => {
            let pending = match auth::begin().await {
                Ok(p) => p,
                Err(why) => return state.lang.github_login_failed(&why.to_string()),
            };
            // **The code goes on screen before the wait starts.** It is the only thing the person
            // can act on, and printing it after the polling loop would be printing it too late.
            state.timeline.say(state.lang.github_code(
                &pending.user_code,
                &pending.verification_uri,
                role,
            ));
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(pending.expires_in);
            let mut wait = std::time::Duration::from_secs(pending.interval);
            loop {
                if std::time::Instant::now() >= deadline {
                    return state.lang.github_login_failed("the code expired");
                }
                tokio::time::sleep(wait).await;
                match auth::poll(&pending).await {
                    auth::Poll::Waiting { interval } => {
                        wait = std::time::Duration::from_secs(interval)
                    }
                    auth::Poll::Failed(why) => return state.lang.github_login_failed(&why),
                    auth::Poll::Done(token) => {
                        // **The login is asked for straight away** so `/github` can name the
                        // account without a round trip, and a token that does not work is caught
                        // here rather than at the first tool call.
                        let login = match crate::github::api::Github::new(token.clone()) {
                            Ok(client) => client.me().await.unwrap_or_default(),
                            Err(_) => String::new(),
                        };
                        let mut accounts = auth::Accounts::load();
                        accounts.set(role, Some(auth::Account { token, login: login.clone() }));
                        if let Err(e) = accounts.save() {
                            return state.lang.github_login_failed(&e.to_string());
                        }
                        return state.lang.github_logged_in(&login, role);
                    }
                }
            }
        }
    }
}

/// Carries out what the `/github` screen asked for.
///
/// **Every path ends with the screen updated**, because the screen is what the person is looking
/// at — a silent success reads exactly like a key that did not register.
async fn run_github_ask(state: &mut State, ask: crate::githubform::Ask) {
    use crate::github::auth;
    use crate::githubform::Ask;

    match ask {
        // The browser route. `run_github` already knows how to wait on a code, and it puts the
        // code in the transcript — so the screen steps aside while that happens.
        Ask::LoginUser => {
            state.github_form = None;
            let said =
                run_github(state, Some(crate::command::AccountAction::Login(auth::Role::User)))
                    .await;
            if !said.is_empty() {
                state.timeline.say(said);
            }
            // Reopen it with whatever the sign-in settled on, so the screen is never stale.
            reopen_github(state);
        }
        Ask::LogoutUser => {
            let worked = auth::Accounts::forget(auth::Role::User);
            let note = match worked {
                true => state.lang.github_logged_out(auth::Role::User),
                false => state.lang.github_nothing_to_log_out().to_string(),
            };
            settle_github(state, note, worked);
        }
        Ask::ClearReviewer => {
            let worked = auth::Accounts::forget(auth::Role::Reviewer);
            let note = match worked {
                true => state.lang.github_logged_out(auth::Role::Reviewer),
                false => state.lang.github_nothing_to_log_out().to_string(),
            };
            settle_github(state, note, worked);
        }
        // **A pasted token is checked before it is kept.** A token that does not work must fail
        // here, where it can be pasted again, and not at the first review weeks later.
        Ask::SetReviewer(token) => {
            let login = match crate::github::api::Github::new(token.clone()) {
                Ok(client) => client.me().await,
                Err(e) => Err(anyhow::anyhow!(e.to_string())),
            };
            match login {
                Ok(login) => {
                    let mut accounts = auth::Accounts::load();
                    accounts.set(
                        auth::Role::Reviewer,
                        Some(auth::Account { token, login: login.clone() }),
                    );
                    let note = match accounts.save() {
                        Ok(()) => state.lang.github_logged_in(&login, auth::Role::Reviewer),
                        Err(e) => state.lang.github_login_failed(&e.to_string()),
                    };
                    settle_github(state, note, true);
                }
                Err(e) => {
                    let note = state.lang.github_token_refused(&e.to_string());
                    settle_github(state, note, false);
                }
            }
        }
    }
}

/// Puts the answer on the screen and re-reads who is connected.
fn settle_github(state: &mut State, note: String, worked: bool) {
    let accounts = crate::github::auth::Accounts::load();
    if let Some(form) = state.github_form.as_mut() {
        form.user = accounts.exactly(crate::github::auth::Role::User).map(|a| a.login.clone());
        form.reviewer =
            accounts.exactly(crate::github::auth::Role::Reviewer).map(|a| a.login.clone());
        form.settled(note, worked);
    }
}

/// Opens the screen again after something that had to close it.
fn reopen_github(state: &mut State) {
    let accounts = crate::github::auth::Accounts::load();
    state.github_form = Some(crate::githubform::Form::new(
        accounts.exactly(crate::github::auth::Role::User).map(|a| a.login.clone()),
        accounts.exactly(crate::github::auth::Role::Reviewer).map(|a| a.login.clone()),
    ));
}

/// Switches the agent. **If it cannot be found, nothing changes.**
///
/// Falling back quietly produces a hard-to-diagnose state where the status line shows the new
/// name but sending fails with `Agent not found` — a spot `conn.rs` already went through.
async fn switch_agent(
    api: &Arc<AttaccaApiClient>,
    state: &mut State,
    session: &mut Session,
    agent_id: &mut String,
    name: &str,
) {
    match Session::agent_id_named(api, name).await {
        Ok(id) => {
            *agent_id = id;
            state.agent = name.to_string();
            // **A session's agent is fixed at creation and there is no API to change it**
            // (`ZNewSession.agent_id`). So we stage a new session — nothing is created on
            // the server yet, and it opens on the next message.
            session.stage_new_default();
            // The previous session's turn goes off-screen — clear it so the status line does
            // not freeze on "working".
            leave_session(state);
            state.timeline.say(state.lang.agent_staged(name));
        }
        Err(e) => state.timeline.say(format!("{e}")),
    }
}

/// `/plugin`. **Installing means putting someone else's code on this computer** — say so.
async fn run_plugin(state: &mut State, what: crate::command::Plugin) -> String {
    use crate::command::Plugin as P;
    use crate::plugin;

    match what {
        // **The list is a panel now** (`finish_command` routes it there) — this arm
        // is only a safety net for a direct call.
        P::List => state.lang.plugin_list_text(&plugin::discover(&state.cwd)),
        // **Asks where before fetching anything.** On this machine it is there for every project;
        // in the project it travels with the repo and shows up in `git status`. The address cannot
        // say which of those was wanted, so the person does (`Pick::InstallPlugin`).
        P::Add(source) => {
            state.picker = Some(crate::picker::Picker::plugin_target(source, state.lang));
            String::new()
        }
        P::Remove(name) => match plugin::remove(&name) {
            Ok(()) => state.lang.plugin_removed(&name),
            Err(why) => why,
        },
        P::Update(name) => {
            let done = plugin::update(name.as_deref()).await;
            state.lang.plugin_update_text(&done)
        }
        P::Unknown(why) => state.lang.plugin_unknown(&why),
    }
}

async fn flush_queue(
    api: &Arc<AttaccaApiClient>,
    session: &mut Session,
    agent_id: &str,
    state: &mut State,
    tx: &mpsc::UnboundedSender<AppMsg>,
) {
    if !std::mem::take(&mut state.flush_queue) {
        return;
    }
    if agent_id.is_empty() {
        state.set_error(state.lang.agent_cannot_send());
        return;
    }
    while let Some(text) = state.queued.first().cloned() {
        match send(api, session, agent_id, &text, state.last_cursor, state.mode, tx).await {
            Ok(announced) => {
                state.queued.remove(0);
                // **A held message goes up when it really goes out, not when it was typed.**
                // Until then it is still editable — ↑ pulls it back out of the queue — and an
                // echo left behind by a message that was pulled back would be a line nobody
                // ever sent. The same rule `remember_sent` follows on the next line.
                state.timeline.echo(text.as_str());
                state.remember_sent(&text);
                // **The first queued message can open a work or job too.** Change the mode
                // while work is running and hold a message, and that message becomes the
                // goal — so the opening has to be announced here as well.
                if let Some(said) = opened_text(state.lang, announced) {
                    state.timeline.say(said);
                }
            }
            Err(e) => {
                state.set_error(e.to_string());
                return;
            }
        }
    }
}

/// Sets the session staging so the next message goes where the mode decided. **It only runs
/// at the moment the mode changes.**
///
/// **With a conversation in progress, the mode does not hijack it.** Switching to work or job
/// keeps the current session going, and a new work or job opens in a new thread — this used
/// to stage the moment the mode changed, so the next message quietly opened a new session.
/// That really was confusing. We tell the user "the current conversation is untouched".
///
/// The decision itself is `Mode::route()`'s; here we only carry it to the session and say so.
/// **It says nothing.** Switching to 일/작업 used to push an explanation into the transcript,
/// and a transcript entry is permanent — with no server events yet it landed at the very top of
/// the screen and stayed there (2026-08-07 user decision to drop it). The bottom bar already
/// names the mode in its own colour, and `/mode` with no argument spells out what each one does.
fn restage(state: &State, session: &mut Session) {
    let route = state.mode.route();
    // With a session in hand, do not stage — `open_for`'s "no staging and an id means append"
    // works as-is, so the next message goes to the current conversation.
    if session.id().is_some() {
        session.set_route(crate::mode::Route::Session);
        return;
    }
    session.set_route(route);
}

/// Sends, and if something new was opened, says so.
///
/// **There are two call sites, so it lives here as one** (Enter in the input, and submit on
/// a question card). Fixing only one would leave two paths doing the same thing while saying
/// different words.
async fn send_and_tell(
    api: &Arc<AttaccaApiClient>,
    state: &mut State,
    session: &mut Session,
    agent_id: &str,
    text: &str,
    tx: &mpsc::UnboundedSender<AppMsg>,
) {
    match send(api, session, agent_id, text, state.last_cursor, state.mode, tx).await {
        Ok(announced) => {
            if let Some(said) = opened_text(state.lang, announced) {
                state.timeline.say(said);
            }
        }
        Err(e) => state.set_error(e.to_string()),
    }
}

/// What to say about what was newly opened. **`None` means say nothing** — a line per
/// message when all that happened was appending would bury the conversation under it.
///
/// There are two send paths (the input and the queue), so it lives here in one place.
fn opened_text(
    lang: crate::lang::Lang,
    announced: Option<(crate::mode::Route, String)>,
) -> Option<String> {
    match announced? {
        (crate::mode::Route::Work, id) => Some(lang.opened_work(&id)),
        (crate::mode::Route::Job, id) => Some(lang.opened_job(&id)),
        (crate::mode::Route::Session, _) => None,
    }
}

async fn send(
    api: &Arc<AttaccaApiClient>,
    session: &mut Session,
    agent_id: &str,
    text: &str,
    after: Option<i64>,
    mode: Mode,
    tx: &mpsc::UnboundedSender<AppMsg>,
) -> anyhow::Result<Option<(crate::mode::Route, String)>> {
    let opened = session.open_for(api, agent_id, text, mode).await?;
    let id = opened.id;
    // **For jobs and works the opening request already consumed the first message**
    // (`ZNewJob::message`). Sending again here puts the same words in twice, and the job
    // reads that as a new instruction.
    if !opened.sent {
        crate::conn::within(api, api.send_message(id.clone(), text.to_string(), vec![]))
            .await
            .map_err(|e| anyhow::anyhow!(crate::lang::current().send_failed(&e.to_string())))?;
    }

    // **`after` must not be left empty.** Empty means "only live frames from now on", and
    // the `chat_user` event for the message just sent was already recorded before the stream
    // opened, so it is never seen — the sent message disappears from the transcript. If
    // nothing has been seen yet, re-read from 0.
    spawn_stream(Arc::clone(api), session, id, Some(after.unwrap_or(0)), tx.clone());
    Ok(opened.announced)
}

/// Reads the turn stream in the background and forwards it as actions.
///
/// **Opening one abandons the last.** `turn_events` is a live subscription that never ends by
/// itself, and this used to be called on every message and every switch with nothing ever closed
/// — so a session talked to five times had five subscriptions handing over the same frames, and
/// `push_delta` appends them all. The answer being streamed came out five times over.
///
/// Every frame goes out **tagged with its session and this stream's number.** The receiver drops
/// frames from a session that was left and from a stream that was abandoned (`frame_is_current`):
/// aborting the task stops it sending more, but not what it already queued.
///
/// **Abandoning does not stop the turn**; it keeps running on the server and is read back as
/// history on return.
fn spawn_stream(
    api: Arc<AttaccaApiClient>,
    session: &mut Session,
    session_id: String,
    after: Option<i64>,
    tx: mpsc::UnboundedSender<AppMsg>,
) {
    let gen = session.next_stream();
    let task = tokio::spawn(async move {
        let tag = || Some(Origin { session: session_id.clone(), stream: Some(gen) });
        match crate::conn::within(&api, api.turn_events(session_id.clone(), after)).await {
            Ok(mut stream) => {
                // `Streaming` splits into head and items. head carries the current running
                // state.
                let running = stream.head.running;
                let _ = tx.send((tag(), Action::Frame(Frame::Status { running })));
                while let Some(frame) = stream.items.next().await {
                    match frame {
                        Ok(f) => {
                            if tx.send((tag(), Action::Frame(frame_from(f)))).is_err() {
                                break; // The app ended.
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "the turn stream dropped");
                            break;
                        }
                    }
                }
                // When the stream ends the turn has ended too. The status line must not
                // freeze on "working".
                //
                // **An abandoned stream never reaches here** — abort cuts the task inside
                // `next()`. Otherwise an old stream ending would report the *new* stream's turn
                // finished, and the activity line would go idle in the middle of one.
                let _ = tx.send((tag(), Action::Frame(Frame::Status { running: false })));
            }
            Err(e) => tracing::error!(error = %e, "could not open the turn stream"),
        }
    });
    session.holds_stream(task.abort_handle());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Entry, EntryKind};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn state() -> State {
        State::new()
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    /// **A mode picked before the connection has to reach the gate.** Shift+Tab works the whole
    /// time "connecting…" is on screen — first enrollment sits there for as long as it takes to
    /// walk to a browser — but `last_mode` is only seeded once that loop ends, so the main loop's
    /// edge check could never fire for a mode changed inside it. The bar said one mode while the
    /// gate went on using another, and no later keypress reconciled them.
    ///
    /// This pins the contract the pre-connection loop now honours: whatever the keys leave in
    /// `state`, the gate decides by.
    #[test]
    fn a_mode_picked_before_connecting_still_reaches_the_gate() {
        let mut s = state();
        let bridge = crate::tools::bridge::Bridge::new();
        bridge.sync(s.mode, &s.config);
        let write = crate::tools::gate::Call::new("code_edit", "edit", "a.rs".into());
        assert_eq!(bridge.decide(&write), crate::tools::gate::Decision::Run, "normal runs");

        // Cycle to plan the way the pre-connection loop does — `on_key` then `apply`, nothing else.
        while s.mode != Mode::Plan {
            for action in on_key(&s, key(KeyCode::BackTab, KeyModifiers::SHIFT)) {
                apply(&mut s, &action);
            }
        }
        bridge.sync(s.mode, &s.config);
        assert!(
            matches!(bridge.decide(&write), crate::tools::gate::Decision::Refuse(_)),
            "the gate is still on the old mode"
        );
    }

    /// Shift+Enter and Alt+Enter are newlines, not submits. With the kitty keyboard protocol
    /// on, Shift+Enter arrives separately as Enter+SHIFT, and Alt+Enter (ESC+\r) is the
    /// fallback for terminals without the protocol.
    #[test]
    fn shift_enter_and_alt_enter_insert_a_newline_instead_of_submitting() {
        let mut s = state();
        apply(&mut s, &Action::Insert('a'));
        assert_eq!(on_key(&s, key(KeyCode::Enter, KeyModifiers::ALT)), vec![Action::Insert('\n')]);
        assert_eq!(
            on_key(&s, key(KeyCode::Enter, KeyModifiers::SHIFT)),
            vec![Action::Insert('\n')]
        );
        assert_eq!(
            on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)),
            vec![Action::Submit("a".into())]
        );
    }

    /// Deciding on the answer to `CSI ? u`. The format is `CSI ? <flags> u` — only a terminal
    /// that knows the protocol can answer, so the format alone is enough.
    #[test]
    fn kitty_probe_ok_accepts_flag_responses() {
        assert!(kitty_probe_ok(b"\x1b[?1u"), "flag 1 as-is");
        assert!(kitty_probe_ok(b"\x1b[?3u"), "several flags");
        assert!(kitty_probe_ok(b"\x1b[?1;2u"), "event type too");
        assert!(kitty_probe_ok(b"x\x1b[?1u"), "even with typed characters in front");
        assert!(!kitty_probe_ok(b""), "no answer");
        assert!(!kitty_probe_ok(b"abc"), "plain text");
        assert!(!kitty_probe_ok(b"\x1b[?zzu"), "non-numeric flags");
    }

    /// A paste keeps its newlines — splitting on Enter would fire off the first line of a
    /// multi-line prompt on its own.
    #[test]
    fn paste_inserts_multiline_verbatim() {
        let mut s = state();
        apply(&mut s, &Action::Paste("한 줄\n두 줄".into()));
        assert_eq!(s.input.text, "한 줄\n두 줄");
        assert_eq!(s.input.height(40), 2);
    }

    /// A paste lands at the cursor. Pasting mid-text leaves what is before and after intact.
    #[test]
    fn paste_lands_at_the_cursor() {
        let mut s = state();
        apply(&mut s, &Action::Insert('가'));
        apply(&mut s, &Action::Insert('다'));
        apply(&mut s, &Action::Left);
        apply(&mut s, &Action::Paste("나\n라".into()));
        assert_eq!(s.input.text, "가나\n라다");
    }

    /// Windows sends press and release as separate KeyEvents. Without filtering the release,
    /// one press types twice — the bug where `/exit` becomes `//eexxitit` (ratatui issue
    /// #347). macOS/Linux have no release event, so this test simply guards it there.
    #[test]
    fn a_key_release_is_not_typed_twice() {
        let s = state();
        assert_eq!(
            on_key(&s, key(KeyCode::Char('x'), KeyModifiers::NONE)),
            vec![Action::Insert('x')],
            "a press should be input"
        );
        let release =
            KeyEvent::new_with_kind(KeyCode::Char('x'), KeyModifiers::NONE, KeyEventKind::Release);
        assert_eq!(
            on_key(&s, release),
            vec![],
            "a release should not be input — on Windows one press would type twice"
        );
    }

    fn enroll() -> EnrollView {
        EnrollView {
            code: "WXQR-7KBD".into(),
            uri: "https://attacca.example/settings/zyris/device".into(),
            expires_at: Instant::now() + Duration::from_secs(600),
            phase: EnrollPhase::Waiting,
        }
    }

    /// **Esc is the only key that closes the enrollment code window.** If another key clears
    /// it away, the approval step goes by without the code ever being seen — `y` stays a
    /// plain character here, not an answer to anything.
    #[test]
    fn the_enroll_window_closes_only_with_esc() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Enroll(enroll())));
        assert!(s.enroll.is_some());

        // Any other key leaves the window as it is.
        assert_eq!(
            on_key(&s, key(KeyCode::Char('y'), KeyModifiers::NONE)),
            vec![],
            "y must not be taken as an approval while enrolling"
        );
        assert_eq!(on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)), vec![]);
        assert_eq!(on_key(&s, key(KeyCode::Up, KeyModifiers::NONE)), vec![]);

        // Esc alone closes it.
        assert_eq!(on_key(&s, key(KeyCode::Esc, KeyModifiers::NONE)), vec![Action::EnrollClose]);
        apply(&mut s, &Action::EnrollClose);
        assert!(s.enroll.is_none(), "Esc should close it");
    }

    /// **The way out is not blocked even with the enrollment code window up.** Ctrl+C always
    /// works — a screen left dead while covering the code leaves no way out.
    #[test]
    fn ctrl_c_still_quits_while_the_enroll_window_is_up() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Enroll(enroll())));
        assert_eq!(
            on_key(&s, key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Action::ArmQuit]
        );
    }

    /// **The window closes on its own once approval arrives.** Even without the user pressing
    /// Esc, once attached there is no reason for the enrollment window to remain.
    #[test]
    fn approving_clears_the_enroll_window() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Enroll(enroll())));
        apply(&mut s, &Action::Frame(Frame::EnrollDone));
        assert!(s.enroll.is_none(), "approved, but the window is still there");
    }

    /// A lapse or a denial does not close the window, it **only changes what it says** — a
    /// new code redraws over it, so closing would lose the intermediate state.
    #[test]
    fn a_lapsed_or_denied_code_changes_the_phase_not_the_window() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Enroll(enroll())));
        apply(&mut s, &Action::Frame(Frame::EnrollPhase(EnrollPhase::Lapsed)));
        assert_eq!(s.enroll.as_ref().unwrap().phase, EnrollPhase::Lapsed);
        assert!(s.enroll.is_some(), "a lapse must not close the window");

        apply(&mut s, &Action::Frame(Frame::EnrollPhase(EnrollPhase::Denied)));
        assert_eq!(s.enroll.as_ref().unwrap().phase, EnrollPhase::Denied);
    }

    /// The last thing the app said.
    fn last_system(state: &mut State) -> String {
        state
            .timeline
            .items()
            .iter()
            .rev()
            .find_map(|i| match i {
                crate::timeline::Item::System { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn typed(state: &mut State, text: &str) {
        for c in text.chars() {
            apply(state, &Action::Insert(c));
        }
    }

    /// **Slash commands never reach the server.** One typo must not spend credits.
    #[test]
    fn a_slash_command_never_reaches_the_server() {
        let mut s = State::new();
        apply(&mut s, &Action::Submit("/cwd".into()));
        assert_eq!(s.command_out.as_deref(), Some("/cwd"));
        assert!(s.queued.is_empty(), "the command leaked into the queue");
    }

    /// A command runs now even mid-work — changing the mode has no reason to wait for a turn.
    #[test]
    fn a_command_runs_even_while_a_turn_is_going() {
        let mut s = State::new();
        s.running = true;
        apply(&mut s, &Action::Submit("/mode 계획".into()));
        assert_eq!(s.command_out.as_deref(), Some("/mode 계획"));
        assert!(s.queued.is_empty(), "{:?}", s.queued);
    }

    /// **With a conversation in progress the mode does not hijack it.** Switching to work or
    /// job still sends the next message to the current session — this used to stage the
    /// moment the mode changed and quietly open a new session. That really was confusing.
    #[test]
    fn restage_keeps_the_active_conversation() {
        let mut s = State::new();
        s.lang = crate::lang::Lang::Ko;
        let mut session = Session::new(None);
        session.switch_to("지금-세션".into(), None);
        s.mode = crate::mode::Mode::Work;
        restage(&s, &mut session);
        assert_eq!(session.pending_open(), None, "must not hijack the conversation in progress");
    }

    /// **Changing the mode writes nothing into the transcript.** A transcript entry is
    /// permanent — with no server events yet it sat at the very top of the screen and stayed
    /// there. The bottom bar names the mode already.
    #[test]
    fn changing_the_mode_says_nothing_on_screen() {
        for mode in [crate::mode::Mode::Work, crate::mode::Mode::Job, crate::mode::Mode::Plan] {
            let mut s = State::new();
            let mut session = Session::new(None);
            s.mode = mode;
            restage(&s, &mut session);
            assert!(s.timeline.items().is_empty(), "{mode:?} left something on screen");

            // And the same with a conversation already in progress.
            let mut s = State::new();
            let mut session = Session::new(None);
            session.switch_to("지금-세션".into(), None);
            s.mode = mode;
            restage(&s, &mut session);
            assert!(s.timeline.items().is_empty(), "{mode:?} spoke over the conversation");
        }
    }

    /// With no conversation the mode decides what opens — the first message becomes the work
    /// or job.
    #[test]
    fn restage_stages_an_open_only_without_a_conversation() {
        let mut s = State::new();
        let mut session = Session::new(None);
        s.mode = crate::mode::Mode::Job;
        restage(&s, &mut session);
        assert_eq!(session.pending_open(), Some(crate::mode::Route::Job));
    }

    /// An ordinary message still goes to the server.
    #[test]
    fn a_normal_message_still_goes_to_the_server() {
        let mut s = State::new();
        apply(&mut s, &Action::Submit("안녕".into()));
        assert!(s.command_out.is_none());
    }

    /// `/mode 계획` changes the mode and says so on screen. **Korean names are accepted
    /// too** — someone on the English screen should still be able to type what their hands
    /// know.
    #[test]
    fn the_mode_command_changes_the_mode_and_says_so() {
        let mut s = State::new();
        s.lang = crate::lang::Lang::Ko;
        run_command(&mut s, "/mode 계획");
        assert_eq!(s.mode, crate::mode::Mode::Plan);
        assert!(last_system(&mut s).contains("계획"), "{}", last_system(&mut s));

        let mut s = State::new();
        s.lang = crate::lang::Lang::En;
        run_command(&mut s, "/mode plan");
        assert_eq!(s.mode, crate::mode::Mode::Plan);
        assert!(last_system(&mut s).contains("plan"), "{}", last_system(&mut s));
    }

    /// Without an argument `/mode` opens a panel instead of a line of text — the
    /// current mode and the three alternatives at a glance. Esc closes it, and
    /// nothing was dumped into the conversation.
    #[test]
    fn mode_without_an_argument_opens_a_panel() {
        let mut s = State::new();
        s.mode = crate::mode::Mode::Job;
        run_command(&mut s, "/mode");
        let panel = s.panel.as_ref().expect("the mode panel should be up");
        assert!(
            panel.lines.iter().any(|l| l.to_string().contains('❯')),
            "the current mode is not marked: {panel:?}"
        );
        assert!(s.timeline.items().is_empty(), "no text went into the conversation");
        apply(&mut s, &Action::PanelClose);
        assert!(s.panel.is_none(), "Esc closes the panel");
    }

    /// ↑↓ scroll the panel, and so does the wheel while it is open — the transcript
    /// is hidden behind it, so scrolling that instead would move unseen text.
    #[test]
    fn keys_and_wheel_scroll_the_panel() {
        let mut s = State::new();
        run_command(&mut s, "/mode");
        for a in on_key(&s, key(KeyCode::Down, KeyModifiers::NONE)) {
            apply(&mut s, &a);
        }
        assert_eq!(s.panel.as_ref().unwrap().scroll, 1);
        apply(&mut s, &Action::Wheel(-1));
        assert_eq!(s.panel.as_ref().unwrap().scroll, 2);
        apply(&mut s, &Action::Wheel(5));
        assert_eq!(s.panel.as_ref().unwrap().scroll, 0);
        // While the panel is open the wheel must not move the transcript.
        assert_eq!(s.scroll.top, 0, "the transcript scroll moved");
    }

    /// Tab moves focus onto the account panel's logout button, and Enter activates
    /// it — the same path as typing `/account logout`, not a second logout flow.
    #[test]
    fn the_account_button_focuses_with_tab_and_activates_with_enter() {
        let mut s = State::new();
        s.panel = Some(crate::panel::account(
            crate::lang::Lang::Ko,
            "루마",
            "me@standoor.org",
            "user-1",
            None,
            None,
            &[],
        ));
        // Tab: focus moves to the button.
        for a in on_key(&s, key(KeyCode::Tab, KeyModifiers::NONE)) {
            apply(&mut s, &a);
        }
        assert!(s.panel.as_ref().unwrap().button_focused, "Tab must focus the button");
        // Enter: the button activates — the logout command is queued, the panel closes.
        for a in on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)) {
            apply(&mut s, &a);
        }
        assert_eq!(s.command_out.as_deref(), Some("/account logout"));
        assert!(s.panel.is_none(), "the panel closes when the button activates");
    }

    /// A panel without a button keeps the old keys — Tab does nothing, Enter closes.
    #[test]
    fn a_buttonless_panel_ignores_tab_and_enter_closes() {
        let mut s = State::new();
        run_command(&mut s, "/mode");
        let actions = on_key(&s, key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(actions.is_empty(), "no button: Tab must not focus anything: {actions:?}");
        for a in on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)) {
            apply(&mut s, &a);
        }
        assert!(s.panel.is_none(), "Enter still closes a panel without a button");
    }

    /// An unknown command says what does exist. Just "unknown" means getting it wrong again
    /// next time.
    #[test]
    fn an_unknown_command_lists_what_exists() {
        let mut s = State::new();
        run_command(&mut s, "/nope");
        assert!(last_system(&mut s).contains("/help"), "{}", last_system(&mut s));
    }

    /// `/status` paints the whole picture — thread, project, agent, mode, usage, cwd — in
    /// both languages, and a session-less state says so honestly.
    #[test]
    fn status_shows_the_session_picture() {
        let mut s = State::new();
        s.lang = crate::lang::Lang::Ko;
        let mut session = Session::new(None);
        session.switch_to("세션-1".into(), Some("프로젝트-1".into()));
        s.agent = "Main Agent".into();
        s.usage.model = Some("claude-opus-5-1m".into());
        s.usage.context_tokens = Some(500_000);
        s.usage.credits_used = Some("1.23".into());
        let said = s.lang.status_text(&status_info(&s, &session));
        assert!(said.contains("세션-1"), "{said}");
        assert!(said.contains("프로젝트-1"), "{said}");
        assert!(said.contains("Main Agent"), "{said}");
        assert!(said.contains("claude-opus-5-1m"), "{said}");
        assert!(said.contains("1.23"), "{said}");

        // The English screen says the same things, and a session-less state says so honestly.
        let mut s = State::new();
        s.lang = crate::lang::Lang::En;
        s.agent = "Main Agent".into();
        let session = Session::new(None);
        let said = s.lang.status_text(&status_info(&s, &session));
        assert!(said.contains("none yet"), "{said}");
        assert!(said.contains("Main Agent"), "{said}");
    }

    /// `/clear` empties **only the screen.** Clearing must not read as the session being gone.
    #[test]
    fn clearing_says_the_session_is_still_there() {
        let mut s = State::new();
        apply(&mut s, &work_start(1));
        run_command(&mut s, "/clear");
        assert!(last_system(&mut s).contains("thread"), "{}", last_system(&mut s));
        assert_eq!(s.timeline.items().len(), 1, "after clearing only the one notice remains");
    }

    /// `/config` opens the settings panel — the values are marked there, so looking at
    /// the settings is looking at the panel.
    #[test]
    fn the_config_command_opens_the_settings_panel() {
        let mut s = State::new();
        s.lang = crate::lang::Lang::Ko;
        run_command(&mut s, "/config");
        assert!(s.panel.is_some(), "the panel must open");
        assert_eq!(s.panel.as_ref().unwrap().title, "설정");
        assert!(s.panel.as_ref().unwrap().form.is_some(), "it is a form, not a listing");
    }

    /// Drives the settings form the way the keyboard does.
    fn press(state: &mut State, code: KeyCode) {
        for action in on_key(state, key(code, KeyModifiers::NONE)) {
            apply(state, &action);
        }
    }

    /// ←→ change the value, ↑↓ change the row, and Enter takes the lot — leaving the
    /// saving itself to the I/O side, which is what `config_out` says.
    #[test]
    fn the_settings_form_takes_the_arrows_and_enter_saves() {
        let mut s = State::new();
        s.lang = crate::lang::Lang::Ko;
        run_command(&mut s, "/config");
        assert_eq!(s.config.dir_access, crate::config::DirAccess::Deny, "the default");

        // The cursor opens on the first row, so → toggles directory access.
        press(&mut s, KeyCode::Right);
        assert_eq!(
            s.panel.as_ref().unwrap().form.unwrap().draft.dir_access,
            crate::config::DirAccess::Allow
        );
        // **Nothing has moved outside the draft yet.**
        assert_eq!(s.config.dir_access, crate::config::DirAccess::Deny, "not saved yet");
        assert!(!s.config_out);

        // Down twice to the mode row, then → picks the first mode.
        press(&mut s, KeyCode::Down);
        press(&mut s, KeyCode::Down);
        press(&mut s, KeyCode::Right);
        assert_eq!(s.panel.as_ref().unwrap().form.unwrap().draft.default_mode, Some(Mode::ALL[0]));

        press(&mut s, KeyCode::Enter);
        assert!(s.panel.is_none(), "Enter closes the form");
        assert_eq!(s.config.dir_access, crate::config::DirAccess::Allow);
        assert_eq!(s.config.default_mode, Some(Mode::ALL[0]));
        assert!(s.config_out, "the I/O side still has to save it and tell the gate");
    }

    /// **Esc throws the draft away.** A form you cannot back out of is a form nobody
    /// experiments in.
    #[test]
    fn esc_closes_the_settings_form_and_changes_nothing() {
        let mut s = State::new();
        s.lang = crate::lang::Lang::Ko;
        let before = s.config;
        run_command(&mut s, "/config");
        press(&mut s, KeyCode::Right);
        press(&mut s, KeyCode::Down);
        press(&mut s, KeyCode::Right);
        press(&mut s, KeyCode::Esc);
        assert!(s.panel.is_none(), "Esc closes the form");
        assert_eq!(s.config, before, "nothing was kept");
        assert_eq!(s.lang, crate::lang::Lang::Ko, "not even the language");
        assert!(!s.config_out, "nothing to save");
    }

    /// Typing must not leak into the message box behind the form, and the form does not
    /// scroll — it is built to fit.
    #[test]
    fn the_settings_form_swallows_everything_else() {
        let mut s = State::new();
        run_command(&mut s, "/config");
        press(&mut s, KeyCode::Char('x'));
        press(&mut s, KeyCode::Backspace);
        assert_eq!(s.input.len_chars(), 0, "typing leaked into the message box");
        assert!(s.panel.is_some(), "the form stayed open");
        assert_eq!(s.panel.as_ref().unwrap().scroll, 0);
    }

    /// `option value` changes the setting and says so — the panel only shows.
    #[test]
    fn the_config_command_sets_and_reports_settings() {
        let mut s = State::new();
        s.lang = crate::lang::Lang::Ko;
        run_command(&mut s, "/config dir allow");
        assert_eq!(s.config.dir_access, crate::config::DirAccess::Allow);
        assert!(last_system(&mut s).contains("허용"), "{}", last_system(&mut s));

        run_command(&mut s, "/config mode 계획");
        assert_eq!(s.config.default_mode, Some(crate::mode::Mode::Plan));

        run_command(&mut s, "/config mode off");
        assert_eq!(s.config.default_mode, None);
    }

    /// `/config lang` changes the screen language.
    #[test]
    fn the_config_command_changes_the_language() {
        let mut s = State::new();
        run_command(&mut s, "/config lang en");
        assert_eq!(s.lang, crate::lang::Lang::En);
    }

    /// `/quit` only raises the flag. Quitting is the I/O side's job — a running turn is
    /// stopped there too.
    #[test]
    fn the_quit_command_asks_the_io_side_to_leave() {
        let mut s = State::new();
        assert!(!s.quitting);
        run_command(&mut s, "/quit");
        assert!(s.quitting);
    }

    /// With nothing changed, say so.
    #[test]
    fn the_changes_list_says_when_nothing_was_touched() {
        let said =
            crate::lang::Lang::Ko.changes_text(&[], std::path::Path::new("/home/ruma/zyris-code"));
        assert!(said.contains("없습니다"), "{said}");
    }

    /// **A created file is marked as such.** Reverting means deleting it, which carries a
    /// different weight.
    #[test]
    fn the_changes_list_shows_counts_and_marks_new_files() {
        let cwd = std::path::Path::new("/home/ruma/zyris-code");
        let said = crate::lang::Lang::Ko.changes_text(
            &[
                crate::undo::Changed {
                    path: cwd.join("src/app.rs"),
                    edits: 3,
                    created: false,
                    added: 42,
                    removed: 7,
                },
                crate::undo::Changed {
                    path: cwd.join("src/enroll.rs"),
                    edits: 1,
                    created: true,
                    added: 90,
                    removed: 0,
                },
            ],
            cwd,
        );
        // Paths are shortened against the working directory — ten absolute paths cannot be
        // told apart by eye.
        assert!(said.contains("`src/app.rs`  +42 −7"), "{said}");
        assert!(said.contains("3번 고침"), "{said}");
        assert!(said.contains("새로 만든 것"), "{said}");
        assert!(!said.contains("/home/ruma"), "an absolute path came out verbatim:\n{said}");
    }

    /// **Typing `/` brings the list up.** Without it there is no way to know what commands
    /// exist.
    #[test]
    fn typing_a_slash_opens_the_command_list() {
        let mut s = State::new();
        typed(&mut s, "/");
        assert!(s.picker.is_some(), "the list did not come up");
        assert!(matches!(
            s.picker.as_ref().map(|p| &p.level),
            Some(crate::picker::Level::Commands)
        ));
    }

    /// It has to narrow as typing goes on for picking to be worth anything.
    #[test]
    fn typing_narrows_the_command_list() {
        let mut s = State::new();
        typed(&mut s, "/mo");
        let rows = &s.picker.as_ref().expect("there is no list").rows;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].label, "/mode");
    }

    /// **Characters are still plain input while the list is up.** If k/j were taken as
    /// movement keys, `/skills` could not be typed.
    #[test]
    fn letters_still_type_while_the_command_list_is_open() {
        let mut s = State::new();
        typed(&mut s, "/s");
        assert_eq!(
            on_key(&s, key(KeyCode::Char('k'), KeyModifiers::NONE)),
            vec![Action::Insert('k')]
        );
        assert_eq!(
            on_key(&s, key(KeyCode::Char('j'), KeyModifiers::NONE)),
            vec![Action::Insert('j')]
        );
    }

    /// **Typing a path has to make the list go away.** `/home/...` is not a command.
    #[test]
    fn a_path_closes_the_command_list() {
        let mut s = State::new();
        typed(&mut s, "/home");
        assert!(s.picker.is_none(), "the list is still up on a path");
    }

    /// Once an argument starts, the list has done its job.
    #[test]
    fn the_command_list_closes_once_an_argument_starts() {
        let mut s = State::new();
        typed(&mut s, "/mode ");
        assert!(s.picker.is_none(), "the list covers the screen while typing an argument");
    }

    /// **A fully typed command must run on the first Enter.** If Enter only ever meant
    /// "pick" because the list is up, typing it to the end and pressing would just rewrite
    /// the same text and do nothing.
    #[test]
    fn a_fully_typed_command_runs_on_the_first_enter() {
        let mut s = State::new();
        typed(&mut s, "/rules");
        assert!(s.picker.is_some(), "the situation has to be one where the list is up");
        assert_eq!(
            on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)),
            vec![Action::Submit("/rules".into())]
        );
    }

    /// Something not fully typed is right to pick from the list.
    #[test]
    fn a_half_typed_command_still_picks_from_the_list() {
        let mut s = State::new();
        typed(&mut s, "/ru");
        assert_eq!(on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)), vec![Action::PickConfirm]);
    }

    /// Submitting closes the list. Left open it covers the screen while the answer arrives.
    #[test]
    fn submitting_closes_the_command_list() {
        let mut s = State::new();
        typed(&mut s, "/cwd");
        apply(&mut s, &Action::Submit("/cwd".into()));
        assert!(s.picker.is_none(), "the list is still there");
    }

    /// Erasing it all closes the list too.
    #[test]
    fn erasing_the_slash_closes_the_list() {
        let mut s = State::new();
        typed(&mut s, "/m");
        apply(&mut s, &Action::Backspace);
        apply(&mut s, &Action::Backspace);
        assert!(s.picker.is_none(), "erased it all, but the list is still there");
    }

    // ── Lists that load off the loop ───────────────────────────────────

    /// **← answers at once.** The list used to be fetched on the draw loop, so between the
    /// press and the server's answer neither keys nor drawing happened — on a big account
    /// that is a window that looks hung.
    #[test]
    fn pressing_left_puts_the_list_up_before_its_rows_arrive() {
        let mut s = state();
        apply(&mut s, &Action::OpenPicker);
        let p = s.picker.as_ref().expect("the list did not open");
        assert!(p.loading, "it must say it is loading: {p:?}");
        assert!(p.rows.is_empty(), "{p:?}");
    }

    fn thread_rows(ids: &[(&str, crate::picker::ThreadStatus)]) -> crate::picker::Picker {
        crate::picker::Picker::sessions(
            "p1".into(),
            "프로젝트".into(),
            ids.iter().map(|(id, st)| ((*id).to_string(), (*id).to_string(), *st)).collect(),
            crate::lang::Lang::Ko,
        )
    }

    /// **A dot that lands late patches its own row.** Deriving each outcome is a request of
    /// its own, so they arrive one at a time — redrawing the whole list per dot would reset
    /// the cursor under whoever is choosing.
    #[test]
    fn a_thread_status_that_lands_late_fills_in_only_its_own_row() {
        use crate::picker::ThreadStatus;
        let mut s = state();
        s.picker = Some(thread_rows(&[("a", ThreadStatus::Unknown), ("b", ThreadStatus::Unknown)]));
        apply(
            &mut s,
            &Action::Frame(Frame::ThreadStatus { id: "b".into(), status: ThreadStatus::Failed }),
        );
        let rows = &s.picker.as_ref().unwrap().rows;
        let at = |id: &str| rows.iter().find(|r| r.id.as_deref() == Some(id)).unwrap().status;
        assert_eq!(at("b"), Some(ThreadStatus::Failed));
        assert_eq!(at("a"), Some(ThreadStatus::Unknown), "an unrelated row moved");
        assert_eq!(s.thread_status.get("b").copied(), Some(ThreadStatus::Failed));
    }

    /// **A running dot is not overwritten by a late outcome.** Running is what is happening
    /// now; the derived one is only how the last turn ended.
    #[test]
    fn a_late_outcome_never_overwrites_a_running_dot() {
        use crate::picker::ThreadStatus;
        let mut s = state();
        s.picker = Some(thread_rows(&[("a", ThreadStatus::Running)]));
        apply(
            &mut s,
            &Action::Frame(Frame::ThreadStatus { id: "a".into(), status: ThreadStatus::Success }),
        );
        let rows = &s.picker.as_ref().unwrap().rows;
        assert_eq!(rows[1].status, Some(ThreadStatus::Running), "{rows:?}");
    }

    /// **A derivation that came back empty leaves a settled dot alone.** It knows less than the
    /// row already does, and writing it back would turn a read thread grey again.
    #[test]
    fn an_empty_derivation_never_greys_out_a_settled_dot() {
        use crate::picker::ThreadStatus;
        let mut s = state();
        s.picker = Some(thread_rows(&[("a", ThreadStatus::Success)]));
        apply(
            &mut s,
            &Action::Frame(Frame::ThreadStatus { id: "a".into(), status: ThreadStatus::Unknown }),
        );
        let rows = &s.picker.as_ref().unwrap().rows;
        assert_eq!(rows[1].status, Some(ThreadStatus::Success), "{rows:?}");
    }

    /// **A refresh never blanks a dot that is already on screen.** It only knows the outcomes
    /// it had cached when it started, and it runs every few seconds — a rebuild that dropped
    /// the rest would make them blink out and back for as long as the derivation took.
    #[test]
    fn a_refresh_keeps_the_dots_that_already_landed() {
        use crate::picker::ThreadStatus;
        let mut s = state();
        s.picker = Some(thread_rows(&[("a", ThreadStatus::Success), ("b", ThreadStatus::Unknown)]));
        apply(
            &mut s,
            &Action::Frame(Frame::Picker {
                picker: thread_rows(&[("a", ThreadStatus::Unknown), ("b", ThreadStatus::Failed)]),
                thread_was_running: None,
            }),
        );
        let rows = &s.picker.as_ref().unwrap().rows;
        let at = |id: &str| rows.iter().find(|r| r.id.as_deref() == Some(id)).unwrap().status;
        assert_eq!(at("a"), Some(ThreadStatus::Success), "a settled dot was blanked");
        assert_eq!(at("b"), Some(ThreadStatus::Failed), "a fresh dot was not taken");
    }

    /// **A list that arrives after the person moved on is dropped.** A slow project list
    /// landing once they are already inside a project would throw them back out.
    #[test]
    fn a_list_that_arrives_too_late_is_dropped() {
        let mut s = state();
        s.picker = Some(thread_rows(&[("a", crate::picker::ThreadStatus::Unknown)]));
        apply(
            &mut s,
            &Action::Frame(Frame::Picker {
                picker: crate::picker::Picker::projects(vec![], crate::lang::Lang::Ko),
                thread_was_running: None,
            }),
        );
        assert!(
            matches!(
                s.picker.as_ref().map(|p| &p.level),
                Some(crate::picker::Level::Sessions { .. })
            ),
            "the thread list was replaced by a stale project list"
        );
    }

    /// **The thread on screen stays until the new one's history lands.** Blanking at the
    /// moment of the click would leave an empty window for however long the fetch takes.
    #[test]
    fn switching_threads_keeps_the_old_one_on_screen_until_the_new_one_arrives() {
        let mut s = state();
        s.timeline.say("앞선 대화");
        s.connected = true;
        s.loading_history = true;
        assert!(s.timeline.items().iter().count() > 0, "it was cleared too early");
        assert_eq!(
            crate::widgets::activity::parts(&s).1,
            s.lang.loading(),
            "the activity line must say what is going on"
        );

        apply(&mut s, &Action::Frame(Frame::History { entries: vec![] }));
        assert!(!s.loading_history);
        assert!(s.timeline.items().is_empty(), "the previous thread was left behind");
    }

    // ── The new-project form

    /// With the form open, characters go to its active field — they must not leak into the
    /// input below.
    #[test]
    fn typing_goes_to_the_form_while_it_is_open() {
        let mut s = state();
        s.new_project = Some(crate::newproject::Form::new());
        apply(&mut s, &Action::Insert('가'));
        apply(&mut s, &Action::FormNext);
        apply(&mut s, &Action::Insert('나'));
        let form = s.new_project.as_ref().unwrap();
        assert_eq!(form.name.text, "가");
        assert_eq!(form.description.text, "나");
        assert!(s.input.text.is_empty(), "it leaked into the input below: {:?}", s.input.text);
    }

    /// Creating. **An empty name is not passed over quietly** — the reason goes on the form.
    #[test]
    fn confirming_without_a_name_says_so() {
        let mut s = state();
        s.new_project = Some(crate::newproject::Form::new());
        apply(&mut s, &Action::FormConfirm);
        let form = s.new_project.as_ref().unwrap();
        assert!(form.error.is_some(), "the name is empty but there is no reason given");
        assert!(s.project_out.is_none(), "must not call the server");
    }

    /// With a name, (name, description) goes to the creating slot — it is not created yet.
    #[test]
    fn confirming_with_a_name_stages_creation() {
        let mut s = state();
        s.new_project = Some(crate::newproject::Form::new());
        for c in "제목".chars() {
            apply(&mut s, &Action::Insert(c));
        }
        apply(&mut s, &Action::FormConfirm);
        assert_eq!(s.project_out, Some(("제목".to_string(), String::new())));
        assert!(s.new_project.is_some(), "the form should remain until it is created");
    }

    /// Esc closes the form — the list stays underneath, so it returns right to that spot.
    #[test]
    fn escaping_closes_the_form_not_the_list() {
        let mut s = state();
        s.picker = Some(crate::picker::Picker::loading_projects());
        s.new_project = Some(crate::newproject::Form::new());
        apply(&mut s, &Action::FormCancel);
        assert!(s.new_project.is_none());
        assert!(s.picker.is_some(), "closing the list too would mean refetching it");
    }

    /// With the form open, keys go to the form.
    #[test]
    fn keys_route_to_the_form_while_it_is_open() {
        let mut s = state();
        s.new_project = Some(crate::newproject::Form::new());
        assert_eq!(on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)), vec![Action::FormConfirm]);
        assert_eq!(on_key(&s, key(KeyCode::Esc, KeyModifiers::NONE)), vec![Action::FormCancel]);
        assert_eq!(on_key(&s, key(KeyCode::Tab, KeyModifiers::NONE)), vec![Action::FormNext]);
        assert_eq!(on_key(&s, key(KeyCode::BackTab, KeyModifiers::NONE)), vec![Action::FormPrev]);
    }

    fn work_start(seq: i64) -> Action {
        Action::Frame(Frame::Event {
            cursor: seq,
            entry: Some(Entry { seq, kind: EntryKind::WorkStart(String::new()) }),
            todo: None,
        })
    }

    /// The fold key Ctrl+O targets — the last card's head.
    fn last_card_key(s: &mut State) -> i64 {
        let item = s
            .timeline
            .items()
            .iter()
            .rev()
            .find(|i| matches!(i, crate::timeline::Item::Work { .. }))
            .expect("no work card");
        item.seq()
    }

    /// The key people press reflexively when the screen breaks. It must get no other meaning.
    #[test]
    fn ctrl_l_asks_for_a_full_repaint() {
        let s = state();
        assert_eq!(
            on_key(&s, key(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            vec![Action::Repaint]
        );
    }

    /// Repainting is the screen's business alone. Touching state would let Ctrl+L change the
    /// work.
    #[test]
    fn repainting_changes_no_state() {
        let mut s = state();
        s.input.insert('가');
        s.view_top = 7;
        apply(&mut s, &Action::Repaint);
        assert_eq!(s.input.text, "가");
        assert_eq!(s.view_top, 7);
    }

    /// A press anywhere starts a drag — blank space included. Only the question screen is
    /// excepted: a click there picks an option instead.
    #[test]
    fn pressing_anywhere_starts_a_drag() {
        let mut s = state();
        apply(&mut s, &Action::Press(5, 3));
        assert_eq!(s.drag, Some(crate::selection::Drag::new((3, 5))));
        assert!(s.dragging, "the drag must be live even over empty cells");
    }

    /// `link_at` returns the URL under a cell in the transcript. Columns are display columns
    /// of the visible line, offset by `view_origin` — same mapping `inject_links` uses.
    #[test]
    fn link_at_finds_the_url_under_a_cell() {
        let mut s = state();
        s.view_origin = (2, 1);
        s.view_links = vec![vec![crate::markdown::Link {
            start: 3,
            end: 8,
            url: "https://example.com/x".into(),
        }]];
        assert_eq!(
            s.link_at(2 + 3, 1),
            Some("https://example.com/x".to_string()),
            "first link column"
        );
        assert_eq!(
            s.link_at(2 + 7, 1),
            Some("https://example.com/x".to_string()),
            "last covered column"
        );
        assert_eq!(s.link_at(2 + 8, 1), None, "column past the link's end");
        assert_eq!(s.link_at(2 + 2, 1), None, "column before the link's start");
        assert_eq!(s.link_at(2, 1), None, "offset column maps to no link");
    }

    /// `link_at` returns `None` outside the transcript area and on lines with no links.
    #[test]
    fn link_at_is_none_outside_the_transcript() {
        let mut s = state();
        s.view_origin = (0, 0);
        s.view_links =
            vec![vec![crate::markdown::Link { start: 0, end: 2, url: "https://e.com/".into() }]];
        assert_eq!(s.link_at(1, 5), None, "line beyond the transcript");
        assert_eq!(s.link_at(1, 0), Some("https://e.com/".to_string()));
        assert_eq!(s.view_links.len(), 1);
    }

    /// A Ctrl+click on a link becomes `Action::OpenLink`; a plain press still starts a drag.
    /// This is exercised through `apply` — the mouse-to-action mapping itself is I/O-side.
    #[test]
    fn open_link_is_a_noop_in_apply() {
        let mut s = state();
        apply(&mut s, &Action::OpenLink("https://example.com/".into()));
        assert!(s.drag.is_none(), "opening a link must not start a selection");
        assert!(!s.dragging, "no drag was begun");
        assert!(s.selection.is_none(), "no selection was made");
    }

    /// The drag extracts from the last drawn screen, not from the conversation alone — the
    /// enrollment code and the status line are selectable too.
    #[test]
    fn dragging_extracts_what_is_on_the_screen() {
        let mut s = state();
        s.screen =
            vec!["first row".to_string(), "second row".to_string(), "세 번째 줄".to_string()];
        apply(&mut s, &Action::Press(0, 0));
        apply(&mut s, &Action::DragTo(4, 2));
        assert_eq!(s.selection.as_deref(), Some("first row\nsecond row\n세 번"));
    }

    /// A drag that covers only blank space selects nothing — the range exists, the text is empty.
    #[test]
    fn dragging_blank_space_selects_nothing() {
        let mut s = state();
        s.screen = vec!["     ".to_string(), "     ".to_string()];
        apply(&mut s, &Action::Press(0, 0));
        apply(&mut s, &Action::DragTo(4, 1));
        assert_eq!(s.selection, None);
    }

    /// A click on empty space folds nothing and leaves no range behind.
    #[test]
    fn clicking_blank_space_does_nothing() {
        let mut s = state();
        apply(&mut s, &Action::Press(30, 12));
        apply(&mut s, &Action::Release);
        assert!(s.drag.is_none());
        assert!(s.selection.is_none());
    }

    /// A click on a work card header still folds it. The drag is in screen coordinates now,
    /// so the row is mapped back to a transcript content row on release.
    #[test]
    fn clicking_a_card_header_folds_it() {
        let mut s = state();
        s.view_origin = (0, 0);
        s.view_top = 0;
        s.view_height = 10;
        s.view_cards.insert(2, 5);
        apply(&mut s, &Action::Press(1, 2));
        apply(&mut s, &Action::Release);
        assert_eq!(s.folds.get(&5).map(|f| f.open), Some(true));
    }

    /// The selection is anchored to the screen; scrolling moves the text out from under it.
    /// Keeping the highlight would point at text different from what was copied.
    #[test]
    fn scrolling_drops_the_drag() {
        let mut s = state();
        s.drag = Some(crate::selection::Drag::new((0, 0)));
        apply(&mut s, &Action::Wheel(-1));
        assert!(s.drag.is_none());
    }

    /// PageUp/PageDown scroll the conversation by a page — the keyboard path that still
    /// works where the wheel does not (mobile SSH, tmux without mouse).
    #[test]
    fn page_keys_scroll_the_transcript() {
        let mut s = state();
        s.view_total = 100;
        s.view_height = 10;
        s.scroll.on_content(100, 10);
        for a in on_key(&s, key(KeyCode::PageUp, KeyModifiers::NONE)) {
            apply(&mut s, &a);
        }
        assert_eq!(s.scroll.top, 80, "one page up");
        for a in on_key(&s, key(KeyCode::PageDown, KeyModifiers::NONE)) {
            apply(&mut s, &a);
        }
        assert_eq!(s.scroll.top, 90, "one page down lands at the bottom");
    }

    /// Page-scrolling drops the drag for the same reason the wheel does — the highlight
    /// would point at different text.
    #[test]
    fn page_scrolling_drops_the_drag() {
        let mut s = state();
        s.drag = Some(crate::selection::Drag::new((0, 0)));
        apply(&mut s, &Action::Page(1));
        assert!(s.drag.is_none());
    }

    /// The title is written by the server — text we did not write. A control character mixed
    /// in cuts the OSC short there and dumps the rest onto the screen as literal text.
    #[test]
    fn a_title_cannot_break_out_of_the_escape_sequence() {
        let out = title_for_osc("깨\u{7}뜨리\u{1b}기\n다음 줄");
        assert_eq!(out, "깨뜨리기다음 줄");
        assert!(!out.chars().any(char::is_control));
    }

    /// A very long title is cut short. Terminals differ in the length they accept.
    #[test]
    fn a_very_long_title_is_cut_short() {
        assert_eq!(title_for_osc(&"가".repeat(500)).chars().count(), 120);
    }

    #[test]
    fn enter_submits_the_typed_text() {
        let mut s = state();
        for c in "안녕".chars() {
            apply(&mut s, &Action::Insert(c));
        }
        let actions = on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(actions, vec![Action::Submit("안녕".into())]);
    }

    #[test]
    fn enter_on_an_empty_input_does_nothing() {
        let s = state();
        assert!(on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)).is_empty());
    }

    /// **Reasoning is hidden from the start.** A pile of thinking must not push out the
    /// screen someone came to for the answer.
    #[test]
    fn a_work_card_starts_folded_and_stays_that_way_while_reasoning_streams() {
        let mut s = state();
        apply(&mut s, &work_start(1));
        assert!(!s.folds[&1].open, "it should be folded from the start");
        apply(
            &mut s,
            &Action::Frame(Frame::Delta { kind: ZDeltaKind::Reasoning, text: "생각 중".into() }),
        );
        assert!(!s.folds[&1].open, "streaming reasoning must not unfold it on its own");
    }

    /// **Nor does it fold on its own.** The screen must not move by itself while it is
    /// unfolded and being read.
    #[test]
    fn an_opened_card_is_never_folded_behind_the_users_back() {
        let mut s = state();
        apply(&mut s, &work_start(1));
        apply(
            &mut s,
            &Action::Frame(Frame::Delta { kind: ZDeltaKind::Reasoning, text: "생각".into() }),
        );
        apply(&mut s, &Action::ToggleFold);
        let key = last_card_key(&mut s);
        assert!(s.folds[&key].open, "one Ctrl+O unfolds the card");
        assert!(s.folds[&key].user_touched, "a manual toggle is remembered");

        for kind in [ZDeltaKind::Reasoning, ZDeltaKind::Assistant] {
            apply(&mut s, &Action::Frame(Frame::Delta { kind, text: "무언가".into() }));
        }
        assert!(s.folds[&key].open, "a delta folded a card the person opened");
    }

    #[test]
    fn ctrl_o_toggles_the_latest_card() {
        let mut s = state();
        apply(&mut s, &work_start(1));
        let actions = on_key(&s, key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(actions, vec![Action::ToggleFold]);
    }

    /// One `todo_add`, as it actually arrives — a `tool_call` event off the wire.
    fn todo_added(seq: i64, id: &str, content: &str) -> Action {
        let payload = serde_json::json!({
            "name": "todo_add",
            "arguments": {"content": content},
            "result": {"id": id, "content": content, "status": "pending"},
        });
        let event = zyris_attacca::ZSessionEvent {
            seq,
            cursor: seq,
            kind: "tool_call".into(),
            payload,
            created_at: None,
        };
        Action::Frame(Frame::Event {
            cursor: seq,
            entry: crate::event::entry_from(&event),
            todo: crate::todos::change_from(&event),
        })
    }

    /// **The whole path, from the event to the list.** `todos.rs` being right is no use if the
    /// frame does not carry the change or `apply` drops it on the floor.
    #[test]
    fn a_todo_tool_call_lands_on_the_sessions_plan() {
        let mut s = state();
        apply(&mut s, &todo_added(1, "t1", "테스트 고치기"));
        assert_eq!(s.todos.items().len(), 1, "{:?}", s.todos.items());
        assert_eq!(s.todos.items()[0].title, "테스트 고치기");
        assert_eq!(s.todos.counts(), (0, 1));
    }

    /// **The plan belongs to the session.** Opening another thread must not leave the one just
    /// left showing its tasks — the same reason the timeline is torn down here.
    #[test]
    fn opening_another_thread_leaves_its_plan_behind() {
        let mut s = state();
        apply(&mut s, &todo_added(1, "t1", "앞 쓰레드의 할 일"));
        apply(&mut s, &Action::Frame(Frame::History { entries: vec![] }));
        assert!(s.todos.is_empty(), "{:?}", s.todos.items());
    }

    #[test]
    fn ctrl_t_unfolds_the_plan_and_folds_it_again() {
        let s = state();
        assert_eq!(
            on_key(&s, key(KeyCode::Char('t'), KeyModifiers::CONTROL)),
            vec![Action::ToggleTodos]
        );
        let mut s = s;
        assert!(!s.todos_open, "it starts folded");
        apply(&mut s, &Action::ToggleTodos);
        assert!(s.todos_open);
        apply(&mut s, &Action::ToggleTodos);
        assert!(!s.todos_open);
    }

    /// **Clicking the activity line opens the plan** — the count is right there, and reaching for
    /// a key to see what it counts is a step too many.
    #[test]
    fn clicking_the_line_that_counts_the_plan_opens_it() {
        let mut s = state();
        apply(&mut s, &todo_added(1, "t1", "할 일"));
        s.activity_row = Some(9);
        apply(&mut s, &Action::Press(4, 9));
        apply(&mut s, &Action::Release);
        assert!(s.todos_open, "a click on the activity line must open the plan");
    }

    /// **With no plan that line is ordinary text.** Taking the click there would cost the ability
    /// to select it, and open nothing in return.
    #[test]
    fn clicking_that_line_with_no_plan_takes_nothing() {
        let mut s = state();
        s.activity_row = Some(9);
        apply(&mut s, &Action::Press(4, 9));
        apply(&mut s, &Action::Release);
        assert!(!s.todos_open);
    }

    /// The position for resuming must not be lost.
    #[test]
    fn the_last_cursor_is_remembered_for_resume() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Event { cursor: 42, entry: None, todo: None }));
        assert_eq!(s.last_cursor, Some(42));
    }

    /// **A frame from a stale session must not reach the screen.** Leave work running and
    /// switch to another session, and the previous session's turn keeps running on the
    /// server with its stream still sending — a tag differing from the current session is
    /// dropped. Anything from off-screen (tools, the bridge) always passes.
    #[test]
    fn a_frame_from_a_stale_session_is_dropped() {
        // What came from off-screen (None) passes regardless of the session.
        assert!(frame_is_current(&None, Some("현재"), 1));
        assert!(frame_is_current(&None, None, 1));
        // A frame from its own session passes.
        assert!(frame_is_current(&Some(Origin::asked("a")), Some("a"), 1));
        // **A frame from a stale session is dropped** — the previous session's messages must
        // not keep coming up after switching the screen.
        assert!(!frame_is_current(&Some(Origin::asked("옛 세션")), Some("새 세션"), 1));
        // A stream frame arriving while there is no session yet is stale too.
        assert!(!frame_is_current(&Some(Origin::asked("어떤 세션")), None, 1));
    }

    /// **A second stream on the same session is the one that doubled the words.**
    ///
    /// `turn_events` is a subscription that never ends on its own, and this app opened one on
    /// every message and every switch, closing none. Two of them hand over the same `Delta`, and
    /// `push_delta` appends — so a session talked to five times streamed its answer five times
    /// over, interleaved. Aborting the old task stops it sending more; this drops what it had
    /// already queued.
    #[test]
    fn a_frame_from_an_abandoned_stream_of_the_same_session_is_dropped() {
        let live = |gen| Some(Origin { session: "a".into(), stream: Some(gen) });
        assert!(frame_is_current(&live(2), Some("a"), 2), "the live stream must pass");
        assert!(!frame_is_current(&live(1), Some("a"), 2), "the abandoned one must not");
        // A one-shot answer (history, a title) carries no stream number and is not affected —
        // nothing about it can pile up.
        assert!(frame_is_current(&Some(Origin::asked("a")), Some("a"), 2));
    }

    /// **Leaving a session abandons its stream then and there**, so the next opening gets a
    /// number of its own and anything still queued from the old one is stale.
    #[test]
    fn leaving_a_session_gives_up_its_stream() {
        let mut s = crate::conn::Session::new(None);
        let first = s.next_stream();
        s.switch_to("a".into(), None);
        assert_ne!(s.stream_gen(), first, "switching must abandon the stream");
        let after_switch = s.stream_gen();
        s.stage_new_default();
        assert_ne!(s.stream_gen(), after_switch, "staging a new one must too");
    }

    /// The wheel moves against the viewport size as last drawn — apply has to stay pure so it
    /// cannot know the size itself and reads what the widget wrote down.
    #[test]
    fn the_wheel_uses_the_last_drawn_viewport() {
        let mut s = state();
        s.view_total = 100;
        s.view_height = 10;
        s.scroll.on_content(100, 10);
        apply(&mut s, &Action::Wheel(1));
        assert_eq!(s.scroll.top, 87);
    }

    /// **Ctrl+C never copies, even with a selection.** It is the one key that stops or quits —
    /// with meanings overlapping there is no telling what happens when it matters.
    #[test]
    fn ctrl_c_never_copies_even_with_a_selection() {
        let mut s = state();
        s.selection = Some("고른 것".into());
        s.running = true;
        assert_eq!(
            on_key(&s, key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Action::Cancel]
        );
    }

    #[test]
    fn ctrl_c_cancels_the_turn_when_nothing_is_selected() {
        let mut s = state();
        s.running = true;
        assert_eq!(
            on_key(&s, key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Action::Cancel]
        );
    }

    /// **The window has to be closable even when the cancel does not take.**
    ///
    /// When the server hangs, `running` stays true. If Ctrl+C only ever went to cancel then,
    /// every press would just send the same request again and never reach quitting.
    #[test]
    fn ctrl_c_after_a_cancel_goes_to_quitting() {
        let mut s = state();
        s.running = true;
        let k = key(KeyCode::Char('c'), KeyModifiers::CONTROL);

        let first = on_key(&s, k);
        assert_eq!(first, vec![Action::Cancel], "the first should be a cancel");
        apply(&mut s, &first[0]);

        let second = on_key(&s, k);
        assert_eq!(second, vec![Action::ArmQuit], "the second arms the quit even while running");
        apply(&mut s, &second[0]);

        assert_eq!(on_key(&s, k), vec![Action::Quit]);
    }

    /// The request to stop lasts only for that turn. On the next turn Ctrl+C has to go back
    /// to cancelling.
    #[test]
    fn asking_to_stop_lasts_only_for_that_turn() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Status { running: true }));
        apply(&mut s, &Action::Cancel);
        assert!(s.stopping);

        // The same state arrives many times while running. It must not release each time.
        apply(&mut s, &Action::Frame(Frame::Status { running: true }));
        assert!(s.stopping, "the same state arriving again mid-run must not release it");

        apply(&mut s, &Action::Frame(Frame::Status { running: false }));
        assert!(!s.stopping, "it should release once the turn ends");
    }

    /// One press arms, the second quits. One accidental press must not quit.
    #[test]
    fn ctrl_c_needs_two_presses_to_quit() {
        let mut s = state();
        let k = key(KeyCode::Char('c'), KeyModifiers::CONTROL);

        let first = on_key(&s, k);
        assert_eq!(first, vec![Action::ArmQuit], "the first should arm the quit");
        apply(&mut s, &first[0]);

        assert_eq!(on_key(&s, k), vec![Action::Quit], "the second should quit");
    }

    /// The arming releases on its own once time passes — a Ctrl+C much later turning into a
    /// quit is a nasty surprise.
    #[test]
    fn the_quit_warning_expires() {
        let mut s = state();
        apply(&mut s, &Action::ArmQuit);
        assert!(s.quit_pending_at(Instant::now()));

        let later = Instant::now() + QUIT_WINDOW + Duration::from_millis(1);
        assert!(!s.quit_pending_at(later), "it should release after 1.5 seconds");
    }

    /// **A notice fades once time passes too.**
    ///
    /// Holding on to a past circumstance means that spot can no longer say what is happening
    /// now — "Zyris로는 아직 만들 수 없습니다" used to stay forever.
    #[test]
    fn a_notice_fades_on_its_own() {
        let mut s = state();
        s.set_status("Zyris로는 아직 만들 수 없습니다");
        assert_eq!(s.status_at(Instant::now()), Some("Zyris로는 아직 만들 수 없습니다"));

        let later = Instant::now() + STATUS_WINDOW + Duration::from_millis(1);
        assert_eq!(s.status_at(later), None, "it should be gone once time passes");
    }

    /// Ctrl+U wipes everything typed. Wherever the cursor is, nothing may remain.
    #[test]
    fn ctrl_u_wipes_the_whole_input() {
        let mut s = state();
        for c in "지울 말".chars() {
            apply(&mut s, &Action::Insert(c));
        }
        s.input.home();
        let actions = on_key(&s, key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(actions, vec![Action::ClearInput]);
        apply(&mut s, &Action::ClearInput);
        assert_eq!(s.input.text, "");
        assert_eq!(s.input.cursor, 0);
    }

    /// As long as the terminal reports it, Ctrl+Backspace does the same thing.
    #[test]
    fn ctrl_backspace_wipes_it_too() {
        let s = state();
        assert_eq!(
            on_key(&s, key(KeyCode::Backspace, KeyModifiers::CONTROL)),
            vec![Action::ClearInput]
        );
    }

    /// A plain Backspace still deletes one character — the arm above must not swallow it.
    #[test]
    fn a_plain_backspace_still_deletes_one_character() {
        let s = state();
        assert_eq!(
            on_key(&s, key(KeyCode::Backspace, KeyModifiers::NONE)),
            vec![Action::Backspace]
        );
    }

    fn sent(texts: &[&str]) -> State {
        let mut s = state();
        for t in texts {
            apply(&mut s, &Action::Submit(t.to_string()));
        }
        s
    }

    /// With the input empty, ↑ brings back the last thing sent.
    #[test]
    fn up_brings_back_the_last_thing_sent() {
        let mut s = sent(&["첫 말", "둘째 말"]);
        assert_eq!(on_key(&s, key(KeyCode::Up, KeyModifiers::NONE)), vec![Action::RecallOlder]);
        apply(&mut s, &Action::RecallOlder);
        assert_eq!(s.input.text, "둘째 말");
        assert_eq!(
            s.input.cursor,
            s.input.len_chars(),
            "the cursor must sit at the end to keep typing"
        );
    }

    /// **It must not stop at the second ↑.** After a recall the input is no longer empty, so
    /// the single condition "only when empty" would only ever walk back one step.
    #[test]
    fn up_keeps_walking_further_back() {
        let mut s = sent(&["첫 말", "둘째 말"]);
        apply(&mut s, &Action::RecallOlder);
        assert_eq!(on_key(&s, key(KeyCode::Up, KeyModifiers::NONE)), vec![Action::RecallOlder]);
        apply(&mut s, &Action::RecallOlder);
        assert_eq!(s.input.text, "첫 말");
        // There is nothing further back. It holds at the top.
        apply(&mut s, &Action::RecallOlder);
        assert_eq!(s.input.text, "첫 말");
    }

    /// Walking down past the bottom empties the input — back to where the typing was.
    #[test]
    fn down_walks_back_out_of_the_history() {
        let mut s = sent(&["첫 말", "둘째 말"]);
        apply(&mut s, &Action::RecallOlder);
        apply(&mut s, &Action::RecallNewer);
        assert_eq!(s.input.text, "");
        assert!(!s.recalling());
    }

    /// Editing a recalled message leaves the history. Otherwise one ↓ would lose the edit.
    #[test]
    fn editing_a_recalled_message_leaves_the_history() {
        let mut s = sent(&["보낸 말"]);
        apply(&mut s, &Action::RecallOlder);
        apply(&mut s, &Action::Insert('!'));
        assert!(!s.recalling());
        assert_eq!(on_key(&s, key(KeyCode::Down, KeyModifiers::NONE)), vec![]);
        assert_eq!(s.input.text, "보낸 말!");
    }

    /// With nothing ever sent, ↑ does nothing.
    #[test]
    fn up_does_nothing_with_an_empty_history() {
        let mut s = state();
        apply(&mut s, &Action::RecallOlder);
        assert_eq!(s.input.text, "");
    }

    /// The same message sent twice is remembered once — passing the same line twice while
    /// recalling makes recall look broken.
    #[test]
    fn the_same_message_twice_is_remembered_once() {
        let s = sent(&["같은 말", "같은 말"]);
        assert_eq!(s.sent.len(), 1);
    }

    /// **What is typed mid-turn is held, not sent.** It does not enter the sent history yet —
    /// only what actually went out may count as "sent".
    #[test]
    fn typing_during_a_turn_queues_instead_of_sending() {
        let mut s = state();
        s.running = true;
        apply(&mut s, &Action::Submit("일하는 중에 친 말".into()));
        assert_eq!(s.queued, vec!["일하는 중에 친 말"]);
        assert!(s.sent.is_empty(), "it entered the sent history before being sent");
        assert_eq!(s.input.text, "", "the input must be cleared");
    }

    /// Says to drain the queue the moment the turn ends. The sending itself is done by the I/O side.
    #[test]
    fn the_end_of_a_turn_asks_for_the_queue_to_be_flushed() {
        let mut s = state();
        s.running = true;
        apply(&mut s, &Action::Submit("나중에 보낼 말".into()));
        apply(&mut s, &Action::Frame(Frame::Status { running: false }));
        assert!(s.flush_queue, "the turn ended but there is no signal to send");
    }

    /// With an empty queue there is nothing to send when the turn ends.
    #[test]
    fn an_empty_queue_asks_for_nothing() {
        let mut s = state();
        s.running = true;
        apply(&mut s, &Action::Frame(Frame::Status { running: false }));
        assert!(!s.flush_queue);
    }

    /// **↑ takes from the queue first.** That is the only message still open to editing.
    #[test]
    fn up_pulls_the_queued_message_back_first() {
        let mut s = state();
        apply(&mut s, &Action::Submit("이미 보낸 말".into()));
        s.running = true;
        apply(&mut s, &Action::Submit("대기 중인 말".into()));

        apply(&mut s, &Action::RecallOlder);
        assert_eq!(s.input.text, "대기 중인 말");
        assert!(s.queued.is_empty(), "once taken it must leave the queue");
        assert_eq!(s.input.cursor, s.input.len_chars());
    }

    /// Taking one out and pressing ↑ again must not lose what is being edited now.
    #[test]
    fn pulling_a_queued_message_back_stops_the_walk() {
        let mut s = state();
        apply(&mut s, &Action::Submit("이미 보낸 말".into()));
        s.running = true;
        apply(&mut s, &Action::Submit("대기 중인 말".into()));
        apply(&mut s, &Action::RecallOlder);
        assert_eq!(on_key(&s, key(KeyCode::Up, KeyModifiers::NONE)), vec![]);
    }

    /// **No delta ever touches a fold.**
    ///
    /// This is where the "it unfolds by itself" a user hit came from — pausing to think while
    /// answering made a Reasoning delta arrive late, and each one refolded the card so the
    /// answer being read was pushed down. Auto-folding is gone, so there is no room for that now.
    #[test]
    fn no_delta_ever_changes_a_fold() {
        let mut s = state();
        apply(&mut s, &work_start(1));
        for kind in [ZDeltaKind::Reasoning, ZDeltaKind::Assistant, ZDeltaKind::Reasoning] {
            apply(&mut s, &Action::Frame(Frame::Delta { kind, text: "무언가".into() }));
            assert!(!s.folds[&1].open, "a delta unfolded the card");
        }
    }

    /// **A new stretch of working carries no choice of the person's.** What they folded was that
    /// card, not every card to come — and a fold inherited from an earlier one would look like the
    /// screen deciding for them.
    #[test]
    fn a_new_work_run_does_not_inherit_a_fold() {
        let mut s = state();
        apply(&mut s, &work_start(1));
        apply(&mut s, &Action::ToggleFold);
        let first = last_card_key(&mut s);
        assert!(s.folds[&first].user_touched, "Ctrl+O must be remembered on the card");

        // Speaking closes the stretch; what is thought next opens a fresh one.
        apply(
            &mut s,
            &Action::Frame(Frame::Event {
                cursor: 2,
                entry: Some(Entry { seq: 2, kind: EntryKind::Agent("먼저 볼게요".into()) }),
                todo: None,
            }),
        );
        apply(
            &mut s,
            &Action::Frame(Frame::Delta { kind: ZDeltaKind::Reasoning, text: "새 생각".into() }),
        );
        let second = last_card_key(&mut s);
        assert_ne!(second, first, "speaking must have opened a new card");
        let f = s.folds.get(&second).copied().unwrap_or_default();
        assert!(!f.user_touched, "a fresh card must not be the person's choice");
    }

    // ── Tool approval ──────────────────────────────────────────────────

    /// An open shell that never reaches the screen becomes a ghost shell.
    /// What runs in the background must be on screen. **Unseen, a person quits the app not knowing.**
    #[test]
    fn a_background_job_shows_up_and_leaves_when_it_ends() {
        let mut s = state();
        let start = Frame::JobStart { id: "b1".into(), label: "cargo build".into() };
        apply(&mut s, &Action::Frame(start.clone()));
        assert_eq!(s.jobs.len(), 1);
        // The same id arriving twice must not produce two rows.
        apply(&mut s, &Action::Frame(start));
        assert_eq!(s.jobs.len(), 1);

        apply(&mut s, &Action::Frame(Frame::JobEnded { id: "b1".into(), ok: true, secs: 252 }));
        assert!(s.jobs.is_empty());
        // A finished one is said once on the status line and then disappears.
        assert!(s.status().is_some_and(|t| t.contains("b1")), "{:?}", s.status());
    }

    /// The activity line **picks something more specific than "working…".** It does so even while
    /// a turn is running — that turn is usually waiting on this job.
    #[test]
    fn the_activity_line_prefers_the_background_job_over_working() {
        let mut s = state();
        s.connected = true;
        s.running = true;
        apply(
            &mut s,
            &Action::Frame(Frame::JobStart { id: "b1".into(), label: "cargo build".into() }),
        );
        let (_, text, _) = crate::widgets::activity_parts_at(&s, std::time::Instant::now());
        assert!(text.contains("b1") && text.contains("cargo build"), "{text}");
    }

    /// **`Esc 정지` stops this session's turn and nothing else.** A tool call reaches this node
    /// with no session on it — attacca sends `zyris__node__cap__tool` and nothing more — and
    /// another window on the same directory shares the node besides. So work running here while
    /// this conversation is idle belongs to somebody else: shown, because the machine really is
    /// busy, but without a hint that would not do what it says.
    #[test]
    fn work_that_is_not_this_conversations_gets_no_stop_hint() {
        let mut s = state();
        s.connected = true;
        let job = Action::Frame(Frame::JobStart { id: "b1".into(), label: "cargo build".into() });
        apply(&mut s, &job);

        s.running = true;
        let (mine, _, hint) = crate::widgets::activity_parts_at(&s, std::time::Instant::now());
        assert_eq!(hint, s.lang.esc_stops(), "our own turn can be stopped");

        s.running = false;
        let (theirs, text, hint) = crate::widgets::activity_parts_at(&s, std::time::Instant::now());
        assert!(text.contains("cargo build"), "the machine being busy is still said: {text}");
        assert_eq!(hint, "", "nothing here for Esc to stop");
        assert_ne!(theirs, mine, "and it must not be painted as this conversation's");
    }

    /// The same rule for `exec`, which used to hand out the hint unconditionally.
    #[test]
    fn a_command_running_for_someone_else_gets_no_stop_hint() {
        let mut s = state();
        s.connected = true;
        apply(&mut s, &Action::Frame(Frame::ExecStart { id: 1, command: "sleep 30".into() }));
        let (_, text, hint) = crate::widgets::activity_parts_at(&s, std::time::Instant::now());
        assert!(text.contains("sleep 30"), "{text}");
        assert_eq!(hint, "", "this session has no turn to stop");
    }

    /// **What a conversation had to say goes with it.** "could not send" from the thread just
    /// left, on the line that says what is happening here, reads as this thread failing — and a
    /// notice outranks "loading…", so it would stand for the whole fetch.
    #[test]
    fn news_about_the_thread_just_left_does_not_follow_you() {
        let mut s = state();
        s.set_error("보내지 못했습니다");
        assert!(s.status().is_some());
        apply(&mut s, &Action::Frame(Frame::History { entries: vec![] }));
        assert_eq!(s.status(), None, "{:?}", s.status());
    }

    /// `/jobs` says **only the list and how to stop**. Dumping logs would cover the transcript.
    #[test]
    fn the_jobs_command_lists_what_runs_and_how_to_stop_it() {
        let mut s = state();
        // The words come from `lang` — hardcoding them here breaks when the screen language changes.
        assert_eq!(jobs_text(&s.jobs, s.lang), s.lang.jobs_none());
        apply(
            &mut s,
            &Action::Frame(Frame::JobStart { id: "b1".into(), label: "cargo build".into() }),
        );
        let text = jobs_text(&s.jobs, s.lang);
        assert!(text.contains("b1") && text.contains("cargo build"), "{text}");
        assert!(text.contains("/jobs stop"), "{text}");
    }

    #[test]
    fn opening_and_closing_a_shell_updates_the_shells_list() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::ShellOpened { id: "p1".into(), name: "zsh".into() }));
        assert_eq!(s.shells.len(), 1);
        // The same one arriving twice must not produce two entries.
        apply(&mut s, &Action::Frame(Frame::ShellOpened { id: "p1".into(), name: "zsh".into() }));
        assert_eq!(s.shells.len(), 1);
        apply(&mut s, &Action::Frame(Frame::ShellClosed { id: "p1".into() }));
        assert!(s.shells.is_empty());
    }

    /// Cancelling only means something while something is running.
    #[test]
    fn esc_cancels_only_while_a_turn_runs() {
        let mut s = state();
        assert!(on_key(&s, key(KeyCode::Esc, KeyModifiers::NONE)).is_empty());
        apply(&mut s, &Action::Frame(Frame::Status { running: true }));
        assert_eq!(on_key(&s, key(KeyCode::Esc, KeyModifiers::NONE)), vec![Action::Cancel]);
    }

    /// **Closing the window stops it on the server too.** Otherwise the far side keeps thinking
    /// after this side is gone, failing to find the missing node on every tool call, burning credit.
    #[test]
    fn quitting_mid_turn_stops_the_turn_on_the_server() {
        let mut s = state();
        let mut session = Session::new(None);
        session.switch_to("세션-1".into(), None);

        apply(&mut s, &Action::Frame(Frame::Status { running: true }));
        assert_eq!(turn_to_stop(&s, &session), Some("세션-1".into()));
    }

    /// With nothing running we do not ask it to stop — no pointless round trip on the way out.
    #[test]
    fn quitting_while_idle_says_nothing_to_the_server() {
        let mut s = state();
        let mut session = Session::new(None);
        session.switch_to("세션-1".into(), None);
        assert_eq!(turn_to_stop(&s, &session), None);

        // A session that never sent a first message does not exist on the server. There is nothing to stop.
        apply(&mut s, &Action::Frame(Frame::Status { running: true }));
        assert_eq!(turn_to_stop(&s, &Session::new(None)), None);
    }

    /// **Leaving a session clears the previous one's turn state.** Without that the status line
    /// freezes on "working" (a new session has no stream, so nobody sets it back to false) and
    /// what was held goes out to the wrong session.
    #[test]
    fn leaving_a_session_clears_the_old_turn_state() {
        let mut s = state();
        s.running = true;
        s.stopping = true;
        s.queued.push("나중에 보낼 말".into());
        s.flush_queue = true;

        leave_session(&mut s);

        assert!(!s.running, "running is still set");
        assert!(!s.stopping, "stopping is still set");
        assert!(s.queued.is_empty(), "the queue was not drained: {:?}", s.queued);
        assert!(!s.flush_queue, "the flush signal is still set");
    }

    /// **A message sent in a new thread is not tied to the previous session's turn.** The previous
    /// turn state was cleared (`leave_session`), so what is typed here does not leak into the queue
    /// and goes straight into the sent history. Without that clearing, `running` would still be set,
    /// this message would join the queue, and a new session has no stream to drain it.
    #[test]
    fn a_fresh_thread_does_not_queue_messages_behind_the_old_turn() {
        let mut s = state();
        s.running = true; // the previous session's turn is running
        apply(&mut s, &Action::Submit("앞 턴에 담아 둔 말".into()));
        assert_eq!(s.queued, vec!["앞 턴에 담아 둔 말"]);

        // A new thread was opened — the previous session's turn state gets cleared.
        leave_session(&mut s);
        assert!(
            s.queued.is_empty(),
            "the queue from the previous turn survived into the new screen: {:?}",
            s.queued
        );

        // A message sent in a new thread does not leak into the queue; it enters the sent history at once.
        apply(&mut s, &Action::Submit("새 쓰레드에서 보낸 말".into()));
        assert!(
            s.queued.is_empty(),
            "the message for the new thread went to the queue: {:?}",
            s.queued
        );
        assert_eq!(s.sent, vec!["새 쓰레드에서 보낸 말"]);
    }

    /// What the strip shows is replaced wholesale, never merged.
    #[test]
    fn a_git_frame_replaces_what_the_strip_shows() {
        let mut s = State::new();
        assert_eq!(s.repo, None);
        let got = crate::repo::Repo { branch: "main".into(), staged: 1, ..Default::default() };
        apply(&mut s, &Action::Frame(Frame::Git(Some(got.clone()))));
        assert_eq!(s.repo, Some(got));
        // Leaving a repository behind must clear it, not keep the last thing it said.
        apply(&mut s, &Action::Frame(Frame::Git(None)));
        assert_eq!(s.repo, None);
    }

    /// **An Enter arriving mid-burst becomes a newline** — the paste protection for
    /// terminals without bracketed paste. But only when Enter really means "send" there.
    #[test]
    fn a_burst_enter_while_typing_becomes_a_newline() {
        let mut s = state();
        apply(&mut s, &Action::Insert('a'));
        assert!(
            enter_becomes_newline(&s, &key(KeyCode::Enter, KeyModifiers::NONE), true),
            "a burst Enter must be a newline"
        );
    }

    /// Outside a burst an Enter is a send — the interval between keys decides.
    #[test]
    fn a_lone_enter_is_not_a_paste_newline() {
        let mut s = state();
        apply(&mut s, &Action::Insert('a'));
        assert!(!enter_becomes_newline(&s, &key(KeyCode::Enter, KeyModifiers::NONE), false));
    }

    /// A Windows key-release must not become a newline — the same filtering `on_key` does.
    #[test]
    fn a_burst_release_enter_is_not_a_newline() {
        let mut s = state();
        apply(&mut s, &Action::Insert('a'));
        let release =
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release);
        assert!(!enter_becomes_newline(&s, &release, true));
    }

    /// With the command picker open, Enter is a pick. A burst must not swallow it — the
    /// case where a quick ↓→Enter inserted a newline into `/m` instead of picking.
    #[test]
    fn a_burst_enter_does_not_swallow_the_pickers_confirm() {
        let mut s = state();
        for c in "/m".chars() {
            apply(&mut s, &Action::Insert(c));
        }
        assert!(s.picker.is_some(), "the command list must be open");
        assert!(!enter_becomes_newline(&s, &key(KeyCode::Enter, KeyModifiers::NONE), true));
    }

    /// With the new-project form open, Enter confirms — it must not be swallowed by a burst.
    #[test]
    fn a_burst_enter_does_not_swallow_the_form_confirm() {
        let mut s = state();
        s.new_project = Some(crate::newproject::Form::new());
        assert!(!enter_becomes_newline(&s, &key(KeyCode::Enter, KeyModifiers::NONE), true));
    }

    /// While choosing an answer to a question, Enter confirms — same spot as a list.
    #[test]
    fn a_burst_enter_does_not_swallow_the_questions_confirm() {
        let mut s = state();
        s.asking = Some((
            7,
            crate::question::Answering::new(vec![crate::question::Step {
                header: None,
                question: "어느 쪽?".into(),
                multi: false,
                options: vec![
                    crate::question::Opt { label: "A".into(), description: None },
                    crate::question::Opt { label: "B".into(), description: None },
                ],
            }]),
        ));
        assert!(!enter_becomes_newline(&s, &key(KeyCode::Enter, KeyModifiers::NONE), true));
    }

    /// While typing free text in a question, that input is the send target — pasting
    /// several lines in a burst must not submit on the first line.
    #[test]
    fn a_burst_enter_keeps_multiline_pastes_in_the_free_answer() {
        let mut s = state();
        s.asking = Some((
            7,
            crate::question::Answering::new(vec![crate::question::Step {
                header: None,
                question: "하고 싶은 말?".into(),
                multi: false,
                options: vec![],
            }]),
        ));
        {
            let (_, a) = s.asking.as_mut().unwrap();
            a.typing = true;
            a.input.insert_str("한 줄");
        }
        assert!(enter_becomes_newline(&s, &key(KeyCode::Enter, KeyModifiers::NONE), true));
        // An empty field has nothing to newline — it submits as-is.
        {
            let (_, a) = s.asking.as_mut().unwrap();
            a.input.take();
        }
        assert!(!enter_becomes_newline(&s, &key(KeyCode::Enter, KeyModifiers::NONE), true));
    }

    /// **A failing terminal feature does not kill the screen.** On Windows crossterm 0.29's
    /// kitty keyboard protocol command (`PushKeyboardEnhancementFlags`) dies with
    /// `Unsupported`. Bundled in one `execute!`, that single command killed the whole
    /// screen — with no screen, instead of the enrollment code window the shell's waiting
    /// line was all you saw. The helper swallows the failure and carries on.
    #[test]
    fn a_failing_terminal_feature_does_not_kill_the_screen() {
        // The same failure `PushKeyboardEnhancementFlags` emits on Windows.
        struct Unsupported;
        impl crossterm::Command for Unsupported {
            fn write_ansi(&self, _f: &mut impl std::fmt::Write) -> std::fmt::Result {
                Ok(())
            }
            #[cfg(windows)]
            fn execute_winapi(&self) -> std::io::Result<()> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "kitty keyboard protocol is not supported on Windows",
                ))
            }
        }
        // It must pass quietly — only the failed feature is lost, the screen still comes up.
        terminal_feature("unsupported", Unsupported);
    }
}
