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
    // How far into the blink we are, in milliseconds — see `activity::blink_on`.
    blink_ms: u64,
) {
    // The centered box. Sized to the list, but never taller than the screen.
    // The two separator lines take a row each, so they must be counted for everything to fit.
    let rule = picker.is_create(0) && picker.rows.len() > 1;
    // **The box is as wide as its widest row, capped by the screen.** A fixed 64 cut a command's
    // name with an `…` on a screen three times that wide. The rows carry names only — every
    // description lives under the list (`detail_of`), so a long one no longer stretches the box.
    let widest = picker.rows.iter().map(row_need).max().unwrap_or(0);
    let w = ((widest as u16 + 4).max(64)).min(area.width.saturating_sub(4)).max(20);
    let inner_w = w.saturating_sub(2) as usize;
    // **The note area keeps room for every row's note, not only this one's.** Counted over all
    // rows, so the box is one height whichever row the cursor is on — a list whose box jumps up
    // and down as `↑↓` walks it is the same fault the panel's foot exists to prevent.
    //
    // **Held to one line until it is asked for.** The reserve is the *longest* note in the list, so
    // a list carrying one paragraph pays for it under every other row: on the `/` list two notes
    // wrap onto a second line at this width and all nineteen rows carried two (measured 2026-09-14).
    // One line still says what the row is, and `Tab` opens the whole of it (`Picker::expanded`) —
    // which is the shape this box had all along.
    let shows_note = picker.rows.iter().any(has_note);
    let detail_rows = if picker.expanded {
        picker
            .rows
            .iter()
            .filter_map(|row| detail_of(row, inner_w))
            .map(|note| note.len())
            .max()
            .unwrap_or(0)
    } else {
        // **One row, not the cursor's own count.** A height that followed the cursor is the very
        // thing this area is built to avoid; this way every row of the list gets the same box.
        shows_note as usize
    };
    // **A long note must not squeeze the list out of the box.** A project description is arbitrary
    // text and a paragraph of it is normal, while the note area comes out of the same box as the
    // rows, the rule and the key hints. It is capped at what the screen can spare once one list row
    // and the hints have their own — the cursor must never be what gets cut off. Only a note longer
    // than the screen bites this; an ordinary one is drawn whole.
    let detail_rows = detail_rows.min(area.height.saturating_sub(7) as usize);
    let mut detail = cursor_detail(picker, inner_w);
    // **What is left out is marked.** `…` is this app's word for "there is more", and with the note
    // held to a line it is also the only sign that `Tab` has something to open. Unmarked, a
    // description would lose its own ending — the fault this area was reshaped to fix.
    let cut = detail.len() > detail_rows;
    detail.truncate(detail_rows);
    if cut {
        if let Some(last) = detail.last_mut() {
            *last = mark_more(last, inner_w.saturating_sub(2));
        }
    }
    let want_h =
        (picker.rows.len() as u16).saturating_add(5 + rule as u16 + detail_rows as u16).max(6);
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
                row_line(&picker.rows[i], i == picker.cursor, picker.is_create(i), width, blink_ms)
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

    // **The note in full, for the row the cursor is on.** Every note is down here, short or long:
    // a list whose descriptions sat beside short rows and under long ones changed shape as the
    // eye walked it, and where a description is belongs to the window, not to its length.
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
            // **`Tab` is only promised where it does something.** A list whose rows carry no note
            // draws no note area, and a key that does nothing reads as broken — the same reason a
            // list that was not cut shows no overflow mark.
            lang.picker_keys(back, shows_note),
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
    blink_ms: u64,
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
            ThreadStatus::Running if crate::widgets::activity::blink_on(blink_ms) => {
                theme::accent()
            }
            ThreadStatus::Running => theme::text_muted(),
            ThreadStatus::Success => theme::success(),
            ThreadStatus::Failed => theme::danger(),
        };
        Span::styled("●", Style::default().fg(colour))
    });
    let label = label_to_fit(width, &row.label, row.status.is_some());
    let mut spans =
        vec![Span::styled(if on { "❯ " } else { "  " }, Style::default().fg(theme::accent()))];
    // The status dot sits at the left, right after the cursor marker — a thread's state is
    // the first thing the eye lands on, before reading its title.
    if let Some(dot) = status_span {
        spans.push(dot);
        spans.push(Span::styled(" ", Style::default().fg(theme::text_muted())));
    }
    // **The name, and nothing else.** The note is under the list (`cursor_detail`), so the row
    // ends where the name does whatever the length of that name's sentence.
    spans.push(Span::styled(label, Style::default().fg(fg)));
    Line::from(spans)
}

