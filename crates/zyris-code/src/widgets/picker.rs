//! The project/session list. Overlaid in the center of the screen.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::markdown::display_width;
use crate::picker::{Picker, Slot};
use crate::theme;

pub fn draw(frame: &mut Frame, area: Rect, picker: &Picker, lang: crate::lang::Lang) {
    // The centered box. Sized to the list, but never taller than the screen.
    // The separator line takes one more row, so it must be counted for everything to fit.
    let rule = picker.is_create(0) && picker.rows.len() > 1;
    let want_h = (picker.rows.len() as u16).saturating_add(4 + rule as u16).max(6);
    let h = want_h.min(area.height.saturating_sub(2)).max(3);
    let w = 64.min(area.width.saturating_sub(4)).max(20);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    // Without clearing behind, the conversation shows through.
    frame.render_widget(Clear, box_area);
    // **Also scrub wide characters straddling the border.** If the leading half of a wide
    // character remains just outside the box's left edge, it bleeds into the box and breaks the
    // border. `Clear` only clears inside the box, so we must remove this half ourselves.
    scrub_left_edge(frame, box_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(Span::styled(
            format!(" {} ", picker.title(lang)),
            Style::default().fg(theme::TEXT_HEADING).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if picker.loading {
        lines
            .push(Line::from(Span::styled(lang.loading(), Style::default().fg(theme::TEXT_MUTED))));
    }

    // **The pure side decides** where each row goes (`picker::slots`). Here we just draw.
    let width = inner.width as usize;
    let body_h = inner.height.saturating_sub(1) as usize;
    for slot in crate::picker::slots(&picker.rows, picker.cursor, body_h) {
        lines.push(match slot {
            Slot::Row(i) => {
                row_line(&picker.rows[i], i == picker.cursor, picker.is_create(i), width)
            }
            Slot::Rule => {
                Line::from(Span::styled("─".repeat(width), Style::default().fg(theme::BORDER)))
            }
            Slot::More { count, up } => Line::from(Span::styled(
                lang.pick_more(up, count),
                Style::default().fg(theme::TEXT_MUTED),
            )),
        });
    }

    // The meaning of ← changes with the level. Say it plainly.
    let back = match picker.level {
        crate::picker::Level::Projects => lang.picker_close(),
        crate::picker::Level::Sessions { .. } => lang.picker_back(),
        // These two are outside the project hierarchy, so there's nowhere to go back to.
        crate::picker::Level::Agents
        | crate::picker::Level::Commands
        | crate::picker::Level::Languages => lang.picker_esc_close(),
    };
    lines.push(Line::from(Span::styled(
        lang.picker_keys(back),
        Style::default().fg(theme::TEXT_MUTED),
    )));

    frame.render_widget(Paragraph::new(lines), inner);
}

/// One row of the list.
///
/// **"Create new" is a different color.** Mixed into the session list it would read as one
/// session, but it's something you create, not pick.
fn row_line(row: &crate::picker::Row, on: bool, create: bool, width: usize) -> Line<'static> {
    let fg = match (row.enabled, create, on) {
        (false, _, _) => theme::BORDER_LIGHT,
        (true, true, _) => theme::ACCENT,
        (true, false, true) => theme::TEXT_HEADING,
        (true, false, false) => theme::TEXT,
    };
    let (label, note) = split(width, &row.label, row.note.as_deref());
    let mut spans = vec![
        Span::styled(if on { "❯ " } else { "  " }, Style::default().fg(theme::ACCENT)),
        Span::styled(label.clone(), Style::default().fg(fg)),
    ];
    if let Some(note) = note {
        let used = 2 + display_width(&label);
        let pad = width.saturating_sub(used + display_width(&note));
        spans.push(Span::styled(" ".repeat(pad), Style::default().fg(theme::TEXT_MUTED)));
        spans.push(Span::styled(note, Style::default().fg(theme::TEXT_MUTED)));
    }
    Line::from(spans)
}

/// However short the note is, it's worth showing at least this much. Narrower than this, drop it entirely.
const NOTE_MIN: usize = 8;

/// Splits one line into (name, note). **The name comes first.**
///
/// Give the note the room first and the name gets cut — `/agent` actually got truncated to
/// `/a…`, and you couldn't tell what command it was in the list. **The name is identity and the
/// note is garnish**: when the name is cut, the reason to pick that line disappears, but without the note you can still guess from the name.
///
/// Still, the name must always be truncated — session titles have arbitrary lengths, and left
/// alone they'd punch through the box and collapse the screen.
fn split(width: usize, label: &str, note: Option<&str>) -> (String, Option<String>) {
    let label = truncate(label, width.saturating_sub(2));
    let Some(note) = note else {
        return (label, None);
    };
    // Leave at least two columns between the name and the note. Stuck together, they read as one word.
    let room = width.saturating_sub(2 + display_width(&label) + 2);
    if room < NOTE_MIN {
        return (label, None);
    }
    (label, Some(truncate(note, room)))
}

/// Truncates to fit the column count. When cut, appends `…` to show it was cut.
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

/// Replaces the leading half of a wide character straddling the box's left edge with a space.
///
/// The enrollment-code window (`enroll.rs`) does the same — every overlaid window uses this path.
pub(crate) fn scrub_left_edge(frame: &mut Frame, box_area: Rect) {
    if box_area.x == 0 {
        return;
    }
    let x = box_area.x - 1;
    let buf = frame.buffer_mut();
    for y in box_area.y..box_area.y.saturating_add(box_area.height) {
        if !buf.area.contains((x, y).into()) {
            continue;
        }
        if display_width(buf[(x, y)].symbol()) > 1 {
            buf[(x, y)].set_symbol(" ");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The name never gets shaved.** `/agent` actually got truncated to `/a…` and you couldn't
    /// tell what command it was — the reason was a long note.
    #[test]
    fn a_long_note_never_eats_into_the_name() {
        let (label, _) =
            split(62, "/agent", Some("에이전트를 고릅니다. 다음 메시지에서 새 thread가 열립니다"));
        assert_eq!(label, "/agent");
    }

    /// If there's no room for the note, drop the note. A half-cut note is unreadable.
    #[test]
    fn a_note_is_dropped_rather_than_squeezed_to_nothing() {
        // A width where the name fits exactly and no room is left for the note.
        let (label, note) = split(14, "가나다라마", Some("설명"));
        assert_eq!(label, "가나다라마", "이름이 깎였다");
        assert!(note.is_none(), "{note:?}");
    }

    /// Still, the name is cut — a session title punching through the box would collapse the screen.
    #[test]
    fn a_very_long_name_is_still_cut_to_fit() {
        let (label, _) = split(20, &"가".repeat(40), None);
        assert!(display_width(&label) <= 18, "{}칸: {label}", display_width(&label));
        assert!(label.ends_with('…'), "잘렸다는 표시가 없다: {label}");
    }

    /// When both fit, both show.
    #[test]
    fn both_fit_when_there_is_room() {
        let (label, note) = split(40, "/cwd", Some("도구가 도는 자리"));
        assert_eq!(label, "/cwd");
        assert_eq!(note.as_deref(), Some("도구가 도는 자리"));
    }
}
