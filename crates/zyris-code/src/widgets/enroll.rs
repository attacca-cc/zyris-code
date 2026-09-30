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

/// Appends `text` as however many lines it takes at this width, broken between words.
///
/// **Nothing in this box may be cut.** `Paragraph` drops whatever runs past the edge without a
/// mark, so both of the sentences here — the one saying what to do with the code, and the one
/// saying whose account to approve with — ended mid-word against the right border. Every line of
/// this window is the only copy of what it says; there is no scrolling back for the rest.
fn wrapped(lines: &mut Vec<Line<'static>>, text: &str, width: u16, colour: ratatui::style::Color) {
    for row in crate::wrap::words(text, width as usize) {
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
    attached: bool,
) -> Vec<crate::app::ScreenLink> {
    // A box in the center of the screen. The code must show large, so give it more room than the list window.
    let w = 64.min(area.width.saturating_sub(4)).max(30.min(area.width));
    // **The width is settled before a single line is built**, because wrapping needs it and the
    // height falls out of how many lines the wrapping produced.
    let text_width = w.saturating_sub(2);

    let mut lines: Vec<Line<'static>> = Vec::new();
    // Which drawn lines hold the URL, so their cells can be handed back as a link.
    let mut uri_row: Option<usize> = None;
    let mut uri_rows: Vec<usize> = Vec::new();

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
            uri_rows = uri_lines(&mut lines, &view.uri, text_width);
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
        lang.enroll_keys(attached),
        Style::default().fg(theme::subtle()),
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

    let links = uri_row.map_or_else(Vec::new, |row| link_rows(inner, row, &uri_rows, &view.uri));
    frame.render_widget(Paragraph::new(lines), inner);
    links
}

/// Appends the URL as however many rows it takes, **by column** — an address has no word to
/// break at, and cut at the border it was the one line here that could not be read in full.
/// Gives back each row's width.
pub(crate) fn uri_lines(lines: &mut Vec<Line<'static>>, uri: &str, width: u16) -> Vec<usize> {
    let style = Style::default().fg(theme::tool()).add_modifier(Modifier::UNDERLINED);
    crate::wrap::columns(uri, width as usize)
        .into_iter()
        .map(|row| {
            let w = display_width(&row);
            lines.push(Line::from(Span::styled(row, style)));
            w
        })
        .collect()
}

/// One link per drawn row of the URL starting at line `first` of `inner`. **Only rows actually on
/// screen**: a short terminal cuts the box, and a link on a row that was never drawn would be
/// clickable over whatever is there.
pub(crate) fn link_rows(
    inner: Rect,
    first: usize,
    widths: &[usize],
    url: &str,
) -> Vec<crate::app::ScreenLink> {
    widths
        .iter()
        .enumerate()
        .filter_map(|(i, w)| {
            let y = inner.y.checked_add((first + i) as u16)?;
            (y < inner.y.saturating_add(inner.height)).then(|| crate::app::ScreenLink {
                row: y,
                start: inner.x,
                end: inner.x.saturating_add((*w).min(inner.width as usize) as u16),
                url: url.to_string(),
            })
        })
        .collect()
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
                let rows = crate::wrap::words(text, width as usize);
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

    /// **The address wraps inside the box and every row of it opens it.** It was one line, cut
    /// at the border of a narrow box — the one line here that must be read in full.
    #[test]
    fn a_long_address_wraps_and_every_row_is_a_link() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let uri = format!("https://example.com/device/{}", "x".repeat(60));
        let view = EnrollView {
            code: "ABCD-1234".into(),
            uri: uri.clone(),
            expires_at: std::time::Instant::now(),
            phase: EnrollPhase::Waiting,
        };
        let mut term = Terminal::new(TestBackend::new(40, 40)).expect("terminal");
        let mut links = Vec::new();
        term.draw(|f| links = draw(f, f.area(), &view, Lang::En, true)).expect("draw");
        assert!(links.len() > 1, "{links:?}");
        assert!(links.iter().all(|l| l.url == uri && l.end <= 40));
        let buf = term.backend().buffer().clone();
        let drawn: String = links
            .iter()
            .map(|l| (l.start..l.end).map(|x| buf[(x, l.row)].symbol()).collect::<String>())
            .collect();
        assert_eq!(drawn, uri);
    }
}
