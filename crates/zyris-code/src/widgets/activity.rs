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

/// Whether the dot is lit. Flips every eight frames (0.4s) at 20fps.
///
/// The blink is set by frame count rather than a clock so that **tests do not have to wait
/// on time**. The drawing side stays pure.
pub fn blink_on(tick: u64) -> bool {
    (tick / 8).is_multiple_of(2)
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
    // we say here what is running, people wait up to 55 seconds blind.
    // **Saying you asked to stop comes first.** Until the server answers, "working" stays up,
    // and while it keeps showing, people think Ctrl+C did not work and press again.
    if state.running && state.stopping {
        return (theme::warning(), lang.stopping().to_string(), lang.ctrl_c_quits());
    }
    if let Some((_, command, since)) = &state.running_exec {
        let secs = now.saturating_duration_since(*since).as_secs();
        return (theme::accent(), lang.running_command(command, secs), lang.esc_stops());
    }
    // **What runs in the background is more specific than "working…".** It is shown even while a
    // turn is running — that turn is usually waiting on this job, and what a person wants to know
    // is what has been running and for how long. Unseen, they quit the app and kill the build.
    if let Some(job) = state.jobs.first() {
        let secs = now.saturating_duration_since(job.since).as_secs();
        let text = lang.background_job(state.jobs.len(), &job.id, &job.label, secs);
        let hint = if state.running { lang.esc_stops() } else { "" };
        return (theme::accent(), text, hint);
    }
    if state.running {
        return (theme::accent(), lang.working().to_string(), lang.esc_stops());
    }
    if state.asking.is_some() {
        return (theme::warning(), lang.waiting_answer().to_string(), lang.waiting_answer_hint());
    }
    // No hint when idle. An always-on hint stops getting read.
    (theme::text_muted(), lang.idle().to_string(), "")
}

pub fn draw(frame: &mut Frame, area: Rect, state: &State) {
    let (colour, label, hint) = parts(state);

    // The dot blinks only while working. A still dot does not say "it is running".
    let lit = !state.running || blink_on(state.tick);
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
