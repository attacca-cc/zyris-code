//! The `/github` screen. Overlaid in the centre, like the new-project form.
//!
//! **The token is never drawn.** Only its type prefix and its length (`githubform::masked`) — this
//! screen gets shared over SSH, screenshotted, and scrolled back through.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::githubform::{masked, Field, Form};
use crate::markdown::display_width;
use crate::theme;

/// Draws the screen. **Answers where the approval address landed** so the caller can register it
/// as a link — the widget cannot reach into `State`.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    form: &Form,
    lang: crate::lang::Lang,
) -> Option<crate::app::ScreenLink> {
    // Two rows, a blank, the hint, and a note when there is one — plus the two border lines. A
    // code being waited on takes three more.
    let h = 7u16
        .saturating_add(form.note.is_some() as u16)
        .saturating_add(if form.pending.is_some() { 3 } else { 0 });
    let h = h.min(area.height.saturating_sub(2)).max(6);
    let w = 66.min(area.width.saturating_sub(4)).max(28);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    frame.render_widget(Clear, box_area);
    // **Scrub the rest of wide characters straddling the border**, or the frame looks broken where
    // the conversation's text is cut in half by the left edge.
    crate::widgets::picker::scrub_left_edge(frame, box_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::accent()))
        .title(Span::styled(
            format!(" {} ", lang.github_form_title()),
            Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let width = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // The person. **A button, not a field** — its value is whatever GitHub said, never typed.
    lines.push(row(
        lang.github_row_user(),
        match &form.user {
            Some(login) => login.clone(),
            None => lang.github_not_connected().to_string(),
        },
        form.user.is_some(),
        form.field == Field::User,
        width,
    ));

    // The reviewer. Shows what is connected, or what is being pasted.
    let typed = form.token.text.trim();
    let (value, filled) = match (typed.is_empty(), &form.reviewer) {
        (false, _) => (masked(typed), true),
        (true, Some(login)) => (login.clone(), true),
        (true, None) => (lang.github_paste_token().to_string(), false),
    };
    lines.push(row(
        lang.github_row_reviewer(),
        value,
        filled,
        form.field == Field::Reviewer,
        width,
    ));

    // **A code being waited on comes before the hint**, because it is the only thing on the
    // screen the person can act on and it stops being any use when it expires.
    let mut uri_row = None;
    if let Some((code, uri)) = &form.pending {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("   {code}   "),
            Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
        )));
        uri_row = Some(lines.len());
        lines.push(Line::from(Span::styled(
            uri.clone(),
            Style::default().fg(theme::tool()).add_modifier(Modifier::UNDERLINED),
        )));
    }

    lines.push(Line::from(""));
    // What the row under the cursor would do, so Enter is never a guess.
    lines.push(Line::from(Span::styled(
        match (form.busy, form.pending.is_some()) {
            (_, true) => lang.github_approve_it().to_string(),
            (true, _) => lang.github_working().to_string(),
            _ => match form.field {
                Field::User => match form.user.is_some() {
                    true => lang.github_enter_disconnect().to_string(),
                    false => lang.github_enter_browser().to_string(),
                },
                Field::Reviewer => lang.github_reviewer_help().to_string(),
            },
        },
        Style::default().fg(theme::text_muted()),
    )));
    if let Some(note) = &form.note {
        lines.push(Line::from(Span::styled(
            note.clone(),
            Style::default().fg(theme::text_heading()),
        )));
    }
    lines.push(Line::from(Span::styled(
        lang.github_form_keys(),
        Style::default().fg(theme::border_light()),
    )));

    let link = uri_row.zip(form.pending.as_ref()).and_then(|(row, (_, uri))| {
        let y = inner.y.checked_add(row as u16)?;
        // Only when it really is on screen — a short terminal cuts the box, and a link on a row
        // that was never drawn would fire on a click over whatever is there instead.
        if y >= inner.y.saturating_add(inner.height) {
            return None;
        }
        let width = display_width(uri).min(inner.width as usize) as u16;
        Some(crate::app::ScreenLink {
            row: y,
            start: inner.x,
            end: inner.x.saturating_add(width),
            url: uri.clone(),
        })
    });

    frame.render_widget(Paragraph::new(lines), inner);
    link
}

/// One row: a fixed-width label, then the value.
///
/// **The label column is fixed** so the two values line up. A ragged left edge on a two-row form
/// reads as a drawing mistake.
fn row(label: &str, value: String, filled: bool, focused: bool, width: usize) -> Line<'static> {
    const LABEL: usize = 12;
    let marker = if focused { "❯ " } else { "  " };
    let pad = LABEL.saturating_sub(display_width(label));
    let colour = match (focused, filled) {
        (true, _) => theme::text_heading(),
        (false, true) => theme::text(),
        // A row with nothing in it is a placeholder, and must not read as a value.
        (false, false) => theme::text_muted(),
    };
    let room = width.saturating_sub(marker.len() + LABEL + 1);
    let value = crate::markdown::truncate_to(&value, room);
    Line::from(vec![
        Span::styled(
            marker.to_string(),
            Style::default().fg(if focused { theme::accent() } else { theme::border_light() }),
        ),
        Span::styled(
            format!("{label}{}", " ".repeat(pad)),
            Style::default().fg(theme::text_muted()),
        ),
        Span::styled(
            format!(" {value}"),
            Style::default().fg(colour).add_modifier(if focused {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
    ])
}
