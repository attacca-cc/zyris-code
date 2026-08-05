//! Dump the screen as text and assert on it. Cell assertions miss some things, but without checking coordinates,
//! just looking at "what is on which line" catches layout regressions.

use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::Terminal;
use zyris_code::app::{apply, Action, Frame as AppFrame, State};
use zyris_code::event::{Entry, EntryKind};
use zyris_code::markdown::display_width;
use zyris_code::widgets;

/// A write sink that counts the bytes that went out. Same trick as `perf.rs` — hook the real crossterm backend to a memory
/// buffer and see **whether a wide trailing cell actually goes out on the wire**. Looking only at the cell buffer
/// won't show the cells the diff skipped.
#[derive(Clone, Default)]
struct Wire(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Wire {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Wire {
    fn take(&self) -> Vec<u8> {
        let mut b = self.0.lock().unwrap();
        std::mem::take(&mut *b)
    }
}

/// Strips escape sequences, leaving only **what was actually written to the wire** —
/// whether the trailing cell was cleared with a space shows up here.
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Dumps the screen as text.
///
/// **Wide characters occupy two cells, and ratatui fills the second one with a blank.** If the cells were
/// just concatenated, a wide character would split across cells and no string could be found — when a cell
/// of width 2 is met, the next cell is skipped.
fn dump(state: &mut State, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| widgets::draw(f, state)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| {
            let mut line = String::new();
            let mut x = 0;
            while x < buf.area.width {
                let symbol = buf[(x, y)].symbol();
                line.push_str(symbol);
                x += display_width(symbol).max(1) as u16;
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The input box is the third line from the bottom: bottom bar · **divider** · input box.
///
/// The line between the bottom bar (mode·agent) and the input box is not decoration. Left blank, the mode marker
/// reads like the tail of the input box, and "what mode am I in" doesn't register.
#[test]
fn a_rule_separates_the_input_box_from_the_bottom_bar() {
    let mut s = State::new();
    apply(&mut s, &Action::Insert('안'));
    let screen = dump(&mut s, 40, 10);
    let lines: Vec<&str> = screen.lines().collect();
    assert!(lines[lines.len() - 3].contains('안'), "입력이 그 자리에 없다:\n{screen}");
    assert!(lines[lines.len() - 2].starts_with('─'), "가름선이 없다:\n{screen}");
    // The default screen language is English (the `lang::Lang` default). We look at the mode name in the bottom bar.
    assert!(lines[lines.len() - 1].contains("normal"), "맨 아래가 하단 바가 아니다:\n{screen}");
}

/// Long input wraps to the next line. If it were cut off, you couldn't tell what you're typing.
#[test]
fn a_long_input_wraps_instead_of_being_cut_off() {
    let mut s = State::new();
    for c in "가나다라마바사아자차카타파하".chars() {
        apply(&mut s, &Action::Insert(c));
    }
    let screen = dump(&mut s, 20, 14);
    let lines: Vec<&str> = screen.lines().collect();
    // The input box grows upward from above the bottom bar.
    let tail = lines[lines.len() - 4..].join("\n");
    assert!(tail.contains("파하"), "뒷글자가 잘렸다:\n{screen}");
    assert!(
        lines.iter().filter(|l| l.contains('가') || l.contains('파')).count() >= 1,
        "\n{screen}"
    );
}

#[test]
fn a_user_message_appears_above_the_input() {
    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry { seq: 1, kind: EntryKind::User("안녕하세요".into()) }),
        }),
    );
    let screen = dump(&mut s, 40, 10);
    assert!(screen.contains("안녕하세요"), "\n{screen}");
}

/// A cell's background colour. Pairs with `cell_fg`.
fn cell_bg(state: &mut State, w: u16, h: u16, x: u16, y: u16) -> Option<ratatui::style::Color> {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| widgets::draw(f, state)).unwrap();
    term.backend().buffer()[(x, y)].style().bg
}

fn said(state: &mut State, seq: i64, kind: EntryKind) {
    apply(state, &Action::Frame(AppFrame::Event { cursor: seq, entry: Some(Entry { seq, kind }) }));
}

/// **Where the user spoke is painted with a background.** And it must run to the right edge even after the
/// text ends — if it broke at the text width, it would look like a smudge rather than a band.
#[test]
fn a_user_message_gets_a_band_across_the_full_width() {
    let mut s = State::new();
    s.sidebar_on = false;
    said(&mut s, 1, EntryKind::User("안녕".into()));
    assert_eq!(
        cell_bg(&mut s, 60, 12, 1, 0),
        Some(zyris_code::theme::USER_BG),
        "글자 자리가 안 칠해졌다"
    );
    assert_eq!(
        cell_bg(&mut s, 60, 12, 59, 0),
        Some(zyris_code::theme::USER_BG),
        "끝까지 안 이어진다"
    );
}

/// Answers have no background. Painting everything would make nothing distinguishable.
#[test]
fn an_agent_answer_has_no_band() {
    let mut s = State::new();
    s.sidebar_on = false;
    said(&mut s, 1, EntryKind::Agent("그렇습니다".into()));
    assert_ne!(cell_bg(&mut s, 60, 12, 3, 0), Some(zyris_code::theme::USER_BG));
}

/// **By default, the page background is not painted.** The terminal is left to use its own background.
///
/// If the app painted it, only the area outside the grid (window padding, leftover pixels the grid doesn't
/// fit) would change colour, creating a stripe at the screen edge — a spot the app can't touch, so there is
/// no way to fix it by painting. Using the background the person chose is better everywhere (`theme::page_bg`).
#[test]
fn nothing_but_the_user_band_paints_a_background_by_default() {
    let mut s = State::new();
    s.sidebar_on = false;
    said(&mut s, 1, EntryKind::Agent("안녕하세요".into()));
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    let frame = term.draw(|f| widgets::draw(f, &mut s)).unwrap();
    for cell in frame.buffer.content.iter() {
        assert!(
            cell.bg == Color::Reset || cell.bg == zyris_code::theme::USER_BG,
            "터미널 배경을 덮었다: symbol={:?} bg={:?}",
            cell.symbol(),
            cell.bg
        );
    }
}

/// **It must be possible to turn it off.** Some remote terminals genuinely suffer from wide-char ghosting — that's
/// what `$ZYRIS_CODE_BG` is for. The decision is pure, so we test without touching the environment variable.
#[test]
fn the_page_background_can_be_asked_for_by_name_or_by_hex() {
    use zyris_code::theme::page_bg_from;
    assert_eq!(page_bg_from(None), None, "안 주면 안 칠한다");
    assert_eq!(page_bg_from(Some("")), None, "빈 값도 안 준 것이다");
    assert_eq!(page_bg_from(Some("zyris")), Some(zyris_code::theme::BG));
    assert_eq!(page_bg_from(Some("#101820")), Some(Color::Rgb(0x10, 0x18, 0x20)));
    assert_eq!(page_bg_from(Some("none")), None, "끄는 쪽도 명시할 수 있다");
    // **A single typo must not kill the app.** If it can't be read, it falls back to not painting.
    assert_eq!(page_bg_from(Some("보라색")), None);
}

/// **The self-healing frame forces every cell to be re-emitted.** Instead of clearing, it overwrites, so the
/// ghost of the trailing cell behind a wide character is wiped — no clear, so nothing flickers. Even if the diff
/// is identical, to reach the wire the cell must be planted with `AlwaysUpdate`, and the flag clears after one frame.
#[test]
fn a_heal_frame_forces_every_cell_to_be_resent() {
    let mut s = State::new();
    s.sidebar_on = false;
    said(&mut s, 1, EntryKind::Agent("안녕하세요".into()));
    s.force_update = true;

    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    let frame = term.draw(|f| widgets::draw(f, &mut s)).unwrap();
    assert!(!s.force_update, "강제 플래그는 한 프레임이면 풀려야 한다");
    assert!(
        frame
            .buffer
            .content
            .iter()
            .all(|c| { c.diff_option == ratatui::buffer::CellDiffOption::AlwaysUpdate }),
        "모든 칸이 강제 재출력이어야 한다"
    );

    // The next draw goes back to a normal diff — the flag was cleared, so no AlwaysUpdate remains.
    let frame2 = term.draw(|f| widgets::draw(f, &mut s)).unwrap();
    assert!(
        frame2
            .buffer
            .content
            .iter()
            .all(|c| { c.diff_option == ratatui::buffer::CellDiffOption::None }),
        "한 프레임 뒤에는 일반 diff로 돌아와야 한다"
    );
}

