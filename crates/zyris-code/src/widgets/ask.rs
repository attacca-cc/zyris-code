//! The screen that answers questions. **It takes over the input field's spot.**
//!
//! Placed as a card inside the conversation flow it gets scrolled away, but while a turn is
//! blocked waiting for an answer, the task at hand must not leave the screen. So it is pinned at the always-visible bottom.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::markdown::display_width;
use crate::question::{Answering, RowKind};
use crate::theme;

/// Height needed in this state. Used when the layout settles positions.
pub fn height(a: &Answering, max: u16) -> u16 {
    // The review screen also shows the answers, so it takes more lines.
    let extra = if a.in_review() { a.steps.len() as u16 + 2 } else { 1 };
    let want = 2 + extra + a.rows().len() as u16;
    want.min(max).max(3)
}

/// Converts a screen y-coordinate to which row of the list it is. `None` if outside the list.
///
/// Line layout must match `draw` — if it drifts, clicks pick the wrong row.
pub fn row_at(a: &Answering, area: Rect, y: u16) -> Option<usize> {
    let head = if a.in_review() { 2 + a.steps.len() as u16 + 1 } else { 2 };
    let first = area.y + head;
    if y < first {
        return None;
    }
    let i = (y - first) as usize;
    (i < a.rows().len()).then_some(i)
}

