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
    assert!(lines[lines.len() - 3].contains('안'), "the input is not in its place:\n{screen}");
    assert!(lines[lines.len() - 2].starts_with('─'), "no rule:\n{screen}");
    // The default screen language is English (the `lang::Lang` default). We look at the mode name in the bottom bar.
    assert!(
        lines[lines.len() - 1].contains("normal"),
        "the bottom line is not the status bar:\n{screen}"
    );
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
    assert!(tail.contains("파하"), "the trailing characters were cut:\n{screen}");
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
            todo: None,
        }),
    );
    let screen = dump(&mut s, 40, 10);
    assert!(screen.contains("안녕하세요"), "\n{screen}");
}

/// **What was typed is on screen the moment it is submitted**, with no server round trip.
///
/// Every user line in this suite used to be injected as a server event, so nobody ever checked
/// the one thing a person does first — type something and look for it. Without the local echo it
/// is missing for the whole round trip, and in 일/작업 mode the first message rides inside
/// `ZNewJob::message` and may never come back at all.
#[test]
fn what_was_just_submitted_is_on_screen_without_waiting_for_the_server() {
    let mut s = State::new();
    // Attached — nothing is sent before the first connection.
    s.ever_connected = true;
    apply(&mut s, &Action::Submit("이걸 해 주세요".into()));
    let screen = dump(&mut s, 40, 10);
    assert!(screen.contains("이걸 해 주세요"), "\n{screen}");
}

/// And when the server's own copy lands, the words do not end up on screen twice.
#[test]
fn the_servers_copy_of_a_submitted_message_does_not_double_it() {
    let mut s = State::new();
    apply(&mut s, &Action::Submit("안녕하세요".into()));
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry { seq: 1, kind: EntryKind::User("안녕하세요".into()) }),
            todo: None,
        }),
    );
    let screen = dump(&mut s, 40, 12);
    assert_eq!(screen.matches("안녕하세요").count(), 1, "\n{screen}");
}

/// A slash command is not a message — it must not leave a user line behind.
#[test]
fn a_slash_command_leaves_no_message_on_screen() {
    let mut s = State::new();
    apply(&mut s, &Action::Submit("/cwd".into()));
    let screen = dump(&mut s, 40, 10);
    assert!(!screen.contains("/cwd"), "\n{screen}");
}

/// **What you type appears as you type it.** Only a pty test covered this, so a rendering
/// regression in the input box could only be caught by a script that needs a live server.
#[test]
fn typed_text_shows_in_the_input_box() {
    let mut s = State::new();
    for c in "안녕".chars() {
        apply(&mut s, &Action::Insert(c));
    }
    let screen = dump(&mut s, 40, 10);
    assert!(screen.contains("안녕"), "\n{screen}");
}

/// A cell's background colour. Pairs with `cell_fg`.
fn cell_bg(state: &mut State, w: u16, h: u16, x: u16, y: u16) -> Option<ratatui::style::Color> {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| widgets::draw(f, state)).unwrap();
    term.backend().buffer()[(x, y)].style().bg
}

fn said(state: &mut State, seq: i64, kind: EntryKind) {
    let entry = Some(Entry { seq, kind });
    apply(state, &Action::Frame(AppFrame::Event { cursor: seq, entry, todo: None }));
}

/// **A scrolled-up view keeps looking at the same words when the width changes.**
///
/// `Scroll.top` is an absolute index into a line list that is rebuilt from scratch every layout,
/// so re-wrapping at a new width used to move the text out from under it — always toward older
/// content, because the clamp can only push the index down. Alt-tabbing back on Windows re-lays
/// out for exactly this reason, and the report was "scrolling pulls up old chat".
#[test]
fn a_scrolled_view_keeps_its_place_when_the_width_changes() {
    let mut s = State::new();
    // **The replies have to be long enough to wrap differently at the two widths.** With text
    // that fits on one line either way nothing is relaid out and the test proves nothing — it
    // passed against the broken code until the replies got this long.
    for seq in 1..=12 {
        said(&mut s, seq, EntryKind::User(format!("메시지 번호 {seq} 입니다")));
        said(&mut s, seq + 100, EntryKind::Agent(format!("답 {seq} — {}", "가나다라".repeat(30))));
    }
    // Draw once wide so the viewport metrics exist, then scroll up off the bottom.
    dump(&mut s, 80, 12);
    apply(&mut s, &Action::Wheel(6));
    dump(&mut s, 80, 12);
    let before = s.rows_cache.anchor_at(s.view_top).map(|(seq, _)| seq);
    assert!(before.is_some(), "nothing is anchored at the top of the viewport");

    // Re-wrap narrower — every wrap point moves and the line count grows, so the same absolute
    // index would land somewhere much earlier in the conversation.
    let narrow = dump(&mut s, 46, 12);
    let after = s.rows_cache.anchor_at(s.view_top).map(|(seq, _)| seq);
    assert_eq!(before, after, "the top of the viewport moved to a different message:\n{narrow}");
}

/// Sticking to the bottom is unaffected — the bottom is its own anchor.
#[test]
fn a_view_stuck_to_the_bottom_stays_there_when_the_width_changes() {
    let mut s = State::new();
    for seq in 1..=10 {
        said(&mut s, seq, EntryKind::User(format!("줄 {seq}")));
    }
    dump(&mut s, 80, 8);
    let narrow = dump(&mut s, 40, 8);
    assert!(narrow.contains("줄 10"), "the last message must stay in view:\n{narrow}");
}

/// **Where the user spoke is painted with a background.** And it must run to the right edge even after the
/// text ends — if it broke at the text width, it would look like a smudge rather than a band.
#[test]
fn a_user_message_gets_a_band_across_the_full_width() {
    let mut s = State::new();
    said(&mut s, 1, EntryKind::User("안녕".into()));
    assert_eq!(
        cell_bg(&mut s, 60, 12, 1, 0),
        Some(zyris_code::theme::user_bg()),
        "글자 자리가 안 칠해졌다"
    );
    assert_eq!(
        cell_bg(&mut s, 60, 12, 59, 0),
        Some(zyris_code::theme::user_bg()),
        "끝까지 안 이어진다"
    );
}

/// Answers have no background. Painting everything would make nothing distinguishable.
#[test]
fn an_agent_answer_has_no_band() {
    let mut s = State::new();
    said(&mut s, 1, EntryKind::Agent("그렇습니다".into()));
    assert_ne!(cell_bg(&mut s, 60, 12, 3, 0), Some(zyris_code::theme::user_bg()));
}