/// **With a background, the trailing cell goes out on the wire when a wide char becomes a narrow one.**
/// A single space must actually be written after 'a' — that space erases the wide char's right half.
/// (The cursor already sits there, so only the space goes out, no cursor movement.)
#[test]
fn a_wide_char_with_a_background_emits_its_trailing_cell_when_replaced() {
    let area = Rect::new(0, 0, 6, 1);
    let wire = Wire::default();
    let mut backend = CrosstermBackend::new(wire.clone());

    let mut prev = Buffer::empty(area);
    prev.set_string(2, 0, "한", Style::default().bg(zyris_code::theme::BG));
    let mut next = Buffer::empty(area);
    next.set_string(2, 0, "a", Style::default().bg(zyris_code::theme::BG));

    wire.take();
    backend.draw(prev.diff_iter(&next)).unwrap();
    let out = String::from_utf8_lossy(&wire.take()).into_owned();
    assert_eq!(strip_ansi(&out), "a ", "trailing 칸이 지워지지 않았다: {out:?}");
}

/// **Without a background, the trailing cell doesn't go out on the wire — the very cause of ghosting.**
/// This test doesn't pin the current behavior; rather, it documents that before `theme::BG` was laid on every cell,
/// this cell was never erased when a wide char narrowed.
#[test]
fn a_wide_char_without_a_background_skips_its_trailing_cell() {
    let area = Rect::new(0, 0, 6, 1);
    let wire = Wire::default();
    let mut backend = CrosstermBackend::new(wire.clone());

    let mut prev = Buffer::empty(area);
    prev.set_string(2, 0, "한", Style::default());
    let mut next = Buffer::empty(area);
    next.set_string(2, 0, "a", Style::default());

    wire.take();
    backend.draw(prev.diff_iter(&next)).unwrap();
    let out = String::from_utf8_lossy(&wire.take()).into_owned();
    assert_eq!(strip_ansi(&out), "a", "배경 없이 trailing 칸이 나가면 안 된다(예전 동작): {out:?}");
}

/// There is no header. The conversation starts on the very first line — no reason to give a line to the app name and directory.
#[test]
fn there_is_no_header_taking_up_the_top_line() {
    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry { seq: 1, kind: EntryKind::User("첫 줄".into()) }),
        }),
    );
    let screen = dump(&mut s, 40, 10);
    let top = screen.lines().next().unwrap();
    assert!(!top.contains("zyris-code"), "머리글이 남아 있다:\n{screen}");
    assert!(top.contains("첫 줄"), "맨 윗줄이 대화가 아니다:\n{screen}");
}

/// If drawing panics, the terminal is left broken. It happens especially at narrow widths.
#[test]
fn drawing_at_a_very_narrow_width_does_not_panic() {
    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry {
                seq: 1, kind: EntryKind::Agent("한글 **강조** `코드`".into())
            }),
        }),
    );
    let _ = dump(&mut s, 12, 6);
}

/// **"Connected" only shows briefly.** It appears once in the status line right after connecting (`Connected`)
/// and disappears on its own — there is no reason to keep announcing it. Normally, with no state, nothing shows.
#[test]
fn a_healthy_connection_is_not_announced_anywhere() {
    let mut s = State::new();
    s.connected = true;
    let screen = dump(&mut s, 40, 10);
    assert!(!screen.contains("연결됨"), "연결 표시가 남아 있다:\n{screen}");
}

/// A broken connection must always be said aloud — silent failure is the worst kind.
#[test]
fn a_broken_connection_is_always_said_out_loud() {
    let mut s = State::new();
    s.connected = false;
    let screen = dump(&mut s, 40, 10);
    assert!(screen.contains("Connecting"), "끊겼는데 아무 말이 없다:\n{screen}");

    // The Korean screen says it in the same place.
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.connected = false;
    let screen = dump(&mut s, 40, 10);
    assert!(screen.contains("연결 중"), "{screen}");
}

/// The status line is fifth from the bottom: bottom bar · divider · input box · divider · **status line**.
const ACTIVITY_FROM_BOTTOM: usize = 5;

/// What's happening now sits **between** the chat and the input box — a blank line above, the input box's divider below.
#[test]
fn what_is_happening_now_sits_between_the_chat_and_the_input_box() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.connected = true;
    let screen = dump(&mut s, 40, 12);
    let lines: Vec<&str> = screen.lines().collect();
    let at = lines.len() - ACTIVITY_FROM_BOTTOM;
    assert!(lines[at].contains("쉬는 중"), "상태 줄이 그 자리에 없다:\n{screen}");
    assert!(lines[at - 1].trim().is_empty(), "대화와 안 떨어졌다:\n{screen}");
    assert!(lines[at + 1].starts_with('─'), "바로 아래가 가름선이 아니다:\n{screen}");

    s.running = true;
    let screen = dump(&mut s, 40, 12);
    let lines: Vec<&str> = screen.lines().collect();
    assert!(
        lines[lines.len() - ACTIVITY_FROM_BOTTOM].contains("작업 중"),
        "작업 중이 안 뜬다:\n{screen}"
    );
}

/// The quit hint must show before anything else.
#[test]
fn the_quit_hint_wins_over_everything_else() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.connected = true;
    s.running = true;
    s.quit_armed_at = Some(std::time::Instant::now());
    let screen = dump(&mut s, 60, 12);
    assert!(screen.contains("한 번 더"), "종료 안내가 없다:\n{screen}");
    let lines: Vec<&str> = screen.lines().collect();
    assert!(
        !lines[lines.len() - ACTIVITY_FROM_BOTTOM].contains("작업 중"),
        "작업 중이 안내를 덮었다:\n{screen}"
    );
}

/// A single cell's foreground colour. Blinking appears as colour rather than text, so we check it this way.
fn cell_fg(state: &mut State, w: u16, h: u16, x: u16, y: u16) -> Option<ratatui::style::Color> {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| widgets::draw(f, state)).unwrap();
    term.backend().buffer()[(x, y)].style().fg
}

/// The dot blinks only while working. A still dot can't say anything is running, and
/// blinking while idle would make it look like something is going on.
#[test]
fn the_dot_blinks_only_while_working() {
    // The dot is the **leftmost cell** of the status line.
    const DOT_X: u16 = 0;
    const H: u16 = 12;
    let y = H - ACTIVITY_FROM_BOTTOM as u16;

    let mut s = State::new();
    s.connected = true;

    s.running = false;
    s.tick = 0;
    let idle_a = cell_fg(&mut s, 40, H, DOT_X, y);
    s.tick = 8;
    let idle_b = cell_fg(&mut s, 40, H, DOT_X, y);
    assert_eq!(idle_a, idle_b, "쉬는 중인데 점이 깜박인다");

    s.running = true;
    s.tick = 0;
    let on = cell_fg(&mut s, 40, H, DOT_X, y);
    s.tick = 8;
    let off = cell_fg(&mut s, 40, H, DOT_X, y);
    assert_ne!(on, off, "작업 중인데 점이 안 깜박인다");
}

/// The mode and agent sit at the left of the bottom bar, in a fixed spot.
#[test]
fn the_mode_and_agent_sit_at_the_left_of_the_bottom_bar() {
    let mut s = State::new();
    s.agent = "Main Agent".into();
    let screen = dump(&mut s, 40, 10);
    let bottom = screen.lines().last().unwrap();
    assert!(bottom.trim_start().starts_with("normal"), "모드가 맨 왼쪽이 아니다: {bottom:?}");
    assert!(bottom.contains("Main Agent"), "에이전트가 없다: {bottom:?}");
}

