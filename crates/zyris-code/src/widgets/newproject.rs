//! New-project form. Overlaid in the center of the screen — it sits on top of the list, so Esc
//! closes it and the list is right there underneath.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::markdown::display_width;
use crate::newproject::{Field, Form};
use crate::theme;

pub fn draw(frame: &mut Frame, area: Rect, form: &Form, lang: crate::lang::Lang) {
    // Name and description fields + hint line + (error line) + two border lines.
    let h = 5u16.saturating_add(form.error.is_some() as u16);
    let h = h.min(area.height.saturating_sub(2)).max(5);
    let w = 60.min(area.width.saturating_sub(4)).max(24);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    // Without clearing the back, the list shows through.
    frame.render_widget(Clear, box_area);
    // **Scrub the rest of wide characters straddling the border.** If the list's text leaves only
    // its first half outside the box's left edge, the border looks broken.
    crate::widgets::picker::scrub_left_edge(frame, box_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::accent()))
        .title(Span::styled(
            format!(" {} ", lang.project_form_title()),
            Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

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
/// broken. The value is truncated to the width, and a cursor is appended when the field is active.
fn field_line(
    label: &'static str,
    placeholder: &'static str,
    input: &crate::input::Input,
    on: bool,
    width: usize,
) -> Line<'static> {
    let mut spans =
        vec![Span::styled(format!("{label} "), Style::default().fg(theme::text_muted()))];
    spans.push(Span::styled("> ", Style::default().fg(theme::accent())));
    if input.text.is_empty() {
        spans.push(Span::styled(placeholder, Style::default().fg(theme::border_light())));
    } else {
        // A paste can bring newlines — since it's a single-line field, show them as spaces.
        let shown = truncate(&input.text.replace('\n', " "), width.saturating_sub(3));
        spans.push(Span::styled(shown, Style::default().fg(theme::text())));
    }
    if on {
        spans.push(Span::styled("▎", Style::default().fg(theme::accent())));
    }
    Line::from(spans)
}

/// Truncates to fit the field. When cut, appends `…` to show something was cut.
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