/// **By default, the page background is not painted.** The terminal is left to use its own background.
///
/// If the app painted it, only the area outside the grid (window padding, leftover pixels the grid doesn't
/// fit) would change colour, creating a stripe at the screen edge — a spot the app can't touch, so there is
/// no way to fix it by painting. Using the background the person chose is better everywhere (`theme::page_bg`).
#[test]
fn nothing_but_the_user_band_paints_a_background_by_default() {
    let mut s = State::new();
    said(&mut s, 1, EntryKind::Agent("안녕하세요".into()));
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    let frame = term.draw(|f| widgets::draw(f, &mut s)).unwrap();
    for cell in frame.buffer.content.iter() {
        assert!(
            cell.bg == Color::Reset || cell.bg == zyris_code::theme::user_bg(),
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
    assert_eq!(page_bg_from(None), None, "with none given, nothing is painted");
    assert_eq!(page_bg_from(Some("")), None, "an empty value counts as not given");
    assert_eq!(page_bg_from(Some("zyris")), Some(zyris_code::theme::bg()));
    assert_eq!(page_bg_from(Some("#101820")), Some(Color::Rgb(0x10, 0x18, 0x20)));
    assert_eq!(page_bg_from(Some("none")), None, "turning it off can be stated explicitly too");
    // **A single typo must not kill the app.** If it can't be read, it falls back to not painting.
    assert_eq!(page_bg_from(Some("보라색")), None);
}

/// **The self-healing frame forces every cell to be re-emitted.** Instead of clearing, it overwrites, so the
/// ghost of the trailing cell behind a wide character is wiped — no clear, so nothing flickers. Even if the diff
/// is identical, to reach the wire the cell must be planted with `AlwaysUpdate`, and the flag clears after one frame.
#[test]
fn a_heal_frame_forces_every_cell_to_be_resent() {
    let mut s = State::new();
    said(&mut s, 1, EntryKind::Agent("안녕하세요".into()));
    s.force_update = true;

    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    let frame = term.draw(|f| widgets::draw(f, &mut s)).unwrap();
    assert!(!s.force_update, "the force flag must clear after one frame");
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

/// **A blank-only heal forces only blank cells to be resent.** Residue always hides on a
/// blank cell — content cells are redrawn whenever they change, so the old glyph has no
/// place to peek through. A full rewrite overlapped a streaming frame on a slow SSH link
/// and showed the same word twice; writing only spaces can never corrupt or double content.
///
/// The cell right after a wide character (a blank) also gets `AlwaysUpdate` planted, but the
/// diff always skips it (`cell_width > 1` branch) — the wide char's right half is never
/// erased. That is pinned by `a_blank_heal_never_marks_a_wide_char_itself`.
#[test]
fn a_blank_heal_forces_only_blank_cells_to_be_resent() {
    let mut s = State::new();
    said(&mut s, 1, EntryKind::Agent("안녕하세요".into()));
    s.force_update_blank = true;

    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    let frame = term.draw(|f| widgets::draw(f, &mut s)).unwrap();
    assert!(!s.force_update_blank, "the force flag must clear after one frame");
    let blank_count = frame.buffer.content.iter().filter(|c| c.symbol() == " ").count();
    assert!(blank_count > 0, "화면에 빈 칸이 하나도 없다 — 테스트가 헛돈다");
    assert!(
        frame.buffer.content.iter().all(|c| {
            c.diff_option == ratatui::buffer::CellDiffOption::AlwaysUpdate || c.symbol() != " "
        }),
        "빈 칸은 강제 재출력, 내용 칸은 그대로여야 한다"
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

/// **A blank heal never marks a wide char itself.** The trailing cell behind a wide
/// character is blank, so `AlwaysUpdate` gets planted on it — but the diff always skips it
/// when emitting the wide char, or the right half of the glyph would be erased. The heal
/// only plants blanks, so the wide char's own cell is never a target in the first place.
#[test]
fn a_blank_heal_never_marks_a_wide_char_itself() {
    let mut s = State::new();
    said(&mut s, 1, EntryKind::Agent("안녕".into()));
    s.force_update_blank = true;

    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    let frame = term.draw(|f| widgets::draw(f, &mut s)).unwrap();

    let mut wide_chars = 0;
    for c in frame.buffer.content.iter() {
        let w = c.symbol().chars().next().map(unicode_width::UnicodeWidthChar::width);
        if w.is_some_and(|w| w.unwrap_or(0) > 1) {
            wide_chars += 1;
            assert!(
                c.diff_option != ratatui::buffer::CellDiffOption::AlwaysUpdate,
                "전각 글자 자체에 강제 재출력을 심으면 안 된다"
            );
        }
    }
    assert!(wide_chars > 0, "전각 글자가 하나도 없다 — 테스트가 헛돈다");
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
    prev.set_string(2, 0, "한", Style::default().bg(zyris_code::theme::bg()));
    let mut next = Buffer::empty(area);
    next.set_string(2, 0, "a", Style::default().bg(zyris_code::theme::bg()));

    wire.take();
    backend.draw(prev.diff_iter(&next)).unwrap();
    let out = String::from_utf8_lossy(&wire.take()).into_owned();
    assert_eq!(strip_ansi(&out), "a ", "the trailing cell was not cleared: {out:?}");
}

/// **Without a background, the trailing cell doesn't go out on the wire — the very cause of ghosting.**
/// This test doesn't pin the current behavior; rather, it documents that before `theme::bg()` was laid on every cell,
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
    assert_eq!(
        strip_ansi(&out),
        "a",
        "a trailing cell must not go out without a background (the old behaviour): {out:?}"
    );
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
            todo: None,
        }),
    );
    let screen = dump(&mut s, 40, 10);
    let top = screen.lines().next().unwrap();
    assert!(!top.contains("zyris-code"), "a header is still drawn:\n{screen}");
    assert!(top.contains("첫 줄"), "the top line is not the transcript:\n{screen}");
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
            todo: None,
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
    assert!(!screen.contains("연결됨"), "the connected marker is still shown:\n{screen}");
}

/// A broken connection must always be said aloud — silent failure is the worst kind.
#[test]
fn a_broken_connection_is_always_said_out_loud() {
    let mut s = State::new();
    s.connected = false;
    let screen = dump(&mut s, 40, 10);
    assert!(screen.contains("Connecting"), "it disconnected and said nothing:\n{screen}");

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
    assert!(lines[at].contains("쉬는 중"), "the status line is not in its place:\n{screen}");
    assert!(lines[at - 1].trim().is_empty(), "it is not separated from the transcript:\n{screen}");
    assert!(lines[at + 1].starts_with('─'), "the line right below is not a rule:\n{screen}");

    s.running = true;
    let screen = dump(&mut s, 40, 12);
    let lines: Vec<&str> = screen.lines().collect();
    assert!(
        lines[lines.len() - ACTIVITY_FROM_BOTTOM].contains("작업 중"),
        "작업 중이 안 뜬다:\n{screen}"
    );
}

/// Puts one task on the session's plan, the way the server does: a `todo_add` tool call.
fn planned(state: &mut State, seq: i64, content: &str, status: &str) {
    let event = zyris_attacca::ZSessionEvent {
        seq,
        cursor: seq,
        kind: "tool_call".into(),
        payload: serde_json::json!({
            "name": "todo_add",
            "arguments": {"content": content},
            "result": {"id": format!("t{seq}"), "content": content, "status": status},
        }),
        created_at: None,
    };
    apply(
        state,
        &Action::Frame(AppFrame::Event {
            cursor: seq,
            entry: zyris_code::event::entry_from(&event),
            todo: zyris_code::todos::change_from(&event),
        }),
    );
}

/// **How far along the plan is rides on the line that says what is happening.** Otherwise nothing
/// on screen says the todo list exists, and the key that opens it is one nobody would press.
#[test]
fn the_activity_line_counts_the_plan_beside_what_it_is_doing() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.connected = true;
    s.running = true;
    planned(&mut s, 1, "테스트 고치기", "completed");
    planned(&mut s, 2, "빌드 돌리기", "pending");
    let screen = dump(&mut s, 40, 12);
    let lines: Vec<&str> = screen.lines().collect();
    let at = lines.len() - ACTIVITY_FROM_BOTTOM;
    assert!(lines[at].contains("작업 중"), "{screen}");
    assert!(lines[at].contains("(1/2)"), "the plan is not counted:\n{screen}");
}

/// **A session with no plan says nothing about one.** An empty `(0/0)` on every screen is noise.
#[test]
fn a_session_without_a_plan_shows_no_count() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.connected = true;
    let screen = dump(&mut s, 40, 12);
    assert!(!screen.contains("(0/0)"), "{screen}");
}

