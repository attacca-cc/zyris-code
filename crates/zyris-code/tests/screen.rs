//! 화면을 텍스트로 떠서 단언한다. 셀 단언이 못 잡는 것이 있지만, 좌표를 검사하지 않고
//! "무엇이 어느 줄에 있는가"만 보면 레이아웃 회귀는 잡힌다.

use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::Terminal;
use zyris_code::app::{apply, Action, Frame as AppFrame, State};
use zyris_code::event::{Entry, EntryKind};
use zyris_code::markdown::display_width;
use zyris_code::widgets;

/// 나간 바이트를 세는 쓰기 대상. `perf.rs`와 같은 수법 — 진짜 crossterm 백엔드를 메모리
/// 버퍼에 물려 **전각 trailing 칸이 실제로 선로에 나가는가**를 본다. 셀 버퍼만 보면
/// diff가 건너뛴 칸이 보이지 않는다.
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

/// 이스케이프 시퀀스를 벗긴다. 선로에 **무엇이 실제로 쓰였는가**만 남긴다 —
/// trailing 칸이 space로 지워졌는지가 여기서 보인다.
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

/// 화면을 텍스트로 뜬다.
///
/// **전각 문자는 셀 두 칸을 차지하고 ratatui는 뒤 칸을 공백으로 채운다.** 셀을 그대로
/// 이어 붙이면 "안녕"이 "안 녕"이 되어 아무 문자열도 찾을 수 없다 — 폭이 2인 셀을 만나면
/// 다음 칸을 건너뛴다.
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

/// 입력란은 아래에서 셋째 줄이다: 하단 바 · **가름선** · 입력란.
///
/// 하단 바(모드·에이전트)와 입력란 사이의 선은 장식이 아니다. 빈 줄로 두면 모드 표시가
/// 입력란의 꼬리처럼 읽혀 "내가 지금 무슨 모드인가"가 눈에 안 들어온다.
#[test]
fn a_rule_separates_the_input_box_from_the_bottom_bar() {
    let mut s = State::new();
    apply(&mut s, &Action::Insert('안'));
    let screen = dump(&mut s, 40, 10);
    let lines: Vec<&str> = screen.lines().collect();
    assert!(lines[lines.len() - 3].contains('안'), "입력이 그 자리에 없다:\n{screen}");
    assert!(lines[lines.len() - 2].starts_with('─'), "가름선이 없다:\n{screen}");
    // 기본 화면 말은 영어다(`lang::Lang` 기본값). 하단 바의 모드 이름으로 본다.
    assert!(lines[lines.len() - 1].contains("normal"), "맨 아래가 하단 바가 아니다:\n{screen}");
}

/// 긴 입력은 다음 줄로 내려간다. 잘려 나가면 무엇을 치고 있는지 알 수 없다.
#[test]
fn a_long_input_wraps_instead_of_being_cut_off() {
    let mut s = State::new();
    for c in "가나다라마바사아자차카타파하".chars() {
        apply(&mut s, &Action::Insert(c));
    }
    let screen = dump(&mut s, 20, 14);
    let lines: Vec<&str> = screen.lines().collect();
    // 입력란은 하단 바 위에서부터 자란다.
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

/// 셀의 배경색. `cell_fg`와 짝이다.
fn cell_bg(state: &mut State, w: u16, h: u16, x: u16, y: u16) -> Option<ratatui::style::Color> {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| widgets::draw(f, state)).unwrap();
    term.backend().buffer()[(x, y)].style().bg
}

fn said(state: &mut State, seq: i64, kind: EntryKind) {
    apply(state, &Action::Frame(AppFrame::Event { cursor: seq, entry: Some(Entry { seq, kind }) }));
}

/// **사용자가 말한 자리는 배경이 칠해진다.** 그리고 글이 끝난 뒤에도 오른쪽 끝까지
/// 이어져야 한다 — 글자 폭에서 끊기면 밴드가 아니라 얼룩으로 보인다.
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

/// 답변에는 배경이 없다. 다 칠하면 아무것도 구별되지 않는다.
#[test]
fn an_agent_answer_has_no_band() {
    let mut s = State::new();
    s.sidebar_on = false;
    said(&mut s, 1, EntryKind::Agent("그렇습니다".into()));
    assert_ne!(cell_bg(&mut s, 60, 12, 3, 0), Some(zyris_code::theme::USER_BG));
}

/// **기본으로는 페이지 배경을 안 칠한다.** 터미널이 자기 배경을 쓰게 두는 것이다.
///
/// 앱이 칠하면 격자 밖(창 패딩, 격자에 안 들어맞는 남는 픽셀)만 색이 달라져 화면
/// 가장자리에 띠가 생긴다 — 앱이 손댈 수 없는 자리라 칠해서 맞출 방법이 없다.
/// 사람이 고른 배경을 쓰는 편이 어디서나 낫다(`theme::page_bg`).
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

