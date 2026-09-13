//! The screen that answers questions. **It takes over the input field's spot.**
//!
//! Placed as a card inside the conversation flow it gets scrolled away, but while a turn is
//! blocked waiting for an answer, the task at hand must not leave the screen. So it is pinned at
//! the always-visible bottom.
//!
//! **Nothing in this card is ever cut.** A question is the whole reason the card is up, and a
//! question can be long — an agent asking about a design decision writes a paragraph, and its
//! options carry descriptions of their own. Every line wraps (`crate::wrap`) and the height is the
//! number of lines that came out of it; when even that does not fit the room the card is given, the
//! list scrolls to keep the cursor's row in view and the hint says how many lines are off screen.
//!
//! **`card()` is the only place that lays this out.** The drawing and the mouse hit-test both go
//! through it, because they used to work the row out on their own — and the moment a line wrapped,
//! a click would have picked the row above or below the one under the pointer.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::markdown::display_width;
use crate::question::{Act, Answering, RowKind};
use crate::theme;
use crate::wrap;

/// The card, laid out at one width: what to draw, and which row every drawn line belongs to.
#[derive(Debug)]
pub struct Card {
    /// Everything to draw, top to bottom — the rule, the visible body, the hint.
    pub lines: Vec<Line<'static>>,
    /// Which row of [`Answering::rows`] each **body** line carries. `None` for a line that belongs
    /// to no row: the question's own text, the blank above the review's actions.
    pub owners: Vec<Option<usize>>,
    /// The body line the visible window starts at. Zero unless the body did not fit.
    pub top: usize,
    /// Where the terminal's own cursor has to go, in cells of the area the card is drawn in.
    ///
    /// **This is not decoration — it is where the input method draws what it is composing.** A
    /// Korean syllable being assembled is drawn by the IME *at the terminal's cursor*, and with the
    /// cursor left where the input box used to be, the preedit appeared one line up on the activity
    /// line while the finished word landed correctly in the field (reported 2026-09-13).
    /// `widgets/input.rs` has always set this; the question card did not.
    pub caret: Option<(u16, u16)>,
}

