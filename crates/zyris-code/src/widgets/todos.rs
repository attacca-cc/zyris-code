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
///
/// **Done tasks sink to the bottom only when the list is too long.** A list that fits keeps the
/// order tasks were added in — a task changing state must not jump the line under the reader's
/// eyes. But when some of the plan has to be hidden behind "+N more", what belongs in that hidden
/// space is the finished work: the person is looking at what is left to do, and a task already
/// struck through at the top of a screenful pushes the one actually in hand off the edge.
/// Reordering happens only then, and even then done tasks keep their relative order, so a finished
/// list reads the same way it was written.
pub fn lines(items: &[Todo], lang: Lang, width: usize, rows: usize) -> Vec<Line<'static>> {
    if rows == 0 || items.is_empty() {
        return vec![];
    }
    let overflow = items.len() > rows;
    // What to draw, in drawing order. When it overflows, the done ones go last, each group keeping
    // the order they arrived in.
    let ordered: Vec<&Todo> = if overflow {
        let mut ordered = Vec::with_capacity(items.len());
        ordered.extend(items.iter().filter(|t| t.status != Status::Done));
        ordered.extend(items.iter().filter(|t| t.status == Status::Done));
        ordered
    } else {
        items.iter().collect()
    };
    let shown = if overflow { rows.saturating_sub(1) } else { items.len() };
    let mut out: Vec<Line<'static>> =
        ordered.iter().take(shown).enumerate().map(|(i, todo)| row(todo, i + 1, width)).collect();
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
    // **The title says it too, not only the dot** (2026-08-18 user request). A dot is two cells at
    // the far left of a row that can run the width of the screen; the eye reading down a plan is on
    // the words, and asking it to keep glancing back to the margin to find out which task is in
    // hand is asking for something a colour can just say.
    //
    // Waiting is plain text, in hand is blue, and a finished task is struck through and dimmed —
    // the one state where the words themselves are no longer worth reading, said in the way every
    // checklist everywhere says it.
    let (dot, title_style) = match todo.status {
        Status::Pending => (theme::border_light(), Style::default().fg(theme::text())),
        Status::Doing => (
            theme::accent(),
            Style::default().fg(theme::in_progress()).add_modifier(Modifier::BOLD),
        ),
        // **A finished task dims but stays.** Dropping it would make the list shrink as work goes
        // on, and the count on the activity line would have nothing to point at.
        Status::Done => (
            theme::success(),
            Style::default().fg(theme::text_muted()).add_modifier(Modifier::CROSSED_OUT),
        ),
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

    /// **The words say the state as well as the dot does** (2026-08-18 user request). Reading down
    /// a plan, the eye is on the titles; making it glance back to a two-cell dot in the margin to
    /// find out which task is in hand is work a colour can do for it.
    #[test]
    fn a_tasks_own_words_say_which_state_it_is_in() {
        let title_of = |status| {
            let line = row(&todo("빌드", status), 1, 40);
            line.spans.last().expect("a row ends with its title").style
        };
        let waiting = title_of(Status::Pending);
        let doing = title_of(Status::Doing);
        let done = title_of(Status::Done);

        assert_eq!(
            waiting.fg,
            Some(theme::text()),
            "a task nobody has started reads as plain text"
        );
        assert_eq!(doing.fg, Some(theme::in_progress()), "the one in hand is not marked out");
        assert_eq!(done.fg, Some(theme::text_muted()), "a finished task did not dim");
        assert!(
            done.add_modifier.contains(Modifier::CROSSED_OUT),
            "a finished task is not struck through",
        );
        // And the three do not read as each other.
        assert_ne!(waiting.fg, doing.fg);
        assert_ne!(doing.fg, done.fg);
        assert_ne!(waiting.fg, done.fg);
        // Only the finished one is struck through ‒ a line through work still to do reads as
        // cancelled.
        for still_to_do in [waiting, doing] {
            assert!(!still_to_do.add_modifier.contains(Modifier::CROSSED_OUT));
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
        let items: Vec<Todo> =
            (1..=10).map(|i| todo(&format!("할 일 {i}"), Status::Pending)).collect();
        let out = lines(&items, Lang::Ko, 40, 4);
        assert_eq!(out.len(), 4, "it must not exceed the rows it was given");
        assert_eq!(plain(&out[3]), "  ↓ 7개 더");
        assert!(plain(&out[2]).contains("할 일 3"), "{:?}", plain(&out[2]));
    }

    /// **When the list overflows, finished tasks sink to the bottom.** The visible rows are what
    /// the person is looking at — what is left to do — so a struck-through task at the top of a
    /// screenful must not push the one actually in hand off the edge. Done tasks still keep their
    /// relative order, so the finished stretch reads the way it was written.
    #[test]
    fn done_tasks_sink_to_the_bottom_only_when_the_list_overflows() {
        // Fits: order is exactly the order tasks were added in, done or not.
        let items = [
            todo("첫째", Status::Done),
            todo("둘째", Status::Doing),
            todo("셋째", Status::Pending),
        ];
        let fits = lines(&items, Lang::Ko, 40, 3);
        assert_eq!(plain(&fits[0]), "  ● 1. 첫째");
        assert_eq!(plain(&fits[1]), "  ● 2. 둘째");
        assert_eq!(plain(&fits[2]), "  ● 3. 셋째");

        // Overflow: the done one goes below the not-done ones, which keep their relative order.
        let many = [
            todo("완료된 일", Status::Done),
            todo("진행 중", Status::Doing),
            todo("대기 중", Status::Pending),
            todo("또 하나", Status::Pending),
            todo("마지막", Status::Pending),
        ];
        let out = lines(&many, Lang::Ko, 40, 3);
        assert_eq!(out.len(), 3);
        assert!(plain(&out[0]).contains("진행 중"), "{:?}", plain(&out[0]));
        assert!(plain(&out[1]).contains("대기 중"), "{:?}", plain(&out[1]));
        assert_eq!(plain(&out[2]), "  ↓ 3개 더", "{:?}", plain(&out[2]));
        // The finished task is hidden (counted in "더"), not shown ahead of active work.
        assert!(!out.iter().any(|l| plain(l).contains("완료된 일")));
    }

    /// **Sinking is about where, never about dropping.** A list that overflows shows the active
    /// tasks first; the done ones come after them, and keep their own relative order — the same
    /// one the finished stretch had when it was written. One doing task followed by three done
    /// ones shows the doing task, then the first done one (the earliest finished) on the next
    /// visible row, and the rest counted as hidden.
    #[test]
    fn done_tasks_keep_their_relative_order_when_sunk() {
        let items = [
            todo("둘", Status::Done),
            todo("하나", Status::Doing),
            todo("셋", Status::Done),
            todo("넷", Status::Done),
        ];
        let out = lines(&items, Lang::Ko, 40, 3);
        let texts: Vec<String> = out.iter().map(plain).collect();
        // One doing task, then the earliest-finished of the done ones, then the overflow count.
        assert!(texts[0].contains("하나"), "{:?}", texts);
        assert!(texts[1].contains("둘"), "{:?}", texts);
        assert_eq!(texts[2], "  ↓ 2개 더", "{:?}", texts);
    }

    #[test]
    fn a_list_that_fits_shows_every_task() {
        let items: Vec<Todo> =
            (1..=3).map(|i| todo(&format!("할 일 {i}"), Status::Pending)).collect();
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