/// **되켤 수 있어야 한다.** 전각 잔상이 실제로 거슬리는 원격 터미널이 있다 — 그때 쓰는
/// 것이 `$ZYRIS_CODE_BG`다. 판정이 순수하므로 환경변수를 흔들지 않고 본다.
#[test]
fn the_page_background_can_be_asked_for_by_name_or_by_hex() {
    use zyris_code::theme::page_bg_from;
    assert_eq!(page_bg_from(None), None, "안 주면 안 칠한다");
    assert_eq!(page_bg_from(Some("")), None, "빈 값도 안 준 것이다");
    assert_eq!(page_bg_from(Some("zyris")), Some(zyris_code::theme::BG));
    assert_eq!(page_bg_from(Some("#101820")), Some(Color::Rgb(0x10, 0x18, 0x20)));
    assert_eq!(page_bg_from(Some("none")), None, "끄는 쪽도 명시할 수 있다");
    // **오타 하나로 앱이 죽으면 안 된다.** 못 읽으면 안 칠하는 것으로 떨어진다.
    assert_eq!(page_bg_from(Some("보라색")), None);
}

/// **자가치유 프레임은 모든 칸을 강제로 다시 내보낸다.** 지우지 않고 덮어써서 전각
/// 글자 뒤 trailing 칸의 잔상을 치운다 — clear가 없으므로 깜빡이지 않는다. diff가
/// 같아도 선로에 실리려면 `AlwaysUpdate`로 심어져야 하고, 한 프레임이면 플래그가 풀린다.
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
        frame.buffer.content.iter().all(|c| {
            c.diff_option == ratatui::buffer::CellDiffOption::AlwaysUpdate
        }),
        "모든 칸이 강제 재출력이어야 한다"
    );

    // 다음 draw는 일반 diff로 돌아간다 — 플래그가 풀렸으니 AlwaysUpdate가 안 남는다.
    let frame2 = term.draw(|f| widgets::draw(f, &mut s)).unwrap();
    assert!(
        frame2.buffer.content.iter().all(|c| {
            c.diff_option == ratatui::buffer::CellDiffOption::None
        }),
        "한 프레임 뒤에는 일반 diff로 돌아와야 한다"
    );
}

/// **배경이 있으면 전각이 좁은 글자로 바뀔 때 trailing 칸이 선로에 나간다.**
/// 'a' 뒤에 공백 하나가 실제로 쓰여야 한다 — 그 공백이 전각의 오른쪽 반쪽을 지운다.
/// (커서가 이미 그 자리에 서 있으므로 커서 이동 없이 space만 나간다.)
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

/// **배경이 없으면 trailing 칸이 선로에 안 나간다 — 잔상의 원인 그 자체다.**
/// 이 테스트는 지금 동작을 고정하는 것이 아니라, `theme::BG`를 전 칸에 깔기 전에는
/// 전각이 좁아질 때 이 칸이 영영 안 지워졌다는 것을 문서로 남긴다.
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

/// 머리글은 없다. 맨 윗줄부터 대화다 — 앱 이름과 디렉터리에 한 줄을 내줄 이유가 없다.
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

/// 그리다가 패닉하면 터미널이 망가진 채 남는다. 좁은 폭에서 특히 잘 난다.
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

/// **"연결됨"은 잠깐만 보인다.** 붙은 직후 상태 줄로 한 번 보여 주고(`Connected`)
/// 저절로 사라진다 — 계속 알릴 이유가 없다. 상태가 없는 평소에는 아무것도 없다.
#[test]
fn a_healthy_connection_is_not_announced_anywhere() {
    let mut s = State::new();
    s.connected = true;
    let screen = dump(&mut s, 40, 10);
    assert!(!screen.contains("연결됨"), "연결 표시가 남아 있다:\n{screen}");
}

/// 끊긴 것은 반드시 말한다 — 조용한 실패가 제일 나쁘다.
#[test]
fn a_broken_connection_is_always_said_out_loud() {
    let mut s = State::new();
    s.connected = false;
    let screen = dump(&mut s, 40, 10);
    assert!(screen.contains("Connecting"), "끊겼는데 아무 말이 없다:\n{screen}");

    // 한국어 화면도 같은 자리에서 말한다.
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.connected = false;
    let screen = dump(&mut s, 40, 10);
    assert!(screen.contains("연결 중"), "{screen}");
}

/// 상태 줄은 아래에서 다섯째다: 하단 바 · 가름선 · 입력란 · 가름선 · **상태 줄**.
const ACTIVITY_FROM_BOTTOM: usize = 5;

/// 지금 무슨 일인지는 대화와 입력란 **사이**에 있다 — 위는 빈 줄, 아래는 입력란의 가름선.
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

