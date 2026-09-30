//! New-project form. Overlaid in the center of the screen — it sits on top of the list, so Esc
//! closes it and the list is right there underneath.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::markdown::display_width;
use crate::newproject::{Field, Form};
use crate::theme;

pub fn draw(frame: &mut Frame, area: Rect, form: &Form, lang: crate::lang::Lang) {
    // Name and description fields + hint line + (error line) + two border lines.
    let h = 5u16.saturating_add(form.error.is_some() as u16);
    let h = h.min(area.height.saturating_sub(2)).max(5);
    let w = 60.min(area.width.saturating_sub(4)).max(24.min(area.width));
    let inner = super::overlay(frame, area, w, h, lang.project_form_title());

    let width = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(field_line(
        lang.project_name(),
        lang.project_name_placeholder(),
        &form.name,
        form.field == Field::Name,
        width,
    ));
    lines.push(field_line(
        lang.project_description(),
        lang.project_description_placeholder(),
        &form.description,
        form.field == Field::Description,
        width,
    ));
    if let Some(err) = &form.error {
        lines.push(Line::from(Span::styled(err.clone(), Style::default().fg(theme::danger()))));
    }
    lines.push(Line::from(Span::styled(
        lang.project_form_keys().to_string(),
        Style::default().fg(theme::text_muted()),
    )));

    frame.render_widget(Paragraph::new(lines), inner);
}

/// One line of the form: field name + value. **The active field gets the cursor.**
///
/// When the value is empty, dimly say what belongs there — without a reason, an empty field looks
/// broken.
///
/// **The value is a window around the caret**, fitted to what the label and `> ` leave. It used to
/// keep the head of a long value and was budgeted as if the label took no room, so the end being
/// typed — and the `…` and the caret — fell off the box; and the caret was drawn after the text
/// wherever it really was. The caret now marks the character it stands on.
fn field_line(
    label: &'static str,
    placeholder: &'static str,
    input: &crate::input::Input,
    on: bool,
    width: usize,
) -> Line<'static> {
    let label = format!("{label} ");
    let room = width.saturating_sub(display_width(&label) + 2).max(1);
    let mut spans = vec![
        Span::styled(label, Style::default().fg(theme::text_muted())),
        Span::styled("> ", Style::default().fg(theme::accent())),
    ];
    let caret = Style::default().fg(theme::accent());
    if input.text.is_empty() {
        let shown = crate::markdown::truncate_to(placeholder, room.saturating_sub(on as usize));
        spans.push(Span::styled(shown, Style::default().fg(theme::subtle())));
        if on {
            spans.push(Span::styled("▮", caret));
        }
        return Line::from(spans);
    }
    // A paste can bring newlines or tabs — it's a single-line field, so they show as spaces.
    let text: String = input.text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let at = text.char_indices().nth(input.cursor).map_or(text.len(), |(i, _)| i);
    let (before, after) = if on { text.split_at(at) } else { ("", text.as_str()) };
    let (lead, left, under, right, trail) = window(before, after, room, on);
    let body = Style::default().fg(theme::text());
    let mark = Style::default().fg(theme::text_muted());
    if lead {
        spans.push(Span::styled("…", mark));
    }
    spans.push(Span::styled(left, body));
    if on {
        match under {
            Some(g) => spans.push(Span::styled(g, body.add_modifier(Modifier::REVERSED))),
            None => spans.push(Span::styled("▮", caret)),
        }
    }
    spans.push(Span::styled(right, body));
    if trail {
        spans.push(Span::styled("…", mark));
    }
    Line::from(spans)
}

/// Fits `before` + caret + `after` into `room` columns, keeping the caret: `(cut at the front,
/// the part before the caret, the character under it, the part after, cut at the back)`. With
/// `on` false there is no caret and the head of `after` is kept.
fn window(
    before: &str,
    after: &str,
    room: usize,
    on: bool,
) -> (bool, String, Option<String>, String, bool) {
    use unicode_segmentation::UnicodeSegmentation;
    let mut rest = after.graphemes(true);
    let under = if on { rest.next().map(str::to_string) } else { None };
    let caret_w = if on { under.as_deref().map_or(1, display_width).max(1) } else { 0 };
    let rest: Vec<&str> = rest.collect();
    // What of `before` fits, taken from its end; one column goes to `…` when not all of it does.
    let fit_tail = |avail: usize| {
        let mut w = 0;
        let mut out: Vec<&str> = Vec::new();
        for g in before.graphemes(true).rev() {
            if w + display_width(g) > avail {
                break;
            }
            w += display_width(g);
            out.push(g);
        }
        out.reverse();
        (out.concat(), w)
    };
    let avail = room.saturating_sub(caret_w);
    let (mut left, mut left_w) = fit_tail(avail);
    let lead = left.len() < before.len();
    if lead {
        (left, left_w) = fit_tail(avail.saturating_sub(1));
    }
    let mut right_room = avail.saturating_sub(left_w + lead as usize);
    let total: usize = rest.iter().map(|g| display_width(g)).sum();
    let trail = total > right_room;
    if trail {
        right_room = right_room.saturating_sub(1);
    }
    let mut right = String::new();
    let mut w = 0;
    for g in rest {
        if w + display_width(g) > right_room {
            break;
        }
        w += display_width(g);
        right.push_str(g);
    }
    (lead, left, under, right, trail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// **What is being typed stays on screen**, inside the box, with the caret on the character
    /// it stands on — the head of a long value used to be kept and the tail, `…` and caret fell off.
    #[test]
    fn a_long_value_keeps_the_caret_and_the_end_in_view() {
        let mut input = crate::input::Input::new();
        input.insert_str(&format!("{}END", "a".repeat(70)));
        let line = field_line("Name", "", &input, true, 30);
        let text = shown(&line);
        assert_eq!(display_width(&text), 30, "{text:?}");
        assert!(text.ends_with("END▮"), "{text:?}");
        assert!(text.contains('…'), "{text:?}");

        input.cursor = 1;
        let line = field_line("Name", "", &input, true, 30);
        let text = shown(&line);
        assert!(display_width(&text) <= 30 && text.ends_with('…'), "{text:?}");
        let under = line.spans.iter().find(|s| s.style.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(under.map(|s| s.content.as_ref()), Some("a"), "{line:?}");
    }
}
