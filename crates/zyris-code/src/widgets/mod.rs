//! 그리기. 위젯은 상태를 props처럼 받아 그리기만 한다 — 여기서 로직을 굴리지 않는다.
//!
//! ```text
//! │   (대화)                            │ 사이드바
//! │ ● 작업 중…                 Esc 정지 │ 지금 무슨 일인가
//! ├─────────────────────────────────────┤
//! │ > 입력                              │ 입력란 (내용에 따라 자란다)
//! ├─────────────────────────────────────┤
//! │ 기본·Main Agent                     │ 하단 바
//! ```
//!
//! **입력란은 위아래로 선에 물린다.** 빈 줄로 띄우면 위의 상태 줄은 입력란의 머리말처럼,
//! 아래의 하단 바는 꼬리처럼 읽힌다 — 선을 그으면 어디까지가 입력란인지 한눈에 보인다.
//!
//! **빈 줄은 두지 않는다.** 대화와 활동 줄 사이에 한 줄 띄워 뒀는데, 화면 아래쪽에 쓰지도
//! 않는 여백으로만 보였다. 그 한 줄은 대화에 준다.
//!
//! 머리글은 없다. 앱 이름과 디렉터리는 한 번 보면 되는 것이라 자리를 계속 차지할
//! 이유가 없어 뺐다 — 그만큼 대화가 한 줄 더 보인다.

mod activity;
mod approve;
mod ask;
mod enroll;
mod input;
mod newproject;
mod picker;
mod sidebar;
mod status;
mod transcript;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::Frame;

/// 사이드바 경계에 글이 닿지 않게 두는 한 칸. 왼쪽 여백(`rows::PAD`)과 달리 마커가
/// 서지 않으므로 한 칸이면 된다.
const SIDE_GAP: u16 = 1;

use crate::app::State;