/// 종료 예고는 다른 무엇보다 먼저 보여야 한다.
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

/// 한 셀의 글자색. 깜박임은 글자가 아니라 색으로 나타나므로 이걸로 본다.
fn cell_fg(state: &mut State, w: u16, h: u16, x: u16, y: u16) -> Option<ratatui::style::Color> {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| widgets::draw(f, state)).unwrap();
    term.backend().buffer()[(x, y)].style().fg
}

/// 작업 중일 때만 점이 깜박인다. 멈춘 점은 돌아가고 있다는 것을 말해 주지 못하고,
/// 쉬는 중에 깜박이면 뭔가 도는 줄 알게 된다.
#[test]
fn the_dot_blinks_only_while_working() {
    // 점은 상태 줄의 **맨 왼쪽 칸**이다.
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

/// 모드와 에이전트는 하단 바 왼쪽에, 자리가 고정이어야 한다.
#[test]
fn the_mode_and_agent_sit_at_the_left_of_the_bottom_bar() {
    let mut s = State::new();
    s.agent = "Main Agent".into();
    let screen = dump(&mut s, 40, 10);
    let bottom = screen.lines().last().unwrap();
    assert!(bottom.trim_start().starts_with("normal"), "모드가 맨 왼쪽이 아니다: {bottom:?}");
    assert!(bottom.contains("Main Agent"), "에이전트가 없다: {bottom:?}");
}

/// Shift+Tab으로 기본 → 계획 → work → job → 기본.
///
/// **모드를 더하면서 `Mode::next`를 안 고치면 새 모드에 키로는 영영 못 간다** —
/// `/mode`로만 갈 수 있는 모드는 아무도 안 쓴다.
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

/// 하단 바가 **지금 무슨 모드인지 글자로** 말해야 한다. 넷을 눈으로 가리는 길이 그것뿐이다.
#[test]
fn the_status_bar_names_the_mode_it_is_in() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use zyris_code::app::on_key;

    let mut s = State::new();
    let key = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    // **맨 아랫줄만 본다.** 화면 전체에서 찾으면 사이드바나 활동 줄의 다른 글자가 우연히
    // 걸려, 하단 바가 비어 있어도 초록으로 지나간다.
    for expected in ["plan", "work", "job", "normal"] {
        for a in on_key(&s, key) {
            apply(&mut s, &a);
        }
        let screen = dump(&mut s, 80, 24);
        let bar = screen.lines().last().unwrap_or_default().trim_start();
        assert!(bar.starts_with(expected), "하단 바가 '{expected}'로 시작하지 않는다: {bar:?}");
    }
}

/// 작업 카드 머리를 클릭하면 접히고 펴져야 한다. 좌표 변환이 어긋나면 엉뚱한 줄이 걸린다.
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
    // 한 번 그려야 위젯이 좌표와 카드 위치를 남긴다.
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

/// 드래그하면 선택이 잡히고, **놓아도 그대로 남는다** — I/O 자리가 그것을 클립보드로
/// 내보낸다. 떼자마자 지우면 내보낼 것이 없다.
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

    // **놓으면 그대로 남는다.** 클립보드로 내보내는 것은 I/O 자리가 하므로 여기서는
    // 범위가 살아 있는 것까지 본다 — 사라지면 내보낼 것이 없다.
    apply(&mut s, &Action::Release);
    assert_eq!(s.selection.as_deref(), Some(selected.as_str()));
}

/// 움직이지 않은 누름은 선택이 아니다 — 클릭만으로 선택이 잡히면 복사가 오작동한다.
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

/// 마우스를 떼도 선택이 남아야 한다. 떼자마자 사라지면 Ctrl+C를 누를 틈이 없다.
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

/// 뗀 뒤 마우스가 움직여도 범위가 자라면 안 된다.
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

/// 스크롤해도 선택이 날아가면 안 된다 — 내용 좌표라 따라가야 한다.
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
    // 항목 사이에는 빈 줄이 있으므로 여러 줄에 걸쳐 끌어야 글자가 확실히 잡힌다.
    apply(&mut s, &Action::Press(ox, oy));
    apply(&mut s, &Action::DragTo(ox + 6, oy + 2));
    apply(&mut s, &Action::Release);
    let before = s.selection.clone();
    assert!(before.is_some(), "선택이 안 잡혔다: rows={:?}", &s.rows_cache.plain()[s.view_top..]);

    apply(&mut s, &Action::Wheel(2));
    let _ = dump(&mut s, 60, 10);
    assert_eq!(s.selection, before, "휠을 굴렸다고 선택이 날아가면 안 된다");
}

/// 반전은 고른 열만이어야 한다. 줄 통째로 반전되면 고른 것과 보이는 것이 다르다.
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

/// 질문이 오면 저절로 답하기 모드가 된다 — 턴이 막혀 있으니 사람이 따로 열 이유가 없다.
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

