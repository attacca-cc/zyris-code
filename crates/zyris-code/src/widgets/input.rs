//! The input field. Fixed just above the bottom bar and grows with its content.
//!
//! **Puts up the real terminal cursor.** Drawing a fake cursor with reversed cells is possible,
//! but then the real cursor `ratatui::init()` hid lingers somewhere on screen and **the IME pops
//! its composition window right there** — which is why composing Hangul showed the in-progress
//! glyph in the wrong place. With the real cursor in place, the composition window follows, and the terminal handles blinking natively (more accurate than our counting).
//!
//! The composing glyph itself is out of our hands. fcitx5 draws it in its own overlay and sends
//! only the finished character, so the app cannot choose that glyph's font or size.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;
use crate::theme;

/// Cells the prompt `"> "` occupies.
const PROMPT_WIDTH: u16 = 2;

/// A single divider line.
///
/// The input is **bound by the same line above and below.** Leaving the space below blank would
/// make the bottom bar read as the input's tail, which erases the mode from view. With a single
/// line drawn, "this far is the input" is obvious at a glance.
pub fn rule(frame: &mut Frame, area: Rect) {
    let line = Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(theme::border()),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

pub fn draw(frame: &mut Frame, area: Rect, state: &State) {
    // **A terminal can report no size at all**, and everything below assumes there is a row to put
    // the cursor on: `area.height - 1` then goes negative and takes the whole app with it. Measured
    // on a pseudo-terminal opened without a window size — a debug build panics outright, and a
    // release build wraps to 65535 and draws the cursor somewhere off the screen.
    if area.width == 0 || area.height == 0 {
        return;
    }
    let inner = area.width.saturating_sub(PROMPT_WIDTH);
    let (wrapped, (crow, ccol)) = state.input.wrapped(inner);

    // When space runs short, keep the **line with the cursor** visible by keeping the tail.
    // Keeping the head would grow the typed text off-screen.
    let rows = area.height.saturating_sub(1).max(1) as usize;
    let start = (crow as usize + 1).saturating_sub(rows).min(wrapped.len().saturating_sub(1));

    // **The strip belongs to the ordinary input state only.** This row is shared with `ask`,
    // which paints it `ACCENT` to say "this is a question", and with `approve`, which draws no
    // rule at all. Putting standing context into either would blunt a signal doing real work.
    let mut lines = vec![Line::from(crate::repo::spans(
        area.width,
        &state.cwd,
        state.home.as_deref(),
        state.repo.as_ref(),
        state.pull.as_ref(),
    ))];
    for (i, text) in wrapped[start..].iter().take(rows).enumerate() {
        // The prompt is only on the first line. Continuation lines start at the same column so the text stays tidy.
        let head = if start + i == 0 { "> " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(head, Style::default().fg(theme::accent())),
            Span::styled(text.clone(), Style::default().fg(theme::text())),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);

    // Cursor position, counted with full-width glyphs as 2 cells — counting characters would push it left of Hangul.
    let col = PROMPT_WIDTH + ccol;
    let usable = area.width.saturating_sub(1);
    let y = area.y + 1 + (crow as usize).saturating_sub(start) as u16;
    frame.set_cursor_position((area.x + col.min(usable), y.min(area.y + area.height - 1)));
}
