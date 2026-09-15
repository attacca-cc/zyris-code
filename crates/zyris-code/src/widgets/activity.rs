//! The one line right above the input — **what is happening right now.**
//!
//! This beats keeping a connection status pinned in a corner of the screen. What people want
//! to know is not "am I connected" but "is it my turn, or do I wait". The connection is only
//! mentioned when it drops — no reason to keep announcing what works.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;
use crate::markdown::display_width;
use crate::theme;

/// How long each half of the blink lasts. **A duration, not a number of frames.**
///
/// Eight frames used to mean this, which was 0.4s at 20fps — and 0.13s the day the local default
/// became 60fps (`render_cadence`). A blink is a tempo, and a frame count is not one: the dot was
/// reported as flickering the same afternoon (2026-09-14).
pub const BLINK_HALF_MS: u64 = 400;

/// Whether the dot is lit, given how long it has been blinking.
///
/// **Milliseconds, so tests still never wait on time.** The caller reads the clock
/// (`State::blink_ms`); this stays a pure function of a number, and the same number lights the
/// dot at any frame rate.
pub fn blink_on(elapsed_ms: u64) -> bool {
    (elapsed_ms / BLINK_HALF_MS).is_multiple_of(2)
}

/// What appears on this line: (dot color, text, hint). Pure — tests look at this.
pub fn parts(state: &State) -> (ratatui::style::Color, String, &'static str) {
    parts_at(state, std::time::Instant::now())
}

/// The variant that takes a time. Tests set the elapsed time and inspect it.
pub fn parts_at(
    state: &State,
    now: std::time::Instant,
) -> (ratatui::style::Color, String, &'static str) {
    let lang = state.lang;
    // What you need to know now goes on top. The quit notice comes before anything else.
    if state.quit_pending() {
        return (theme::warning(), lang.quit_armed().to_string(), "");
    }
    // Notices disappear on their own after a while — `State::status` makes that call.
    //
    // **An error is not a notice.** These were one colour, so "could not send" and "connected"
    // looked identical on the one line that exists to say what is going on. `set_error` marks
    // the ones that mean something is wrong.
    if let Some(s) = state.status_at(now) {
        let colour = match state.status_severity_at(now) {
            crate::app::Severity::Error => theme::danger(),
            crate::app::Severity::Notice => theme::notice(),
        };
        return (colour, s.to_string(), "");
    }
    // **Waiting is not failing.** Before the first enrolment is approved there is nothing wrong
    // yet — the code is on screen and a person is walking to a browser.
    if !state.connected {
        return (theme::notice(), lang.connecting().to_string(), "");
    }
    // **More specific than "working…".** A command gives its result once, when done, so unless
    // we say here what is running, people wait it out blind — and a command is no longer cut at a
    // minute, so that wait is as long as the command is.
    // **Saying you asked to stop comes first.** Until the server answers, "working" stays up,
    // and while it keeps showing, people think the key they pressed did not work and press it
    // again — the hint at the end of this line says which key that is (Esc).
    // **Fetching a thread's history is something happening**, and on a long one it takes a
    // while. Unsaid, the window looks stuck on the thread the person just left.
    if state.loading_history {
        return (theme::notice(), lang.loading().to_string(), "");
    }
    if state.running && state.stopping {
        return (theme::warning(), lang.stopping().to_string(), lang.ctrl_c_quits());
    }
    // **How far along the plan is rides on the end of whichever line describes the moment.**
    // Not on the transient ones above — a notice or an error owns the line while it is up, and a
    // count beside "could not send" says nothing about it.
    let plan = plan_count(state);
    // **What is running on this machine is not necessarily this conversation's.** A tool call
    // arrives as `zyris__node__cap__tool` with no session on it, so the node cannot tell which
    // conversation asked — and another window on the same directory shares this node besides. So
    // ownership is read off the one thing that is known: whether **this** session has a turn
    // running. If it does not, the work is somebody else's — shown, because the machine really is
    // busy, but dimmed and without a hint that would not do what it says.
    let ours = state.running;
    let colour = if ours { theme::accent() } else { theme::text_muted() };
    // **`Esc 정지` stops this session's turn and nothing else.** Beside work that belongs to
    // another conversation it is a lie, and pressing it would look broken.
    let stop = if ours { lang.esc_stops() } else { "" };
    if let Some((_, command, since)) = &state.running_exec {
        let secs = now.saturating_duration_since(*since).as_secs();
        return (colour, lang.running_command(command, secs) + &plan, stop);
    }
    // **What runs in the background is more specific than "working…".** It is shown even while a
    // turn is running — that turn is usually waiting on this job, and what a person wants to know
    // is what has been running and for how long. Unseen, they quit the app and kill the build.
    // **Other conversations' jobs are not this line's news.** A job outlives the thread that
    // started it, and this window runs commands for every session on the account — so a row from a
    // conversation nobody is looking at used to sit here, describing work this conversation never
    // asked for. Naming it as somebody else's (which this did next) is still this conversation
    // being told about work that is not happening here: the line's one job is to say what is going
    // on *now*, and a build that belongs to another thread is not an answer to that. It is not
    // hidden — `/jobs` lists it, marked, and quitting the app still kills it with the rest.
    let ours: Vec<_> = state
        .jobs
        .iter()
        .filter(|j| j.session.as_deref().is_none_or(|s| Some(s) == state.session_id.as_deref()))
        .collect();
    if let Some(job) = ours.first() {
        let secs = now.saturating_duration_since(job.since).as_secs();
        let text = lang.background_job(ours.len(), &job.id, &job.label, secs);
        return (colour, text + &plan, stop);
    }
    if state.running {
        return (theme::accent(), lang.working().to_string() + &plan, lang.esc_stops());
    }
    if state.asking.is_some() {
        return (theme::warning(), lang.waiting_answer().to_string(), lang.waiting_answer_hint());
    }
    // No hint when idle. An always-on hint stops getting read.
    //
    // **The count stays after the turn ends.** A plan left half-finished is exactly what a person
    // wants to see when the agent stops, and if it only showed while running there would be no
    // moment left in which to open the list and read it.
    (theme::text_muted(), lang.idle().to_string() + &plan, "")
}

