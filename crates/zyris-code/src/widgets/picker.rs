//! The project/session list. Overlaid in the center of the screen.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::markdown::display_width;
use crate::picker::{Picker, Slot};
use crate::theme;

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    picker: &mut Picker,
    lang: crate::lang::Lang,
    tick: u64,
) {
    // The centered box. Sized to the list, but never taller than the screen.
    // The two separator lines take a row each, so they must be counted for everything to fit.
    let rule = picker.is_create(0) && picker.rows.len() > 1;
    // **The box is as wide as its widest row, capped by the screen.** A fixed 64 cut a command's
    // description with an `…` on a screen three times that wide, and a description cut in half is
    // worth less than either showing it whole or not showing it at all.
    let widest = picker.rows.iter().map(row_need).max().unwrap_or(0);
    let w = ((widest as u16 + 4).max(64)).min(area.width.saturating_sub(4)).max(20);
    let inner_w = w.saturating_sub(2) as usize;
    // **The note area keeps room for every row's note, not only this one's.** Counted over all
    // rows, so the box is one height whichever row the cursor is on — a list whose box jumps up
    // and down as `↑↓` walks it is the same fault the panel's foot exists to prevent.
    let detail_rows = picker
        .rows
        .iter()
        .filter_map(|row| detail_of(row, inner_w))
        .map(|note| note.len())
        .max()
        .unwrap_or(0);
    let detail = cursor_detail(picker, inner_w);
    let want_h = (picker.rows.len() as u16)
        .saturating_add(5 + rule as u16 + detail_rows as u16)
        .max(6);
    let h = want_h.min(area.height.saturating_sub(2)).max(3);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    // Without clearing behind, the conversation shows through.
    frame.render_widget(Clear, box_area);
    // **Also scrub wide characters straddling the border.** If the leading half of a wide
    // character remains just outside the box's left edge, it bleeds into the box and breaks the
    // border. `Clear` only clears inside the box, so we must remove this half ourselves.
    scrub_left_edge(frame, box_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::accent()))
        // **The title is cut to fit, with a `…`.** A project name is arbitrary text, and
        // ratatui draws an over-long title straight over its own border — the box loses its
        // top-right corner and the row ends mid-word with nothing saying it was cut.
        // Four columns go to the two corners and the space either side of the title.
        .title(Span::styled(
            format!(
                " {} ",
                crate::markdown::truncate_to(&picker.title(lang), w.saturating_sub(4) as usize)
            ),
            Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if picker.loading {
        lines.push(Line::from(Span::styled(
            lang.loading(),
            Style::default().fg(theme::text_muted()),
        )));
    }

    // **The pure side decides** where each row goes (`picker::slots`). Here we just draw.
    // The rule and the key hints at the foot take a row each.
    let width = inner.width as usize;
    // The rule, the hint and the whole note area take rows from the body — the box cannot grow
    // past the screen. The area's size is fixed, so the rows the list gets are too.
    let body_h = inner.height.saturating_sub(2 + detail_rows as u16) as usize;
    // **Where the window ended up is stored back.** Without it the layout would be derived
    // from the cursor alone every frame, which pins the cursor to an edge — see `window_top`.
    let (laid, top) = crate::picker::slots(&picker.rows, picker.cursor, picker.top, body_h);
    picker.top = top;
    for slot in laid {
        lines.push(match slot {
            Slot::Row(i) => {
                row_line(&picker.rows[i], i == picker.cursor, picker.is_create(i), width, tick)
            }
            Slot::Rule => {
                Line::from(Span::styled("─".repeat(width), Style::default().fg(theme::border())))
            }
            Slot::More { count, up } => Line::from(Span::styled(
                lang.pick_more(up, count),
                Style::default().fg(theme::text_muted()),
            )),
        });
    }

    // The key hints are not a row of the list, so a rule marks where the list ended. Without it
    // the last entry and the hints run together and the hints read as one more thing to pick.
    lines.push(Line::from(Span::styled("─".repeat(width), Style::default().fg(theme::border()))));

    // **The note in full, for the row the cursor is on.** It left its row because it did not fit;
    // it is here because it is the sentence saying what that row does.
    for row in &detail {
        lines.push(Line::from(Span::styled(
            format!("  {row}"),
            Style::default().fg(theme::text_muted()),
        )));
    }
    // **The rows left over stay empty, and that is the point.** A row whose note is one line and
    // a row whose note is two must not give the list two different heights.
    for _ in detail.len()..detail_rows {
        lines.push(Line::from(""));
    }

    // The meaning of ← changes with the level. Say it plainly.
    let back = match picker.level {
        crate::picker::Level::Projects => lang.picker_close(),
        crate::picker::Level::Sessions { .. } => lang.picker_back(),
        // These two are outside the project hierarchy, so there's nowhere to go back to.
        crate::picker::Level::Agents
        | crate::picker::Level::Commands
        | crate::picker::Level::Files { .. }
        | crate::picker::Level::History { .. }
        | crate::picker::Level::PluginTarget { .. } => lang.picker_esc_close(),
    };
    // **A pending deletion replaces the hints rather than sitting beside them.** It is the only
    // thing the keys can do while it is up, so leaving "Enter choose" underneath would say Enter
    // still opens the row — the one reading that would be answering a different question.
    match &picker.confirm {
        Some(armed) => lines.push(Line::from(Span::styled(
            lang.picker_delete_ask(&armed.name),
            Style::default().fg(theme::danger()),
        ))),
        None => lines.push(Line::from(Span::styled(
            lang.picker_keys(back),
            Style::default().fg(theme::text_muted()),
        ))),
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// One row of the list.
///
/// **"Create new" is a different color.** Mixed into the session list it would read as one
/// session, but it's something you create, not pick.
fn row_line(
    row: &crate::picker::Row,
    on: bool,
    create: bool,
    width: usize,
    tick: u64,
) -> Line<'static> {
    use crate::picker::ThreadStatus;
    let fg = match (row.enabled, create, on) {
        (false, _, _) => theme::border_light(),
        (true, true, _) => theme::accent(),
        (true, false, true) => theme::text_heading(),
        (true, false, false) => theme::text(),
    };
    // A running thread blinks its status dot; a finished one holds its colour steady.
    //
    // **Running's dim half must not land on the unknown grey.** Both would be the same dot at
    // rest, and a still frame could not tell a thread that is working from one nobody has read
    // yet — so the unknown one sits at the dimmest grey the palette has and the blink swings
    // between orange and a brighter grey.
    let status_span = row.status.map(|s| {
        let colour = match s {
            ThreadStatus::Unknown => theme::border_light(),
            ThreadStatus::Running if crate::widgets::activity::blink_on(tick) => theme::accent(),
            ThreadStatus::Running => theme::text_muted(),
            ThreadStatus::Success => theme::success(),
            ThreadStatus::Failed => theme::danger(),
        };
        Span::styled("●", Style::default().fg(colour))
    });
    let (label, note) = split(width, &row.label, row.note.as_deref(), row.status.is_some());
    let mut spans =
        vec![Span::styled(if on { "❯ " } else { "  " }, Style::default().fg(theme::accent()))];
    // The status dot sits at the left, right after the cursor marker — a thread's state is
    // the first thing the eye lands on, before reading its title.
    if let Some(dot) = status_span {
        spans.push(dot);
        spans.push(Span::styled(" ", Style::default().fg(theme::text_muted())));
    }
    spans.push(Span::styled(label.clone(), Style::default().fg(fg)));
    if let Some(note) = note {
        let used = 2 + (row.status.is_some() as usize) * 2 + display_width(&label);
        let pad = width.saturating_sub(used + display_width(&note));
        spans.push(Span::styled(" ".repeat(pad), Style::default().fg(theme::text_muted())));
        spans.push(Span::styled(note, Style::default().fg(theme::text_muted())));
    }
    Line::from(spans)
}

/// However short the note is, it's worth showing at least this much. Narrower than this, drop it entirely.
const NOTE_MIN: usize = 8;

/// How wide a row wants to be: the caret, the status dot, the name, a gap and the note.
fn row_need(row: &crate::picker::Row) -> usize {
    2 + (row.status.is_some() as usize) * 2
        + display_width(&row.label)
        + row.note.as_deref().map_or(0, |note| 2 + display_width(note))
}

/// The note `row` would put under the list **if the cursor were on it** — `None` when the row
/// holds its own note, or has none.
///
/// The caller counts these over every row, which is how the note area comes to have one size
/// whatever the cursor is on.
fn detail_of(row: &crate::picker::Row, width: usize) -> Option<Vec<String>> {
    let note = row.note.as_deref()?;
    if split(width, &row.label, Some(note), row.status.is_some()).1.is_some() {
        // The row already says it, and saying it twice is not a feature.
        return None;
    }
    // Two columns of indent, so it reads as a note about the row above rather than another row.
    Some(crate::wrap::words(note, width.saturating_sub(2)))
}

/// The note the cursor's row could not show inline, wrapped to the box — empty when the row held
/// it, or has none.
fn cursor_detail(picker: &Picker, width: usize) -> Vec<String> {
    picker.rows.get(picker.cursor).and_then(|row| detail_of(row, width)).unwrap_or_default()
}

/// Splits one line into (name, note). **The name comes first.**
///
/// Give the note the room first and the name gets cut — `/agent` actually got truncated to
/// `/a…`, and you couldn't tell what command it was in the list. **The name is identity and the
/// note is garnish**: when the name is cut, the reason to pick that line disappears, but without the note you can still guess from the name.
///
/// Still, the name must always be truncated — session titles have arbitrary lengths, and left
/// alone they'd punch through the box and collapse the screen.
fn split(width: usize, label: &str, note: Option<&str>, status: bool) -> (String, Option<String>) {
    // The status dot and its trailing space take two columns on the left, before the label.
    let dot = if status { 2 } else { 0 };
    let label = truncate(label, width.saturating_sub(2 + dot));
    let Some(note) = note else {
        return (label, None);
    };
    // Leave at least two columns between the name and the note. Stuck together, they read as one word.
    let room = width.saturating_sub(2 + dot + display_width(&label) + 2);
    if room < NOTE_MIN || display_width(note) > room {
        // **A note that does not fit is left off the row rather than cut.** `…` keeps the beginning
        // of a sentence and throws the end away — and on a description the end is the part that
        // says what the thing does. The whole note goes under the list for the row the cursor is
        // on, where the keys are acting (`cursor_detail`).
        return (label, None);
    }
    (label, Some(note.to_string()))
}

/// Truncates to fit the column count. When cut, appends `…` to show it was cut.
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

/// Replaces the leading half of a wide character straddling the box's left edge with a space.
///
/// The enrollment-code window (`enroll.rs`) does the same — every overlaid window uses this path.
pub(crate) fn scrub_left_edge(frame: &mut Frame, box_area: Rect) {
    if box_area.x == 0 {
        return;
    }
    let x = box_area.x - 1;
    let buf = frame.buffer_mut();
    for y in box_area.y..box_area.y.saturating_add(box_area.height) {
        if !buf.area.contains((x, y).into()) {
            continue;
        }
        if display_width(buf[(x, y)].symbol()) > 1 {
            buf[(x, y)].set_symbol(" ");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A long project name must not punch through the box.** ratatui draws an over-long
    /// title straight over its own border: the top-right corner disappears and the name ends
    /// mid-word with nothing saying it was cut.
    #[test]
    fn a_long_project_name_is_cut_inside_the_box() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let long = "아주 길고 긴 이름을 가진 프로젝트 그리고 더 길어지는 이름";
        let mut picker = Picker::loading_sessions("p1".into(), long.into());
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).expect("terminal");
        terminal.draw(|f| draw(f, f.area(), &mut picker, crate::lang::Lang::Ko, 0)).expect("draw");
        // The box is centred, so its top border is not row zero — find the row it landed on.
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> =
            buffer.content.chunks(80).map(|row| row.iter().map(|c| c.symbol()).collect()).collect();
        let top = rows.iter().find(|r| r.contains('┌')).expect("no box was drawn").clone();
        assert!(top.contains('…'), "the title was cut with no sign of it: {top:?}");
        assert!(!top.contains(long), "the whole name still went out: {top:?}");
        // The border has to survive: the box is drawn 64 wide and centred, so both corners
        // sit inside this row.
        assert!(top.contains('┌') && top.contains('┐'), "the title ate the border: {top:?}");
    }

    /// Renders the box and gives back its rows, trimmed of the screen either side.
    fn render(picker: &mut Picker, w: u16, h: u16) -> Vec<String> {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal.draw(|f| draw(f, f.area(), picker, crate::lang::Lang::Ko, 0)).expect("draw");
        // **The cell behind a wide character has to be skipped.** ratatui fills it with a space,
        // so joining every cell turns `새 쓰레드` into `새  쓰 레 드` and no search ever matches.
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0u16;
                while x < w {
                    let symbol = buf[(x, y)].symbol();
                    row.push_str(symbol);
                    x += display_width(symbol).max(1) as u16;
                }
                row
            })
            .filter(|row: &String| row.contains('│') || row.contains('┌') || row.contains('└'))
            .collect()
    }

    /// **The create row survives all the way to the screen, wherever the cursor is.** `slots`
    /// being right is not enough — the widget draws what it is given, and this is the thing a
    /// person looks for when they want a new thread.
    #[test]
    fn the_create_row_is_drawn_even_when_the_list_is_scrolled_to_the_bottom() {
        let items: Vec<(String, String, crate::picker::ThreadStatus)> = (0..40)
            .map(|i| {
                (format!("s{i}"), format!("지난 대화 {i}"), crate::picker::ThreadStatus::Unknown)
            })
            .collect();
        let mut picker = Picker::sessions("p1".into(), "기본".into(), items, crate::lang::Lang::Ko);
        picker.cursor = 40;
        let rows = render(&mut picker, 80, 20);
        let head = rows
            .iter()
            .position(|r| r.contains("＋ 새 쓰레드"))
            .expect("the create row is gone from a scrolled list");
        // It is the first thing under the top border, above everything that scrolls.
        assert!(head <= 1, "the create row is not at the top: {rows:#?}");
        assert!(
            rows.iter().any(|r| r.contains("↑") || r.contains("더")),
            "nothing says how many are hidden above: {rows:#?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("지난 대화 39")),
            "the cursor row is off screen: {rows:#?}"
        );
        // The box keeps its walls — a row that runs past the border collapses the screen.
        assert!(rows.iter().all(|r| r.matches('│').count() == 2 || !r.contains('│')), "{rows:#?}");
    }

    /// **The name never gets shaved.** `/agent` actually got truncated to `/a…` and you couldn't
    /// tell what command it was — the reason was a long note.
    #[test]
    fn a_long_note_never_eats_into_the_name() {
        let (label, _) = split(
            62,
            "/agent",
            Some("에이전트를 고릅니다. 다음 메시지에서 새 thread가 열립니다"),
            false,
        );
        assert_eq!(label, "/agent");
    }

    /// A thread's status is drawn as a coloured dot at the left, before the title — running
    /// orange, failed red, success green. It is the row's status, not a text note.
    #[test]
    fn a_thread_status_becomes_a_dot_on_the_left() {
        use crate::picker::ThreadStatus;
        let row = crate::picker::Row {
            id: Some("s1".into()),
            label: "대화".into(),
            note: None,
            enabled: true,
            status: Some(ThreadStatus::Running),
        };
        let line = row_line(&row, false, false, 20, 8);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("●"), "no status dot: {text:?}");
        // The dot sits before the title — the cursor marker, then the dot, then the label.
        let dot_pos = line.spans.iter().position(|s| s.content.as_ref() == "●").unwrap();
        let label_pos = line.spans.iter().position(|s| s.content.as_ref() == "대화").unwrap();
        assert!(dot_pos < label_pos, "status dot must precede the title: {text:?}");
        // A finished thread holds its colour, a running one blinks (tick on/off).
        let ok_row = crate::picker::Row {
            id: Some("s1".into()),
            label: "대화".into(),
            note: None,
            enabled: true,
            status: Some(ThreadStatus::Success),
        };
        let failed_row = crate::picker::Row {
            id: Some("s1".into()),
            label: "대화".into(),
            note: None,
            enabled: true,
            status: Some(ThreadStatus::Failed),
        };
        let running_col =
            line.spans.iter().find(|s| s.content.as_ref() == "●").and_then(|s| s.style.fg);
        let ok_col = row_line(&ok_row, false, false, 20, 0)
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "●")
            .and_then(|s| s.style.fg);
        let failed_col = row_line(&failed_row, false, false, 20, 0)
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "●")
            .and_then(|s| s.style.fg);
        assert_eq!(running_col, Some(theme::text_muted()), "running dot off-phase must be dim");
        assert_eq!(ok_col, Some(theme::success()));
        assert_eq!(failed_col, Some(theme::danger()));
    }

    fn dot(status: crate::picker::ThreadStatus, tick: u64) -> Line<'static> {
        let row = crate::picker::Row {
            id: Some("s1".into()),
            label: "대화".into(),
            note: None,
            enabled: true,
            status: Some(status),
        };
        row_line(&row, false, false, 20, tick)
    }

    /// **A dot that lands must change the colour and nothing else.** The outcomes are derived one
    /// request at a time, so on a busy project they trickle in over seconds — if the dot were
    /// absent until then, every title would jump two columns right as its own dot appeared and
    /// the whole list would squirm.
    #[test]
    fn a_dot_landing_moves_the_title_not_at_all() {
        use crate::picker::ThreadStatus;
        let plain = |line: &Line<'static>| -> String {
            line.spans.iter().map(|s| s.content.as_ref()).collect()
        };
        let before = plain(&dot(ThreadStatus::Unknown, 0));
        for after in
            [ThreadStatus::Running, ThreadStatus::Success, ThreadStatus::Failed].map(|s| dot(s, 0))
        {
            assert_eq!(plain(&after), before, "the row moved as its dot landed");
        }
    }

    /// **No two dots share a colour** — least of all `Unknown` and the dim half of `Running`'s
    /// blink, which would leave a still frame unable to say whether a thread is working or
    /// merely unread.
    #[test]
    fn every_dot_colour_says_something_different() {
        use crate::picker::ThreadStatus;
        let colour = |status, tick| {
            dot(status, tick)
                .spans
                .iter()
                .find(|s| s.content.as_ref() == "●")
                .and_then(|s| s.style.fg)
                .expect("no dot was drawn")
        };
        // Tick 0 and 8 are the two halves of the blink.
        let seen = [
            colour(ThreadStatus::Unknown, 0),
            colour(ThreadStatus::Running, 0),
            colour(ThreadStatus::Running, 8),
            colour(ThreadStatus::Success, 0),
            colour(ThreadStatus::Failed, 0),
        ];
        for (i, a) in seen.iter().enumerate() {
            for b in &seen[i + 1..] {
                assert_ne!(a, b, "two dots wear the same colour: {seen:?}");
            }
        }
    }

    /// If there's no room for the note, drop the note. A half-cut note is unreadable.
    #[test]
    fn a_note_is_dropped_rather_than_squeezed_to_nothing() {
        // A width where the name fits exactly and no room is left for the note.
        let (label, note) = split(14, "가나다라마", Some("설명"), false);
        assert_eq!(label, "가나다라마", "the name was truncated");
        assert!(note.is_none(), "{note:?}");
    }

    /// Still, the name is cut — a session title punching through the box would collapse the screen.
    #[test]
    fn a_very_long_name_is_still_cut_to_fit() {
        let (label, _) = split(20, &"가".repeat(40), None, false);
        assert!(display_width(&label) <= 18, "{} columns: {label}", display_width(&label));
        assert!(label.ends_with('…'), "no marker saying it was cut: {label}");
    }

    /// **A note too long for its row is left off, not cut.** `…` keeps the beginning of a sentence
    /// and throws the end away — and on a description the end is the part that says what it does.
    #[test]
    fn a_note_too_long_for_the_row_is_left_off_rather_than_cut() {
        let long = "에이전트를 고릅니다. 다음 메시지에서 새 쓰레드가 열립니다";
        let (label, note) = split(62, "/agent", Some(long), false);
        assert_eq!(label, "/agent");
        assert!(note.is_none(), "a half-sentence was drawn: {note:?}");
    }

    /// Every row of the screen, wide characters not double-counted.
    fn screen(picker: &mut Picker, w: u16, h: u16) -> Vec<String> {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal.draw(|f| draw(f, f.area(), picker, crate::lang::Lang::Ko, 0)).expect("draw");
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0u16;
                while x < w {
                    let symbol = buf[(x, y)].symbol();
                    row.push_str(symbol);
                    x += display_width(symbol).max(1) as u16;
                }
                row
            })
            .collect()
    }

    /// **The box grows with the window.** A fixed 64 cut `/github`'s description with an `…` on a
    /// screen three times that wide. The box is now as wide as its widest row, so the whole note
    /// sits beside its command.
    #[test]
    fn a_wider_terminal_shows_a_command_note_whole() {
        let mut picker = Picker::commands(crate::lang::Lang::Ko, &[]);
        let rows = screen(&mut picker, 120, 24);
        assert!(!rows.iter().any(|r| r.contains('…')), "{rows:#?}");
        assert!(
            rows.iter().any(|r| r.contains("/github") && r.contains("login reviewer")),
            "the note was not beside its command:\n{}",
            rows.join("\n")
        );
    }

    /// **A note that does not fit goes under the list, whole, for the row the cursor is on.**
    /// Knowing what a command does before typing it is the whole point of the list.
    #[test]
    fn a_note_that_does_not_fit_goes_under_the_list_in_full() {
        let mut picker = Picker::commands(crate::lang::Lang::Ko, &[]);
        let at = picker.rows.iter().position(|r| r.label == "/agent").expect("no /agent row");
        picker.cursor = at;
        let rows = screen(&mut picker, 60, 24);
        let joined = rows.join("\n");
        assert!(!joined.contains('…'), "{joined}");
        // The note is wider than the box, so it is here on two lines — and whole.
        assert!(joined.contains("에이전트를 고릅니다"), "{joined}");
        assert!(joined.contains("열립니다"), "the end of the note was lost: {joined}");
    }

    /// The box the widget drew: its top and bottom row, and its left and right column.
    fn box_rect(screen: &[String]) -> (usize, usize, usize, usize) {
        let top = screen.iter().position(|r| r.contains('┌')).expect("no box was drawn");
        let bottom = screen.iter().rposition(|r| r.contains('└')).expect("no box was drawn");
        let left = screen[top].find('┌').expect("no left corner");
        let right = screen[top].rfind('┐').expect("no right corner");
        (top, bottom, left, right)
    }

    /// **The list keeps its height as the cursor walks it.** The note under the list is one line
    /// for one row and two for the next, and the box used to follow it up and down — so the rows
    /// the keys were moving through slid under them.
    #[test]
    fn the_picker_box_is_one_height_wherever_the_cursor_is() {
        let mut picker = Picker::commands(crate::lang::Lang::Ko, &[]);
        let mut rects = Vec::new();
        for at in 0..picker.rows.len() {
            picker.cursor = at;
            rects.push(box_rect(&screen(&mut picker, 60, 30)));
        }
        assert!(rects.windows(2).all(|w| w[0] == w[1]), "the box moved: {rects:?}");
    }

    /// When both fit, both show.
    #[test]
    fn both_fit_when_there_is_room() {
        let (label, note) = split(40, "/cwd", Some("도구가 도는 자리"), false);
        assert_eq!(label, "/cwd");
        assert_eq!(note.as_deref(), Some("도구가 도는 자리"));
    }
}