/// 이미 답이 간 질문은 다시 열리면 안 된다.
#[test]
fn an_answered_question_does_not_reopen() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(question_event(1, serde_json::json!({"status": "answered"}))));
    assert!(s.asking.is_none());
}

/// 고른 다음 제출 줄에서 Enter를 치면 답이 실리고 곧바로 보낼 상태가 된다.
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
    // 그 조작 줄로 커서를 옮긴다. **횟수를 반드시 제한한다** — 없는 줄을 찾으면
    // 커서가 목록을 순환해 영영 끝나지 않는다. 실제로 이 자리가 무한 루프로 돌면서
    // 테스트 바이너리가 CPU를 100% 먹었다.
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

    press(&mut s, KeyCode::Down); // B안으로
    press(&mut s, KeyCode::Enter); // 고르기

    // 다 물어보면 검토 화면으로 넘어가고, 제출은 거기에만 있다.
    go_to(&mut s, Act::Next);
    press(&mut s, KeyCode::Enter);
    go_to(&mut s, Act::Submit);
    press(&mut s, KeyCode::Enter);

    assert!(s.asking.is_none(), "제출하면 질문이 닫힌다");
    assert!(s.submit_now, "곧바로 보낼 표시가 서야 한다");
    assert!(s.input.text.contains("어느 쪽으로 갈까요?"), "질문을 실어야 한다: {}", s.input.text);
    assert!(s.input.text.contains("B안"), "{}", s.input.text);
}

/// 제출 줄은 언제나 맨 아래에 있고, 질문 UI는 입력란 자리에 그려진다.
#[test]
fn the_question_replaces_the_input_box() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));
    let screen = dump(&mut s, 70, 16);
    // 질문 화면에는 앞뒤로 오가는 것만 있다. 제출은 다 물어본 뒤 검토 화면에만 나온다.
    assert!(screen.contains("건너뛰기"), "건너뛰기 줄이 없다\n{screen}");
    assert!(!screen.contains("제출"), "질문 화면에 제출이 있다\n{screen}");
    assert!(screen.contains("✎ 직접 입력"), "자유 입력 줄이 없다\n{screen}");
    // 질문이 열려 있는 동안 평소 입력 프롬프트는 자리를 내준다.
    let bottom: Vec<&str> = screen.lines().rev().take(3).collect();
    assert!(
        !bottom.iter().any(|l| l.trim_start().starts_with("> ")),
        "입력란이 아직 있다\n{screen}"
    );
}

/// 질문이 열려 있는 동안 글자는 아래 입력란으로 새면 안 된다.
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

/// ← 는 입력란이 비어 있을 때만 목록을 연다. 글자가 있으면 커서 이동이 먼저다.
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

/// 목록이 열리면 키가 그쪽으로 가고, 대화 위에 겹쳐 보인다.
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

    // 목록이 열려 있으면 글자가 입력란으로 새면 안 된다.
    for a in on_key(&s, KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)) {
        apply(&mut s, &a);
    }
    assert_eq!(s.input.text, "");

    // ↓ 는 목록을 움직인다.
    for a in on_key(&s, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)) {
        apply(&mut s, &a);
    }
    assert_eq!(s.picker.as_ref().unwrap().cursor, 2);

    // Esc 는 뒤로가기 액션을 낸다. 닫는 것은 I/O 자리가 한다.
    assert_eq!(on_key(&s, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), vec![Action::PickBack]);
}

/// 두 생성 줄이 서로 다르게 동작한다. **세션은 바로 만들고, 프로젝트는 양식으로 받는다.**
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

/// 새 프로젝트 양식: 제목·이름·설명 칸이 보이고, 친 글자는 이름 칸으로 간다.
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

    // 친 글자는 양식의 이름 칸으로 간다 — 아래 입력란에 새면 안 된다.
    for a in on_key(&s, key(KeyCode::Char('가'))) {
        apply(&mut s, &a);
    }
    assert_eq!(s.new_project.as_ref().unwrap().name.text, "가");
    assert_eq!(s.input.text, "");
    let screen = dump(&mut s, 70, 16);
    assert!(screen.contains('가'), "친 글자가 화면에 없다\n{screen}");

    // Esc는 양식을 닫는다.
    for a in on_key(&s, key(KeyCode::Esc)) {
        apply(&mut s, &a);
    }
    assert!(s.new_project.is_none(), "Esc가 양식을 안 닫았다");
}

/// 세션 제목은 길이가 제멋대로다. 안 자르면 상자를 뚫고 나가 화면이 무너진다.
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

/// 목록은 ← 로 연다. → 는 커서 이동이다.
#[test]
fn left_arrow_opens_the_picker_and_right_does_not() {
    use crossterm::event::KeyCode;
    use zyris_code::app::on_key;

    let s = State::new();
    assert_eq!(on_key(&s, key(KeyCode::Left)), vec![Action::OpenPicker]);
    assert_eq!(on_key(&s, key(KeyCode::Right)), vec![Action::Right]);
}

