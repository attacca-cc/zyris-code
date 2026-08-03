//! 질문에 답하는 화면. **입력란 자리를 대신 차지한다.**
//!
//! 대화 흐름 안에 카드로 두면 스크롤에 밀려 사라지는데, 답을 기다리느라 턴이 막혀 있는
//! 동안 지금 할 일이 화면 밖으로 나가면 안 된다. 그래서 늘 보이는 아래쪽에 세운다.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::markdown::display_width;
use crate::question::{Answering, RowKind};
use crate::theme;

/// 이 상태에서 필요한 높이. 레이아웃이 자리를 잡을 때 쓴다.
pub fn height(a: &Answering, max: u16) -> u16 {
    // 검토 화면은 답한 내용을 함께 보여주므로 줄이 더 든다.
    let extra = if a.in_review() { a.steps.len() as u16 + 2 } else { 1 };
    let want = 2 + extra + a.rows().len() as u16;
    want.min(max).max(3)
}

/// 화면 y좌표를 목록의 몇 번째 줄인지로 옮긴다. 목록 밖이면 `None`.
///
/// 줄 구성은 `draw`와 같아야 한다 — 어긋나면 클릭이 엉뚱한 줄을 고른다.
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

    // 질문 머리. 여러 단계면 몇 번째인지 붙인다.
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
                // 여러 개 고를 수 있으면 네모, 하나만이면 동그라미 — 모양으로 구별된다.
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
                        // 빈 칸이면 무엇을 하는 자리인지 말해 준다.
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
                    // **적은 내용이 남아 보여야 한다.** 안 보이면 무엇을 썼는지 확인할
                    // 길이 없어 다시 열어 봐야 한다.
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
                        act.label().to_string(),
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

/// 줄이 폭을 넘지 않게 마지막 span부터 깎는다.
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

/// 다 물어본 뒤의 검토 화면. 무엇을 답했는지 보여주고 보낼지 고치게 한다.
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
    for (q, ans) in a.summary() {
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
                act.label().to_string(),
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
