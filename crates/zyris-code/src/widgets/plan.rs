//! The plan waiting to be approved, above the input.
//!
//! **Above it rather than over it, which is where the question panel goes.** A question *is* the
//! message — answering it and typing are the same act, so it takes the input's place. A plan is
//! decided by an ordinary message and the two decisions are not alike: approving is Enter on an
//! empty draft, and anything else is what to change, typed. The draft has to stay reachable for
//! the second one, so the plan sits above it the way the todo list does.
//!
//! **Folded to a peek, opened on request** (Ctrl+P). A plan runs to pages; what is wanted the
//! moment it lands is "there is one, roughly this". The whole of it is on the timeline above as
//! the tool call it arrived in — that scrolls, and this cannot.
//!
//! Layout is pure ([`crate::plan::Plan`]) and this only draws it, so the height reserved and the
//! rows put in it come from one count.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;
use crate::lang::Lang;
use crate::markdown::truncate_to;
use crate::plan::Plan;
use crate::theme;

/// The left margin, matching the todo list's, so the two stack without stepping.
const PAD: &str = "  ";

/// How many rows the panel wants, given how many it may have. `0` when there is no plan.
///
/// **The conversation gives up the room, never the input.** A plan is pages long and the reply to
/// it is typed; taking the input's rows to show more of the plan would take away the way to answer.
pub fn height(state: &State, avail: u16) -> u16 {
    let Some(plan) = &state.plan else { return 0 };
    if avail == 0 {
        return 0;
    }
    // One for the header, and one for the "N more" line when something is held back.
    let room = (avail as usize).saturating_sub(1);
    let shown = plan.shown(room);
    let more = usize::from(plan.hidden(room) > 0 && shown + 1 < room + 1);
    (1 + shown + more).min(avail as usize) as u16
}

/// The rows. Pure, so what is drawn and how tall it is cannot disagree.
pub fn lines(plan: &Plan, lang: Lang, width: usize, rows: usize) -> Vec<Line<'static>> {
    if rows == 0 {
        return vec![];
    }
    let room = rows.saturating_sub(1);
    let shown = plan.shown(room);
    let hidden = plan.hidden(room);

    let mut out = vec![Line::from(vec![
        Span::styled(PAD, Style::default().fg(theme::text_muted())),
        Span::styled(
            lang.plan_title(),
            Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", lang.plan_keys(plan.open)),
            Style::default().fg(theme::text_muted()),
        ),
    ])];

    let body = plan.body();
    let room_for_text = width.saturating_sub(PAD.len() + 2);
    for line in body.iter().take(shown) {
        out.push(Line::from(vec![
            Span::raw(PAD),
            Span::styled("│ ", Style::default().fg(theme::border_light())),
            Span::styled(truncate_to(line, room_for_text), Style::default().fg(theme::text())),
        ]));
    }
    if hidden > 0 && out.len() < rows {
        out.push(Line::from(vec![
            Span::raw(PAD),
            Span::styled(lang.plan_more(hidden), Style::default().fg(theme::text_muted())),
        ]));
    }
    out.truncate(rows);
    out
}

pub fn draw(frame: &mut Frame, area: Rect, state: &State) {
    let Some(plan) = &state.plan else { return };
    if area.height == 0 {
        return;
    }
    let rows = lines(plan, state.lang, area.width as usize, area.height as usize);
    frame.render_widget(Paragraph::new(rows), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Submitted;

    fn plan(lines: usize) -> Plan {
        let markdown = (1..=lines).map(|n| format!("step {n}")).collect::<Vec<_>>().join("\n");
        Plan::new(&Submitted { seq: 1, markdown, decided: false })
    }

    fn plain(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// **It never takes more rows than it was given.** It sits between the conversation and the
    /// input, and one row too many pushes the input off the bottom of the screen.
    #[test]
    fn the_panel_never_exceeds_the_room_it_was_given() {
        for rows in 1..=12 {
            let out = lines(&plan(20), Lang::En, 60, rows);
            assert!(out.len() <= rows, "{} rows drawn into {rows}", out.len());
        }
        let mut open = plan(20);
        open.open = true;
        for rows in 1..=30 {
            assert!(lines(&open, Lang::En, 60, rows).len() <= rows);
        }
    }

    /// **What is held back is said.** A plan that simply stops reads as a short plan, and the
    /// person approves something they have not seen the end of.
    #[test]
    fn a_folded_plan_says_how_much_it_is_not_showing() {
        let out = lines(&plan(20), Lang::En, 60, 6);
        let last = plain(out.last().expect("rows"));
        assert!(last.contains("17"), "{last}");

        // Opened, with room for all of it, there is nothing left to say.
        let mut open = plan(4);
        open.open = true;
        let out = lines(&open, Lang::En, 60, 8);
        assert!(
            !plain(out.last().expect("rows")).contains("more"),
            "{:?}",
            plain(&out[out.len() - 1])
        );
    }

    /// The header says what the keys do, because nothing else on screen does.
    #[test]
    fn the_header_says_how_to_answer_it() {
        let out = lines(&plan(3), Lang::En, 80, 6);
        let head = plain(&out[0]);
        assert!(head.contains("Enter"), "{head}");
        assert!(head.to_lowercase().contains("ctrl+p"), "{head}");
    }
}