/// 창 안에서 → 는 먹지 않고, ← 는 뒤로가기다.
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
    // 프로젝트 단계든 세션 단계든 ← 는 같은 액션이다. **무엇이 되는지는 I/O가 정한다** —
    // 세션 단계면 프로젝트 목록으로, 프로젝트 단계면 닫기.
    assert_eq!(on_key(&s, key(KeyCode::Left)), vec![Action::PickBack]);
    assert_eq!(on_key(&s, key(KeyCode::Esc)), vec![Action::PickBack]);
}

/// 세션 단계의 ← 는 **닫기가 아니라 뒤로가기**다.
///
/// `apply`는 picker를 건드리지 않아야 한다 — I/O가 세션→프로젝트로 되돌려 놓은 것을
/// `apply`가 다시 보면 "프로젝트 단계니 닫자"가 되어 뒤로가기가 닫기로 변한다.
#[test]
fn going_back_from_sessions_never_closes_the_picker_in_apply() {
    use zyris_code::picker::{Level, Picker};

    let mut s = State::new();
    s.picker =
        Some(Picker::sessions("p1".into(), "기본".into(), vec![], zyris_code::lang::Lang::Ko));
    apply(&mut s, &Action::PickBack);
    assert!(s.picker.is_some(), "세션 단계에서 닫히면 안 된다");

    // I/O가 프로젝트 목록으로 되돌려 놓은 뒤에도 apply가 닫으면 안 된다.
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

/// 사이드바는 기본으로 켜져 있고 Ctrl+B로 토글된다.
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

/// 태스크는 todo_* 도구 호출에서 모은다 — todo_change 이벤트에는 본문이 없다.
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

/// **왼쪽 칸의 어떤 글도 사이드바 경계선에 닿지 않는다.** 대화만이 아니라 입력란도다.
///
/// 여백을 대화에만 주면 긴 입력이 접혀 내려올 때 글자가 경계선에 딱 붙어 두 칸이
/// 한 덩어리로 보인다.
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

/// 좁은 화면에서는 사이드바를 접는다 — 대화가 먼저다.
#[test]
fn a_narrow_screen_drops_the_sidebar() {
    let mut s = State::new();
    let screen = dump(&mut s, 50, 12);
    assert!(!screen.contains("사용량"), "좁은데 사이드바가 남아 있다\n{screen}");
}

/// 픽커가 열려도 어느 줄도 화면 폭을 넘지 않아야 한다 — 전각이 섞여도.
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

/// 직접 써 넣은 답은 고른 것과 다르게 보여야 한다 — 선택지에 없던 답이라는 사실이 정보다.
#[test]
fn typed_answers_look_different_from_chosen_ones_in_history() {
    let mut s = State::new();
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

/// 활성 질문은 아래 패널에만 있어야 한다. 대화 영역에 또 그리면 두 벌로 보인다.
#[test]
fn the_active_question_is_not_drawn_twice() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));
    let screen = dump(&mut s, 70, 18);
    let count = screen.matches("어느 쪽으로 갈까요?").count();
    assert_eq!(count, 1, "질문이 {count}번 그려졌다\n{screen}");
}

/// 마지막 질문을 지나면 검토 화면이 뜨고, 거기서만 제출/고치기/답하지 않기가 나온다.
#[test]
fn the_review_screen_appears_after_the_last_question() {
    use zyris_code::question::{Act, RowKind};

    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));

    // 질문 화면에는 제출이 없어야 한다 — 아직 안 본 질문을 남긴 채 보내기 쉽다.
    let rows = s.asking.as_ref().unwrap().1.rows();
    assert!(!rows.contains(&RowKind::Action(Act::Submit)), "질문 화면에 제출이 있다");

    // 아무것도 안 고르면 건너뛰기, 고르면 다음.
    assert!(rows.contains(&RowKind::Action(Act::Skip)), "안 골랐으면 건너뛰기여야 한다");
    apply(&mut s, &Action::AskConfirm); // 첫 선택지 고르기
    let rows = s.asking.as_ref().unwrap().1.rows();
    assert!(rows.contains(&RowKind::Action(Act::Next)), "고른 뒤에는 다음이어야 한다");

    // 다음 → 검토
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

/// 직접 적은 내용은 목록에 남아 보여야 한다 — 안 보이면 뭘 썼는지 확인할 길이 없다.
#[test]
fn typed_free_text_stays_visible_in_the_list() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(question_event(1, serde_json::Value::Null)));

    let a = &mut s.asking.as_mut().unwrap().1;
    a.cursor = a.current().free_row();
    apply(&mut s, &Action::AskConfirm); // 타자 시작
    for c in "내가 쓴 답".chars() {
        apply(&mut s, &Action::Insert(c));
    }
    apply(&mut s, &Action::AskConfirm); // 확정

    let screen = dump(&mut s, 70, 18);
    assert!(screen.contains("내가 쓴 답"), "적은 내용이 안 보인다\n{screen}");
}