/// How wide a row wants to be: the caret, the status dot and the name.
///
/// **The note is not counted**, because no row draws one any more — it is under the list.
fn row_need(row: &crate::picker::Row) -> usize {
    2 + (row.status.is_some() as usize) * 2 + display_width(&row.label)
}

/// The note `row` puts under the list — its description, wrapped to the box, or `None` when the
/// row has none.
///
/// **Every note goes here, and it goes here for every row, not only the cursor's.** A note short
/// enough to sit beside its name used to stay on the row, so the list had descriptions in two
/// places at once and a row's shape depended on how long its sentence happened to be. One place
/// is the point; `/mode` puts its sentences under the list for the same reason.
///
/// The caller counts these over every row, which is how the note area comes to have one size
/// whatever the cursor is on.
fn detail_of(row: &crate::picker::Row, width: usize) -> Option<Vec<String>> {
    if !has_note(row) {
        return None;
    }
    let note = row.note.as_deref().unwrap_or_default();
    // Two columns of indent, so it reads as a note about the row above rather than another row.
    Some(crate::wrap::words(note, width.saturating_sub(2)))
}

/// Whether this row has a note to put under the list.
///
/// **A note of spaces is no note.** It would otherwise keep a row of the box for something that
/// draws nothing, and the hint would promise `Tab` for it.
fn has_note(row: &crate::picker::Row) -> bool {
    row.note.as_deref().is_some_and(|note| !note.trim().is_empty())
}