/// Shift+Tab cycles normal → plan → work → job → normal.
///
/// **If you add a mode without updating `Mode::next`, the new mode can never be reached by key** —
/// a mode you can only reach via `/mode` is used by nobody.
#[test]
fn shift_tab_cycles_the_mode() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use zyris_code::app::on_key;
    use zyris_code::mode::Mode;

    let mut s = State::new();
    let key = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    for expected in [Mode::Plan, Mode::Work, Mode::Job, Mode::Normal] {
        for a in on_key(&s, key) {
            apply(&mut s, &a);
        }
        assert_eq!(s.mode, expected);
    }
}

/// The bottom bar must say **in text which mode it is in**. That is the only way to tell the four apart by eye.
#[test]
fn the_status_bar_names_the_mode_it_is_in() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use zyris_code::app::on_key;

    let mut s = State::new();
    let key = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    // **Only look at the very bottom line.** Searching the whole screen could accidentally catch other text in the
    // sidebar or activity line, letting an empty bottom bar pass as green.
    for expected in ["plan", "work", "job", "normal"] {
        for a in on_key(&s, key) {
            apply(&mut s, &a);
        }
        let screen = dump(&mut s, 80, 24);
        let bar = screen.lines().last().unwrap_or_default().trim_start();
        assert!(bar.starts_with(expected), "하단 바가 '{expected}'로 시작하지 않는다: {bar:?}");
    }
}

/// Clicking a work card's header must fold and unfold it. If the coordinate transform is off, the wrong line gets hit.
#[test]
fn clicking_a_work_card_toggles_it() {
    use zyris_code::rows::Fold;

    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry { seq: 1, kind: EntryKind::WorkStart("작업".into()) }),
        }),
    );
    // It must be drawn once so the widget records coordinates and card positions.
    let _ = dump(&mut s, 60, 12);

    let (row, seq) = s.view_cards.iter().map(|(r, q)| (*r, *q)).next().expect("카드가 없다");
    assert_eq!(seq, 1);

    let (ox, oy) = s.view_origin;
    let y = oy + (row - s.view_top) as u16;
    apply(&mut s, &Action::Press(ox + 1, y));
    apply(&mut s, &Action::Release);

    assert_eq!(s.folds[&1], Fold { open: true }, "클릭으로 펴져야 한다");

    apply(&mut s, &Action::Press(ox + 1, y));
    apply(&mut s, &Action::Release);
    assert!(!s.folds[&1].open, "다시 클릭하면 접혀야 한다");
}

/// Dragging selects text, and the selection **survives the release** — the I/O layer exports it to the clipboard.
/// If it were cleared the moment you let go, there would be nothing to export.
#[test]
fn dragging_selects_text_and_the_selection_survives_the_release() {
    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry {
                seq: 1, kind: EntryKind::Agent("안녕하세요 반갑습니다".into())
            }),
        }),
    );
    let _ = dump(&mut s, 60, 12);

    let (ox, oy) = s.view_origin;
    apply(&mut s, &Action::Press(ox, oy));
    apply(&mut s, &Action::DragTo(ox + 10, oy));
    let selected = s.selection.clone().expect("선택이 안 잡혔다");
    assert!(selected.contains("안녕"), "{selected:?}");

    // **It stays after release.** Exporting to the clipboard is the I/O layer's job, so here we only
    // check that the range is still alive — if it disappeared, there would be nothing to export.
    apply(&mut s, &Action::Release);
    assert_eq!(s.selection.as_deref(), Some(selected.as_str()));
}

/// A press without movement is not a selection — if a click alone selected, copy would misfire.
#[test]
fn a_click_without_moving_does_not_select() {
    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry { seq: 1, kind: EntryKind::Agent("본문".into()) }),
        }),
    );
    let _ = dump(&mut s, 60, 12);

    let (ox, oy) = s.view_origin;
    apply(&mut s, &Action::Press(ox + 1, oy));
    apply(&mut s, &Action::DragTo(ox + 1, oy));
    assert!(s.selection.is_none());
}

/// The selection must survive releasing the mouse. If it vanished on release, there would be no time to press Ctrl+C.
#[test]
fn the_selection_survives_releasing_the_mouse() {
    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry {
                seq: 1, kind: EntryKind::Agent("안녕하세요 반갑습니다".into())
            }),
        }),
    );
    let _ = dump(&mut s, 60, 12);

    let (ox, oy) = s.view_origin;
    apply(&mut s, &Action::Press(ox, oy));
    apply(&mut s, &Action::DragTo(ox + 10, oy));
    apply(&mut s, &Action::Release);

    assert!(s.selection.is_some(), "떼고 나서 선택이 사라졌다");
    assert!(s.drag.is_some(), "반전 표시도 남아 있어야 한다");
    assert!(!s.dragging, "버튼은 뗀 상태여야 한다");
}

/// Moving the mouse after release must not grow the range.
#[test]
fn moving_after_release_does_not_grow_the_selection() {
    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry {
                seq: 1, kind: EntryKind::Agent("안녕하세요 반갑습니다".into())
            }),
        }),
    );
    let _ = dump(&mut s, 60, 12);

    let (ox, oy) = s.view_origin;
    apply(&mut s, &Action::Press(ox, oy));
    apply(&mut s, &Action::DragTo(ox + 6, oy));
    let before = s.selection.clone();
    apply(&mut s, &Action::Release);
    apply(&mut s, &Action::DragTo(ox + 20, oy));
    assert_eq!(s.selection, before, "뗀 뒤에는 안 자라야 한다");
}

/// Scrolling must not lose the selection — it is in content coordinates, so it should follow along.
#[test]
fn scrolling_keeps_the_selection() {
    let mut s = State::new();
    for i in 1..30 {
        apply(
            &mut s,
            &Action::Frame(AppFrame::Event {
                cursor: i,
                entry: Some(Entry {
                    seq: i, kind: EntryKind::Agent(format!("줄 {i} 내용입니다"))
                }),
            }),
        );
    }
    let _ = dump(&mut s, 60, 10);

    let (ox, oy) = s.view_origin;
    // Entries have blank lines between them, so you must drag across several rows to reliably grab text.
    apply(&mut s, &Action::Press(ox, oy));
    apply(&mut s, &Action::DragTo(ox + 6, oy + 2));
    apply(&mut s, &Action::Release);
    let before = s.selection.clone();
    assert!(before.is_some(), "선택이 안 잡혔다: rows={:?}", &s.rows_cache.plain()[s.view_top..]);

    apply(&mut s, &Action::Wheel(2));
    let _ = dump(&mut s, 60, 10);
    assert_eq!(s.selection, before, "휠을 굴렸다고 선택이 날아가면 안 된다");
}

/// The highlight must cover only the selected columns. If the whole row were reversed, what's selected and what's shown would differ.
#[test]
fn the_highlight_covers_only_the_selected_columns() {
    use ratatui::style::Modifier;

    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry { seq: 1, kind: EntryKind::Agent("abcdefghij".into()) }),
        }),
    );
    let _ = dump(&mut s, 60, 12);

    let (ox, oy) = s.view_origin;
    apply(&mut s, &Action::Press(ox, oy));
    apply(&mut s, &Action::DragTo(ox + 4, oy));

    let mut term = ratatui::Terminal::new(TestBackend::new(60, 12)).unwrap();
    term.draw(|f| widgets::draw(f, &mut s)).unwrap();
    let buf = term.backend().buffer().clone();

    let y = oy;
    let reversed = |x: u16| buf[(x, y)].style().add_modifier.contains(Modifier::REVERSED);
    assert!(reversed(ox), "고른 첫 칸은 반전이어야 한다");
    assert!(reversed(ox + 3), "고른 마지막 칸도 반전");
    assert!(!reversed(ox + 8), "안 고른 칸은 반전이 아니어야 한다");
}