/// **The plan opens under the line that counted it, and takes its room from the conversation.**
/// The input box and the bottom bar must stay exactly where they were — the room a person types
/// in cannot shrink because the agent wrote itself a longer list.
#[test]
fn the_plan_unfolds_under_the_line_that_counts_it() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.connected = true;
    planned(&mut s, 1, "테스트 고치기", "completed");
    planned(&mut s, 2, "빌드 돌리기", "in_progress");
    let folded = dump(&mut s, 40, 14);
    assert!(!folded.contains("1. 테스트 고치기"), "it must start folded:\n{folded}");

    apply(&mut s, &Action::ToggleTodos);
    let screen = dump(&mut s, 40, 14);
    let lines: Vec<&str> = screen.lines().collect();
    let at = lines.iter().position(|l| l.contains("쉬는 중")).expect(&screen);
    assert!(lines[at + 1].contains("1. 테스트 고치기"), "{screen}");
    assert!(lines[at + 2].contains("2. 빌드 돌리기"), "{screen}");
    // Everything below the plan is where it always was.
    assert!(lines[lines.len() - 2].starts_with('─'), "the rule moved:\n{screen}");
    assert!(lines[lines.len() - 1].contains("일반"), "the bottom bar moved:\n{screen}");
}

/// **The `/github` screen draws both rows and never the token.** A form whose pure side is right
/// is no use if the widget does not put it on screen, and a token on screen is a token in every
/// screenshot and scrollback of this session.
#[test]
fn the_github_screen_shows_both_rows_and_hides_the_token() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.github_form = Some(zyris_code::githubform::Form::new(Some("ruma".into()), None));
    let screen = dump(&mut s, 80, 20);
    assert!(screen.contains("ruma"), "the connected account is missing:\n{screen}");
    assert!(screen.contains("리뷰 계정"), "the reviewer row is missing:\n{screen}");

    // Paste a token into the reviewer row and it must not appear.
    apply(&mut s, &Action::FormNext);
    apply(&mut s, &Action::Paste("github_pat_11ABCDE_secretsecret".into()));
    let screen = dump(&mut s, 80, 20);
    assert!(!screen.contains("secretsecret"), "the token was drawn:\n{screen}");
    assert!(screen.contains("github_pat_"), "the token's kind is not shown:\n{screen}");
}

/// **The person's row takes no typing.** Letting it swallow keys would look like a field that
/// never fills, and the keys would be gone.
#[test]
fn typing_on_the_account_row_of_the_github_screen_goes_nowhere() {
    let mut s = State::new();
    s.github_form = Some(zyris_code::githubform::Form::new(None, None));
    apply(&mut s, &Action::Insert('x'));
    let form = s.github_form.as_ref().expect("the screen closed");
    assert!(form.token.text.is_empty(), "{:?}", form.token.text);
}

/// Esc closes it, and Enter on the reviewer row asks the I/O side for what was pasted.
#[test]
fn the_github_screen_hands_a_pasted_token_to_the_io_side() {
    let mut s = State::new();
    s.github_form = Some(zyris_code::githubform::Form::new(Some("ruma".into()), None));
    apply(&mut s, &Action::FormNext);
    apply(&mut s, &Action::Paste("github_pat_abc".into()));
    apply(&mut s, &Action::FormConfirm);
    assert_eq!(
        s.github_out,
        Some(zyris_code::githubform::Ask::SetReviewer("github_pat_abc".into()))
    );
    apply(&mut s, &Action::FormCancel);
    assert!(s.github_form.is_none(), "Esc must close it");
}

/// **Signing in must not freeze the app.** Device flow is a wait of up to fifteen minutes, and
/// running it on the draw loop took the whole screen down — including the code it was waiting on.
/// The code arrives as a frame instead, from a task off the loop.
#[test]
fn the_device_code_reaches_the_github_screen_as_a_frame() {
    use zyris_code::app::GithubNews;
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.github_form = Some(zyris_code::githubform::Form::new(None, None));
    apply(&mut s, &Action::FormConfirm);
    assert_eq!(s.github_out, Some(zyris_code::githubform::Ask::LoginUser));

    apply(
        &mut s,
        &Action::Frame(AppFrame::Github(GithubNews::Code {
            code: "WXQR-7KBD".into(),
            uri: "https://github.com/login/device".into(),
        })),
    );
    let screen = dump(&mut s, 80, 24);
    assert!(screen.contains("WXQR-7KBD"), "the code is not on screen:\n{screen}");
    assert!(screen.contains("github.com/login/device"), "{screen}");
    // And the address is Ctrl+clickable, like the enrolment window's.
    let link = s
        .screen_links
        .iter()
        .find(|l| l.url == "https://github.com/login/device")
        .expect("the address was not registered as a link");
    assert_eq!(s.link_at(link.start, link.row).as_deref(), Some("https://github.com/login/device"));
}

/// When it settles, the code goes and the answer takes its place.
#[test]
fn the_github_screen_takes_the_answer_when_the_sign_in_settles() {
    use zyris_code::app::GithubNews;
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    let mut form = zyris_code::githubform::Form::new(None, None);
    form.pending = Some(("WXQR-7KBD".into(), "https://github.com/login/device".into()));
    form.busy = true;
    s.github_form = Some(form);
    apply(
        &mut s,
        &Action::Frame(AppFrame::Github(GithubNews::Settled {
            note: "이었습니다".into(),
            worked: true,
        })),
    );
    let form = s.github_form.as_ref().expect("the screen closed");
    assert!(form.pending.is_none(), "the spent code is still up");
    assert!(!form.busy);
    let screen = dump(&mut s, 80, 24);
    assert!(!screen.contains("WXQR-7KBD"), "{screen}");
    assert!(screen.contains("이었습니다"), "{screen}");
}

/// **Closing the screen must not lose the answer.** Esc closes it while the sign-in carries on in
/// the background, so what it has to say goes where everything without a window goes.
#[test]
fn an_answer_that_arrives_after_the_screen_closed_is_still_said() {
    use zyris_code::app::GithubNews;
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    apply(
        &mut s,
        &Action::Frame(AppFrame::Github(GithubNews::Settled {
            note: "이었습니다".into(),
            worked: true,
        })),
    );
    let screen = dump(&mut s, 80, 14);
    assert!(screen.contains("이었습니다"), "{screen}");
}