/// Lays the card out at this width, in at most `room` lines — rule and hint included.
pub fn card(a: &Answering, width: u16, room: usize, lang: crate::lang::Lang) -> Card {
    let w = width as usize;
    // Below three there is no room for the rule, one line and the hint; the caller's own clamp is
    // what keeps this in step with the space it hands over.
    let room = room.max(3);
    let mut body: Vec<Line<'static>> = Vec::new();
    let mut owners: Vec<Option<usize>> = Vec::new();
    // Where the caret lands, in body coordinates, while the free-text row is being typed into.
    let mut caret: Option<(usize, usize)> = None;

    if a.in_review() {
        body.push(Line::from(Span::styled(
            lang.answered().to_string(),
            Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
        )));
        owners.push(None);
        for (q, ans) in a.summary(lang) {
            let skipped = ans == lang.skipped();
            add(
                &mut body,
                &mut owners,
                wrap::line(
                    Line::from(vec![
                        Span::styled("  ", Style::default().fg(theme::text_muted())),
                        Span::styled(format!("{q}  "), Style::default().fg(theme::text_muted())),
                        Span::styled(
                            ans,
                            Style::default().fg(if skipped {
                                theme::border_light()
                            } else {
                                theme::text()
                            }),
                        ),
                    ]),
                    w,
                ),
                None,
            );
        }
        body.push(Line::from(""));
        owners.push(None);
    } else {
        // Question header. When there are multiple steps, append which one it is.
        let step = a.current();
        let mut head = vec![Span::styled("? ", Style::default().fg(theme::accent()))];
        if let Some(h) = &step.header {
            head.push(Span::styled(format!("[{h}] "), Style::default().fg(theme::text_muted())));
        }
        head.push(Span::styled(
            step.question.clone(),
            Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
        ));
        if a.steps.len() > 1 {
            head.push(Span::styled(
                format!("  ∙  {}/{}", a.step + 1, a.steps.len()),
                Style::default().fg(theme::text_muted()),
            ));
        }
        add(&mut body, &mut owners, wrap::line(Line::from(head), w), None);
    }

    for (i, row) in a.rows().into_iter().enumerate() {
        let first = body.len();
        add(&mut body, &mut owners, row_lines(a, &row, i, lang, w), Some(i));
        // **The caret in the free-text row.** Wrapping only the text *before* the cursor gives the
        // line and column it sits on, with the same wrapping the drawn line got — so a long answer
        // that has already wrapped puts the caret on the right line of it.
        if a.typing && matches!(row, RowKind::Free) {
            let before: String = a.input.text.chars().take(a.input.cursor).collect();
            let pre = Line::from(vec![
                Span::styled("  ", Style::default().fg(theme::accent())),
                Span::styled("✎ ", Style::default().fg(theme::accent())),
                Span::styled(before, Style::default().fg(theme::text())),
            ]);
            let parts = wrap::line(pre, w);
            let y = first + parts.len().saturating_sub(1);
            let x = parts.last().map_or(0usize, |line| display_width(&line.to_string()));
            caret = Some((x, y));
        }
    }

    // **When the body does not fit, keep the cursor's row in view.** The card is pinned above the
    // bars and its room is whatever is left, so the row the keys are acting on has to be on screen:
    // a list that scrolled the cursor off would make the arrows look dead and Enter act on
    // something nobody can see.
    let body_room = room.saturating_sub(2);
    let over = body.len() > body_room;
    let top = if over {
        let at = owners.iter().position(|o| *o == Some(a.cursor)).unwrap_or(0);
        // One line of context above, so the cursor's row does not sit against the rule.
        at.saturating_sub(1).min(body.len() - body_room)
    } else {
        0
    };
    let shown = body.len().saturating_sub(top).min(body_room);
    let above = top;
    let below = body.len() - top - shown;

    let mut hint = if a.in_review() {
        // The review screen's keys are its own — the answers are already given, so "choose" and
        // "type" would both be describing a list that is no longer being answered.
        lang.review_keys().to_string()
    } else if a.typing {
        lang.typing_keys().to_string()
    } else {
        lang.choosing_keys().to_string()
    };
    // Off-screen rows are counted out loud. Silently showing four of nine rows is how a list comes
    // to look like it holds four.
    if above > 0 {
        hint.push_str(&lang.pick_more(true, above));
    }
    if below > 0 {
        hint.push_str(&lang.pick_more(false, below));
    }

    let mut lines = vec![Line::from(Span::styled(
        "─".repeat(w),
        Style::default().fg(theme::accent()),
    ))];
    lines.extend(body[top..top + shown].iter().cloned());
    lines.push(Line::from(Span::styled(hint, Style::default().fg(theme::text_muted()))));
    // The rule is drawn above the body, so a caret on body line `y` is on screen line `y + 1` —
    // and the window may have scrolled, hence the `top`.
    let caret = caret.map(|(x, y)| (x as u16, (y + 1).saturating_sub(top) as u16));
    Card { lines, owners, top, caret }
}

/// How many lines the card wants, never more than the caller can give it.
pub fn height(a: &Answering, width: u16, max: u16, lang: crate::lang::Lang) -> u16 {
    card(a, width, max as usize, lang).lines.len() as u16
}

/// Converts a screen y-coordinate to which row of the list it is. `None` if outside the list.
///
/// **It lays the card out again rather than counting rows**, so a wrapped question can never put a
/// click one row out — see the note on `card`.
pub fn row_at(a: &Answering, area: Rect, y: u16, lang: crate::lang::Lang) -> Option<usize> {
    if y < area.y {
        return None;
    }
    let card = card(a, area.width, area.height as usize, lang);
    let line = (y - area.y) as usize;
    // Line 0 is the rule and the last line is the hint; neither is a row.
    if line == 0 || line >= card.lines.len().saturating_sub(1) {
        return None;
    }
    card.owners.get(card.top + line - 1).copied().flatten()
}

pub fn draw(frame: &mut Frame, area: Rect, a: &Answering, lang: crate::lang::Lang) {
    let card = card(a, area.width, area.height as usize, lang);
    frame.render_widget(Paragraph::new(card.lines), area);
    // **The terminal's cursor goes where the typing is.** Without this the input method draws the
    // syllable it is composing wherever the cursor was last left — which, with the input box
    // replaced by this card, is the activity line above it.
    if let Some((x, y)) = card.caret {
        frame.set_cursor_position((area.x + x, (area.y + y).min(area.y + area.height.saturating_sub(1))));
    }
}