/// 빈 칸으로 들어가면 무엇을 하는 자리인지 말해 준다.
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

/// 껐다 켜도 답할 수 있어야 한다 — 히스토리에서 되읽은 질문도 열려야 한다.
///
/// 서버는 답을 안 하면 계속 기다린다. 재생된 질문이 안 열리면 답할 길이 없다.
#[test]
fn a_pending_question_from_history_opens_for_answering() {
    let mut s = State::new();
    // 세션을 되읽는 것과 같은 경로: 이벤트를 그대로 다시 흘려보낸다.
    apply(&mut s, &Action::Frame(question_event(7, serde_json::Value::Null)));
    assert!(s.asking.is_some(), "되읽은 질문이 안 열렸다");

    let screen = dump(&mut s, 70, 16);
    assert!(screen.contains("어느 쪽으로 갈까요?"), "\n{screen}");
}

/// 이미 답이 간 질문은 되읽어도 다시 열리면 안 된다.
#[test]
fn an_already_answered_question_from_history_stays_closed() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(question_event(7, serde_json::json!({"status": "answered"}))));
    assert!(s.asking.is_none());
}

/// 사용량의 숫자는 **왼쪽 끝이 한 줄로 맞아야 한다.** 이름 길이가 제각각이라(크레딧 6칸,
/// 컨텍스트 8칸) 그냥 붙이면 계단처럼 어긋나 한눈에 비교가 안 된다.
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
            // 이름 뒤 공백을 건너뛴 자리 = 값이 시작하는 칸. **바이트 자리다** —
            // 글자 수로 자르면 한글 한가운데를 갈라 패닉한다.
            let at = line.find(key).unwrap() + key.len();
            display_width(&line[..at]) + line[at..].chars().take_while(|c| *c == ' ').count()
        })
        .collect();
    assert!(cols.windows(2).all(|w| w[0] == w[1]), "값의 왼쪽 끝이 안 맞는다: {cols:?}\n{screen}");
}

/// 컨텍스트는 **쓴 양 / 담을 수 있는 양**이다. 숫자 하나만으로는 여유로운지 꽉 찼는지 모른다.
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

/// **모르는 모델이면 최대를 지어내지 않는다.** 짐작한 수를 보여주면 그게 맞는 줄 안다.
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

/// **아직 안 보낸 말이 있으면 하단 바가 말한다.** 들고 있는 것을 안 알리면 보냈다고 믿는다.
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

    // 대기열이 비면 그 표시도 사라진다.
    s.queued.clear();
    let screen = dump(&mut s, 60, 12);
    assert!(!screen.contains("대기"), "빈 대기열인데 표시가 남았다:\n{screen}");
}

// ── 파일을 고친 도구의 diff ────────────────────────────────────────────────

/// 화면을 뜨면서 **같은 좌표계로** 각 칸의 전경색도 가져온다.
///
/// `dump`와 따로 뜨면 전각 뒤 칸을 건너뛰는 규칙이 어긋나 글자와 색이 한 칸씩 밀린다.
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

/// 그 글자가 시작하는 칸의 색. 없으면 `None`.
fn colour_of_line_containing(
    screen: &str,
    colours: &[Vec<Option<ratatui::style::Color>>],
    needle: &str,
) -> Option<ratatui::style::Color> {
    let (row, line) = screen.lines().enumerate().find(|(_, l)| l.contains(needle))?;
    let at = line[..line.find(needle)?].chars().count();
    colours.get(row)?.get(at).copied().flatten()
}

/// 파일을 하나 고친 도구가 들어 있는 작업 카드. 카드는 펴져 있고 도구 줄은 접혀 있다.
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

/// 접혀 있어도 무엇이 얼마나 바뀌었는지는 보여야 한다.
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

/// 펼치면 실제로 바뀐 줄이 보이고, 추가와 삭제의 색이 달라야 한다.
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

/// **diff가 있으면 JSON 덤프 대신 diff를 보여준다.** 둘 다 나오면 화면이 두 배로 길어지고
/// 사람이 읽는 것은 diff 쪽이다.
#[test]
fn an_expanded_edit_shows_the_diff_instead_of_the_raw_json() {
    let mut s = state_with_edit_tool();
    expand_the_tool_row(&mut s);
    let screen = dump(&mut s, 80, 24);
    assert!(!screen.contains("인자"), "원본 JSON이 diff와 함께 나왔다:\n{screen}");
}

// ── 사이드바: 도구가 어디서 도는가 ─────────────────────────────────────────