fn question_event(seq: i64, result: serde_json::Value) -> AppFrame {
    AppFrame::Event {
        cursor: seq,
        entry: zyris_code::event::entry_from(&zyris_attacca::ZSessionEvent {
            seq,
            cursor: seq,
            kind: "tool_call".into(),
            payload: serde_json::json!({
                "kind": "tool_call", "name": "question",
                "arguments": {"questions": [
                    {"header": "방식", "question": "어느 쪽으로 갈까요?",
                     "options": [{"label": "A안", "description": "빠르다"}, {"label": "B안"}]}
                ]},
                "result": result, "error": null
            }),
            created_at: None,
        }),
    }
}

/// When a question arrives, answering mode turns on by itself — the turn is blocked, so there's no reason to open it manually.
#[test]
fn a_question_opens_for_answering_and_shows_its_options() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));
    assert!(s.asking.is_some(), "답하기 모드로 안 들어갔다");

    let screen = dump(&mut s, 70, 16);
    assert!(screen.contains("어느 쪽으로 갈까요?"), "\n{screen}");
    assert!(screen.contains("A안"), "\n{screen}");
    assert!(screen.contains("빠르다"), "설명도 보여야 한다\n{screen}");
    assert!(screen.contains("직접 입력"), "자유 입력 대안이 없다\n{screen}");
}

/// An already-answered question must not reopen.
#[test]
fn an_answered_question_does_not_reopen() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(question_event(1, serde_json::json!({"status": "answered"}))));
    assert!(s.asking.is_none());
}

/// After choosing, pressing Enter on the submit row fills the answer and marks it for immediate sending.
#[test]
fn choosing_then_submitting_fills_the_answer_and_marks_it_for_sending() {
    use crossterm::event::KeyCode;
    use zyris_code::app::on_key;
    use zyris_code::question::{Act, RowKind};

    let mut s = State::new();
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));

    let press = |s: &mut State, code| {
        for a in on_key(s, key(code)) {
            apply(s, &a);
        }
    };
    // Move the cursor to that action row. **The number of tries must be capped** — if the row isn't found,
    // the cursor cycles the list forever and never ends. This spot actually spun in an infinite loop and
    // pegged the test binary's CPU at 100%.
    let go_to = |s: &mut State, want: Act| {
        let rows = s.asking.as_ref().map_or(0, |(_, a)| a.rows().len());
        for _ in 0..=rows {
            if matches!(
                s.asking.as_ref().and_then(|(_, a)| a.row_at(a.cursor)),
                Some(RowKind::Action(act)) if act == want
            ) {
                return;
            }
            press(s, KeyCode::Down);
        }
        panic!("{want:?} 줄이 없다: {:?}", s.asking.as_ref().map(|(_, a)| a.rows()));
    };

    press(&mut s, KeyCode::Down); // to option B
    press(&mut s, KeyCode::Enter); // select it

    // Once all questions are asked, it moves to the review screen, and submit lives only there.
    go_to(&mut s, Act::Next);
    press(&mut s, KeyCode::Enter);
    go_to(&mut s, Act::Submit);
    press(&mut s, KeyCode::Enter);

    assert!(s.asking.is_none(), "제출하면 질문이 닫힌다");
    assert!(s.submit_now, "곧바로 보낼 표시가 서야 한다");
    assert!(s.input.text.contains("어느 쪽으로 갈까요?"), "질문을 실어야 한다: {}", s.input.text);
    assert!(s.input.text.contains("B안"), "{}", s.input.text);
}

/// The submit row is always at the bottom, and the question UI is drawn in the input box's place.
#[test]
fn the_question_replaces_the_input_box() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));
    let screen = dump(&mut s, 70, 16);
    // The question screen only has back-and-forth. Submit appears only on the review screen after all questions are asked.
    assert!(screen.contains("건너뛰기"), "건너뛰기 줄이 없다\n{screen}");
    assert!(!screen.contains("제출"), "질문 화면에 제출이 있다\n{screen}");
    assert!(screen.contains("✎ 직접 입력"), "자유 입력 줄이 없다\n{screen}");
    // While a question is open, the usual input prompt gives up its place.
    let bottom: Vec<&str> = screen.lines().rev().take(3).collect();
    assert!(
        !bottom.iter().any(|l| l.trim_start().starts_with("> ")),
        "입력란이 아직 있다\n{screen}"
    );
}

/// While a question is open, typed characters must not leak into the input box below.
#[test]
fn typing_while_a_question_is_open_does_not_leak_into_the_message_box() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use zyris_code::app::on_key;

    let mut s = State::new();
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));
    for a in on_key(&s, KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)) {
        apply(&mut s, &a);
    }
    assert_eq!(s.input.text, "", "질문 중에 글자가 입력란으로 샜다");
}

/// ← opens the list only when the input is empty. If there's text, cursor movement comes first.
#[test]
fn left_arrow_opens_the_picker_only_when_the_input_is_empty() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use zyris_code::app::on_key;

    let mut s = State::new();
    let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(on_key(&s, left), vec![Action::OpenPicker]);

    apply(&mut s, &Action::Insert('가'));
    assert_eq!(on_key(&s, left), vec![Action::Left], "글자가 있으면 커서 이동");
}

/// When the list opens, keys go to it, and it overlays the conversation.
#[test]
fn the_picker_overlays_the_conversation_and_takes_the_keys() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use zyris_code::app::on_key;
    use zyris_code::picker::Picker;

    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry { seq: 1, kind: EntryKind::Agent("뒤에 있는 대화".into()) }),
        }),
    );
    s.picker = Some(Picker::projects(
        vec![("p1".into(), "기본 프로젝트".into(), true), ("p2".into(), "zyris".into(), false)],
        zyris_code::lang::Lang::Ko,
    ));

    let screen = dump(&mut s, 70, 16);
    assert!(screen.contains("프로젝트"), "\n{screen}");
    assert!(screen.contains("＋ 새 프로젝트"), "생성 줄이 없다\n{screen}");
    assert!(screen.contains("zyris"), "\n{screen}");

    // While the list is open, typed characters must not leak into the input box.
    for a in on_key(&s, KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)) {
        apply(&mut s, &a);
    }
    assert_eq!(s.input.text, "");

    // ↓ moves through the list.
    for a in on_key(&s, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)) {
        apply(&mut s, &a);
    }
    assert_eq!(s.picker.as_ref().unwrap().cursor, 2);

    // Esc emits a back action. Closing it is the I/O layer's job.
    assert_eq!(on_key(&s, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), vec![Action::PickBack]);
}

/// The two create rows behave differently. **Sessions are created right away; projects go through a form.**
#[test]
fn the_create_rows_behave_differently_by_level() {
    use zyris_code::picker::{Pick, Picker};

    let projects =
        Picker::projects(vec![("p1".into(), "기본".into(), true)], zyris_code::lang::Lang::Ko);
    let mut at_create = projects.clone();
    at_create.cursor = 0;
    assert_eq!(at_create.pick(), Some(Pick::NewProject));

    let sessions = Picker::sessions("p1".into(), "기본".into(), vec![], zyris_code::lang::Lang::Ko);
    assert_eq!(sessions.pick(), Some(Pick::NewSession { project_id: "p1".into() }));
}

/// New project form: title, name, and description fields are shown, and typed characters go to the name field.
#[test]
fn the_new_project_form_renders_and_types_into_the_name_field() {
    use crossterm::event::KeyCode;
    use zyris_code::app::on_key;
    use zyris_code::newproject::Form;

    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.new_project = Some(Form::new());

    let screen = dump(&mut s, 70, 16);
    assert!(screen.contains("새 프로젝트"), "제목이 없다\n{screen}");
    assert!(screen.contains("이름"), "이름 칸이 없다\n{screen}");
    assert!(screen.contains("설명"), "설명 칸이 없다\n{screen}");
    assert!(screen.contains("Enter 만들기"), "안내가 없다\n{screen}");

    // Typed characters go to the form's name field — they must not leak into the input box below.
    for a in on_key(&s, key(KeyCode::Char('가'))) {
        apply(&mut s, &a);
    }
    assert_eq!(s.new_project.as_ref().unwrap().name.text, "가");
    assert_eq!(s.input.text, "");
    let screen = dump(&mut s, 70, 16);
    assert!(screen.contains('가'), "친 글자가 화면에 없다\n{screen}");

    // Esc closes the form.
    for a in on_key(&s, key(KeyCode::Esc)) {
        apply(&mut s, &a);
    }
    assert!(s.new_project.is_none(), "Esc가 양식을 안 닫았다");
}