/// **A form takes the keys, never the server's news.**
///
/// Both forms returned early for every action, frames included — so while one was open, timeline
/// events were dropped and `last_cursor` stopped advancing, which puts a resume in the wrong
/// place. It is also how the GitHub screen's own device code failed to reach it.
#[test]
fn a_form_being_open_does_not_swallow_what_the_server_says() {
    for open_a_form in [0, 1] {
        let mut s = State::new();
        if open_a_form == 0 {
            s.new_project = Some(zyris_code::newproject::Form::new());
        } else {
            s.github_form = Some(zyris_code::githubform::Form::new(None, None));
        }
        apply(
            &mut s,
            &Action::Frame(AppFrame::Event {
                cursor: 42,
                entry: Some(Entry { seq: 42, kind: EntryKind::Agent("들어온 말".into()) }),
                todo: None,
            }),
        );
        assert_eq!(s.last_cursor, Some(42), "the resume position was lost (form {open_a_form})");
        assert_eq!(s.timeline.items().len(), 1, "the event was dropped (form {open_a_form})");
    }
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
    assert!(screen.contains("한 번 더"), "no quit hint:\n{screen}");
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

/// **The palette reaches the screen.** The dark text is nearly the colour of a light terminal's
/// paper (1.19:1), and this app paints no background of its own — so if the theme did not actually
/// change what is drawn, a light terminal would still show words on words.
#[test]
fn the_light_theme_actually_changes_what_is_drawn() {
    use zyris_code::theme::{self, Theme};

    let mut s = State::new();
    said(&mut s, 1, EntryKind::User("안녕하세요".into()));

    theme::set(Theme::Dark);
    let (dark_text, dark_colours) = dump_with_colours(&mut s, 40, 10);

    theme::set(Theme::Light);
    let (light_text, light_colours) = dump_with_colours(&mut s, 40, 10);

    theme::set(Theme::Dark);
    assert_eq!(dark_text, light_text, "only the colours change, never the layout");
    assert_ne!(dark_colours, light_colours, "the palette never reached the cells");
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
    assert_eq!(idle_a, idle_b, "the dot blinks while idle");

    s.running = true;
    s.tick = 0;
    let on = cell_fg(&mut s, 40, H, DOT_X, y);
    s.tick = 8;
    let off = cell_fg(&mut s, 40, H, DOT_X, y);
    assert_ne!(on, off, "the dot does not blink while working");
}

/// The mode and agent sit at the left of the bottom bar, in a fixed spot.
#[test]
fn the_mode_and_agent_sit_at_the_left_of_the_bottom_bar() {
    let mut s = State::new();
    s.agent = "Main Agent".into();
    let screen = dump(&mut s, 40, 10);
    let bottom = screen.lines().last().unwrap();
    assert!(bottom.trim_start().starts_with("normal"), "the mode is not flush left: {bottom:?}");
    assert!(bottom.contains("Main Agent"), "no agent: {bottom:?}");
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
    // activity line, letting an empty bottom bar pass as green.
    for expected in ["plan", "work", "job", "normal"] {
        for a in on_key(&s, key) {
            apply(&mut s, &a);
        }
        let screen = dump(&mut s, 80, 24);
        let bar = screen.lines().last().unwrap_or_default().trim_start();
        assert!(
            bar.starts_with(expected),
            "the status bar does not start with '{expected}': {bar:?}"
        );
    }
}

/// Clicking a work card's head must fold and unfold it, and its reasoning chip must be a separate
/// target. If the coordinate transform is off, the wrong line gets hit.
#[test]
fn clicking_a_work_card_toggles_it() {
    use zyris_code::rows::Fold;

    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry { seq: 1, kind: EntryKind::WorkStart("작업".into()) }),
            todo: None,
        }),
    );
    // Streaming reasoning gives the card a chip, so there are two targets to tell apart.
    apply(
        &mut s,
        &Action::Frame(AppFrame::Delta {
            kind: zyris_attacca::ZDeltaKind::Reasoning,
            text: "생각".into(),
        }),
    );
    // **The stretch has to be the one being worked on**, or it draws folded into a single line and
    // there is no chip on screen to be a second target.
    apply(&mut s, &Action::Frame(AppFrame::Status { running: true }));
    // It must be drawn once so the widget records coordinates and card positions.
    let _ = dump(&mut s, 60, 12);

    let mut heads: Vec<(usize, i64)> = s.view_cards.iter().map(|(r, q)| (*r, *q)).collect();
    heads.sort();
    assert_eq!(heads.len(), 2, "the card head and its chip must both be clickable: {heads:?}");
    let (row, seq) = heads[0];
    assert_eq!(seq, 1, "the first clickable head is the card: {heads:?}");
    assert_ne!(heads[1].1, 1, "the chip must be a target of its own: {heads:?}");

    let click = |s: &mut State, row: usize| {
        let (ox, oy) = s.view_origin;
        let y = oy + (row - s.view_top) as u16;
        apply(s, &Action::Press(ox + 1, y));
        apply(s, &Action::Release);
    };

    // **A running card draws open**, so the first click folds it.
    click(&mut s, row);
    assert_eq!(s.folds[&seq], Fold { open: false, user_touched: true }, "a click must fold it");
    click(&mut s, row);
    assert!(s.folds[&seq].open, "clicking again must unfold it");
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
            todo: None,
        }),
    );
    let _ = dump(&mut s, 60, 12);

    let (ox, oy) = s.view_origin;
    apply(&mut s, &Action::Press(ox, oy));
    apply(&mut s, &Action::DragTo(ox + 10, oy));
    let selected = s.selection.clone().expect("the selection was not taken");
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
            todo: None,
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
            todo: None,
        }),
    );
    let _ = dump(&mut s, 60, 12);

    let (ox, oy) = s.view_origin;
    apply(&mut s, &Action::Press(ox, oy));
    apply(&mut s, &Action::DragTo(ox + 10, oy));
    apply(&mut s, &Action::Release);

    assert!(s.selection.is_some(), "the selection vanished after releasing");
    assert!(s.drag.is_some(), "the inverted marker must survive too");
    assert!(!s.dragging, "the button must be released");
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
            todo: None,
        }),
    );
    let _ = dump(&mut s, 60, 12);

    let (ox, oy) = s.view_origin;
    apply(&mut s, &Action::Press(ox, oy));
    apply(&mut s, &Action::DragTo(ox + 6, oy));
    let before = s.selection.clone();
    apply(&mut s, &Action::Release);
    apply(&mut s, &Action::DragTo(ox + 20, oy));
    assert_eq!(s.selection, before, "after releasing it must not grow");
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
                todo: None,
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
    assert!(
        before.is_some(),
        "the selection was not taken: rows={:?}",
        &s.rows_cache.plain()[s.view_top..]
    );

    apply(&mut s, &Action::Wheel(2));
    let _ = dump(&mut s, 60, 10);
    assert_eq!(s.selection, before, "scrolling the wheel must not drop the selection");
}

/// The highlight must cover only the selected columns. If the whole row were washed, what's
/// selected and what's shown would differ.
#[test]
fn the_highlight_covers_only_the_selected_columns() {
    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry { seq: 1, kind: EntryKind::Agent("abcdefghij".into()) }),
            todo: None,
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
    // Theme-independent on purpose: `the_light_theme_actually_changes_what_is_drawn` flips the
    // global theme while this runs in parallel, so comparing against a live `selection_bg()`
    // would race. The selection wash is never a default background and the untouched cells
    // keep it, so checking Reset vs non-Reset is stable under any theme.
    let bg = |x: u16| buf[(x, y)].style().bg;
    assert_ne!(bg(ox), Some(Color::Reset), "the first selected cell must be washed");
    assert_ne!(bg(ox + 3), Some(Color::Reset), "the last selected cell is washed too");
    assert_eq!(bg(ox + 8), Some(Color::Reset), "an unselected cell must not be washed");
}

/// Any other input drops the selection — it is anchored to the screen, so once the person
/// types or moves, it points at stale text.
#[test]
fn typing_drops_the_selection() {
    let mut s = State::new();
    apply(
        &mut s,
        &Action::Frame(AppFrame::Event {
            cursor: 1,
            entry: Some(Entry {
                seq: 1, kind: EntryKind::Agent("안녕하세요 반갑습니다".into())
            }),
            todo: None,
        }),
    );
    let _ = dump(&mut s, 60, 12);

    let (ox, oy) = s.view_origin;
    apply(&mut s, &Action::Press(ox, oy));
    apply(&mut s, &Action::DragTo(ox + 10, oy));
    apply(&mut s, &Action::Release);
    assert!(s.selection.is_some(), "the drag must select before an input arrives");
    assert!(s.drag.is_some(), "the drag range is still alive after release");

    apply(&mut s, &Action::Insert('a'));
    assert!(s.selection.is_none(), "typing must drop the selection");
    assert!(s.drag.is_none(), "the drag range must drop with it");
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
        todo: None,
    }
}