/// 도구가 무엇을 기준으로 도는지 모르면 승인 화면의 상대경로를 읽을 수 없다.
///
/// **테스트를 돌리는 자리와 겹치지 않는 경로를 쓴다.** 기본값이 프로세스의 작업
/// 디렉터리라서, 진짜 경로를 넣으면 대입이 아무 일도 안 해도 통과해 버린다.
#[test]
fn the_sidebar_says_which_directory_the_tools_run_in() {
    let mut s = State::new();
    s.cwd = std::path::PathBuf::from("/srv/checkouts/some-repo");
    let screen = dump(&mut s, 100, 24);
    assert!(screen.contains("some-repo"), "작업 디렉터리가 보여야 한다:\n{screen}");
}

/// 안 보이면 유령 셸이 돈다 — 에이전트가 열어 둔 것을 사람이 모른 채로 남는다.
#[test]
fn open_shells_are_listed() {
    let mut s = State::new();
    s.shells = vec![zyris_code::app::Shell { id: "p1".into(), name: "zsh".into() }];
    let screen = dump(&mut s, 100, 24);
    assert!(screen.contains("zsh"), "열린 셸이 보여야 한다:\n{screen}");
}

/// 빈 절이 자리만 차지하면 안 된다. 사이드바는 좁다.
#[test]
fn no_shell_section_when_nothing_is_open() {
    let mut s = State::new();
    let screen = dump(&mut s, 100, 24);
    assert!(!screen.contains("셸"), "열린 셸이 없으면 절 자체가 없어야 한다:\n{screen}");
}

/// 긴 경로는 끝의 두 조각만 남긴다. 사이드바 폭을 넘기면 잘려서 어디인지 알 수 없다.
#[test]
fn a_long_working_directory_keeps_the_part_that_identifies_it() {
    let mut s = State::new();
    s.cwd = std::path::PathBuf::from("/home/ruma/very/deeply/nested/place/zyris-code");
    let screen = dump(&mut s, 100, 24);
    assert!(screen.contains("place/zyris-code"), "끝 두 조각이 보여야 한다:\n{screen}");
    assert!(!screen.contains("/home/ruma/very"), "앞쪽까지 나오면 넘친다:\n{screen}");
}
/// 명령이 도는 동안 **무엇이 도는지** 보여야 한다. `exec`은 완료될 때 한 번만 결과를
/// 주므로, 여기서 말하지 않으면 사람은 최대 55초를 눈뜬장님으로 기다린다.
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

/// 끝나면 사라진다. 안 사라지면 다음 것과 겹쳐 보인다.
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

/// **끝난 것만 지운다.** 다른 번호가 끝났다고 도는 것을 지우면 화면이 거짓말을 한다.
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

/// 경과 시간이 보여야 "돌고는 있다"를 안다. 시각은 테스트가 정한다.
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

/// **Ctrl+C가 먹었다는 것이 보여야 한다.** 서버가 답할 때까지는 "작업 중"이 그대로라,
/// 그 사이 화면이 아무것도 안 바뀌면 안 눌린 줄 알고 또 누른다.
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

// ── 등록 코드 창 ──────────────────────────────────────────────────────────

fn enroll_view() -> zyris_code::app::EnrollView {
    zyris_code::app::EnrollView {
        code: "WXQR-7KBD".into(),
        uri: "https://attacca.example/settings/zyris/device".into(),
        expires_at: std::time::Instant::now() + std::time::Duration::from_secs(600),
        phase: zyris_code::app::EnrollPhase::Waiting,
    }
}

/// 등록 코드가 화면 가운데 상자로 뜬다 — 예전의 stdout 상자가 아니다.
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

/// 거부되면 사정이 바뀐다 — 조용히 닫히면 사람은 무슨 일인지 모른다.
#[test]
fn a_denied_enrollment_says_so_in_the_window() {
    let mut s = State::new();
    apply(&mut s, &Action::Frame(AppFrame::Enroll(enroll_view())));
    apply(&mut s, &Action::Frame(AppFrame::EnrollPhase(zyris_code::app::EnrollPhase::Denied)));
    let screen = dump(&mut s, 80, 24);
    assert!(screen.contains("declined"), "거부가 안 보인다:\n{screen}");
}

/// 등록 코드 창은 대화 위에 겹친다 — 코드를 보는 중에는 뒤가 안 보여도 된다.
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

// ── 승인 화면 ──────────────────────────────────────────────────────────────

fn leaving_ask() -> zyris_code::app::ToolAsk {
    zyris_code::app::ToolAsk {
        id: 1,
        call: zyris_code::tools::gate::Call::new("code_edit", "edit", "x.rs".into())
            .leaving(Some(std::path::PathBuf::from("/home/ruma/attacca/Cargo.toml"))),
        summary: "/home/ruma/attacca/Cargo.toml".into(),
        expired: false,
    }
}

