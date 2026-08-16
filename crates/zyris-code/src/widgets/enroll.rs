//! Enrollment code window. Appears in the center of the screen when re-enrollment starts.
//!
//! The code used to go to stdout where the screen hid it. Now the `EnrollmentUi` hook ships the
//! code via `Frame::Enroll` (`enroll::ScreenEnroll`), and this window takes that spot — expiry and
//! denial are also drawn here via `EnrollPhase`.
//!
//! **It only closes with Esc.** Enrollment keeps running in the background, so even if closed, when
//! approval arrives `EnrollDone` closes the window.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{EnrollPhase, EnrollView};
use crate::markdown::display_width;
use crate::theme;

/// Splits prose to fit the width, breaking between words where it can.
///
/// **Nothing in this box may be cut.** `Paragraph` drops whatever runs past the edge without a
/// mark, so both of the sentences here — the one saying what to do with the code, and the one
/// saying whose account to approve with — ended mid-word against the right border. Every line of
/// this window is the only copy of what it says; there is no scrolling back for the rest.
///
/// A word longer than the width is cut by column instead, so an unbroken run cannot loop.
fn wrap_words(text: &str, width: u16) -> Vec<String> {
    let limit = (width as usize).max(8);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut used = 0usize;
    for word in text.split_whitespace() {
        let w = display_width(word);
        let gap = usize::from(!cur.is_empty());
        if used + gap + w > limit && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            used = 0;
        }
        if w > limit {
            // Longer than a line on its own — fill by column, since there is no break to find.
            for ch in word.chars() {
                let cw = display_width(&ch.to_string()).max(1);
                if used + cw > limit {
                    out.push(std::mem::take(&mut cur));
                    used = 0;
                }
                cur.push(ch);
                used += cw;
            }
            continue;
        }
        if !cur.is_empty() {
            cur.push(' ');
            used += 1;
        }
        cur.push_str(word);
        used += w;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Appends `text` as however many lines it takes at this width.
fn wrapped(lines: &mut Vec<Line<'static>>, text: &str, width: u16, colour: ratatui::style::Color) {
    for row in wrap_words(text, width) {
        lines.push(Line::from(Span::styled(row, Style::default().fg(colour))));
    }
}

/// Draws the window. **Answers where the link landed** so the caller can register it — the widget
/// cannot reach into `State`, and `apply` is pure and cannot know where anything was drawn.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    view: &EnrollView,
    lang: crate::lang::Lang,
) -> Option<crate::app::ScreenLink> {
    // A box in the center of the screen. The code must show large, so give it more room than the list window.
    let w = 64.min(area.width.saturating_sub(4)).max(30);
    // **The width is settled before a single line is built**, because wrapping needs it and the
    // height falls out of how many lines the wrapping produced.
    let text_width = w.saturating_sub(2);

    let mut lines: Vec<Line<'static>> = Vec::new();
    // Which drawn line holds the URL, so its cells can be handed back as a link.
    let mut uri_row: Option<usize> = None;

    match view.phase {
        EnrollPhase::Waiting => {
            wrapped(&mut lines, lang.enroll_steps(), text_width, theme::text());
            lines.push(Line::from(""));
            // The code large and clear. Keep the hyphens so double-click selects it whole — same
            // rule as the upstream box.
            lines.push(Line::from(Span::styled(
                format!("   {}   ", view.code),
                Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
            )));
            // **Where the code goes, said and underlined.** A bare URL on its own line reads as
            // decoration; this says it is the thing to open. Ctrl+click opens it, and the link is
            // registered below so that actually works over an overlay.
            uri_row = Some(lines.len());
            lines.push(Line::from(Span::styled(
                view.uri.clone(),
                Style::default().fg(theme::tool()).add_modifier(Modifier::UNDERLINED),
            )));
            lines.push(Line::from(""));
            let remaining = view.expires_at.saturating_duration_since(std::time::Instant::now());
            wrapped(
                &mut lines,
                &lang.enroll_expires(remaining.as_secs()),
                text_width,
                theme::text_muted(),
            );
            // **Whose account this is, said at the moment it is decided.** Approving hands this
            // computer over, and this window is the only place that fact can still change anything.
            lines.push(Line::from(""));
            wrapped(&mut lines, lang.enroll_warning(), text_width, theme::warning());
        }
        EnrollPhase::Lapsed => {
            wrapped(&mut lines, lang.enroll_lapsed(), text_width, theme::warning());
        }
        EnrollPhase::Denied => {
            wrapped(&mut lines, lang.enroll_denied(), text_width, theme::danger());
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        lang.enroll_keys(),
        Style::default().fg(theme::border_light()),
    )));

    // **The box is as tall as what it has to hold.** A fixed height cut the last lines off without
    // saying so; a short terminal still cuts, but only because there is no room left.
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2)).max(5);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    // Without clearing the back, the conversation shows through.
    frame.render_widget(Clear, box_area);
    // Scrub the rest of wide characters straddling the border — same reason as the picker.
    crate::widgets::picker::scrub_left_edge(frame, box_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::accent()))
        .title(Span::styled(
            format!(" {} ", lang.enroll_title()),
            Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let link = uri_row.and_then(|row| {
        let y = inner.y.checked_add(row as u16)?;
        // **Only if it is actually on screen.** A short terminal cuts the box, and a link
        // registered on a row that was never drawn would be clickable over whatever is there.
        if y >= inner.y.saturating_add(inner.height) {
            return None;
        }
        let width = display_width(&view.uri).min(inner.width as usize) as u16;
        Some(crate::app::ScreenLink {
            row: y,
            start: inner.x,
            end: inner.x.saturating_add(width),
            url: view.uri.clone(),
        })
    });

    frame.render_widget(Paragraph::new(lines), inner);
    link
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Lang;

    /// **Nothing this window says may be cut.** Both sentences are longer than the box, and
    /// `Paragraph` drops the overflow without a mark — the first line read as ending mid-word,
    /// which is how it was reported (2026-08-14).
    #[test]
    fn every_sentence_fits_inside_the_box() {
        // The width the box actually hands its text, at its widest.
        let width = 62u16;
        for lang in [Lang::Ko, Lang::En] {
            for text in [lang.enroll_steps(), lang.enroll_warning(), lang.enroll_denied()] {
                let rows = wrap_words(text, width);
                assert!(!rows.is_empty(), "{text}");
                for row in &rows {
                    assert!(
                        display_width(row) <= width as usize,
                        "{row:?} is {} wide, past {width}",
                        display_width(row)
                    );
                }
                // Wrapping must lose nothing but the spaces it broke on.
                assert_eq!(
                    rows.join(" ").split_whitespace().collect::<Vec<_>>(),
                    text.split_whitespace().collect::<Vec<_>>(),
                    "wrapping dropped or invented words"
                );
            }
        }
    }

    /// A run with no space in it cannot make the line grow, nor loop looking for a break.
    #[test]
    fn an_unbroken_run_is_cut_by_column_instead() {
        let rows = wrap_words(&"x".repeat(40), 10);
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|r| display_width(r) <= 10));
    }
}
