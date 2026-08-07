//! The popup panel — the centered box `/mode`·`/mcp`·`/skills`·`/plugin`·`/account`·`/status`
//! open instead of dumping a wall of text into the conversation.
//!
//! **Drawing only.** The lines come pre-styled from `crate::panel`; keys only scroll
//! (`Action::PanelScroll`) or close (`Action::PanelClose`).

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::markdown::display_width;
use crate::panel::Panel;
use crate::theme;

pub fn draw(frame: &mut Frame, area: Rect, panel: &mut Panel, lang: crate::lang::Lang) {
    // The box grows with the content, never taller than four fifths of the screen.
    // A button adds its own row between the body and the hint.
    let has_button = panel.button.is_some();
    let want_h = (panel.lines.len() as u16).saturating_add(3 + u16::from(has_button)).max(5);
    let h = want_h.min(area.height.saturating_mul(4) / 5).max(3);
    let w = 72.min(area.width.saturating_sub(4)).max(20);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    // Without clearing behind, the conversation shows through.
    frame.render_widget(Clear, box_area);
    // Also scrub wide characters straddling the border — same as the picker.
    crate::widgets::picker::scrub_left_edge(frame, box_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::accent()))
        .title(Span::styled(
            format!(" {} ", panel.title),
            Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let width = inner.width as usize;
    // The last row is the hint line; the button (when present) sits above it;
    // everything above that is body.
    let fixed = 1 + u16::from(has_button);
    let body_rows = inner.height.saturating_sub(fixed) as usize;

    // Clamp the scroll to what actually fits, then draw that window.
    let max = panel.max_scroll(body_rows);
    if panel.scroll > max {
        panel.scroll = max;
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    for i in panel.scroll..panel.scroll.saturating_add(body_rows).min(panel.lines.len()) {
        lines.push(fit(panel.lines[i].clone(), width));
    }
    while lines.len() < body_rows {
        lines.push(Line::from(""));
    }
    if let Some(button) = panel.button {
        lines.push(button_line(button, panel.button_focused, lang, width));
    }
    // **A form answers to different keys, so it says so.** Showing "↑↓ scroll · Esc close"
    // over a box whose ↑↓ move a cursor and whose Esc throws work away would be a lie.
    // The form draws its own language too — the draft may have changed it a moment ago.
    let keys = match (panel.form.map(|f| f.lang), has_button, panel.button_focused) {
        (Some(draft), _, _) => draft.form_keys(),
        (None, true, true) => lang.panel_keys_button_focused(),
        (None, true, false) => lang.panel_keys_button(),
        (None, false, _) => lang.panel_keys(),
    };
    lines.push(Line::from(Span::styled(keys, Style::default().fg(theme::text_muted()))));

    frame.render_widget(Paragraph::new(lines), inner);
}

/// The button row — `[ 로그아웃 ]` when resting, marked `▶ … ◀` and accented
/// when focused. Centered, like a dialog's button.
fn button_line(
    button: crate::panel::PanelButton,
    focused: bool,
    lang: crate::lang::Lang,
    width: usize,
) -> Line<'static> {
    let label = match button {
        crate::panel::PanelButton::Logout => lang.acc_logout_button(),
    };
    let text = if focused { format!("▶ [ {label} ] ◀") } else { format!("[ {label} ]") };
    let pad = " ".repeat(width.saturating_sub(display_width(&text)) / 2);
    // **At rest it is ordinary text; the warning comes when it is about to be pressed.** Painting
    // it `danger()` while nothing had happened made a button that was merely sitting there look
    // like something had already gone wrong. Now the red arrives exactly when it means something:
    // this is the key that logs you out.
    let style = if focused {
        Style::default().fg(theme::danger()).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::text())
    };
    Line::from(Span::styled(format!("{pad}{text}"), style))
}

/// Cuts a line to the box width, keeping each span's style. A wide character that
/// would straddle the edge is dropped whole, and a cut line ends with `…`.
fn fit(line: Line<'static>, width: usize) -> Line<'static> {
    if width == 0 {
        return Line::from("");
    }
    let mut out = Vec::new();
    let mut used = 0usize;
    for span in line.spans {
        let text = span.content.to_string();
        let w = display_width(&text);
        if used + w <= width {
            out.push(span);
            used += w;
            continue;
        }
        let room = width.saturating_sub(used);
        if room > 0 {
            out.push(Span::styled(truncate(&text, room), span.style));
        }
        break;
    }
    Line::from(out)
}

/// Truncates to the column count. When cut, appends `…` to show it was cut.
fn truncate(s: &str, limit: usize) -> String {
    if display_width(s) <= limit {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        if display_width(&out) + display_width(&ch.to_string()) > limit.saturating_sub(1) {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line that fits is left untouched — no fake `…` on lines that fit.
    #[test]
    fn a_line_that_fits_is_untouched() {
        let line = Line::from(Span::styled("안녕", Style::default().fg(theme::text())));
        let got = fit(line, 10);
        assert_eq!(got.to_string(), "안녕");
    }

    /// A cut line ends with `…` and never exceeds the width.
    #[test]
    fn a_cut_line_ends_with_an_ellipsis_and_fits() {
        let line = Line::from("가".repeat(20));
        let got = fit(line, 8);
        let text = got.to_string();
        assert!(text.ends_with('…'), "{text}");
        assert!(display_width(&text) <= 8, "{} wide: {text}", display_width(&text));
    }

    /// A wide character that would straddle the edge is dropped whole, not halved.
    #[test]
    fn a_wide_character_is_never_halved() {
        let line = Line::from("가나다라");
        let got = fit(line, 5);
        let text = got.to_string();
        assert!(display_width(&text) <= 5, "{} wide: {text}", display_width(&text));
        assert!(text.contains('…'), "{text}");
    }
}