/// `" (2/5)"` while this session has a plan, and nothing at all when it does not.
fn plan_count(state: &State) -> String {
    let (done, total) = state.todos.counts();
    if total == 0 {
        return String::new();
    }
    state.lang.todo_count(done, total)
}

pub fn draw(frame: &mut Frame, area: Rect, state: &State) {
    let (colour, label, hint) = parts(state);

    // The dot blinks only while working. A still dot does not say "it is running".
    let lit = !state.running || blink_on(state.blink_ms());
    let dot = Style::default().fg(if lit { colour } else { theme::border_light() });

    // **The dot goes at the far left.** It does not align with the conversation's margin —
    // this line is not the conversation but the screen's own status, and at the left edge the eye always finds it in the same place.
    let mut spans = vec![
        Span::styled("● ", dot),
        Span::styled(label.clone(), Style::default().fg(colour).add_modifier(Modifier::BOLD)),
    ];

    // The hint goes at the right edge. If narrow, drop it entirely — the status comes first.
    let used = 2 + display_width(&label);
    let room = area.width as usize;
    if !hint.is_empty() && used + display_width(hint) + 2 <= room {
        let gap = room - used - display_width(hint);
        spans.push(Span::styled(" ".repeat(gap), Style::default().fg(theme::text())));
        spans.push(Span::styled(hint, Style::default().fg(theme::border_light())));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The blink keeps its tempo whatever the frame rate is.** One second of frames stepped
    /// three ways has to turn the dot the same number of times: eight frames was 0.4s at 20fps and
    /// 0.13s at 60fps, which is what a person saw as a flicker (2026-09-14).
    #[test]
    fn the_blink_keeps_its_tempo_at_any_frame_rate() {
        for frame_ms in [50u64, 33, 16] {
            let states: Vec<bool> = (0..1000u64).step_by(frame_ms as usize).map(blink_on).collect();
            // 1000ms over a 400ms half-period is lit, dark, lit — two turns, never more.
            let turns = states.windows(2).filter(|pair| pair[0] != pair[1]).count();
            assert_eq!(
                turns, 2,
                "the blink ran at another tempo on {frame_ms}ms frames: {states:?}"
            );
        }
    }

    /// Both halves last the same, and the first one starts lit — 400ms on, 400ms off.
    #[test]
    fn a_half_period_is_four_hundred_milliseconds() {
        assert!(blink_on(0), "the dot starts dark");
        assert!(blink_on(BLINK_HALF_MS - 1), "the lit half ended early");
        assert!(!blink_on(BLINK_HALF_MS), "the dark half did not start");
        assert!(!blink_on(BLINK_HALF_MS * 2 - 1), "the dark half ended early");
        assert!(blink_on(BLINK_HALF_MS * 2), "the second half did not close");
    }
}