/// When a question arrives, answering mode turns on by itself — the turn is blocked, so there's no reason to open it manually.
#[test]
fn a_question_opens_for_answering_and_shows_its_options() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));
    assert!(s.asking.is_some(), "it did not enter answering mode");

    let screen = dump(&mut s, 70, 16);
    assert!(screen.contains("어느 쪽으로 갈까요?"), "\n{screen}");
    assert!(screen.contains("A안"), "\n{screen}");
    assert!(screen.contains("빠르다"), "the description must be shown too\n{screen}");
    assert!(screen.contains("직접 입력"), "no free-input alternative\n{screen}");
}

/// **A question with no options is a question all the same.** The tool asks for a free-text step by
/// leaving `options` out, and that is what an open-ended question looks like on the wire — drawn as
/// anything but an answerable screen, there is no way to reply to it.
///
/// It arrives here the way the reported one did: with its wait already run out. A `timeout` result
/// is an ordinary success meaning "nobody replied yet", so the question is still open and the
/// screen has to come up — reopening the thread is the only way back to it.
#[test]
fn an_open_ended_question_whose_wait_ran_out_is_still_answerable() {
    let asked = AppFrame::Event {
        cursor: 1,
        entry: zyris_code::event::entry_from(&zyris_attacca::ZSessionEvent {
            seq: 1,
            cursor: 1,
            kind: "tool_call".into(),
            payload: serde_json::json!({
                "kind": "tool_call", "name": "question",
                "arguments": {"questions": [
                    {"header": "푸시 알림 대상", "question": "계정 UUID를 알려주세요"}
                ]},
                "result": {"status": "timeout", "waited_secs": 600}, "error": null
            }),
            created_at: None,
        }),
        todo: None,
    };

    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    apply(&mut s, &Action::Frame(asked));
    assert!(s.asking.is_some(), "a question that timed out is still waiting to be answered");

    let screen = dump(&mut s, 70, 16);
    assert!(screen.contains("계정 UUID를 알려주세요"), "\n{screen}");
    assert!(screen.contains("직접 입력"), "an open-ended question is all free input\n{screen}");
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
        panic!("no {want:?} row: {:?}", s.asking.as_ref().map(|(_, a)| a.rows()));
    };

    press(&mut s, KeyCode::Down); // to option B
    press(&mut s, KeyCode::Enter); // select it

    // Once all questions are asked, it moves to the review screen, and submit lives only there.
    go_to(&mut s, Act::Next);
    press(&mut s, KeyCode::Enter);
    go_to(&mut s, Act::Submit);
    press(&mut s, KeyCode::Enter);

    assert!(s.asking.is_none(), "submitting closes the question");
    assert!(s.submit_now, "the send-now flag must be set");
    assert!(
        s.input.text.contains("어느 쪽으로 갈까요?"),
        "the question must be carried: {}",
        s.input.text
    );
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
    assert!(screen.contains("건너뛰기"), "no skip row\n{screen}");
    assert!(!screen.contains("제출"), "submit appears on the question screen\n{screen}");
    assert!(screen.contains("✎ 직접 입력"), "no free-input row\n{screen}");
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
    assert_eq!(s.input.text, "", "a keystroke leaked into the input during a question");
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
    assert_eq!(on_key(&s, left), vec![Action::Left], "with text present, it moves the cursor");
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
            todo: None,
        }),
    );
    s.picker = Some(Picker::projects(
        vec![("p1".into(), "기본 프로젝트".into(), true), ("p2".into(), "zyris".into(), false)],
        zyris_code::lang::Lang::Ko,
    ));

    let screen = dump(&mut s, 70, 16);
    assert!(screen.contains("프로젝트"), "\n{screen}");
    assert!(screen.contains("＋ 새 프로젝트"), "no create row\n{screen}");
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

/// The popup panel overlays the conversation with its title in the border, and
/// while it is up keys scroll or close it instead of typing into the input.
#[test]
fn the_panel_overlays_the_conversation_and_takes_the_keys() {
    use crossterm::event::KeyCode;
    use zyris_code::app::on_key;
    use zyris_code::app::run_command;

    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.mode = zyris_code::mode::Mode::Job;
    run_command(&mut s, "/mode");

    let screen = dump(&mut s, 70, 18);
    assert!(screen.contains("모드"), "no title\n{screen}");
    assert!(screen.contains('❯'), "the current mode is not marked\n{screen}");
    assert!(screen.contains("job"), "the current mode name is missing\n{screen}");
    assert!(screen.contains("Esc 닫기"), "no close hint\n{screen}");

    // While the panel is up, typed characters must not leak into the input box.
    for a in on_key(&s, key(KeyCode::Char('x'))) {
        apply(&mut s, &a);
    }
    assert_eq!(s.input.text, "");

    // ↓ scrolls the panel, Esc closes it.
    for a in on_key(&s, key(KeyCode::Down)) {
        apply(&mut s, &a);
    }
    assert_eq!(s.panel.as_ref().unwrap().scroll, 1);
    for a in on_key(&s, key(KeyCode::Esc)) {
        apply(&mut s, &a);
    }
    assert!(s.panel.is_none(), "Esc did not close the panel");
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
    assert!(screen.contains("새 프로젝트"), "no title\n{screen}");
    assert!(screen.contains("이름"), "no name field\n{screen}");
    assert!(screen.contains("설명"), "no description field\n{screen}");
    assert!(screen.contains("Enter 만들기"), "no hint\n{screen}");

    // Typed characters go to the form's name field — they must not leak into the input box below.
    for a in on_key(&s, key(KeyCode::Char('가'))) {
        apply(&mut s, &a);
    }
    assert_eq!(s.new_project.as_ref().unwrap().name.text, "가");
    assert_eq!(s.input.text, "");
    let screen = dump(&mut s, 70, 16);
    assert!(screen.contains('가'), "the typed characters are not on screen\n{screen}");

    // Esc closes the form.
    for a in on_key(&s, key(KeyCode::Esc)) {
        apply(&mut s, &a);
    }
    assert!(s.new_project.is_none(), "Esc did not close the form");
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
            zyris_code::picker::ThreadStatus::Running,
        )],
        zyris_code::lang::Lang::Ko,
    ));
    let screen = dump(&mut s, 70, 16);
    for line in screen.lines() {
        assert!(zyris_code::markdown::display_width(line) <= 70, "it exceeded the width: {line:?}");
    }
    assert!(screen.contains('…'), "no marker saying it was cut\n{screen}");
}

fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
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
    assert!(on_key(&s, key(KeyCode::Right)).is_empty(), "→ must do nothing");
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
    assert!(s.picker.is_some(), "it must not close at the session level");

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
                todo: None,
            }),
        );
    }
    s.picker = Some(Picker::projects(
        vec![("p1".into(), "기본".into(), true)],
        zyris_code::lang::Lang::Ko,
    ));

    let screen = dump(&mut s, 70, 16);
    for line in screen.lines() {
        assert!(
            zyris_code::markdown::display_width(line) <= 70,
            "it exceeded the width: {line:?}\n{screen}"
        );
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
            todo: None,
        }),
    );
    let screen = dump(&mut s, 70, 12);
    assert!(screen.contains("✎ 내가 쓴 답"), "no marker for a typed answer\n{screen}");
    assert!(!screen.contains("직접 입력: 내가 쓴 답"), "the preamble is still shown\n{screen}");
    assert!(screen.contains("A안 (설명)"), "the choice must stay as it was\n{screen}");
}

