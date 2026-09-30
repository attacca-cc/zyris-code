//! The `/github` screen. Overlaid in the centre, like the new-project form.
//!
//! **The token is never drawn.** Only its type prefix and its length (`githubform::masked`) — this
//! screen gets shared over SSH, screenshotted, and scrolled back through.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
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
) -> Vec<crate::app::ScreenLink> {
    // Three rows, a blank, the hint, and a note when there is one — plus the two border lines. A
    // code being waited on takes three more.
    let w = 66.min(area.width.saturating_sub(4)).max(28.min(area.width));
    // The address wraps inside the box, so it takes as many rows as that needs.
    let uri_extra = form.pending.as_ref().map_or(0, |(_, uri)| {
        crate::wrap::columns(uri, w.saturating_sub(2) as usize).len().saturating_sub(1)
    }) as u16;
    let h = 8u16
        .saturating_add(form.note.is_some() as u16)
        .saturating_add(if form.pending.is_some() { 3 + uri_extra } else { 0 });
    let h = h.min(area.height.saturating_sub(2)).max(6);
    let inner = super::overlay(frame, area, w, h, lang.github_form_title());

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

    // Signing. **A button too** — what it says is the address commits go out signed as, which is
    // GitHub's noreply for the account and never something anybody types.
    lines.push(row(
        lang.github_row_signing(),
        match &form.signing {
            Some(email) => email.clone(),
            None => lang.github_signing_off().to_string(),
        },
        form.signing.is_some(),
        form.field == Field::Signing,
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
        uri_row =
            Some((lines.len(), crate::widgets::enroll::uri_lines(&mut lines, uri, width as u16)));
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
                Field::Signing => match (form.signing.is_some(), form.user.is_some()) {
                    (true, _) => lang.github_enter_stop_signing().to_string(),
                    (false, true) => lang.github_enter_sign().to_string(),
                    (false, false) => lang.github_sign_needs_an_account().to_string(),
                },
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
        Style::default().fg(theme::subtle()),
    )));

    // Only the rows really on screen — a short terminal cuts the box, and a link on a row that was
    // never drawn would fire on a click over whatever is there instead.
    let links = match (uri_row, form.pending.as_ref()) {
        (Some((row, widths)), Some((_, uri))) => {
            crate::widgets::enroll::link_rows(inner, row, &widths, uri)
        }
        _ => Vec::new(),
    };

    frame.render_widget(Paragraph::new(lines), inner);
    links
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
