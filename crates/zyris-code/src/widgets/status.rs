//! The one line under the input field — mode and agent on the left, session usage on the right.
//!
//! **Connection status is not here.** There's no reason to always show "connected fine" in a screen
//! corner, so it was removed. What's happening right now is told by the `activity` line above the input.
//!
//! **Usage lives here, at the right edge.** The right sidebar is gone; the numbers that mattered in
//! it — credits, context, tokens — moved down to the same line as the agent, where the eye already
//! lands at the bottom of the screen. When they don't fit beside mode·agent, they are dropped: the
//! status comes first.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;
use crate::theme;
use crate::usage::{compact, context_limit};

/// How much of the project name the bottom bar shows.
///
/// **A name is an identifier, not a sentence** — a few words in, one project is already told
/// from another, and past that it only pushes mode·agent around and crowds out the usage. Cut
/// names get a `…` so a truncated one is never mistaken for the whole thing.
const PROJECT_WIDTH: usize = 16;

/// The mode·agent·project group. Pure, so a test can read it without a terminal.
pub fn left_spans(state: &State) -> Vec<Span<'static>> {
    // Attach at the far left — aligns with the dot of the status line directly above.
    //
    // No spaces around the middle dot. Mode and agent are meant to be read as **one set**, so a gap
    // makes them look like two separate pieces of info — close together, the eye takes them in at once.
    let mut left = vec![
        Span::styled(state.mode.label(state.lang), Style::default().fg(state.mode.color())),
        Span::styled("∙", Style::default().fg(theme::border_light())),
        Span::styled(
            if state.agent.is_empty() { "-" } else { state.agent.as_str() }.to_string(),
            Style::default().fg(theme::text_muted()),
        ),
    ];

    // **Which project the next job or work lands in.** It is decided here and nowhere on
    // screen said, so a work opened after wandering through the list went somewhere the person
    // could not check. Spaced apart from mode·agent, which are one set on their own.
    if let Some(name) = &state.project_name {
        left.push(Span::styled(" ∙ ", Style::default().fg(theme::border_light())));
        left.push(Span::styled(
            crate::markdown::truncate_to(name, PROJECT_WIDTH),
            Style::default().fg(theme::text_muted()),
        ));
    }

    // **If there's unsent text, it must be said.** If it isn't announced, the user believes it was sent.
    if !state.queued.is_empty() {
        left.push(Span::styled(" ∙ ", Style::default().fg(theme::border_light())));
        left.push(Span::styled(
            state.lang.queued(state.queued.len()),
            Style::default().fg(theme::warning()),
        ));
    }
    left
}

pub fn draw(frame: &mut Frame, area: Rect, state: &State) {
    let left_line = Line::from(left_spans(state));
    let usage = usage_spans(state);
    let left_w = left_line.width();
    let usage_w = Line::from(usage.clone()).width();

    let mut spans = left_line.spans;
    // The usage goes at the right edge of the same line. A two-cell gap keeps it from touching the
    // mode·agent; when the line is too narrow the usage is dropped entirely — the status comes first.
    let width = area.width as usize;
    if !usage.is_empty() && left_w + usage_w + 2 <= width {
        spans.push(Span::styled(" ".repeat(width - left_w - usage_w), Style::default()));
        spans.extend(usage);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The three usage numbers — credits, context, tokens — in the order the user asked for them.
///
/// A segment with no value is skipped; with nothing measured at all the whole block is empty and
/// the bottom bar shows mode·agent alone.
fn usage_spans(state: &State) -> Vec<Span<'static>> {
    let u = &state.usage;
    let model = u.model.as_deref();
    let mut segs: Vec<Vec<Span<'static>>> = Vec::new();

    if let Some(credits) = &u.credits_used {
        segs.push(kv(state.lang.credits(), credits.clone()));
    }
    if let Some(used) = u.context_tokens {
        // Context shows as **percent used (used / max)** — `0% (0/0)`.
        // Without a known maximum a percentage would be invented, so only the used amount is written.
        let text = match context_limit(model) {
            Some(max) => {
                let pct = if max > 0 { used.saturating_mul(100) / max } else { 0 };
                format!("{}% ({}/{})", pct, compact(used), compact(max))
            }
            None => compact(used),
        };
        segs.push(kv(state.lang.context(), text));
    }
    if let Some(tokens) = u.total_tokens {
        segs.push(kv(state.lang.total_tokens(), compact(tokens)));
    }

    let mut out = Vec::new();
    for (i, seg) in segs.iter().enumerate() {
        if i > 0 {
            out.push(Span::styled(" ∙ ", Style::default().fg(theme::border_light())));
        }
        out.extend(seg.iter().cloned());
    }
    out
}

/// One `label value` segment — the label muted, the number readable.
fn kv(label: &'static str, value: String) -> Vec<Span<'static>> {
    vec![
        Span::styled(label, Style::default().fg(theme::text_muted())),
        Span::styled(" ", Style::default()),
        Span::styled(value, Style::default().fg(theme::text())),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(state: &State) -> String {
        left_spans(state).iter().map(|s| s.content.as_ref()).collect()
    }

    /// **Which project the next job or work lands in is said on screen.** It was decided by
    /// wandering through the list and then never shown, so a work opened afterwards went
    /// somewhere the person had no way to check.
    #[test]
    fn the_bottom_bar_says_which_project_we_are_in() {
        let mut s = State::new();
        s.agent = "Main Agent".into();
        assert!(!text(&s).contains('·') || !text(&s).contains("Zyris"), "{:?}", text(&s));
        s.project_name = Some("Zyris Project".into());
        assert!(text(&s).contains("Zyris Project"), "{:?}", text(&s));
    }

    /// **A long name is cut with a `…`.** A project name is arbitrary text, and left whole it
    /// pushes mode·agent around and crowds the usage off the right edge.
    #[test]
    fn a_long_project_name_is_cut_on_the_bottom_bar() {
        let mut s = State::new();
        s.project_name = Some("아주 길고 긴 이름을 가진 프로젝트".into());
        let shown = text(&s);
        assert!(shown.contains('…'), "{shown:?}");
        assert!(
            crate::markdown::display_width(&shown) <= 40,
            "the bar was pushed out of shape: {shown:?}"
        );
    }
}