/// The active question must live only in the bottom panel. Drawn again in the conversation area, it would appear twice.
#[test]
fn the_active_question_is_not_drawn_twice() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));
    let screen = dump(&mut s, 70, 18);
    let count = screen.matches("어느 쪽으로 갈까요?").count();
    assert_eq!(count, 1, "the question was drawn {count} times\n{screen}");
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
    assert!(!rows.contains(&RowKind::Action(Act::Submit)), "submit appears on the question screen");

    // Nothing chosen → skip; something chosen → next.
    assert!(rows.contains(&RowKind::Action(Act::Skip)), "with nothing chosen it must be skip");
    apply(&mut s, &Action::AskConfirm); // select the first option
    let rows = s.asking.as_ref().unwrap().1.rows();
    assert!(rows.contains(&RowKind::Action(Act::Next)), "after choosing it must be next");

    // next → review
    let last = rows.len() - 1;
    s.asking.as_mut().unwrap().1.cursor = last;
    apply(&mut s, &Action::AskConfirm);
    assert!(s.asking.as_ref().unwrap().1.in_review(), "it must be the review screen");

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
    assert!(screen.contains("내가 쓴 답"), "what was typed is not visible\n{screen}");
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
    assert!(screen.contains("여기에 직접 적으세요"), "no hint\n{screen}");
}

/// You must be able to answer even after restarting — a question re-read from history must open too.
///
/// The server keeps waiting until the question is answered. If a replayed question doesn't open, there's no way to answer.
#[test]
fn a_pending_question_from_history_opens_for_answering() {
    let mut s = State::new();
    // Same path as re-reading a session: replay the events as they were.
    apply(&mut s, &Action::Frame(question_event(7, serde_json::Value::Null)));
    assert!(s.asking.is_some(), "the replayed question did not open");

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

/// Usage lives on the bottom bar's right edge — the sidebar that used to hold it is gone.
#[test]
fn the_bottom_bar_shows_credits_context_and_tokens() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.usage = zyris_code::usage::Usage {
        model: Some("claude-opus-5-200k".into()),
        context_tokens: Some(132_800),
        total_tokens: Some(11_400_000),
        credits_used: Some("5.0%".into()),
    };
    let screen = dump(&mut s, 90, 16);
    let bar = screen.lines().last().unwrap_or_default();
    assert!(bar.contains("크레딧 5.0%"), "credits missing: {bar:?}");
    assert!(bar.contains("컨텍스트 66% (132.8k/200k)"), "context missing: {bar:?}");
    assert!(bar.contains("총 토큰 11.4M"), "tokens missing: {bar:?}");
}

/// Context is **percent used (used / max)** — a single number can't tell whether it's roomy or full.
#[test]
fn the_bottom_bar_shows_context_as_percent_used_over_max() {
    let mut s = State::new();
    s.usage = zyris_code::usage::Usage {
        model: Some("gpt-4o-128k".into()),
        context_tokens: Some(64_000),
        ..Default::default()
    };
    let screen = dump(&mut s, 90, 16);
    let bar = screen.lines().last().unwrap_or_default();
    assert!(bar.contains("50% (64k/128k)"), "it is not percent (used/max):\n{screen}");
}

/// **For an unknown model, don't invent a limit.** Showing a guessed number makes it look true.
#[test]
fn an_unknown_model_shows_no_limit() {
    let mut s = State::new();
    s.usage = zyris_code::usage::Usage {
        model: Some("어느-새-모델".into()),
        context_tokens: Some(1_000),
        ..Default::default()
    };
    let screen = dump(&mut s, 90, 16);
    let bar = screen.lines().last().unwrap_or_default();
    assert!(bar.contains("1k"), "{bar:?}");
    assert!(!bar.contains("%"), "it printed a percentage for a limit it does not know: {bar:?}");
}

/// The usage block is dropped when it doesn't fit — mode·agent comes first.
#[test]
fn usage_is_dropped_when_the_bottom_bar_is_too_narrow() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.usage = zyris_code::usage::Usage {
        model: Some("claude-opus-5-200k".into()),
        context_tokens: Some(132_800),
        total_tokens: Some(11_400_000),
        credits_used: Some("5.0%".into()),
    };
    let screen = dump(&mut s, 40, 12);
    let bar = screen.lines().last().unwrap_or_default();
    assert!(bar.contains("일반"), "mode·agent must stay: {bar:?}");
    assert!(!bar.contains("크레딧"), "usage leaked onto a line too narrow for it: {bar:?}");
}

/// **The bottom bar says when there are unsent messages.** If it doesn't announce what it's holding, the user believes it was sent.
#[test]
fn the_bottom_bar_says_how_many_messages_are_waiting() {
    let mut s = State::new();
    // Attached — nothing is sent, or queued to be sent, before the first connection.
    s.ever_connected = true;
    s.lang = zyris_code::lang::Lang::Ko;
    s.agent = "Main Agent".into();
    s.running = true;
    apply(&mut s, &Action::Submit("나중에 보낼 말".into()));
    let screen = dump(&mut s, 60, 12);
    let bottom = screen.lines().last().unwrap();
    assert!(bottom.contains("대기 1개"), "the queued marker is missing: {bottom:?}");

    // When the queue empties, the indicator disappears too.
    s.queued.clear();
    let screen = dump(&mut s, 60, 12);
    assert!(
        !screen.contains("대기"),
        "the queue is empty but the marker is still shown:\n{screen}"
    );
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
            todo: None,
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
                    action: "src/app.rs".into(),
                    state: zyris_code::tool_view::ToolState::Ok,
                    detail: zyris_code::tool_view::Detail::Diff(Diff {
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
            todo: None,
        }),
    );
    s.folds.insert(1, Fold { open: true, user_touched: true });
    s
}

fn expand_the_tool_row(state: &mut State) {
    use zyris_code::rows::Fold;
    state.folds.insert(2, Fold { open: true, user_touched: true });
}

/// Even folded, how much changed must be visible.
#[test]
fn a_file_edit_shows_how_many_lines_changed() {
    let mut s = state_with_edit_tool();
    let screen = dump(&mut s, 80, 24);
    assert!(screen.contains("+12"), "the added-line count must be shown:\n{screen}");
    assert!(screen.contains("−3"), "the deleted-line count must be shown:\n{screen}");
    assert!(screen.contains("src/app.rs"), "it must show which file:\n{screen}");
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
    assert!(screen.contains("+새 줄"), "added lines are not visible:\n{screen}");
    assert!(screen.contains("-옛 줄"), "removed lines are not visible:\n{screen}");

    let add = colour_of_line_containing(&screen, &colours, "+새 줄");
    let del = colour_of_line_containing(&screen, &colours, "-옛 줄");
    assert_eq!(add, Some(zyris_code::theme::diff_add()), "added lines are not green:\n{screen}");
    assert_eq!(del, Some(zyris_code::theme::diff_del()), "removed lines are not red:\n{screen}");
    assert_ne!(add, del, "additions and deletions in the same colour cannot be told apart");
}