/// Appends lines that all carry the same row. `None` means the line belongs to no row.
fn add(
    body: &mut Vec<Line<'static>>,
    owners: &mut Vec<Option<usize>>,
    lines: Vec<Line<'static>>,
    owner: Option<usize>,
) {
    for line in lines {
        body.push(line);
        owners.push(owner);
    }
}

/// One row of the list, wrapped to the width. The caret marks the row the keys act on.
fn row_lines(
    a: &Answering,
    row: &RowKind,
    i: usize,
    lang: crate::lang::Lang,
    width: usize,
) -> Vec<Line<'static>> {
    let on = i == a.cursor && !a.typing;
    let caret =
        Span::styled(if on { "❯ " } else { "  " }, Style::default().fg(theme::accent()));
    let line = match row {
        RowKind::Option(j) => {
            let j = *j;
            let step = a.current();
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
                        theme::success()
                    } else {
                        theme::border_light()
                    }),
                ),
                Span::styled(
                    opt.label.clone(),
                    Style::default().fg(if on { theme::text_heading() } else { theme::text() }),
                ),
            ];
            if let Some(d) = &opt.description {
                spans.push(Span::styled(
                    format!("  ‒ {d}"),
                    Style::default().fg(theme::text_muted()),
                ));
            }
            Line::from(spans)
        }
        RowKind::Free => {
            let mut spans = vec![caret];
            if a.typing {
                spans.push(Span::styled("✎ ", Style::default().fg(theme::accent())));
                if a.input.text.is_empty() {
                    // When the field is empty, say what this spot is for.
                    spans.push(Span::styled(
                        lang.type_here(),
                        Style::default().fg(theme::border_light()),
                    ));
                } else {
                    spans.push(Span::styled(
                        a.input.text.clone(),
                        Style::default().fg(theme::text()),
                    ));
                }
            } else if a.free_text().is_empty() {
                spans.push(Span::styled(
                    lang.type_your_own(),
                    Style::default().fg(if on {
                        theme::text_heading()
                    } else {
                        theme::text_muted()
                    }),
                ));
            } else {
                // **What was typed must stay visible.** If it isn't, there is no way to check what
                // was written without opening it again.
                spans.push(Span::styled("✎ ", Style::default().fg(theme::success())));
                spans.push(Span::styled(
                    a.free_text().to_string(),
                    Style::default().fg(theme::text()),
                ));
            }
            Line::from(spans)
        }
        RowKind::Action(act) => {
            // **Refusing is red on the review screen.** It is the one row that throws the whole
            // answer away, and it draws among two that do not.
            let colour = match act {
                Act::Reject if a.in_review() => theme::danger(),
                _ if on => theme::accent(),
                _ => theme::text_muted(),
            };
            Line::from(vec![
                caret,
                Span::styled(
                    act.label(lang).to_string(),
                    Style::default().fg(colour).add_modifier(Modifier::BOLD),
                ),
            ])
        }
    };
    wrap::line(line, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::{Opt, Step};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn asking(steps: Vec<Step>) -> Answering {
        Answering::new(steps)
    }

    fn long_step() -> Step {
        Step {
            header: Some("방식".into()),
            question: "이 작업을 어느 쪽으로 갈까요, 계획을 먼저 세우고 그다음에 실행하는 쪽으로 할까요 아니면 바로 실행할까요?".into(),
            multi: false,
            options: vec![
                Opt {
                    label: "계획을 먼저 세우고 승인을 받은 다음에 실행합니다".into(),
                    description: Some("되돌리기 어려운 변경이라 무엇을 할지 먼저 말해 줍니다".into()),
                },
                Opt { label: "바로 실행".into(), description: None },
            ],
        }
    }

    /// **The terminal cursor goes where the typing is.** A Korean syllable being assembled is drawn
    /// by the input method *at the cursor*; left where the input box used to be, the preedit showed
    /// up on the activity line one row above while the committed word landed correctly in the field
    /// (reported 2026-09-13).
    #[test]
    fn the_caret_sits_at_the_end_of_what_the_free_row_holds() {
        let mut a = asking(vec![Step {
            header: None,
            question: "고르세요".into(),
            multi: false,
            options: vec![],
        }]);
        a.cursor = a.rows().iter().position(|r| matches!(r, RowKind::Free)).expect("a free row");
        a.typing = true;
        a.input.insert_str("한글");
        let card = card(&a, 40, 12, crate::lang::Lang::Ko);
        // `  ` + `✎ ` + the text: the caret is right after it, and the rule is drawn above the body
        // — hence the `+ 1` on the line.
        assert_eq!(card.caret, Some((8, 2)), "the caret is not where the typing is");
    }

    /// Renders the card and gives back its rows, wide characters not double-counted.
    fn render(a: &Answering, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        let room = height(a, w, h.saturating_sub(3), crate::lang::Lang::Ko) as usize;
        let area = Rect { x: 0, y: 0, width: w, height: room as u16 };
        terminal.draw(|f| draw(f, area, a, crate::lang::Lang::Ko)).expect("draw");
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0u16;
                while x < w {
                    let symbol = buf[(x, y)].symbol();
                    row.push_str(symbol);
                    x += crate::markdown::display_width(symbol).max(1) as u16;
                }
                row
            })
            .collect()
    }

    /// **A long question is not cut.** Every word of it reaches the screen, in a card narrower
    /// than the question is long — the report that started this was a question whose tail vanished
    /// against the right border.
    #[test]
    fn a_long_question_arrives_whole() {
        let a = asking(vec![long_step()]);
        let screen = render(&a, 48, 24).join("\n");
        for word in long_step().question.split_whitespace() {
            assert!(screen.contains(word), "the question lost {word:?}:\n{screen}");
        }
        // And the option's description, which used to be the first thing cut.
        assert!(screen.contains("되돌리기"), "{screen}");
        assert!(screen.contains("승인"), "{screen}");
    }

    /// **A click picks the row the pointer is on.** The hit-test lays the card out again, so this
    /// walks every drawn line and checks that the answer names the row that was actually drawn
    /// there — the exact invariant a wrapped line used to be able to break.
    #[test]
    fn a_click_picks_the_row_under_the_pointer() {
        let a = asking(vec![long_step()]);
        let (w, h) = (40u16, 30u16);
        let room = height(&a, w, h.saturating_sub(3), crate::lang::Lang::Ko);
        let area = Rect { x: 0, y: 2, width: w, height: room };
        let card = card(&a, w, room as usize, crate::lang::Lang::Ko);
        let shown = card.lines.len() - 2;
        for i in 0..shown {
            let y = area.y + 1 + i as u16;
            let want = card.owners[card.top + i];
            assert_eq!(row_at(&a, area, y, crate::lang::Lang::Ko), want, "at line {i}");
        }
        // The rule and the hint belong to no row.
        assert_eq!(row_at(&a, area, area.y, crate::lang::Lang::Ko), None);
        assert_eq!(row_at(&a, area, area.y + card.lines.len() as u16 - 1, crate::lang::Lang::Ko), None);
    }

    /// When the list cannot fit, the cursor's row is still on screen and the hint says how much is
    /// missing — silently dropping rows is the other half of the same complaint.
    #[test]
    fn a_list_that_does_not_fit_scrolls_to_the_cursor_and_counts_what_is_missing() {
        let mut a = asking(vec![Step {
            header: None,
            question: "고르세요".into(),
            multi: false,
            options: (0..20).map(|i| Opt { label: format!("선택 {i}"), description: None }).collect(),
        }]);
        a.cursor = 18;
        let room = 8u16;
        let card = card(&a, 40, room as usize, crate::lang::Lang::Ko);
        assert_eq!(card.lines.len() as u16, room, "the card overran its room");
        let shown: String = card.lines[1..card.lines.len() - 1]
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(shown.contains("선택 18"), "the cursor's row scrolled off:\n{shown}");
        let hint = &card.lines[card.lines.len() - 1];
        assert!(hint.to_string().contains('↑'), "nothing says what is hidden above: {hint}");
    }
}