/// Session titles come in arbitrary lengths. Without truncation they'd punch through the box and break the screen.
#[test]
fn long_picker_labels_are_truncated_inside_the_box() {
    use zyris_code::picker::Picker;

    let mut s = State::new();
    s.picker = Some(Picker::sessions(
        "p1".into(),
        "기본".into(),
        vec![(
            "s1".into(),
            "아주아주 긴 세션 제목이 여기 들어가고 계속 이어집니다 정말로 깁니다".into(),
            true,
        )],
        zyris_code::lang::Lang::Ko,
    ));
    let screen = dump(&mut s, 70, 16);
    for line in screen.lines() {
        assert!(zyris_code::markdown::display_width(line) <= 70, "폭을 넘었다: {line:?}");
    }
    assert!(screen.contains('…'), "잘렸다는 표시가 없다\n{screen}");
}

fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
}

fn ctrl(c: char) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char(c),
        crossterm::event::KeyModifiers::CONTROL,
    )
}

/// The list opens with ←. → is cursor movement.
#[test]
fn left_arrow_opens_the_picker_and_right_does_not() {
    use crossterm::event::KeyCode;
    use zyris_code::app::on_key;

    let s = State::new();
    assert_eq!(on_key(&s, key(KeyCode::Left)), vec![Action::OpenPicker]);
    assert_eq!(on_key(&s, key(KeyCode::Right)), vec![Action::Right]);
}

/// Inside the window, → does nothing and ← goes back.
#[test]
fn inside_the_picker_right_does_nothing_and_left_goes_back() {
    use crossterm::event::KeyCode;
    use zyris_code::app::on_key;
    use zyris_code::picker::Picker;

    let mut s = State::new();
    s.picker = Some(Picker::projects(
        vec![("p1".into(), "기본".into(), true)],
        zyris_code::lang::Lang::Ko,
    ));
    assert!(on_key(&s, key(KeyCode::Right)).is_empty(), "→ 는 아무 일도 안 해야 한다");
    // ← is the same action at both the project and session levels. **What it does is decided by I/O** —
    // at the session level it returns to the project list; at the project level it closes.
    assert_eq!(on_key(&s, key(KeyCode::Left)), vec![Action::PickBack]);
    assert_eq!(on_key(&s, key(KeyCode::Esc)), vec![Action::PickBack]);
}

/// At the session level, ← is **going back, not closing**.
///
/// `apply` must not touch the picker — when `apply` sees what I/O has already moved back
/// from sessions to projects, it would conclude "project level, so close" and turn a back into a close.
#[test]
fn going_back_from_sessions_never_closes_the_picker_in_apply() {
    use zyris_code::picker::{Level, Picker};

    let mut s = State::new();
    s.picker =
        Some(Picker::sessions("p1".into(), "기본".into(), vec![], zyris_code::lang::Lang::Ko));
    apply(&mut s, &Action::PickBack);
    assert!(s.picker.is_some(), "세션 단계에서 닫히면 안 된다");

    // Even after I/O has moved it back to the project list, apply must not close it.
    s.picker = Some(Picker::projects(
        vec![("p1".into(), "기본".into(), true)],
        zyris_code::lang::Lang::Ko,
    ));
    apply(&mut s, &Action::PickBack);
    assert!(
        matches!(s.picker.as_ref().map(|p| &p.level), Some(Level::Projects)),
        "apply가 picker를 건드리면 뒤로가기가 닫기가 된다"
    );
}

/// The sidebar is on by default and Ctrl+B toggles it.
#[test]
fn the_sidebar_is_on_by_default_and_ctrl_b_toggles_it() {
    use zyris_code::app::on_key;

    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    assert!(s.sidebar_on);

    let screen = dump(&mut s, 90, 16);
    assert!(screen.contains("사용량"), "\n{screen}");
    assert!(screen.contains("태스크"), "\n{screen}");

    for a in on_key(&s, ctrl('b')) {
        apply(&mut s, &a);
    }
    assert!(!s.sidebar_on);
    let screen = dump(&mut s, 90, 16);
    assert!(!screen.contains("사용량"), "접었는데 남아 있다\n{screen}");
}

/// Tasks are collected from todo_* tool calls — the todo_change event has no body.
#[test]
fn tasks_come_from_todo_tool_calls() {
    let mut s = State::new();
    let entry = zyris_code::event::entry_from(&zyris_attacca::ZSessionEvent {
        seq: 1,
        cursor: 1,
        kind: "tool_call".into(),
        payload: serde_json::json!({
            "kind": "tool_call", "name": "todo_add",
            "arguments": {"content": "사이드바 만들기"},
            "result": null, "error": null
        }),
        created_at: None,
    });
    apply(&mut s, &Action::Frame(AppFrame::Event { cursor: 1, entry }));

    let screen = dump(&mut s, 90, 16);
    assert!(screen.contains("사이드바 만들기"), "\n{screen}");
}

/// **No text in the left column touches the sidebar divider.** Not just the chat — the input box too.
///
/// If the margin were given only to the chat, wrapped long input would sit right against the divider and the
/// two columns would read as one block.
#[test]
fn nothing_in_the_left_column_touches_the_sidebar_divider() {
    let mut s = State::new();
    for c in "가나다라마바사아자차카타파하가나다라마바사아자차카타파하".chars()
    {
        apply(&mut s, &Action::Insert(c));
    }
    let screen = dump(&mut s, 90, 16);
    for line in screen.lines() {
        let Some(at) = line.find('│') else { continue };
        let before: String = line[..at].chars().rev().take(1).collect();
        assert!(before.is_empty() || before == " ", "경계선에 글이 닿았다: {line:?}\n{screen}");
    }
}

/// On a narrow screen the sidebar is dropped — the conversation comes first.
#[test]
fn a_narrow_screen_drops_the_sidebar() {
    let mut s = State::new();
    let screen = dump(&mut s, 50, 12);
    assert!(!screen.contains("사용량"), "좁은데 사이드바가 남아 있다\n{screen}");
}

/// Even with the picker open, no line may exceed the screen width — even with wide characters mixed in.
#[test]
fn the_picker_box_stays_inside_the_screen_with_wide_text_behind() {
    use zyris_code::picker::Picker;

    let mut s = State::new();
    for i in 1..8 {
        apply(
            &mut s,
            &Action::Frame(AppFrame::Event {
                cursor: i,
                entry: Some(Entry {
                    seq: i,
                    kind: EntryKind::Agent(
                        "한글이 잔뜩 들어간 아주 긴 줄입니다 계속 이어집니다".into(),
                    ),
                }),
            }),
        );
    }
    s.picker = Some(Picker::projects(
        vec![("p1".into(), "기본".into(), true)],
        zyris_code::lang::Lang::Ko,
    ));

    let screen = dump(&mut s, 70, 16);
    for line in screen.lines() {
        assert!(zyris_code::markdown::display_width(line) <= 70, "폭을 넘었다: {line:?}\n{screen}");
    }
}

/// A typed answer must look different from a chosen one — that it wasn't among the options is information.
#[test]
fn typed_answers_look_different_from_chosen_ones_in_history() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry {
                seq: 1,
                kind: EntryKind::User("질문?\n  - A안 (설명)\n  - 직접 입력: 내가 쓴 답".into()),
            }),
        }),
    );
    let screen = dump(&mut s, 70, 12);
    assert!(screen.contains("✎ 내가 쓴 답"), "직접 입력 표시가 없다\n{screen}");
    assert!(!screen.contains("직접 입력: 내가 쓴 답"), "머리말이 그대로 남았다\n{screen}");
    assert!(screen.contains("A안 (설명)"), "고른 것은 그대로여야 한다\n{screen}");
}

/// The active question must live only in the bottom panel. Drawn again in the conversation area, it would appear twice.
#[test]
fn the_active_question_is_not_drawn_twice() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));
    let screen = dump(&mut s, 70, 18);
    let count = screen.matches("어느 쪽으로 갈까요?").count();
    assert_eq!(count, 1, "질문이 {count}번 그려졌다\n{screen}");
}