/// **When there's a diff, show the diff instead of the raw JSON dump.** Both would make the screen twice as long,
/// and what people read is the diff.
#[test]
fn an_expanded_edit_shows_the_diff_instead_of_the_raw_json() {
    let mut s = state_with_edit_tool();
    expand_the_tool_row(&mut s);
    let screen = dump(&mut s, 80, 24);
    assert!(!screen.contains("인자"), "the raw JSON came out alongside the diff:\n{screen}");
}
/// While a command runs, **what is running** must be shown. Since `exec` only reports once at completion,
/// without this the person waits up to 55 seconds in the dark.
#[test]
fn a_running_command_is_named_in_the_activity_line() {
    let mut s = State::new();
    s.connected = true;
    s.running = true;
    apply(&mut s, &Action::Frame(AppFrame::ExecStart { id: 1, command: "cargo build -j2".into(), session: None }));
    let screen = dump(&mut s, 80, 12);
    assert!(screen.contains("cargo build -j2"), "what is running is not visible:\n{screen}");
    assert!(
        !screen.contains("작업 중…"),
        "it generalised even though a specific reason was known:\n{screen}"
    );
}

/// It disappears when done. If it lingered, it would overlap the next one.
#[test]
fn a_finished_command_leaves_the_activity_line() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.connected = true;
    s.running = true;
    apply(&mut s, &Action::Frame(AppFrame::ExecStart { id: 1, command: "cargo build".into(), session: None }));
    apply(&mut s, &Action::Frame(AppFrame::ExecDone { id: 1 }));
    let screen = dump(&mut s, 80, 12);
    assert!(!screen.contains("cargo build"), "a finished command is still shown:\n{screen}");
    assert!(
        screen.contains("작업 중…"),
        "the turn is still running but nothing is said:\n{screen}"
    );
}

/// **Only clear the one that finished.** Clearing a running one because a different id finished would make the screen lie.
#[test]
fn finishing_another_command_does_not_clear_the_running_one() {
    let mut s = State::new();
    s.connected = true;
    s.running = true;
    apply(&mut s, &Action::Frame(AppFrame::ExecStart { id: 2, command: "cargo test".into(), session: None }));
    apply(&mut s, &Action::Frame(AppFrame::ExecDone { id: 1 }));
    let screen = dump(&mut s, 80, 12);
    assert!(screen.contains("cargo test"), "the wrong id was cleared:\n{screen}");
}

/// The elapsed time must show so you know it's still running. The test controls the clock.
#[test]
fn the_activity_line_counts_the_seconds() {
    use std::time::{Duration, Instant};
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.connected = true;
    s.running = true;
    apply(&mut s, &Action::Frame(AppFrame::ExecStart { id: 1, command: "sleep 30".into(), session: None }));
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
    assert!(screen.contains("Stopping"), "the stop request is not shown:\n{screen}");
    assert!(screen.contains("Ctrl+C quits"), "it must say what the next Ctrl+C does:\n{screen}");
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

/// **Approving is the moment this computer changes hands**, so the window that shows the code is
/// where whose account it is has to be said. Afterwards there is nothing left to warn about.
///
/// It also draws whole. Both sentences here are longer than the box, and the first one used to run
/// off the right border and simply stop — reported 2026-08-14 as the notice at the top being cut.
#[test]
fn the_enroll_window_says_whose_account_to_approve_with_and_says_it_whole() {
    for lang in [zyris_code::lang::Lang::Ko, zyris_code::lang::Lang::En] {
        let mut s = State::new();
        s.lang = lang;
        apply(&mut s, &Action::Frame(AppFrame::Enroll(enroll_view())));
        let screen = dump(&mut s, 80, 30);

        // Every word of both sentences reached the screen — not just the start of them.
        for sentence in [lang.enroll_steps(), lang.enroll_warning()] {
            for word in sentence.split_whitespace() {
                assert!(screen.contains(word), "{word:?} never made it onto the screen\n{screen}");
            }
        }
        // The code and the address are still there — the warning did not push them off.
        assert!(screen.contains("WXQR-7KBD"), "\n{screen}");
        assert!(screen.contains("https://attacca.example/settings/zyris/device"), "\n{screen}");
    }
}

/// **The address in the enrolment window is Ctrl+clickable.**
///
/// Links used to be found only inside the conversation (`view_links`), so the one URL a person has
/// to open — the page where the code goes — was the one URL they could not click. An overlay's
/// links are registered in absolute screen cells as it draws (`State::screen_links`).
#[test]
fn the_address_in_the_enroll_window_can_be_opened() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(AppFrame::Enroll(enroll_view())));
    let screen = dump(&mut s, 80, 24);
    let uri = "https://attacca.example/settings/zyris/device";
    assert!(screen.contains(uri), "{screen}");

    let found = s.screen_links.iter().find(|l| l.url == uri).expect("no link was registered");
    // **Clickable where it is drawn.** Registering a link on a row nothing was painted on would
    // open a URL from a click on whatever is really there.
    assert_eq!(s.link_at(found.start, found.row).as_deref(), Some(uri));
    assert_eq!(s.link_at(found.end - 1, found.row).as_deref(), Some(uri));
    assert_eq!(s.link_at(found.end, found.row), None, "past the end of the link");
    assert_eq!(s.link_at(found.start, found.row + 1), None, "a different row");
}

/// **A closed overlay leaves nothing clickable behind.** The registry is rebuilt every frame, so a
/// cell it used to own goes back to whatever is under it.
#[test]
fn closing_the_enroll_window_takes_its_link_with_it() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(AppFrame::Enroll(enroll_view())));
    let _ = dump(&mut s, 80, 24);
    assert!(!s.screen_links.is_empty());
    apply(&mut s, &Action::EnrollClose);
    let _ = dump(&mut s, 80, 24);
    assert!(s.screen_links.is_empty(), "{:?}", s.screen_links);
}

/// The enrollment code appears in a box in the middle of the screen — not the old stdout box.
#[test]
fn the_enroll_window_shows_the_code_and_the_address() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(AppFrame::Enroll(enroll_view())));
    let screen = dump(&mut s, 80, 24);
    assert!(screen.contains("WXQR-7KBD"), "the code is not visible:\n{screen}");
    assert!(
        screen.contains("attacca.example/settings/zyris/device"),
        "주소가 안 보인다:\n{screen}"
    );
    assert!(screen.contains("Connect to Attacca"), "no title:\n{screen}");
    assert!(screen.contains("Esc close"), "no hint for the closing key:\n{screen}");
}

/// When denied, the situation changes — closing silently would leave the person wondering what happened.
#[test]
fn a_denied_enrollment_says_so_in_the_window() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(AppFrame::Enroll(enroll_view())));
    apply(&mut s, &Action::Frame(AppFrame::EnrollPhase(zyris_code::app::EnrollPhase::Denied)));
    let screen = dump(&mut s, 80, 24);
    assert!(screen.contains("declined"), "the refusal is not shown:\n{screen}");
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
            todo: None,
        }),
    );
    apply(&mut s, &Action::Frame(AppFrame::Enroll(enroll_view())));
    let screen = dump(&mut s, 80, 24);
    assert!(screen.contains("WXQR-7KBD"), "the code is not visible:\n{screen}");
}

// ── list window ────────────────────────────────────────────────────────────────