/// **어디를 만지는지가 이 승인의 전부다.** 안쪽 일은 아무것도 묻지 않으므로,
/// 창이 떴다는 것 자체가 "여기는 밖이다"라는 뜻이다.
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

/// 마감이 지나도 **창은 남고** 사정만 바뀐다.
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

/// 뒤에 몇 개가 기다리는지 말해야 한다 — 하나 답하고 끝난 줄 알면 안 된다.
#[test]
fn the_screen_says_how_many_are_waiting() {
    let mut s = State::new();
    s.lang = zyris_code::lang::Lang::Ko;
    s.pending = Some(leaving_ask());
    s.ask_queue.push_back(leaving_ask());
    let screen = dump(&mut s, 90, 24);
    assert!(screen.contains("뒤에 1개"), "{screen}");
}

/// 승인 창은 입력란 자리를 차지한다. 둘이 같이 뜨면 어디에 답하는지 알 수 없다.
#[test]
fn the_approval_screen_takes_the_place_of_the_input_box() {
    let mut s = State::new();
    apply(&mut s, &Action::Insert('안'));
    assert!(dump(&mut s, 90, 24).contains('안'));

    s.pending = Some(leaving_ask());
    let screen = dump(&mut s, 90, 24);
    assert!(!screen.contains('안'), "입력란이 같이 떠 있다:\n{screen}");
}

// ── 목록 창 ────────────────────────────────────────────────────────────────

fn long_session_list(n: usize) -> zyris_code::picker::Picker {
    zyris_code::picker::Picker::sessions(
        "p1".into(),
        "zyris".into(),
        (0..n).map(|i| (format!("s{i}"), format!("쓰레드 {i}"), false)).collect(),
        zyris_code::lang::Lang::Ko,
    )
}

/// **잘린 쪽에는 몇 개가 더 있는지 적는다.** 안 적으면 목록이 거기서 끝난 줄 안다.
#[test]
fn a_long_list_shows_how_many_are_left() {
    let mut s = State::new();
    s.picker = Some(long_session_list(40));
    let screen = dump(&mut s, 80, 16);
    assert!(screen.contains("개 더"), "남은 개수가 없다:\n{screen}");
    assert!(screen.contains('↓'), "아래로 남았다는 표시가 없다:\n{screen}");
}

/// 다 들어가면 표시줄이 없다. 안 잘렸는데 "더 있다"고 하면 거짓말이다.
#[test]
fn a_short_list_shows_no_overflow_mark() {
    let mut s = State::new();
    s.picker = Some(long_session_list(2));
    let screen = dump(&mut s, 80, 24);
    assert!(!screen.contains("개 더"), "안 잘렸는데 표시가 있다:\n{screen}");
}

/// **"새로 만들기"는 목록과 갈라 놓는다.** 붙여 두면 세션 하나로 읽힌다.
#[test]
fn the_create_row_is_ruled_off_from_the_sessions() {
    let mut s = State::new();
    s.picker = Some(long_session_list(3));
    let screen = dump(&mut s, 80, 24);
    let lines: Vec<&str> = screen.lines().collect();
    let at = lines.iter().position(|l| l.contains("새 쓰레드")).expect("만들 줄이 없다");
    assert!(lines[at + 1].contains('─'), "가름선이 없다:\n{screen}");
}

/// 만들 줄은 **색이 다르다** — 고르는 것이 아니라 만드는 것이다.
#[test]
fn the_create_row_is_coloured_apart_from_the_list() {
    let mut s = State::new();
    s.picker = Some(long_session_list(3));
    // 커서는 만들 줄에 있다. 그 아래 첫 세션과 색이 달라야 한다.
    // 상자는 가운데에 서고(폭 64), 안쪽 글은 `❯ ` 뒤에서 시작한다.
    const LABEL_X: u16 = 8 + 1 + 2;
    let screen = dump(&mut s, 80, 24);
    let top = screen.lines().position(|l| l.contains("새 쓰레드")).unwrap() as u16;
    let create = cell_fg(&mut s, 80, 24, LABEL_X, top);
    let session = cell_fg(&mut s, 80, 24, LABEL_X, top + 2);
    assert_eq!(create, Some(zyris_code::theme::ACCENT), "만들 줄이 강조색이 아니다");
    assert_ne!(create, session, "만들 줄과 세션이 같은 색이다:\n{screen}");
}

/// 기본 모드도 색을 가진다 — 회색이면 하단 바가 통째로 배경처럼 읽힌다.
#[test]
fn the_default_mode_is_not_just_grey() {
    let mut s = State::new();
    let y = 24 - 1;
    assert_eq!(cell_fg(&mut s, 80, 24, 0, y), Some(zyris_code::theme::SUCCESS));
}