/// `line` with a `…` on the end, cut to `limit` columns so that the mark fits inside the box.
///
/// **Unconditional, unlike [`truncate`].** These lines came out of the wrapper already fitting the
/// box, so `truncate` would hand one back untouched and say nothing about what the note lost.
fn mark_more(line: &str, limit: usize) -> String {
    let limit = limit.max(1);
    let mut out = String::new();
    for ch in line.chars() {
        if display_width(&out) + display_width(&ch.to_string()) > limit.saturating_sub(1) {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// The note under the list: the cursor's row's, wrapped to the box — empty when it has none.
///
/// Only the cursor's is drawn, and that is what keeps the note area a fixed number of rows
/// (`detail_of` is what sizes it) while the keys move: every row's sentence would be a wall.
fn cursor_detail(picker: &Picker, width: usize) -> Vec<String> {
    picker.rows.get(picker.cursor).and_then(|row| detail_of(row, width)).unwrap_or_default()
}

/// The name, cut to fit the row.
///
/// **A name is never left to run long.** Session titles have arbitrary lengths, and left alone
/// they'd punch through the box and collapse the screen.
fn label_to_fit(width: usize, label: &str, status: bool) -> String {
    // The status dot and its trailing space take two columns on the left, before the label.
    let dot = if status { 2 } else { 0 };
    truncate(label, width.saturating_sub(2 + dot))
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
    /// tell what command it was — the reason was a long note. The note is not on the row any more,
    /// and this is the rule that keeps it from coming back.
    #[test]
    fn a_name_is_never_cut_by_a_note() {
        let row = crate::picker::Row {
            id: Some("c1".into()),
            label: "/agent".into(),
            note: Some("에이전트를 고릅니다. 다음 메시지에서 새 thread가 열립니다".into()),
            enabled: true,
            status: None,
        };
        let line = row_line(&row, false, false, 62, 0);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "  /agent", "the row carries more than the name: {text:?}");
    }

    /// **Every note is under the list, however short it is.** A description beside its name only
    /// when it happened to fit left the list with two shapes at once.
    #[test]
    fn a_note_that_would_fit_beside_its_name_still_goes_under_the_list() {
        let row = crate::picker::Row {
            id: Some("c1".into()),
            label: "/cwd".into(),
            note: Some("도구가 도는 자리".into()),
            enabled: true,
            status: None,
        };
        let text: String =
            row_line(&row, false, false, 62, 0).spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains("도구가"), "the note is still on the row: {text:?}");
        let under = detail_of(&row, 62).expect("the note is nowhere");
        assert_eq!(under.concat(), "도구가 도는 자리");
    }

    /// A row with no note puts nothing under the list — the area belongs to the rows that have
    /// something to say.
    #[test]
    fn a_row_without_a_note_puts_nothing_under_the_list() {
        let row = crate::picker::Row {
            id: Some("c1".into()),
            label: "/clear".into(),
            note: None,
            enabled: true,
            status: None,
        };
        assert!(detail_of(&row, 62).is_none());
    }

    /// **A project's description reaches the screen, under the list.** That is the whole point of
    /// the note area for this list: the row is a bare name, so a project you can't place by name
    /// had nothing to read. Reported from the app on 2026-09-13.
    #[test]
    fn a_projects_description_is_drawn_under_the_list() {
        let mut picker = Picker::projects(
            vec![
                ("p1".into(), "기본".into(), Some("계정의 기본 프로젝트".into()), true),
                ("p2".into(), "zyris".into(), Some("zyris 코드 개발".into()), false),
            ],
            crate::lang::Lang::Ko,
        );
        picker.cursor = 2;
        let rows = screen(&mut picker, 60, 20);
        let joined = rows.join("\n");
        assert!(joined.contains("zyris 코드 개발"), "{joined}");
        // The row under the cursor is still a bare name — the description is a line of its own.
        let cursor_row = rows.iter().find(|r| r.contains('❯')).expect("no cursor row");
        assert!(
            !cursor_row.contains("코드 개발"),
            "the description rode along on the row: {cursor_row:?}"
        );
        // And the default marker is on screen when that row is the cursor's.
        picker.cursor = 1;
        let joined = screen(&mut picker, 60, 20).join("\n");
        assert!(joined.contains("기본 ∙ 계정의 기본 프로젝트"), "{joined}");
    }

    /// **A description longer than the screen is cut, not allowed to take the list with it.** The
    /// note area is drawn out of the same box as the rows: left unbounded, a paragraph of project
    /// description would push the list and the key hints off the bottom, and the cursor with them.
    #[test]
    fn a_note_longer_than_the_screen_does_not_push_the_list_out_of_the_box() {
        let long = "아주 긴 설명 ".repeat(40);
        let mut picker = Picker::projects(
            (0..8)
                .map(|i| (format!("p{i}"), format!("프로젝트 {i}"), Some(long.clone()), false))
                .collect(),
            crate::lang::Lang::Ko,
        );
        picker.cursor = 8;
        let rows = screen(&mut picker, 80, 20);
        let joined = rows.join("\n");
        assert!(joined.contains("프로젝트 7"), "the cursor row is off screen:\n{joined}");
        assert!(
            rows.iter().any(|r| r.contains("이동") || r.contains("Enter")),
            "the key hints were pushed out:\n{joined}"
        );
        assert!(rows.iter().all(|r| r.matches('│').count() == 2 || !r.contains('│')), "{joined}");
    }

    /// Still, the name is cut — a session title punching through the box would collapse the screen.
    #[test]
    fn a_very_long_name_is_still_cut_to_fit() {
        let label = label_to_fit(20, &"가".repeat(40), false);
        assert!(display_width(&label) <= 18, "{} columns: {label}", display_width(&label));
        assert!(label.ends_with('…'), "no marker saying it was cut: {label}");
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
        let line = row_line(&row, false, false, 20, crate::widgets::activity::BLINK_HALF_MS);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("●"), "no status dot: {text:?}");
        // The dot sits before the title — the cursor marker, then the dot, then the label.
        let dot_pos = line.spans.iter().position(|s| s.content.as_ref() == "●").unwrap();
        let label_pos = line.spans.iter().position(|s| s.content.as_ref() == "대화").unwrap();
        assert!(dot_pos < label_pos, "status dot must precede the title: {text:?}");
        // A finished thread holds its colour, a running one blinks on and off.
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

    fn dot(status: crate::picker::ThreadStatus, blink_ms: u64) -> Line<'static> {
        let row = crate::picker::Row {
            id: Some("s1".into()),
            label: "대화".into(),
            note: None,
            enabled: true,
            status: Some(status),
        };
        row_line(&row, false, false, 20, blink_ms)
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
        let colour = |status, blink_ms| {
            dot(status, blink_ms)
                .spans
                .iter()
                .find(|s| s.content.as_ref() == "●")
                .and_then(|s| s.style.fg)
                .expect("no dot was drawn")
        };
        // 0ms and the half-period are the two halves of the blink.
        let seen = [
            colour(ThreadStatus::Unknown, 0),
            colour(ThreadStatus::Running, 0),
            colour(ThreadStatus::Running, crate::widgets::activity::BLINK_HALF_MS),
            colour(ThreadStatus::Success, 0),
            colour(ThreadStatus::Failed, 0),
        ];
        for (i, a) in seen.iter().enumerate() {
            for b in &seen[i + 1..] {
                assert_ne!(a, b, "two dots wear the same colour: {seen:?}");
            }
        }
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

    /// **The list is one shape from top to bottom.** No row carries a note, so the cursor walking
    /// the list never changes what a row looks like.
    #[test]
    fn no_row_carries_its_note_beside_the_name() {
        let picker = Picker::commands(crate::lang::Lang::Ko, &[]);
        for row in &picker.rows {
            let text: String = row_line(row, false, false, 62, 0)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert_eq!(text.trim(), row.label, "a note rode along on {:?}: {text:?}", row.label);
        }
    }

    /// **A note that does not fit goes under the list, whole, for the row the cursor is on.**
    /// Knowing what a command does before typing it is the whole point of the list.
    #[test]
    fn a_note_that_does_not_fit_goes_under_the_list_in_full() {
        let mut picker = Picker::commands(crate::lang::Lang::Ko, &[]);
        let at = picker.rows.iter().position(|r| r.label == "/agent").expect("no /agent row");
        picker.cursor = at;
        // **Opened.** Held to a line by default, so the whole of it takes `Tab` (`Picker::expanded`).
        picker.expanded = true;
        let rows = screen(&mut picker, 60, 24);
        let joined = rows.join("\n");
        assert!(!joined.contains('…'), "{joined}");
        // The note is wider than the box, so it is here on two lines — and whole.
        assert!(joined.contains("에이전트를 고릅니다"), "{joined}");
        assert!(joined.contains("열립니다"), "the end of the note was lost: {joined}");
    }

    /// **Every row's note, whichever one is cursor'd, is under the list.** The box was wide enough
    /// for `/github`'s description on a 120-column screen, and it used to be drawn beside the
    /// command there while sitting under it on a narrow one — the same list, two shapes.
    #[test]
    fn a_wide_terminal_still_puts_the_note_under_the_list() {
        let mut picker = Picker::commands(crate::lang::Lang::Ko, &[]);
        let at = picker.rows.iter().position(|r| r.label == "/github").expect("no /github row");
        picker.cursor = at;
        let rows = screen(&mut picker, 120, 24);
        assert!(
            !rows.iter().any(|r| r.contains("/github") && r.contains("login")),
            "the note is still beside the command:\n{}",
            rows.join("\n")
        );
        assert!(
            rows.iter().any(|r| r.contains("login reviewer")),
            "the note is not under the list:\n{}",
            rows.join("\n")
        );
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

    /// **The note is held to a line until it is asked for.** The reserve used to be the longest note
    /// in the list, so one paragraph of description cost every other row a line of box — on the `/`
    /// list two notes wrap onto a second line and all nineteen rows carried two (measured
    /// 2026-09-14). One line, marked `…`; `Picker::expanded` (the `Tab` key) is what shows the rest.
    #[test]
    fn a_long_note_is_held_to_one_line_until_it_is_opened() {
        let long = "이 프로젝트는 TUI를 다룹니다. 그리고 마지막에만 나오는 표식 ZZZ";
        let mut picker = Picker::projects(
            vec![("p1".into(), "zyris".into(), Some(long.into()), false)],
            crate::lang::Lang::Ko,
        );
        let held = screen(&mut picker, 80, 24).join("\n");
        assert!(held.contains('…'), "nothing says the note was cut:\n{held}");
        assert!(!held.contains("ZZZ"), "the whole note was drawn anyway:\n{held}");
        picker.expanded = true;
        let opened = screen(&mut picker, 80, 24).join("\n");
        assert!(opened.contains("ZZZ"), "the rest of the note is nowhere:\n{opened}");
        // And the box is still one height for the list, so opening it does not move the rows.
        let rects = [false, true].map(|expanded| {
            picker.expanded = expanded;
            box_rect(&screen(&mut picker, 80, 24))
        });
        assert_eq!(rects[0].0, rects[1].0, "opening the note moved the top edge: {rects:?}");
    }

    /// **`Tab` is promised only where there is something to open.** A list whose rows carry no note
    /// keeps no row of the box for one, and its hint must not offer a key that does nothing — the
    /// same reason a list that was not cut shows no overflow mark.
    #[test]
    fn the_hint_promises_tab_only_for_a_list_that_carries_a_note() {
        let mut commands = Picker::commands(crate::lang::Lang::Ko, &[]);
        assert!(
            screen(&mut commands, 80, 24).join("\n").contains("Tab 설명"),
            "the list has notes but says nothing about the key"
        );
        let mut sessions = Picker::sessions(
            "p1".into(),
            "zyris".into(),
            vec![("s1".into(), "지난 대화".into(), crate::picker::ThreadStatus::Unknown)],
            crate::lang::Lang::Ko,
        );
        let joined = screen(&mut sessions, 80, 24).join("\n");
        assert!(joined.contains("Enter 고르기"), "no hint at all:\n{joined}");
        assert!(!joined.contains("Tab"), "a key that does nothing was promised:\n{joined}");
    }
}