pub fn draw(frame: &mut Frame, area: Rect, a: &Answering, lang: crate::lang::Lang) {
    let mut lines = vec![Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(theme::ACCENT),
    ))];

    if a.in_review() {
        draw_review(frame, area, a, lines, lang);
        return;
    }

    // Question header. When there are multiple steps, append which one it is.
    let step = a.current();
    let mut head = vec![Span::styled("? ", Style::default().fg(theme::ACCENT))];
    if let Some(h) = &step.header {
        head.push(Span::styled(format!("[{h}] "), Style::default().fg(theme::TEXT_MUTED)));
    }
    head.push(Span::styled(
        step.question.clone(),
        Style::default().fg(theme::TEXT_HEADING).add_modifier(Modifier::BOLD),
    ));
    if a.steps.len() > 1 {
        head.push(Span::styled(
            format!("  ·  {}/{}", a.step + 1, a.steps.len()),
            Style::default().fg(theme::TEXT_MUTED),
        ));
    }
    lines.push(Line::from(head));

    for (i, row) in a.rows().into_iter().enumerate() {
        let on = i == a.cursor && !a.typing;
        let caret = Span::styled(if on { "❯ " } else { "  " }, Style::default().fg(theme::ACCENT));
        match row {
            RowKind::Option(j) => {
                let opt = &step.options[j];
                let chosen = a.is_chosen(j);
                // Square when multiple can be picked, circle when only one — told apart by shape.
                let mark = match (step.multi, chosen) {
                    (true, true) => "[x] ",
                    (true, false) => "[ ] ",
                    (false, true) => "(●) ",
                    (false, false) => "( ) ",
                };
                let mut spans = vec![
                    caret,
                    Span::styled(
                        mark,
                        Style::default().fg(if chosen {
                            theme::SUCCESS
                        } else {
                            theme::BORDER_LIGHT
                        }),
                    ),
                    Span::styled(
                        opt.label.clone(),
                        Style::default().fg(if on { theme::TEXT_HEADING } else { theme::TEXT }),
                    ),
                ];
                if let Some(d) = &opt.description {
                    spans.push(Span::styled(
                        format!("  — {d}"),
                        Style::default().fg(theme::TEXT_MUTED),
                    ));
                }
                lines.push(Line::from(truncate_spans(spans, area.width as usize)));
            }
            RowKind::Free => {
                let mut spans = vec![caret];
                if a.typing {
                    spans.push(Span::styled("✎ ", Style::default().fg(theme::ACCENT)));
                    if a.input.text.is_empty() {
                        // When the field is empty, say what this spot is for.
                        spans.push(Span::styled(
                            lang.type_here(),
                            Style::default().fg(theme::BORDER_LIGHT),
                        ));
                    } else {
                        spans.push(Span::styled(
                            a.input.text.clone(),
                            Style::default().fg(theme::TEXT),
                        ));
                        spans.push(Span::styled("▎", Style::default().fg(theme::ACCENT)));
                    }
                } else if a.free_text().is_empty() {
                    spans.push(Span::styled(
                        lang.type_your_own(),
                        Style::default().fg(if on {
                            theme::TEXT_HEADING
                        } else {
                            theme::TEXT_MUTED
                        }),
                    ));
                } else {
                    // **What was typed must stay visible.** If it isn't, there's no way to check
                    // what was written without opening it again.
                    spans.push(Span::styled("✎ ", Style::default().fg(theme::SUCCESS)));
                    spans.push(Span::styled(
                        a.free_text().to_string(),
                        Style::default().fg(theme::TEXT),
                    ));
                }
                lines.push(Line::from(truncate_spans(spans, area.width as usize)));
            }
            RowKind::Action(act) => {
                let colour = if on { theme::ACCENT } else { theme::TEXT_MUTED };
                lines.push(Line::from(vec![
                    caret,
                    Span::styled(
                        act.label(lang).to_string(),
                        Style::default().fg(colour).add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
        }
    }

    let hint = if a.typing { lang.typing_keys() } else { lang.choosing_keys() };
    lines.push(Line::from(Span::styled(hint.to_string(), Style::default().fg(theme::TEXT_MUTED))));

    frame.render_widget(Paragraph::new(lines), area);
}

/// Trims from the last span so the line doesn't exceed the width.
fn truncate_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut used = 0usize;
    let mut out = Vec::new();
    for span in spans {
        let w = display_width(&span.content);
        if used + w <= width {
            used += w;
            out.push(span);
            continue;
        }
        let room = width.saturating_sub(used);
        if room > 1 {
            let mut cut = String::new();
            for ch in span.content.chars() {
                if display_width(&cut) + display_width(&ch.to_string()) > room - 1 {
                    break;
                }
                cut.push(ch);
            }
            cut.push('…');
            out.push(Span::styled(cut, span.style));
        }
        break;
    }
    out
}

/// Review screen after all questions are asked. Shows what was answered and lets the user send or fix it.
fn draw_review(
    frame: &mut Frame,
    area: Rect,
    a: &Answering,
    mut lines: Vec<Line<'static>>,
    lang: crate::lang::Lang,
) {
    lines.push(Line::from(Span::styled(
        lang.answered().to_string(),
        Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
    )));
    for (q, ans) in a.summary(lang) {
        let skipped = ans == lang.skipped();
        lines.push(Line::from(truncate_spans(
            vec![
                Span::styled("  ", Style::default().fg(theme::TEXT_MUTED)),
                Span::styled(format!("{q}  "), Style::default().fg(theme::TEXT_MUTED)),
                Span::styled(
                    ans,
                    Style::default().fg(if skipped { theme::BORDER_LIGHT } else { theme::TEXT }),
                ),
            ],
            area.width as usize,
        )));
    }
    lines.push(Line::from(""));

    for (i, row) in a.rows().into_iter().enumerate() {
        let RowKind::Action(act) = row else { continue };
        let on = i == a.cursor;
        let colour = match act {
            crate::question::Act::Reject => theme::DANGER,
            _ if on => theme::ACCENT,
            _ => theme::TEXT_MUTED,
        };
        lines.push(Line::from(vec![
            Span::styled(if on { "❯ " } else { "  " }, Style::default().fg(theme::ACCENT)),
            Span::styled(
                act.label(lang).to_string(),
                Style::default().fg(colour).add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    lines.push(Line::from(Span::styled(
        lang.review_keys().to_string(),
        Style::default().fg(theme::TEXT_MUTED),
    )));
    frame.render_widget(Paragraph::new(lines), area);
}
