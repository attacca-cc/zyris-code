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
use crate::theme;

/// Draws the window. **Answers where the link landed** so the caller can register it — the widget
/// cannot reach into `State`, and `apply` is pure and cannot know where anything was drawn.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    view: &EnrollView,
    lang: crate::lang::Lang,
) -> Option<crate::app::ScreenLink> {
    // A box in the center of the screen. The code must show large, so give it more room than the list window.
    let h = 10.min(area.height.saturating_sub(2)).max(5);
    let w = 56.min(area.width.saturating_sub(4)).max(30);
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

    let mut lines: Vec<Line<'static>> = Vec::new();
    // Which drawn line holds the URL, so its cells can be handed back as a link.
    let mut uri_row: Option<usize> = None;

    match view.phase {
        EnrollPhase::Waiting => {
            lines.push(Line::from(Span::styled(
                lang.enroll_steps(),
                Style::default().fg(theme::text()),
            )));
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
            lines.push(Line::from(Span::styled(
                lang.enroll_expires(remaining.as_secs()),
                Style::default().fg(theme::text_muted()),
            )));
        }
        EnrollPhase::Lapsed => {
            lines.push(Line::from(Span::styled(
                lang.enroll_lapsed(),
                Style::default().fg(theme::warning()),
            )));
        }
        EnrollPhase::Denied => {
            lines.push(Line::from(Span::styled(
                lang.enroll_denied(),
                Style::default().fg(theme::danger()),
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        lang.enroll_keys(),
        Style::default().fg(theme::border_light()),
    )));

    let link = uri_row.and_then(|row| {
        let y = inner.y.checked_add(row as u16)?;
        // **Only if it is actually on screen.** A short terminal cuts the box, and a link
        // registered on a row that was never drawn would be clickable over whatever is there.
        if y >= inner.y.saturating_add(inner.height) {
            return None;
        }
        let width = crate::markdown::display_width(&view.uri).min(inner.width as usize) as u16;
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