pub fn draw(frame: &mut Frame, state: &mut State) {
    let full = frame.area();

    // 사이드바를 오른쪽에 떼어 낸다. 좁은 화면에서는 접는다 — 대화가 먼저다.
    let show_side = state.sidebar_on && full.width > sidebar::WIDTH + 40;
    let (area, side) = if show_side {
        let cut = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20), Constraint::Length(sidebar::WIDTH)])
            .split(full);
        // **오른쪽 여백은 사이드바가 있을 때만, 왼쪽 칸 전체에 준다.**
        //
        // 대화만 떼어 놓으면 입력란과 가름선이 사이드바 경계에 그대로 닿는다. 여백의
        // 목적은 "왼쪽 칸의 어떤 글도 경계선에 붙지 않는 것"이므로 여기서 한 번에
        // 준다 — 아래 위젯들은 좁아진 폭을 그냥 받아 쓰면 된다.
        // 사이드바를 접으면 닿을 것이 없으니 화면 끝까지 쓴다.
        //
        // **한 칸이면 된다.** 왼쪽 여백(`rows::PAD`)은 마커(`▌`·`●`·`▸`)가 서는 자리라
        // 두 칸이 필요하지만, 오른쪽은 경계선에 글이 닿지 않게만 하면 되고 그건 한 칸으로
        // 충분하다. 두 칸을 주면 그만큼 대화가 좁아진다.
        let body = Rect { width: cut[0].width.saturating_sub(SIDE_GAP), ..cut[0] };
        (body, Some(cut[1]))
    } else {
        (full, None)
    };
    // 입력란은 내용에 따라 자란다. 화면의 절반을 넘지는 않는다.
    //
    // **입력란 자리는 하나뿐이다.** 질문이 열려 있으면 질문이, 도구가 밖으로 나가려 하면
    // 승인 창이 그 자리를 차지한다 — 둘이 같이 뜨면 사람이 어디에 답하는지 알 수 없다.
    // 승인이 먼저다: 도구 하나가 답을 기다리며 멈춰 있고 저쪽에는 마감이 있다.
    let input_h = if state.pending.is_some() {
        approve::height(state, area.height.saturating_sub(3)).saturating_sub(1)
    } else {
        match &state.asking {
            Some((_, a)) => ask::height(a, area.height.saturating_sub(3)).saturating_sub(1),
            None => state
                .input
                .height(area.width.saturating_sub(2))
                .min((area.height / 2).max(1))
                .max(1),
        }
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),              // 대화
            Constraint::Length(1),           // 지금 무슨 일인가
            Constraint::Length(input_h + 1), // 가름선 + 입력란
            Constraint::Length(1),           // 가름선
            Constraint::Length(1),           // 하단 바
        ])
        .split(area);

    transcript::draw(frame, chunks[0], state);
    activity::draw(frame, chunks[1], state);
    if state.pending.is_some() {
        // 승인 창은 클릭으로 조작하지 않는다 — 답은 y·n·a 세 키뿐이다.
        state.ask_area = None;
        approve::draw(frame, chunks[2], state);
    } else {
        match &state.asking {
            Some((_, a)) => {
                // 클릭을 줄로 옮기려면 이 영역을 알아야 한다.
                state.ask_area = Some(chunks[2]);
                ask::draw(frame, chunks[2], a, state.lang);
            }
            None => {
                state.ask_area = None;
                input::draw(frame, chunks[2], state);
            }
        }
    }
    input::rule(frame, chunks[3]);
    status::draw(frame, chunks[4], state);

    if let Some(side) = side {
        sidebar::draw_divider(frame, Rect { width: 1, ..side });
        sidebar::draw(frame, side, state);
    }

    // 목록은 맨 위에 겹친다 — 열려 있는 동안은 그것이 지금 할 일이다.
    if let Some(p) = &state.picker {
        picker::draw(frame, full, p, state.lang);
    }

    // **새 프로젝트 양식은 목록 위에 얹힌다.** 목록은 그대로 아래에 있으므로 Esc로
    // 닫으면 다시 그 자리로 돌아온다.
    if let Some(form) = &state.new_project {
        newproject::draw(frame, full, form, state.lang);
    }

    // **등록 코드 창은 그 위에 겹친다.** 코드를 보는 중에는 다른 일을 하면 안 된다 —
    // 키 처리도 이것이 제일 위다(`on_key`).
    if let Some(view) = &state.enroll {
        enroll::draw(frame, full, view, state.lang);
    }

    // **기본은 배경을 안 칠한다** — 터미널이 자기 배경을 쓴다. 앱이 칠하면 격자 밖(창
    // 패딩·남는 픽셀)만 색이 달라져 가장자리에 띠가 생긴다. 자세한 것은 `theme::page_bg`.
    //
    // 되켠 사람에게는 남는 칸 전부에 깔아 준다. 모든 칸에 배경이 있어야 ratatui diff가
    // 전각 글자가 좁아질 때 trailing 칸을 지운다.
    if let Some(bg) = crate::theme::page_bg() {
        for cell in frame.buffer_mut().content.iter_mut() {
            if cell.bg == ratatui::style::Color::Reset {
                cell.bg = bg;
            }
        }
    }

    // **자가치유 프레임.** 지우지 않고 덮어써서 전각 글자 뒤 trailing 칸의 잔상을
    // 치운다 — clear가 없으므로 깜빡이지 않는다. `AlwaysUpdate`는 diff의 동등 비교를
    // 우회해 이번 한 프레임의 칸들을 선로에 실리게 한다. 다음 draw는 새 버퍼(옵션
    // `None`)로 돌아가 일반 diff가 된다.
    //
    // 두 가지 모드가 있다:
    // - `force_update`: **모든 칸**을 다시 내보낸다. 쉬는 화면에서 쓴다 — 그때는
    //   스트리밍이 없어 21KB를 보내도 겹칠 일이 없다.
    // - `force_update_blank`: **빈 칸만** 다시 내보낸다. 턴이 도는 동안 쓴다. 잔상은
    //   항상 빈 칸에 남는다(내용 칸은 바뀔 때마다 다시 그려지므로). 공백은 무엇과도
    //   겹쳐도 안전해서 느린 SSH에서 스트리밍 프레임과 섞여도 글이 두 번 보이지
    //   않는다 — 통째 덮어쓰기가 스트리밍과 겹쳐 단어가 두 번 보이던 그 사고가
    //   구조적으로 여기서는 일어날 수 없다.
    //
    // **전각 글자 바로 뒤 칸은 diff가 항상 건너뛴다**(`cell_width > 1` 분기). 빈 칸
    // 모드로 그 칸에 `AlwaysUpdate`를 심어도 선로에는 나가지 않으므로 전각 글자의
    // 오른쪽 반쪽을 지워 버릴 위험이 없다.
    if std::mem::take(&mut state.force_update) {
        use ratatui::buffer::CellDiffOption;
        for cell in frame.buffer_mut().content.iter_mut() {
            cell.set_diff_option(CellDiffOption::AlwaysUpdate);
        }
    }
    if std::mem::take(&mut state.force_update_blank) {
        use ratatui::buffer::CellDiffOption;
        for cell in frame.buffer_mut().content.iter_mut() {
            if cell.symbol() == " " {
                cell.set_diff_option(CellDiffOption::AlwaysUpdate);
            }
        }
    }
}

/// 활동 줄에 나올 것. 시각을 받으므로 테스트가 경과 시간을 정해 놓고 볼 수 있다.
pub fn activity_parts_at(
    state: &State,
    now: std::time::Instant,
) -> (ratatui::style::Color, String, &'static str) {
    activity::parts_at(state, now)
}

/// 질문 화면에서 이 y좌표가 몇 번째 줄인지. 클릭 처리가 쓴다.
pub fn ask_row_at(
    a: &crate::question::Answering,
    area: ratatui::layout::Rect,
    y: u16,
) -> Option<usize> {
    ask::row_at(a, area, y)
}