/// Past the last question, the review screen appears; only there do submit / edit / not-answer show up.
#[test]
fn the_review_screen_appears_after_the_last_question() {
    use zyris_code::question::{Act, RowKind};

    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));

    // The question screen must have no submit — it's too easy to send while unseen questions remain.
    let rows = s.asking.as_ref().unwrap().1.rows();
    assert!(!rows.contains(&RowKind::Action(Act::Submit)), "질문 화면에 제출이 있다");

    // Nothing chosen → skip; something chosen → next.
    assert!(rows.contains(&RowKind::Action(Act::Skip)), "안 골랐으면 건너뛰기여야 한다");
    apply(&mut s, &Action::AskConfirm); // select the first option
    let rows = s.asking.as_ref().unwrap().1.rows();
    assert!(rows.contains(&RowKind::Action(Act::Next)), "고른 뒤에는 다음이어야 한다");

    // next → review
    let last = rows.len() - 1;
    s.asking.as_mut().unwrap().1.cursor = last;
    apply(&mut s, &Action::AskConfirm);
    assert!(s.asking.as_ref().unwrap().1.in_review(), "검토 화면이어야 한다");

    let screen = dump(&mut s, 70, 18);
    assert!(screen.contains("답한 내용"), "\n{screen}");
    assert!(screen.contains("제출"), "\n{screen}");
    assert!(screen.contains("고치기"), "\n{screen}");
    assert!(screen.contains("답하지 않기"), "\n{screen}");
}

/// Typed free text must stay visible in the list — if it isn't shown, there's no way to check what was written.
#[test]
fn typed_free_text_stays_visible_in_the_list() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));

    let a = &mut s.asking.as_mut().unwrap().1;
    a.cursor = a.current().free_row();
    apply(&mut s, &Action::AskConfirm); // start typing
    for c in "내가 쓴 답".chars() {
        apply(&mut s, &Action::Insert(c));
    }
    apply(&mut s, &Action::AskConfirm); // confirm

    let screen = dump(&mut s, 70, 18);
    assert!(screen.contains("내가 쓴 답"), "적은 내용이 안 보인다\n{screen}");
}

/// Entering the empty row tells you what the spot is for.
#[test]
fn an_empty_free_text_row_shows_a_hint() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));
    let a = &mut s.asking.as_mut().unwrap().1;
    a.cursor = a.current().free_row();
    apply(&mut s, &Action::AskConfirm);

    let screen = dump(&mut s, 70, 18);
    assert!(screen.contains("여기에 직접 적으세요"), "안내가 없다\n{screen}");
}

/// You must be able to answer even after restarting — a question re-read from history must open too.
///
/// The server keeps waiting until the question is answered. If a replayed question doesn't open, there's no way to answer.
#[test]
fn a_pending_question_from_history_opens_for_answering() {
    let mut s = State::new();
    // Same path as re-reading a session: replay the events as they were.
    apply(&mut s, &Action::Frame(question_event(7, serde_json::Value::Null)));
    assert!(s.asking.is_some(), "되읽은 질문이 안 열렸다");

    let screen = dump(&mut s, 70, 16);
    assert!(screen.contains("어느 쪽으로 갈까요?"), "\n{screen}");
}

/// A question that's already been answered must not reopen when re-read.
#[test]
fn an_already_answered_question_from_history_stays_closed() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(question_event(7, serde_json::json!({"status": "answered"}))));
    assert!(s.asking.is_none());
}

/// The usage numbers must **line up in one column on the left.** The label lengths differ (credits 6 cells,
/// context 8 cells), so just appending them makes a staircase that can't be compared at a glance.
#[test]
fn the_usage_numbers_line_up_in_one_column() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.sidebar.usage = zyris_code::sidebar::Usage {
        model: Some("claude-opus-5".into()),
        context_tokens: Some(132_800),
        total_tokens: Some(11_400_000),
        credits_used: Some("5.0%".into()),
    };
    let screen = dump(&mut s, 90, 16);
    let cols: Vec<usize> = ["크레딧", "컨텍스트", "총 토큰"]
        .iter()
        .map(|key| {
            let line = screen
                .lines()
                .find(|l| l.contains(key))
                .unwrap_or_else(|| panic!("{key} 줄이 없다:\n{screen}"));
            // After skipping the spaces after the name = the cell where the value starts. **It's a byte offset** —
            // slicing by character count would cut through the middle of a Hangul syllable and panic.
            let at = line.find(key).unwrap() + key.len();
            display_width(&line[..at]) + line[at..].chars().take_while(|c| *c == ' ').count()
        })
        .collect();
    assert!(cols.windows(2).all(|w| w[0] == w[1]), "값의 왼쪽 끝이 안 맞는다: {cols:?}\n{screen}");
}

/// Context is **amount used / amount that fits**. A single number can't tell whether it's roomy or full.
#[test]
fn the_context_shows_how_much_of_the_window_is_used() {
    let mut s = State::new();
    s.sidebar.usage = zyris_code::sidebar::Usage {
        model: Some("claude-opus-5".into()),
        context_tokens: Some(132_800),
        ..Default::default()
    };
    let screen = dump(&mut s, 90, 16);
    assert!(screen.contains("132.8k / 200k"), "쓴 양/최대가 아니다:\n{screen}");
}

/// **For an unknown model, don't invent a limit.** Showing a guessed number makes it look true.
#[test]
fn an_unknown_model_shows_no_limit() {
    let mut s = State::new();
    s.sidebar.usage = zyris_code::sidebar::Usage {
        model: Some("어느-새-모델".into()),
        context_tokens: Some(1_000),
        ..Default::default()
    };
    let screen = dump(&mut s, 90, 16);
    assert!(screen.contains("1k"), "\n{screen}");
    assert!(!screen.contains("1k /"), "모르면서 최대를 붙였다:\n{screen}");
}

/// **The bottom bar says when there are unsent messages.** If it doesn't announce what it's holding, the user believes it was sent.
#[test]
fn the_bottom_bar_says_how_many_messages_are_waiting() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.agent = "Main Agent".into();
    s.running = true;
    apply(&mut s, &Action::Submit("나중에 보낼 말".into()));
    let screen = dump(&mut s, 60, 12);
    let bottom = screen.lines().last().unwrap();
    assert!(bottom.contains("대기 1개"), "대기 표시가 없다: {bottom:?}");

    // When the queue empties, the indicator disappears too.
    s.queued.clear();
    let screen = dump(&mut s, 60, 12);
    assert!(!screen.contains("대기"), "빈 대기열인데 표시가 남았다:\n{screen}");
}

// ── diff of the tool that edited files ────────────────────────────────────────────────

/// Dumps the screen while also fetching each cell's foreground colour **in the same coordinate system**.
///
/// Dumping separately from `dump` would break the skip-after-wide-cell rule and shift text and colour by one cell.
fn dump_with_colours(
    state: &mut State,
    w: u16,
    h: u16,
) -> (String, Vec<Vec<Option<ratatui::style::Color>>>) {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| widgets::draw(f, state)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut text = Vec::new();
    let mut colours = Vec::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        let mut row = Vec::new();
        let mut x = 0;
        while x < buf.area.width {
            let cell = &buf[(x, y)];
            line.push_str(cell.symbol());
            row.push(cell.style().fg);
            x += display_width(cell.symbol()).max(1) as u16;
        }
        text.push(line);
        colours.push(row);
    }
    (text.join("\n"), colours)
}

/// The colour of the cell where that text starts. `None` if absent.
fn colour_of_line_containing(
    screen: &str,
    colours: &[Vec<Option<ratatui::style::Color>>],
    needle: &str,
) -> Option<ratatui::style::Color> {
    let (row, line) = screen.lines().enumerate().find(|(_, l)| l.contains(needle))?;
    let at = line[..line.find(needle)?].chars().count();
    colours.get(row)?.get(at).copied().flatten()
}