fn long_session_list(n: usize) -> zyris_code::picker::Picker {
    zyris_code::picker::Picker::sessions(
        "p1".into(),
        "zyris".into(),
        (0..n)
            .map(|i| {
                (format!("s{i}"), format!("쓰레드 {i}"), zyris_code::picker::ThreadStatus::Unknown)
            })
            .collect(),
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
    assert!(screen.contains("개 더"), "the remaining count is missing:\n{screen}");
    assert!(screen.contains('↓'), "no marker for what is left below:\n{screen}");
}

/// When everything fits, there's no marker. Saying "more" when nothing is cut would be a lie.
#[test]
fn a_short_list_shows_no_overflow_mark() {
    let mut s = State::new();
    s.picker = Some(long_session_list(2));
    let screen = dump(&mut s, 80, 24);
    assert!(!screen.contains("개 더"), "nothing was cut but the marker is there:\n{screen}");
}

/// **"New" is ruled off from the list.** Sitting next to it, it would read as one of the sessions.
#[test]
fn the_create_row_is_ruled_off_from_the_sessions() {
    let mut s = State::new();
    s.picker = Some(long_session_list(3));
    let screen = dump(&mut s, 80, 24);
    let lines: Vec<&str> = screen.lines().collect();
    let at = lines.iter().position(|l| l.contains("새 쓰레드")).expect("no create row");
    assert!(lines[at + 1].contains('─'), "no rule:\n{screen}");
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
    assert_eq!(
        create,
        Some(zyris_code::theme::accent()),
        "the create row is not in the accent colour"
    );
    assert_ne!(create, session, "the create row and the sessions share a colour:\n{screen}");
}

/// The default mode has a colour too — grey would make the whole bottom bar read as background.
#[test]
fn the_default_mode_is_not_just_grey() {
    let mut s = State::new();
    let y = 24 - 1;
    assert_eq!(cell_fg(&mut s, 80, 24, 0, y), Some(zyris_code::theme::success()));
}

/// The divider above the input carries the working directory and what git says about it.
///
/// **Without it nobody can tell which checkout the `src/app.rs` on a tool line belongs to** —
/// this machine keeps several side by side.
#[test]
fn the_divider_above_the_input_carries_the_working_directory() {
    let mut state = State::new();
    state.home = Some(std::path::PathBuf::from("/home/ruma"));
    state.cwd = std::path::PathBuf::from("/home/ruma/zyris-code");
    state.repo =
        Some(zyris_code::repo::Repo { branch: "main".into(), staged: 2, ..Default::default() });
    let screen = dump(&mut state, 80, 24);
    assert!(screen.contains("~/zyris-code"), "{screen}");
    assert!(screen.contains("* main +2"), "{screen}");
}

/// **No git, no residue.** On a machine without git the rule must resume right after the path —
/// a dangling separator is what the piece-list design exists to prevent.
#[test]
fn without_git_the_rule_resumes_right_after_the_path() {
    let mut state = State::new();
    state.home = Some(std::path::PathBuf::from("/home/ruma"));
    state.cwd = std::path::PathBuf::from("/home/ruma/zyris-code");
    state.repo = None;
    let screen = dump(&mut state, 80, 24);
    assert!(screen.contains("─ ~/zyris-code ─"), "{screen}");
}

/// A link in an agent answer is wrapped in an OSC 8 hyperlink sequence, so the terminal
/// opens it on Ctrl+click. The escape sequence lives in the cell symbol — this inspects
/// the buffer directly, because `dump` counts the escape bytes as width and skips cells.
#[test]
fn a_link_in_an_answer_is_wrapped_in_osc8() {
    let mut s = State::new();
    said(&mut s, 1, EntryKind::Agent("[문서](https://example.com/x) 끝".into()));
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    term.draw(|f| widgets::draw(f, &mut s)).unwrap();
    let buf = term.backend().buffer().clone();
    let syms: Vec<String> = buf.content.iter().map(|c| c.symbol().to_string()).collect();
    assert!(
        syms.iter().any(|sym| sym.starts_with("\u{1b}]8;;https://example.com/x")),
        "no OSC 8 open sequence: {syms:?}"
    );
    assert!(
        syms.iter().any(|sym| sym.contains("\u{1b}]8;;\u{1b}\\")),
        "no OSC 8 close sequence: {syms:?}"
    );
}

/// A bare URL in plain text is not wrapped — the terminal detects those itself.
#[test]
fn a_bare_url_in_an_answer_is_not_wrapped() {
    let mut s = State::new();
    said(&mut s, 1, EntryKind::Agent("가 https://example.com 나".into()));
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    term.draw(|f| widgets::draw(f, &mut s)).unwrap();
    let buf = term.backend().buffer().clone();
    assert!(
        buf.content.iter().all(|c| !c.symbol().starts_with("\u{1b}]8;;")),
        "a bare URL must not be wrapped in OSC 8"
    );
}

/// Prints a work card built **from real events**, so the shape and every tool renderer can be
/// looked at together. Not a check — run with `--ignored --nocapture`.
#[test]
#[ignore = "prints a card so the layout can be looked at"]
fn show_a_work_card() {
    use zyris_code::rows::{rows, Fold, Folds};
    use zyris_code::timeline::Timeline;

    let ev = |seq: i64, kind: &str, payload: serde_json::Value| zyris_attacca::ZSessionEvent {
        seq,
        cursor: seq,
        kind: kind.into(),
        payload,
        created_at: None,
    };
    let tool = |seq: i64, tool: &str, cap: &str, args, result| {
        ev(
            seq,
            "tool_call",
            serde_json::json!({
                "name": format!("zyris__arch-zyris-code__{cap}__{tool}"),
                "arguments": args,
                "result": result,
            }),
        )
    };

    let events = vec![
        ev(1, "work_summary", serde_json::json!({"content": "위젯 picker 테스트를 배경에서 실행"})),
        ev(
            10,
            "thinking",
            serde_json::json!({
                "content": "rows.rs가 대화 화면의 정본이므로 거기부터 본다.",
                "title": "현재 파일 상태를 읽는 중",
            }),
        ),
        tool(
            11,
            "read",
            "file_io",
            serde_json::json!({"path": "src/picker.rs"}),
            serde_json::json!({
                "stat": {"path": "src/picker.rs"},
                "content": "fn row_line(&self) -> Line {\n    …\n}",
            }),
        ),
        ev(
            12,
            "thinking",
            serde_json::json!({
                "content": "지금은 오른쪽 끝에 있다. 커서 표시 뒤, 레이블 앞으로 옮긴다.",
                "title": "상태 점을 왼쪽으로 옮긴다",
            }),
        ),
        tool(
            13,
            "grep",
            "search",
            serde_json::json!({"pattern": "fn row_line", "glob": "**/*.rs"}),
            serde_json::json!({
                "hits": [{"path": "src/picker.rs", "line": 88, "text": "fn row_line(&self) -> Line {"}],
                "truncated": false,
                "scanned": 128,
            }),
        ),
        tool(
            14,
            "edit",
            "code_edit",
            serde_json::json!({"path": "src/picker.rs"}),
            serde_json::json!({
                "path": "src/picker.rs",
                "added": 1,
                "removed": 1,
                "diff": "-let dot = right(mark);\n+let dot = left(mark);\n",
            }),
        ),
        // Still in flight: no result, no error.
        tool(
            15,
            "exec",
            "terminal",
            serde_json::json!({"command": "cargo test -j1 -p zyris-code", "timeout_ms": 50000}),
            serde_json::Value::Null,
        ),
    ];

    let mut t = Timeline::new();
    for e in &events {
        if let Some(entry) = zyris_code::event::entry_from(e) {
            t.upsert(entry);
        }
    }
    let items = t.items().to_vec();

    let open = Fold { open: true, user_touched: true };
    for keys in [vec![12], vec![10, 11, 12, 13, 14, 15]] {
        println!("─── 펼친 것: {keys:?} ───");
        let folds: Folds = keys.into_iter().map(|k| (k, open)).collect();
        for line in rows(&items, 78, &folds, zyris_code::lang::Lang::Ko).plain() {
            println!("{line}");
        }
    }
}
