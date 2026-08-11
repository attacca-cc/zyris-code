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
    let want_h = (picker.rows.len() as u16).saturating_add(5 + rule as u16).max(6);
    let h = want_h.min(area.height.saturating_sub(2)).max(3);
    let w = 64.min(area.width.saturating_sub(4)).max(20);
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
        lines
            .push(Line::from(Span::styled(lang.loading(), Style::default().fg(theme::text_muted()))));
    }

    // **The pure side decides** where each row goes (`picker::slots`). Here we just draw.
    // The rule and the key hints at the foot take a row each.
    let width = inner.width as usize;
    let body_h = inner.height.saturating_sub(2) as usize;
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

    // The meaning of ← changes with the level. Say it plainly.
    let back = match picker.level {
        crate::picker::Level::Projects => lang.picker_close(),
        crate::picker::Level::Sessions { .. } => lang.picker_back(),
        // These two are outside the project hierarchy, so there's nowhere to go back to.
        crate::picker::Level::Agents | crate::picker::Level::Commands => lang.picker_esc_close(),
    };
    lines.push(Line::from(Span::styled(
        lang.picker_keys(back),
        Style::default().fg(theme::text_muted()),
    )));

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
    let mut spans = vec![
        Span::styled(if on { "❯ " } else { "  " }, Style::default().fg(theme::accent())),
    ];
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
    if room < NOTE_MIN {
        return (label, None);
    }
    (label, Some(truncate(note, room)))
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
        terminal
            .draw(|f| draw(f, f.area(), &mut picker, crate::lang::Lang::Ko, 0))
            .expect("draw");
        // The box is centred, so its top border is not row zero — find the row it landed on.
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = buffer
            .content
            .chunks(80)
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect();
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
        terminal
            .draw(|f| draw(f, f.area(), picker, crate::lang::Lang::Ko, 0))
            .expect("draw");
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
        let mut picker =
            Picker::sessions("p1".into(), "기본".into(), items, crate::lang::Lang::Ko);
        picker.cursor = 40;
        let rows = render(&mut picker, 80, 20);
        let head = rows.iter().position(|r| r.contains("＋ 새 쓰레드")).expect(
            "the create row is gone from a scrolled list",
        );
        // It is the first thing under the top border, above everything that scrolls.
        assert!(head <= 1, "the create row is not at the top: {rows:#?}");
        assert!(
            rows.iter().any(|r| r.contains("↑") || r.contains("더")),
            "nothing says how many are hidden above: {rows:#?}"
        );
        assert!(rows.iter().any(|r| r.contains("지난 대화 39")), "the cursor row is off screen: {rows:#?}");
        // The box keeps its walls — a row that runs past the border collapses the screen.
        assert!(rows.iter().all(|r| r.matches('│').count() == 2 || !r.contains('│')), "{rows:#?}");
    }

    /// **The name never gets shaved.** `/agent` actually got truncated to `/a…` and you couldn't
    /// tell what command it was — the reason was a long note.
    #[test]
    fn a_long_note_never_eats_into_the_name() {
        let (label, _) =
            split(62, "/agent", Some("에이전트를 고릅니다. 다음 메시지에서 새 thread가 열립니다"), false);
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
        let label_pos =
            line.spans.iter().position(|s| s.content.as_ref() == "대화").unwrap();
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
        let running_col = line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "●")
            .and_then(|s| s.style.fg);
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

    /// When both fit, both show.
    #[test]
    fn both_fit_when_there_is_room() {
        let (label, note) = split(40, "/cwd", Some("도구가 도는 자리"), false);
        assert_eq!(label, "/cwd");
        assert_eq!(note.as_deref(), Some("도구가 도는 자리"));
    }
}