/// A work card containing a tool that edited a file. The card is open and the tool row is folded.
fn state_with_edit_tool() -> State {
    use zyris_code::rows::Fold;
    use zyris_code::tools::diff::{Diff, DiffLine};

    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry {
                seq: 1, kind: EntryKind::WorkStart("파일을 고치는 중".into())
            }),
        }),
    );
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 2,
            entry: Some(Entry {
                seq: 2,
                kind: EntryKind::Tool {
                    name: "zyris__arch__code_edit__edit".into(),
                    summary: "zyris__arch__code_edit__edit · src/app.rs".into(),
                    failed: false,
                    detail: "인자\n{}".into(),
                    todo: None,
                    diff: Some(Diff {
                        path: "src/app.rs".into(),
                        added: 12,
                        removed: 3,
                        lines: vec![
                            DiffLine::Keep("그대로".into()),
                            DiffLine::Del("옛 줄".into()),
                            DiffLine::Add("새 줄".into()),
                        ],
                    }),
                },
            }),
        }),
    );
    s.folds.insert(1, Fold { open: true });
    s
}

fn expand_the_tool_row(state: &mut State) {
    use zyris_code::rows::Fold;
    state.folds.insert(2, Fold { open: true });
}

/// Even folded, how much changed must be visible.
#[test]
fn a_file_edit_shows_how_many_lines_changed() {
    let mut s = state_with_edit_tool();
    let screen = dump(&mut s, 80, 24);
    assert!(screen.contains("+12"), "추가 줄 수가 보여야 한다:\n{screen}");
    assert!(screen.contains("−3"), "삭제 줄 수가 보여야 한다:\n{screen}");
    assert!(screen.contains("src/app.rs"), "어느 파일인지 보여야 한다:\n{screen}");
    assert!(
        !screen.contains("zyris__arch__code_edit__edit"),
        "와이어 이름이 그대로 나오면 한 줄을 다 먹는다:\n{screen}"
    );
}

/// Expanded, the actual changed lines show, and additions and deletions must have different colours.
#[test]
fn an_expanded_edit_paints_additions_green_and_deletions_red() {
    let mut s = state_with_edit_tool();
    expand_the_tool_row(&mut s);
    let (screen, colours) = dump_with_colours(&mut s, 80, 24);
    assert!(screen.contains("+새 줄"), "더해진 줄이 안 보인다:\n{screen}");
    assert!(screen.contains("-옛 줄"), "지워진 줄이 안 보인다:\n{screen}");

    let add = colour_of_line_containing(&screen, &colours, "+새 줄");
    let del = colour_of_line_containing(&screen, &colours, "-옛 줄");
    assert_eq!(add, Some(zyris_code::theme::DIFF_ADD), "더해진 줄이 초록이 아니다:\n{screen}");
    assert_eq!(del, Some(zyris_code::theme::DIFF_DEL), "지워진 줄이 빨강이 아니다:\n{screen}");
    assert_ne!(add, del, "추가와 삭제가 같은 색이면 구분이 안 된다");
}

/// **When there's a diff, show the diff instead of the raw JSON dump.** Both would make the screen twice as long,
/// and what people read is the diff.
#[test]
fn an_expanded_edit_shows_the_diff_instead_of_the_raw_json() {
    let mut s = state_with_edit_tool();
    expand_the_tool_row(&mut s);
    let screen = dump(&mut s, 80, 24);
    assert!(!screen.contains("인자"), "원본 JSON이 diff와 함께 나왔다:\n{screen}");
}

// ── sidebar: where the tools run ─────────────────────────────────────────

/// Without knowing what directory the tools run in, the relative path on the approval screen can't be read.
///
/// **Use a path that doesn't overlap where the tests run.** Since the default is the process's working
/// directory, using a real path would pass even if the assignment did nothing.
#[test]
fn the_sidebar_says_which_directory_the_tools_run_in() {
    let mut s = State::new();
    s.cwd = std::path::PathBuf::from("/srv/checkouts/some-repo");
    let screen = dump(&mut s, 100, 24);
    assert!(screen.contains("some-repo"), "작업 디렉터리가 보여야 한다:\n{screen}");
}

/// If it weren't shown, ghost shells would run — shells the agent left open, unknown to the person.
#[test]
fn open_shells_are_listed() {
    let mut s = State::new();
    s.shells = vec![zyris_code::app::Shell { id: "p1".into(), name: "zsh".into() }];
    let screen = dump(&mut s, 100, 24);
    assert!(screen.contains("zsh"), "열린 셸이 보여야 한다:\n{screen}");
}

/// An empty section must not take up space. The sidebar is narrow.
#[test]
fn no_shell_section_when_nothing_is_open() {
    let mut s = State::new();
    let screen = dump(&mut s, 100, 24);
    assert!(!screen.contains("셸"), "열린 셸이 없으면 절 자체가 없어야 한다:\n{screen}");
}

/// A long path keeps only its last two segments. Beyond the sidebar width it would be truncated and unrecognizable.
#[test]
fn a_long_working_directory_keeps_the_part_that_identifies_it() {
    let mut s = State::new();
    s.cwd = std::path::PathBuf::from("/home/ruma/very/deeply/nested/place/zyris-code");
    let screen = dump(&mut s, 100, 24);
    assert!(screen.contains("place/zyris-code"), "끝 두 조각이 보여야 한다:\n{screen}");
    assert!(!screen.contains("/home/ruma/very"), "앞쪽까지 나오면 넘친다:\n{screen}");
}
/// While a command runs, **what is running** must be shown. Since `exec` only reports once at completion,
/// without this the person waits up to 55 seconds in the dark.
#[test]
fn a_running_command_is_named_in_the_activity_line() {
    let mut s = State::new();
    s.connected = true;
    s.running = true;
    apply(&mut s, &Action::Frame(AppFrame::ExecStart { id: 1, command: "cargo build -j2".into() }));
    let screen = dump(&mut s, 80, 12);
    assert!(screen.contains("cargo build -j2"), "무엇이 도는지 안 보인다:\n{screen}");
    assert!(!screen.contains("작업 중…"), "구체적인 것이 있는데 뭉뚱그렸다:\n{screen}");
}

/// It disappears when done. If it lingered, it would overlap the next one.
#[test]
fn a_finished_command_leaves_the_activity_line() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.connected = true;
    s.running = true;
    apply(&mut s, &Action::Frame(AppFrame::ExecStart { id: 1, command: "cargo build".into() }));
    apply(&mut s, &Action::Frame(AppFrame::ExecDone { id: 1 }));
    let screen = dump(&mut s, 80, 12);
    assert!(!screen.contains("cargo build"), "끝난 명령이 남아 있다:\n{screen}");
    assert!(screen.contains("작업 중…"), "턴은 아직 도는데 아무 말이 없다:\n{screen}");
}

/// **Only clear the one that finished.** Clearing a running one because a different id finished would make the screen lie.
#[test]
fn finishing_another_command_does_not_clear_the_running_one() {
    let mut s = State::new();
    s.connected = true;
    s.running = true;
    apply(&mut s, &Action::Frame(AppFrame::ExecStart { id: 2, command: "cargo test".into() }));
    apply(&mut s, &Action::Frame(AppFrame::ExecDone { id: 1 }));
    let screen = dump(&mut s, 80, 12);
    assert!(screen.contains("cargo test"), "엉뚱한 번호에 지워졌다:\n{screen}");
}

/// The elapsed time must show so you know it's still running. The test controls the clock.
#[test]
fn the_activity_line_counts_the_seconds() {
    use std::time::{Duration, Instant};
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.connected = true;
    s.running = true;
    apply(&mut s, &Action::Frame(AppFrame::ExecStart { id: 1, command: "sleep 30".into() }));
    let start = s.running_exec.as_ref().unwrap().2;
    let (_, label, _) = zyris_code::widgets::activity_parts_at(&s, start + Duration::from_secs(12));
    assert!(label.contains("12초"), "{label}");
    let _ = Instant::now();
}

