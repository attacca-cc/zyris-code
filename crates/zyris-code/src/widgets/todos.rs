//! The session's plan, unfolded under the activity line.
//!
//! **Titles only** — a todo has no separate description, and this list is here to be skimmed at a
//! glance while something else is on screen. What each task is *doing* is said by the colour of
//! its dot, not by a word: `●` never changes width, so a task moving from pending to done cannot
//! shift the row it is on. The same reason the thread list carries a dot on every row.
//!
//! Layout is pure (`lines`) and this widget only draws it, so the height the layout reserves and
//! the rows that go into it come from **one** count.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;
use crate::lang::Lang;
use crate::markdown::truncate_to;
use crate::theme;
use crate::todos::{Status, Todo};

/// The left margin, so the tasks sit under the activity line's text rather than under its dot.
const PAD: &str = "  ";

/// How many rows the list wants, given how many it may have. `0` when it is folded or empty.
///
/// **The conversation keeps the rest.** A long plan must not push what is being read off the top,
/// so the list never takes more than `avail`, and what does not fit is counted on the last row.
pub fn height(state: &State, avail: u16) -> u16 {
    if !state.todos_open {
        return 0;
    }
    let count = state.todos.items().len() as u16;
    count.min(avail)
}

/// The rows themselves. Pure — tests read the list through this.
///
/// `rows` is how many lines there is room for. When the tasks do not fit, the last row says how
/// many are hidden instead of showing one more task: a list that just stops looks complete.
pub fn lines(items: &[Todo], lang: Lang, width: usize, rows: usize) -> Vec<Line<'static>> {
    if rows == 0 || items.is_empty() {
        return vec![];
    }
    let shown = if items.len() <= rows { items.len() } else { rows.saturating_sub(1) };
    let mut out: Vec<Line<'static>> = items
        .iter()
        .take(shown)
        .enumerate()
        .map(|(i, todo)| row(todo, i + 1, width))
        .collect();
    if shown < items.len() {
        let muted = Style::default().fg(theme::text_muted());
        out.push(Line::from(vec![
            Span::raw(PAD),
            Span::styled(lang.todo_more(items.len() - shown), muted),
        ]));
    }
    out
}

/// One task: `  ● 3. what it says`.
fn row(todo: &Todo, number: usize, width: usize) -> Line<'static> {
    // The dot's colour is the only thing that moves as a task progresses — grey waiting, orange
    // under way, green finished. Same three the thread list uses, for the same three meanings.
    let (dot, title_style) = match todo.status {
        Status::Pending => (theme::border_light(), Style::default().fg(theme::text_muted())),
        Status::Doing => {
            (theme::accent(), Style::default().fg(theme::text()).add_modifier(Modifier::BOLD))
        }
        // **A finished task dims but stays.** Dropping it would make the list shrink as work goes
        // on, and the count on the activity line would have nothing to point at.
        Status::Done => (theme::success(), Style::default().fg(theme::text_muted())),
    };
    let head = format!("{number}. ");
    // A title is one line here however it was written, so its own newlines are folded away.
    let title: String = todo.title.split_whitespace().collect::<Vec<_>>().join(" ");
    let room = width.saturating_sub(PAD.len() + 2 + head.len());
    Line::from(vec![
        Span::raw(PAD),
        Span::styled("● ", Style::default().fg(dot)),
        Span::styled(head, Style::default().fg(theme::text_muted())),
        Span::styled(truncate_to(&title, room), title_style),
    ])
}

pub fn draw(frame: &mut Frame, area: Rect, state: &State) {
    if area.height == 0 {
        return;
    }
    let rows = lines(state.todos.items(), state.lang, area.width as usize, area.height as usize);
    frame.render_widget(Paragraph::new(rows), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todo(title: &str, status: Status) -> Todo {
        Todo { id: title.into(), title: title.into(), status }
    }

    fn plain(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn every_task_is_numbered_and_says_only_its_title() {
        let items = [todo("첫 번째", Status::Done), todo("두 번째", Status::Doing)];
        let out = lines(&items, Lang::Ko, 40, 5);
        assert_eq!(plain(&out[0]), "  ● 1. 첫 번째");
        assert_eq!(plain(&out[1]), "  ● 2. 두 번째");
    }

    /// **A task changing status must not move anything.** The dot is one glyph in every state and
    /// only its colour changes — the trap the thread list already fell into once.
    #[test]
    fn finishing_a_task_moves_its_title_not_at_all() {
        let before = plain(&row(&todo("빌드", Status::Pending), 1, 40));
        for status in [Status::Doing, Status::Done] {
            assert_eq!(plain(&row(&todo("빌드", status), 1, 40)), before);
        }
    }

    /// The three states have to be told apart at a glance, so no two share a colour.
    #[test]
    fn each_state_of_a_task_gets_its_own_colour() {
        let dot = |status| row(&todo("x", status), 1, 40).spans[1].style.fg;
        let (pending, doing, done) = (dot(Status::Pending), dot(Status::Doing), dot(Status::Done));
        assert_ne!(pending, doing);
        assert_ne!(doing, done);
        assert_ne!(pending, done);
    }

    /// **What was cut has to say so.** A list that simply stops reads as the whole plan, and the
    /// count on the activity line would then disagree with what is on screen.
    #[test]
    fn a_plan_too_long_for_the_room_counts_what_is_hidden() {
        let items: Vec<Todo> = (1..=10).map(|i| todo(&format!("할 일 {i}"), Status::Pending)).collect();
        let out = lines(&items, Lang::Ko, 40, 4);
        assert_eq!(out.len(), 4, "it must not exceed the rows it was given");
        assert_eq!(plain(&out[3]), "  ↓ 7개 더");
        assert!(plain(&out[2]).contains("할 일 3"), "{:?}", plain(&out[2]));
    }

    #[test]
    fn a_list_that_fits_shows_every_task() {
        let items: Vec<Todo> = (1..=3).map(|i| todo(&format!("할 일 {i}"), Status::Pending)).collect();
        assert_eq!(lines(&items, Lang::Ko, 40, 3).len(), 3);
    }

    /// A long title is cut, never wrapped — one task is one row, so the numbering stays readable.
    #[test]
    fn a_long_title_is_cut_inside_the_row() {
        let items = [todo(&"가".repeat(60), Status::Pending)];
        let out = lines(&items, Lang::Ko, 30, 3);
        assert_eq!(out.len(), 1);
        assert!(crate::markdown::display_width(&plain(&out[0])) <= 30, "{:?}", plain(&out[0]));
        assert!(plain(&out[0]).ends_with('…'));
    }

    /// A title written over several lines still occupies one row.
    #[test]
    fn a_title_with_newlines_stays_on_one_row() {
        let items = [todo("첫 줄\n둘째 줄", Status::Pending)];
        assert_eq!(plain(&lines(&items, Lang::Ko, 40, 3)[0]), "  ● 1. 첫 줄 둘째 줄");
    }

    #[test]
    fn a_folded_or_empty_list_takes_no_rows() {
        assert!(lines(&[], Lang::Ko, 40, 5).is_empty());
        assert!(lines(&[todo("하나", Status::Pending)], Lang::Ko, 40, 0).is_empty());
    }
}