/// **It must be visible that Ctrl+C landed.** Until the server replies, the status stays "working",
/// and if nothing changes on screen in the meantime, the user thinks it didn't register and presses again.
#[test]
fn asking_to_stop_shows_on_the_activity_line() {
    let mut s = State::new();
    s.connected = true;
    apply(&mut s, &Action::Frame(AppFrame::Status { running: true }));
    apply(&mut s, &Action::Cancel);
    let screen = dump(&mut s, 80, 12);
    assert!(screen.contains("Stopping"), "멈추라고 한 것이 안 보인다:\n{screen}");
    assert!(screen.contains("Ctrl+C quits"), "그다음 Ctrl+C가 무엇인지 말해야 한다:\n{screen}");
}

// ── enrollment code window ──────────────────────────────────────────────────────────

fn enroll_view() -> zyris_code::app::EnrollView {
    zyris_code::app::EnrollView {
        code: "WXQR-7KBD".into(),
        uri: "https://attacca.example/settings/zyris/device".into(),
        expires_at: std::time::Instant::now() + std::time::Duration::from_secs(600),
        phase: zyris_code::app::EnrollPhase::Waiting,
    }
}

/// The enrollment code appears in a box in the middle of the screen — not the old stdout box.
#[test]
fn the_enroll_window_shows_the_code_and_the_address() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(AppFrame::Enroll(enroll_view())));
    let screen = dump(&mut s, 80, 24);
    assert!(screen.contains("WXQR-7KBD"), "코드가 안 보인다:\n{screen}");
    assert!(
        screen.contains("attacca.example/settings/zyris/device"),
        "주소가 안 보인다:\n{screen}"
    );
    assert!(screen.contains("Connect to Attacca"), "제목이 없다:\n{screen}");
    assert!(screen.contains("Esc close"), "닫는 키 안내가 없다:\n{screen}");
}

/// When denied, the situation changes — closing silently would leave the person wondering what happened.
#[test]
fn a_denied_enrollment_says_so_in_the_window() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(AppFrame::Enroll(enroll_view())));
    apply(&mut s, &Action::Frame(AppFrame::EnrollPhase(zyris_code::app::EnrollPhase::Denied)));
    let screen = dump(&mut s, 80, 24);
    assert!(screen.contains("declined"), "거부가 안 보인다:\n{screen}");
}

/// The enrollment window overlays the conversation — while reading the code, the background doesn't need to be visible.
#[test]
fn the_enroll_window_overlays_the_conversation() {
    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry { seq: 1, kind: EntryKind::Agent("뒤에 있는 대화".into()) }),
        }),
    );
    apply(&mut s, &Action::Frame(AppFrame::Enroll(enroll_view())));
    let screen = dump(&mut s, 80, 24);
    assert!(screen.contains("WXQR-7KBD"), "코드가 안 보인다:\n{screen}");
}

// ── approval screen ──────────────────────────────────────────────────────────────

fn leaving_ask() -> zyris_code::app::ToolAsk {
    zyris_code::app::ToolAsk {
        id: 1,
        call: zyris_code::tools::gate::Call::new("code_edit", "edit", "x.rs".into())
            .leaving(Some(std::path::PathBuf::from("/home/ruma/attacca/Cargo.toml"))),
        summary: "/home/ruma/attacca/Cargo.toml".into(),
        expired: false,
    }
}

/// **Where it touches is the whole point of this approval.** Since nothing about the inner work is asked,
/// the window appearing at all means "this is outside".
#[test]
fn the_approval_screen_leads_with_the_path_that_leaves() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.pending = Some(leaving_ask());
    let screen = dump(&mut s, 90, 24);
    assert!(screen.contains("작업 디렉터리 밖"), "{screen}");
    assert!(screen.contains("/home/ruma/attacca/Cargo.toml"), "{screen}");
    assert!(screen.contains("code_edit.edit"), "{screen}");
    assert!(screen.contains("y 허용"), "누를 키가 없다:\n{screen}");
}

/// Even past the deadline, the **window stays**; only the situation changes.
#[test]
fn an_expired_ask_stays_on_screen_and_says_so() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    let mut a = leaving_ask();
    a.expired = true;
    s.pending = Some(a);
    let screen = dump(&mut s, 90, 24);
    assert!(screen.contains("기다리다 돌아갔습니다"), "{screen}");
    assert!(screen.contains("y 허용"), "답할 길이 사라지면 안 된다:\n{screen}");
}

/// It must say how many are waiting behind — you shouldn't think answering one is the end.
#[test]
fn the_screen_says_how_many_are_waiting() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.pending = Some(leaving_ask());
    s.ask_queue.push_back(leaving_ask());
    let screen = dump(&mut s, 90, 24);
    assert!(screen.contains("뒤에 1개"), "{screen}");
}

/// The approval window takes the input box's place. If both showed, you couldn't tell where to answer.
#[test]
fn the_approval_screen_takes_the_place_of_the_input_box() {
    let mut s = State::new();
    apply(&mut s, &Action::Insert('안'));
    assert!(dump(&mut s, 90, 24).contains('안'));

    s.pending = Some(leaving_ask());
    let screen = dump(&mut s, 90, 24);
    assert!(!screen.contains('안'), "입력란이 같이 떠 있다:\n{screen}");
}

// ── list window ────────────────────────────────────────────────────────────────

fn long_session_list(n: usize) -> zyris_code::picker::Picker {
    zyris_code::picker::Picker::sessions(
        "p1".into(),
        "zyris".into(),
        (0..n).map(|i| (format!("s{i}"), format!("쓰레드 {i}"), false)).collect(),
        zyris_code::lang::Lang::Ko,
    )
}

/// **At the cut edge, say how many more there are.** Without it, the list looks like it ends there.
#[test]
fn a_long_list_shows_how_many_are_left() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.picker = Some(long_session_list(40));
    let screen = dump(&mut s, 80, 16);
    assert!(screen.contains("개 더"), "남은 개수가 없다:\n{screen}");
    assert!(screen.contains('↓'), "아래로 남았다는 표시가 없다:\n{screen}");
}

/// When everything fits, there's no marker. Saying "more" when nothing is cut would be a lie.
#[test]
fn a_short_list_shows_no_overflow_mark() {
    let mut s = State::new();
    s.picker = Some(long_session_list(2));
    let screen = dump(&mut s, 80, 24);
    assert!(!screen.contains("개 더"), "안 잘렸는데 표시가 있다:\n{screen}");
}

/// **"New" is ruled off from the list.** Sitting next to it, it would read as one of the sessions.
#[test]
fn the_create_row_is_ruled_off_from_the_sessions() {
    let mut s = State::new();
    s.picker = Some(long_session_list(3));
    let screen = dump(&mut s, 80, 24);
    let lines: Vec<&str> = screen.lines().collect();
    let at = lines.iter().position(|l| l.contains("새 쓰레드")).expect("만들 줄이 없다");
    assert!(lines[at + 1].contains('─'), "가름선이 없다:\n{screen}");
}

/// The create row has a **different colour** — it's for creating, not selecting.
#[test]
fn the_create_row_is_coloured_apart_from_the_list() {
    let mut s = State::new();
    s.picker = Some(long_session_list(3));
    // The cursor sits on the create row. Its colour must differ from the first session below it.
    // The box is centered (width 64), and the inner text starts after `❯ `.
    const LABEL_X: u16 = 8 + 1 + 2;
    let screen = dump(&mut s, 80, 24);
    let top = screen.lines().position(|l| l.contains("새 쓰레드")).unwrap() as u16;
    let create = cell_fg(&mut s, 80, 24, LABEL_X, top);
    let session = cell_fg(&mut s, 80, 24, LABEL_X, top + 2);
    assert_eq!(create, Some(zyris_code::theme::ACCENT), "만들 줄이 강조색이 아니다");
    assert_ne!(create, session, "만들 줄과 세션이 같은 색이다:\n{screen}");
}

/// The default mode has a colour too — grey would make the whole bottom bar read as background.
#[test]
fn the_default_mode_is_not_just_grey() {
    let mut s = State::new();
    let y = 24 - 1;
    assert_eq!(cell_fg(&mut s, 80, 24, 0, y), Some(zyris_code::theme::SUCCESS));
}
