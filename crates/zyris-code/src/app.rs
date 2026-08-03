//! 앱 상태와 키 처리.
//!
//! `on_key`와 `apply`는 순수하다 — 이 성질 덕분에 키 바인딩 전체가 표처럼 테스트된다.
//! I/O는 `run()`(Task 10) 한 곳뿐이다.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use zyris_attacca::ZDeltaKind;

use crate::event::{Entry, EntryKind};
use crate::input::Input;
use crate::mode::Mode;
use crate::rows::Folds;
use crate::scroll::Scroll;
use crate::timeline::Timeline;

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// `entry`가 `None`이면 렌더하지 않는 이벤트다. 그래도 커서는 전진한다.
    Event {
        cursor: i64,
        entry: Option<Entry>,
    },
    Delta {
        kind: ZDeltaKind,
        text: String,
    },
    Status {
        running: bool,
    },
    /// 에이전트가 셸을 열었다. **안 알리면 유령 셸이 돈다.**
    ShellOpened {
        id: String,
        name: String,
    },
    ShellClosed {
        id: String,
    },
    /// 도구가 **작업 디렉터리 밖으로 나가려 한다.** 답이 갈 때까지 그 호출은 막혀 있다.
    Ask(ToolAsk),
    /// attacca가 그 호출을 포기했다. 창은 남기고 "이미 지났다"고만 표시한다 —
    /// 창을 치워 버리면 사람이 무엇을 놓쳤는지 모른다.
    Expired(u64),
    /// 명령이 돌기 시작했다. **`exec`은 완료될 때 한 번만 결과를 준다** — 그동안
    /// 아무 말도 안 하면 사람은 최대 55초를 눈뜬장님으로 기다린다.
    ExecStart {
        id: u64,
        command: String,
    },
    ExecDone {
        id: u64,
    },
    /// **소켓이 끊겼다.** zyris `Runner`가 알아서 다시 붙지만, 그동안 화면은 아무 일도
    /// 없는 것처럼 보인다 — 조용한 실패가 제일 나쁘다. 사유를 그대로 들고 온다.
    Disconnected(String),
    /// 화면 밖에서 생긴 일을 사람에게 한 번 알린다(MCP 서버가 안 떴다, 인증이 끊겨
    /// 재등록이 시작됐다 같은 것).
    /// `STATUS_WINDOW` 뒤에 저절로 사라진다.
    Notice(String),
    /// 등록 코드가 발급됐다. **이 순간부터 화면이 코드 표시를 소유한다** —
    /// 상류의 stdout 상자는 조용해진다(`enroll::ScreenEnroll`).
    Enroll(EnrollView),
    /// 코드가 만료됐거나(`Lapsed`) 브라우저에서 거부됐다(`Denied`). 창은 그대로 두고
    /// 사정만 바꿔 말한다.
    EnrollPhase(EnrollPhase),
    /// 승인돼서 자격이 저장됐다. 창을 닫는다.
    EnrollDone,
}

/// 등록 코드 창에 그릴 것.
#[derive(Debug, Clone, PartialEq)]
pub struct EnrollView {
    pub code: String,
    pub uri: String,
    /// 코드가 만료되는 시각. 그리기 쪽이 남은 시간을 이것으로 잰다.
    pub expires_at: std::time::Instant,
    pub phase: EnrollPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollPhase {
    /// 승인을 기다리는 중.
    Waiting,
    /// 코드가 만료됐다. 새 코드를 요청하는 중이고, 오면 `Frame::Enroll`이 다시 온다.
    Lapsed,
    /// 브라우저에서 거부했다.
    Denied,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Insert(char),
    Backspace,
    Delete,
    DeleteWord,
    Left,
    Right,
    Home,
    End,
    Submit(String),
    Wheel(i32),
    ToggleFold,
    /// 마우스를 누른 자리. 화면 좌표다.
    Press(u16, u16),
    /// 누른 채 옮긴 자리.
    DragTo(u16, u16),
    /// 뗀 순간. 움직이지 않았으면 클릭으로 친다.
    Release,
    ClearSelection,
    /// 화면을 통째로 다시 그린다. 상태를 하나도 바꾸지 않는다 — I/O 자리만 처리한다.
    Repaint,
    /// 질문 화면에서의 조작.
    AskUp,
    AskDown,
    AskToggle,
    AskConfirm,
    AskCancel,
    /// 목록 열기/조작.
    OpenPicker,
    PickUp,
    PickDown,
    /// 지금 줄을 고른다. 무엇이 되는지는 I/O 자리가 처리한다.
    PickConfirm,
    /// 뒤로. 세션 단계면 프로젝트 목록으로, 프로젝트 단계면 닫는다.
    PickBack,
    ToggleSidebar,
    CycleMode,
    Approve,
    Deny,
    AlwaysAllow,
    /// 친 것을 통째로 지운다.
    ClearInput,
    /// 보낸 말을 한 칸 거슬러 올라가 되살린다.
    RecallOlder,
    /// 되살리기에서 한 칸 내려온다. 맨 아래로 오면 입력란이 비워진다.
    RecallNewer,
    /// 승인 창의 답. **기다리는 시간에 제한이 없다** — 다른 일을 하다 늦게 와서
    /// 확인할 수도 있으니 창이 저절로 사라지면 안 된다.
    /// Ctrl+C 1회. 종료를 예고만 하고 실제 종료는 두 번째 입력이 한다.
    ArmQuit,
    Cancel,
    Quit,
    /// 등록 코드 창을 닫는다. **Esc로만 닫힌다** — 다른 키가 창을 치우면
    /// 코드를 보지도 못한 채 승인 단계를 지나친다.
    EnrollClose,
    Frame(Frame),
}

pub struct State {
    pub timeline: Timeline,
    pub folds: Folds,
    pub input: Input,
    pub scroll: Scroll,
    pub running: bool,
    pub connected: bool,
    /// 알릴 것과 알린 시각. **시간이 지나면 저절로 사라진다.**
    ///
    /// 지나간 사정("Zyris로는 아직 만들 수 없습니다")을 계속 붙들고 있으면 그 자리가
    /// 지금 무슨 일인지 말해 주지 못한다. 읽을 만큼만 두고 비운다.
    status: Option<(String, Instant)>,
    /// 권한 모드. 아직 서버로 보내지 않는다 — 표시와 전환만 한다.
    pub mode: Mode,
    /// 하단 바에 보여줄 지금 붙은 에이전트 이름.
    pub agent: String,
    /// Ctrl+C를 한 번 누른 시각. 이 안에 또 누르면 종료다.
    pub quit_armed_at: Option<Instant>,
    /// `/quit`가 세운다. I/O 자리가 보고 루프를 빠져나간다.
    ///
    /// `apply`는 순수하므로 여기서 끝낼 수 없다 — `submit_now`·`flush_queue`와 같은 수법이다.
    pub quitting: bool,
    /// 이번 턴을 멈춰 달라고 이미 말했는가.
    ///
    /// **이게 없으면 서버가 굳었을 때 창을 닫을 길이 없다.** Ctrl+C는 도는 중이면 취소로
    /// 가는데, 취소가 먹히지 않아 `running`이 계속 참이면 몇 번을 눌러도 취소만 다시
    /// 나간다. 한 번 말했으면 다음 Ctrl+C는 종료 쪽으로 넘긴다.
    pub stopping: bool,
    /// 대화 영역에서 고른 텍스트. **놓는 순간 시스템 클립보드로 나간다.**
    pub selection: Option<String>,
    /// 이 세션에서 보낸 말. ↑로 되살린다. 최신이 뒤다.
    pub sent: Vec<String>,
    /// 턴이 도는 동안 친 말. **아직 서버에 안 갔다.** 턴이 끝나면 순서대로 보낸다.
    ///
    /// 일하는 중에 보내면 에이전트가 하던 일을 놓칠 수 있고, 무엇보다 **보내고 나면 고칠
    /// 수 없다.** 여기 들고 있으면 ↑로 도로 꺼내 고칠 수 있다.
    pub queued: Vec<String>,
    /// 대기열을 지금 비워야 하는가. I/O 자리가 보고 가져간다.
    pub flush_queue: bool,
    /// 지금 되살려 놓은 자리(`sent`의 인덱스). 글자를 고치면 풀린다.
    recall: Option<usize>,
    /// 재연결 시 `turn_events(after:)`에 넘길 위치.
    pub last_cursor: Option<i64>,
    /// 마지막으로 그린 대화 영역의 줄 수와 높이.
    ///
    /// `apply`는 순수해야 하므로 뷰포트 크기를 스스로 알 수 없다. 위젯이 매 프레임
    /// 여기에 적어 두고 휠 처리가 그 값을 읽는다. 0이면 아직 한 번도 그리지 않은 것이라
    /// 휠이 아무 일도 하지 않는다 — 첫 프레임 전에는 스크롤할 내용도 없다.
    pub view_total: usize,
    pub view_height: usize,
    /// 대화 영역의 왼쪽 위 화면 좌표. 마우스 좌표를 행/열로 옮길 때 쓴다.
    pub view_origin: (u16, u16),
    /// 지금 화면 맨 위에 보이는 줄의 인덱스.
    pub view_top: usize,
    /// 만들어 둔 대화 줄. **바뀐 항목만 다시 그린다.**
    ///
    /// 예전에는 프레임마다 전부 다시 만들었는데 대화가 길어질수록 선형으로 무거워져
    /// 프레임 예산을 넘겼다 — 그래서 글자가 겹쳐 찍히고 랙이 걸렸다.
    pub rows_cache: crate::rows::Cache,
    /// 행 인덱스 → 그 행을 누르면 접히고 펴지는 카드의 seq.
    pub view_cards: std::collections::HashMap<usize, i64>,
    /// 고른 범위. **마우스를 떼도 남는다** — 떼자마자 사라지면 Ctrl+C를 누를 틈이 없다.
    /// 스크롤해도 내용 좌표라 따라간다.
    pub drag: Option<crate::selection::Drag>,
    /// 지금 버튼을 누르고 있는가. 누르는 중에만 범위가 자란다.
    pub dragging: bool,
    /// 지금 답하고 있는 질문 (seq와 상태).
    ///
    /// 질문이 오면 저절로 여기 들어온다 — 답을 기다리느라 턴이 막혀 있으므로 사람이
    /// 따로 열 필요가 없다.
    pub asking: Option<(i64, crate::question::Answering)>,
    /// 질문 화면이 차지한 자리. 클릭을 줄로 옮길 때 쓴다.
    pub ask_area: Option<ratatui::layout::Rect>,
    /// 질문 제출로 채워진 답을 곧바로 보내야 하는가. I/O 자리가 보고 비운다.
    pub submit_now: bool,
    /// 열려 있는 프로젝트/세션 목록.
    pub picker: Option<crate::picker::Picker>,
    /// 오른쪽 사이드바 내용.
    pub sidebar: crate::sidebar::Sidebar,
    /// 사이드바를 보여줄까. 기본은 켜짐.
    pub sidebar_on: bool,
    /// 터미널 창 제목으로 쓸 것. 세션 제목이 생기면 바뀐다.
    pub title: String,
    /// 그린 프레임 수. 깜박이는 표시가 이걸로 위상을 정한다.
    ///
    /// 시계가 아니라 프레임 수인 이유는 **테스트가 시간을 기다리지 않아도 되기**
    /// 때문이다. 그리는 쪽이 순수해진다.
    pub tick: u64,
    /// 도구가 상대경로를 푸는 기준. 화면이 이것을 보여줘야 도구 줄의 `src/app.rs`가
    /// 어느 리포의 것인지 알 수 있다.
    pub cwd: std::path::PathBuf,
    /// 지금 열려 있는 PTY. **안 보이면 유령 셸이 돈다** — 에이전트가 열어 둔 셸을
    /// 사람이 모른 채로 남는다.
    pub shells: Vec<Shell>,
    /// 사람이 친 슬래시 명령. `run()`이 집어 가 실행한다 — `submit_now`와 같은 수법이다.
    pub command_out: Option<String>,
    /// 지금 답을 기다리는 물음. **한 번에 하나만 띄운다** — 둘이 겹치면 무엇에
    /// 답하는지 알 수 없다.
    pub pending: Option<ToolAsk>,
    /// 등록 코드 창. **재등록이 시작되면 저절로 여기 들어온다.**
    ///
    /// `None`이 평소다. 자격이 revoke되거나 리프레시가 영영 실패하면 상류가
    /// `EnrollmentUi::show`를 부르고 그것이 이 자리를 채운다(`enroll::ScreenEnroll`).
    /// Esc로 닫으면 `None`으로 돌아간다 — 등록 자체는 배경에서 계속 돌고,
    /// 승인되면 `EnrollDone`이 닫는다.
    pub enroll: Option<EnrollView>,
    /// 뒤에서 기다리는 물음들. 앞의 것에 답하면 하나가 올라온다.
    pub ask_queue: std::collections::VecDeque<ToolAsk>,
    /// `run()`이 집어 간다. `apply`는 순수하므로 직접 보내지 못한다.
    pub verdict_out: Option<(u64, Verdict)>,
    /// 이번 세션에서 열어 둔 바깥 디렉터리. **디스크에 남기지 않는다.**
    pub grants: crate::tools::gate::Grants,
    /// 지금 도는 명령 — (번호, 명령, 시작한 때). 활동 줄이 이것을 보여준다.
    pub running_exec: Option<(u64, String, Instant)>,
    /// 지금 열려 있는 작업 카드의 seq.
    open_work: Option<i64>,
    /// 자가치유 틱이 세운다. 다음 draw가 **모든 칸을 강제로 다시 내보낸다** —
    /// `AlwaysUpdate` 플래그로 diff를 우회해 덮어쓴다. 지우지 않으므로 깜빡이지 않는다.
    pub force_update: bool,
    /// 화면 말. `/lang`이 바꾸고 `lang::current()`와 함께 움직인다.
    pub lang: crate::lang::Lang,
}

/// 사람에게 물을 도구 호출 하나. **작업 디렉터리 밖으로 나갈 때만 생긴다.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolAsk {
    /// 이 물음의 번호. 답을 그 호출로 되돌려 보낼 때 쓴다.
    pub id: u64,
    pub call: crate::tools::gate::Call,
    pub summary: String,
    /// attacca가 이미 그 호출을 포기했는가. 답해도 와이어로 나가지 않는다.
    pub expired: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
    /// 이번만이 아니라 **그 디렉터리 전체**를 이 세션 동안 연다.
    AllowAlways,
}

/// 에이전트가 열어 둔 셸 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    /// `terminal.open`이 돌려준 PTY 식별자. 닫을 때 이것으로 찾는다.
    pub id: String,
    /// 사람이 읽는 이름. 보통 셸 실행 파일 이름이다.
    pub name: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            timeline: Timeline::new(),
            folds: Folds::new(),
            input: Input::new(),
            scroll: Scroll::new(),
            running: false,
            connected: false,
            status: None,
            mode: Mode::default(),
            agent: String::new(),
            quit_armed_at: None,
            quitting: false,
            stopping: false,
            selection: None,
            sent: Vec::new(),
            queued: Vec::new(),
            flush_queue: false,
            recall: None,
            last_cursor: None,
            view_total: 0,
            view_height: 0,
            view_origin: (0, 0),
            view_top: 0,
            rows_cache: crate::rows::Cache::new(),
            view_cards: std::collections::HashMap::new(),
            drag: None,
            dragging: false,
            asking: None,
            ask_area: None,
            submit_now: false,
            picker: None,
            sidebar: crate::sidebar::Sidebar::new(),
            sidebar_on: true,
            title: "Zyris Code".into(),
            tick: 0,
            // 도구가 쓰는 것과 **같은 자리**여야 한다. 정의는 `tools::working_dir` 하나다.
            cwd: crate::tools::working_dir(),
            shells: Vec::new(),
            command_out: None,
            pending: None,
            enroll: None,
            ask_queue: std::collections::VecDeque::new(),
            verdict_out: None,
            grants: crate::tools::gate::Grants::default(),
            running_exec: None,
            open_work: None,
            force_update: false,
            lang: crate::lang::current(),
        }
    }
}

/// Ctrl+C를 한 번 누른 뒤 이 안에 또 누르면 종료다.
pub const QUIT_WINDOW: Duration = Duration::from_millis(1500);

/// 알림이 화면에 남아 있는 시간. 한 문장을 읽기에 넉넉하다.
pub const STATUS_WINDOW: Duration = Duration::from_secs(6);

/// 끄면서 "턴을 멈춰라"에 답을 기다리는 시간.
///
/// 한 번의 왕복이면 되는 일이라 짧게 잡는다. **넘겨도 창은 닫는다** — 끄려는 사람을
/// 붙잡아 두는 것이 남은 턴 하나보다 나쁘다.
pub const STOP_WAIT: Duration = Duration::from_secs(3);

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// 알릴 것을 세운다. 이 순간부터 `STATUS_WINDOW` 동안만 보인다.
    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now()));
    }

    /// 지금 보여 줄 알림. 시간이 지났으면 `None`이다.
    pub fn status(&self) -> Option<&str> {
        self.status_at(Instant::now())
    }

    /// 시계를 넘겨받는 판. 테스트가 시간을 흉내 낼 수 있게 갈라 놨다.
    pub fn status_at(&self, now: Instant) -> Option<&str> {
        self.status
            .as_ref()
            .filter(|(_, at)| now.duration_since(*at) < STATUS_WINDOW)
            .map(|(s, _)| s.as_str())
    }

    /// 보낸 말로 기억한다. 같은 말을 연달아 보내면 한 번만 남긴다 — 되살릴 때 같은 줄을
    /// 두 번 지나면 되살리기가 고장 난 것처럼 보인다.
    pub fn remember_sent(&mut self, text: &str) {
        if self.sent.last().map(String::as_str) != Some(text) {
            self.sent.push(text.to_string());
        }
    }

    /// 지금 보낸 말을 되살려 놓은 상태인가. ↑↓의 갈래가 이걸 본다.
    pub fn recalling(&self) -> bool {
        self.recall.is_some()
    }

    /// 종료가 예고된 상태인가. 시간이 지나면 저절로 풀린다.
    pub fn quit_pending(&self) -> bool {
        self.quit_pending_at(Instant::now())
    }

    /// 시계를 넘겨받는 판. 테스트가 시간을 흉내 낼 수 있게 갈라 놨다.
    pub fn quit_pending_at(&self, now: Instant) -> bool {
        self.quit_armed_at.is_some_and(|t| now.duration_since(t) < QUIT_WINDOW)
    }

    /// 지금 글자가 들어갈 입력란.
    ///
    /// 질문의 자유 입력을 치는 중이면 그쪽이고, 아니면 아래 입력란이다. 이 갈래가 없으면
    /// 질문에 직접 답을 쓰는 동안 글자가 엉뚱하게 아래 입력란으로 들어간다.
    pub fn editor(&mut self) -> &mut Input {
        match &mut self.asking {
            Some((_, a)) if a.typing => &mut a.input,
            _ => &mut self.input,
        }
    }

    /// 화면 좌표를 대화 내용의 (행, 열)로 옮긴다. 대화 영역 밖이면 `None`.
    ///
    /// 스크롤한 만큼 더해야 한다 — 화면 첫 줄이 내용의 첫 줄이 아니다.
    pub fn content_at(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        let (ox, oy) = self.view_origin;
        if x < ox || y < oy {
            return None;
        }
        let row = (y - oy) as usize;
        if row >= self.view_height {
            return None;
        }
        Some((self.view_top + row, (x - ox) as usize))
    }
}

pub fn on_key(state: &State, key: KeyEvent) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // **등록 코드 창이 제일 위다.** 코드를 보는 중에 다른 키가 엉뚱한 일을 하면
    // 안 된다 — Esc로만 닫고, Ctrl+C(끄기)만 통과한다. 등록 자체는 배경에서
    // 계속 돌므로 Esc로 닫아도 등록이 끊기지는 않는다.
    if state.enroll.is_some() && !(ctrl && matches!(key.code, KeyCode::Char('c'))) {
        return match key.code {
            KeyCode::Esc => vec![Action::EnrollClose],
            _ => vec![],
        };
    }

    // 질문이 열려 있으면 키가 그쪽으로 간다. 답을 기다리느라 턴이 막혀 있으므로
    // 지금 사람이 할 일은 그것 하나다. 종료만은 언제나 통한다.
    if let Some((_, a)) = &state.asking {
        if !(ctrl && matches!(key.code, KeyCode::Char('c'))) {
            return ask_key(a, key, ctrl);
        }
    }

    // **승인 창이 제일 위다.** 도구 하나가 우리 답을 기다리며 멈춰 있고, 이 갈래가
    // 아래에 있으면 `a`가 글자로 입력란에 새어 들어간다. 종료만은 언제나 통한다.
    //
    // Enter는 일부러 뺐다 — 메시지를 치다 무심코 누른 Enter가 파일을 바꾸면 안 된다.
    if state.pending.is_some() && !(ctrl && matches!(key.code, KeyCode::Char('c'))) {
        return match key.code {
            KeyCode::Char('y') => vec![Action::Approve],
            KeyCode::Char('a') => vec![Action::AlwaysAllow],
            KeyCode::Char('n') | KeyCode::Esc => vec![Action::Deny],
            _ => vec![],
        };
    }

    if state.picker.is_some() && !(ctrl && matches!(key.code, KeyCode::Char('c'))) {
        // **명령 목록은 치면서 고르는 것이다.** 글자가 이동키(k·j)로 먹히면
        // `/skills`를 칠 수 없다 — 여기서는 글자가 그대로 입력이고 목록이 좁혀진다.
        let typing =
            matches!(state.picker.as_ref().map(|p| &p.level), Some(crate::picker::Level::Commands));
        return match key.code {
            KeyCode::Up => vec![Action::PickUp],
            KeyCode::Down => vec![Action::PickDown],
            KeyCode::Char('k') if !typing => vec![Action::PickUp],
            KeyCode::Char('j') if !typing => vec![Action::PickDown],
            // **다 친 명령은 Enter 한 번에 돌아야 한다.** 목록이 떠 있다고 Enter가
            // "고르기"로만 먹히면, `/rules`를 끝까지 치고 눌렀는데 입력란에 같은 글이
            // 다시 써질 뿐 아무 일도 안 일어난다 — 실제로 그렇게 걸렸다.
            KeyCode::Enter if typing && typed_a_whole_command(state) => {
                vec![Action::Submit(state.input.text.trim().to_string())]
            }
            KeyCode::Enter => vec![Action::PickConfirm],
            KeyCode::Char(c) if typing && !ctrl => vec![Action::Insert(c)],
            KeyCode::Backspace if typing => vec![Action::Backspace],
            // ← 는 뒤로가기다. 프로젝트 단계에서는 뒤가 없으니 닫는다.
            // 명령 목록에서는 ←가 커서 이동이라 Esc만 닫는다.
            KeyCode::Left if !typing => vec![Action::PickBack],
            KeyCode::Esc => vec![Action::PickBack],
            KeyCode::Left if typing => vec![Action::Left],
            KeyCode::Right if typing => vec![Action::Right],
            // → 는 아무 일도 하지 않는다. 확정은 Enter 하나뿐이다.
            _ => vec![],
        };
    }

    match key.code {
        // **Ctrl+C는 멈추거나 끝내는 키다.** 복사는 여기 없다 — 세 가지 뜻이 겹쳐 있으면
        // 급할 때 무엇이 일어날지 알 수 없다. 고른 글은 놓는 순간 클립보드로 나간다.
        KeyCode::Char('c') if ctrl => {
            // **한 번은 취소, 그다음부터는 종료다.** 취소가 먹히지 않아 `running`이 계속
            // 참으로 남는 자리가 있다 — 서버가 굳었을 때다. 그때도 창은 닫을 수 있어야 한다.
            if state.running && !state.stopping {
                vec![Action::Cancel]
            } else if state.quit_pending() {
                vec![Action::Quit]
            } else {
                vec![Action::ArmQuit]
            }
        }
        // Ctrl+L은 셸·vim·less에서 모두 "화면을 다시 그려라"다. 화면이 깨졌을 때 사람이
        // 반사적으로 누르는 키라, 다른 뜻을 주지 않는다.
        KeyCode::Char('l') if ctrl => vec![Action::Repaint],
        KeyCode::Char('b') if ctrl => vec![Action::ToggleSidebar],
        KeyCode::Char('o') if ctrl => vec![Action::ToggleFold],
        KeyCode::BackTab => vec![Action::CycleMode],
        KeyCode::Char('w') if ctrl => vec![Action::DeleteWord],
        // **친 것을 통째로 지운다.** `Ctrl+U`가 정본이다 — readline·bash·zsh가 다 그렇고,
        // 터미널이 그냥 0x15 한 바이트로 보내므로 **어디서나 도착한다.**
        //
        // `Ctrl+Backspace`도 같이 받는다. 다만 이쪽은 터미널이 알려 줄 때만 온다 —
        // 많은 터미널이 그냥 Backspace와 같은 바이트를 보내 구분이 안 된다. 덤이다.
        KeyCode::Char('u') if ctrl => vec![Action::ClearInput],
        KeyCode::Backspace if ctrl => vec![Action::ClearInput],
        // readline 관행. 화살표가 없는 자리에서도 움직일 수 있어야 한다.
        KeyCode::Char('a') if ctrl => vec![Action::Home],
        KeyCode::Char('e') if ctrl => vec![Action::End],
        // 선택 중이면 Esc가 선택을 푼다. 실행 중 취소보다 앞이다 — 눈앞의 것이 먼저다.
        KeyCode::Esc if state.selection.is_some() => vec![Action::ClearSelection],
        KeyCode::Esc if state.running => vec![Action::Cancel],
        KeyCode::Enter if !state.input.text.is_empty() => {
            vec![Action::Submit(state.input.text.clone())]
        }
        KeyCode::Backspace => vec![Action::Backspace],
        KeyCode::Delete => vec![Action::Delete],
        // **↑는 보낸 말을 되살린다.** 입력란이 비었을 때 시작하고, 한 번 들어가면 계속
        // 거슬러 올라간다 — 두 번째 ↑에서 멈추면 되살리기가 쓸모없다.
        KeyCode::Up if state.input.text.is_empty() || state.recalling() => {
            vec![Action::RecallOlder]
        }
        KeyCode::Down if state.recalling() => vec![Action::RecallNewer],
        // **이 갈래가 아래 Left보다 앞에 있어야 한다.** match 는 위에서부터 고르므로
        // 순서가 뒤바뀌면 목록이 영영 안 열린다.
        //
        // 입력란이 비어 있을 때만 연다. 글자가 있으면 커서 이동이 먼저다.
        KeyCode::Left if state.input.text.is_empty() => vec![Action::OpenPicker],
        KeyCode::Left => vec![Action::Left],
        KeyCode::Right => vec![Action::Right],
        KeyCode::Home => vec![Action::Home],
        KeyCode::End => vec![Action::End],
        KeyCode::Char(c) if !ctrl => vec![Action::Insert(c)],
        _ => vec![],
    }
}

/// 질문 화면의 키. 자유 입력 중에는 글자가 입력으로 간다.
fn ask_key(a: &crate::question::Answering, key: KeyEvent, ctrl: bool) -> Vec<Action> {
    if a.typing {
        return match key.code {
            KeyCode::Enter => vec![Action::AskConfirm],
            KeyCode::Esc => vec![Action::AskCancel],
            KeyCode::Backspace => vec![Action::Backspace],
            KeyCode::Left => vec![Action::Left],
            KeyCode::Right => vec![Action::Right],
            KeyCode::Char(c) if !ctrl => vec![Action::Insert(c)],
            _ => vec![],
        };
    }
    match key.code {
        KeyCode::Up => vec![Action::AskUp],
        KeyCode::Down => vec![Action::AskDown],
        // Enter 하나로 고르고 실행한다. 조작 줄(이전/다음/제출) 위면 그 일을 한다.
        KeyCode::Enter | KeyCode::Char(' ') => vec![Action::AskConfirm],
        KeyCode::Esc => vec![Action::AskCancel],
        _ => vec![],
    }
}

pub fn apply(state: &mut State, action: &Action) {
    // 글자를 고치는 순간 되살리기에서 빠져나온다. 안 그러면 고쳐 놓은 것을 ↓ 한 번에
    // 잃는다 — 되살린 말을 고쳐 보내는 것이 이 기능의 목적인데 그걸 막는 셈이 된다.
    if matches!(action, Action::Insert(_) | Action::Backspace | Action::Delete | Action::DeleteWord)
    {
        state.recall = None;
    }
    match action {
        Action::Insert(c) => {
            state.editor().insert(*c);
            follow_the_slash(state);
        }
        Action::Backspace => {
            state.editor().backspace();
            follow_the_slash(state);
        }
        Action::Delete => state.editor().delete(),
        Action::DeleteWord => state.editor().delete_word(),
        Action::Left => state.editor().left(),
        Action::Right => state.editor().right(),
        Action::Home => state.input.home(),
        Action::End => state.input.end(),
        Action::Submit(text) => {
            state.input.take();
            state.recall = None;
            // 보냈으면 목록은 제 할 일을 다했다. 안 닫으면 화면을 계속 가린다.
            state.picker = None;
            // **슬래시 명령은 서버로 가지 않는다.** 오타 하나가 크레딧을 쓰면 안 되고,
            // 일하는 중이라고 대기열에 담을 이유도 없다 — 모드를 바꾸거나 목록을 보는
            // 일은 턴이 끝나기를 기다릴 것이 아니다.
            if crate::command::is_command(text) {
                state.remember_sent(text);
                state.command_out = Some(text.clone());
                return;
            }
            // **일하는 중이면 보내지 않고 들고 있는다.** 보낸 기록에도 아직 안 넣는다 —
            // 진짜 나간 뒤에 넣어야 "보낸 말"이 거짓말이 되지 않는다.
            if state.running {
                state.queued.push(text.clone());
                return;
            }
            state.remember_sent(text);
        }
        Action::Wheel(notches) => {
            let (total, height) = (state.view_total, state.view_height);
            state.scroll.wheel(*notches, total, height);
        }
        Action::ToggleFold => {
            if let Some(seq) = state.open_work {
                let fold = state.folds.entry(seq).or_default();
                fold.open = !fold.open;
            }
        }
        Action::Press(x, y) => {
            // 질문 화면 위를 누르면 그 줄을 고른다.
            if let (Some(area), Some((_, a))) = (state.ask_area, state.asking.as_ref()) {
                if *y >= area.y && *y < area.y + area.height {
                    if let Some(i) = crate::widgets::ask_row_at(a, area, *y) {
                        if let Some((_, a)) = &mut state.asking {
                            a.cursor = i;
                        }
                        apply(state, &Action::AskConfirm);
                    }
                    return;
                }
            }
            // 새로 누르면 앞 선택은 버린다.
            state.selection = None;
            state.drag = state.content_at(*x, *y).map(crate::selection::Drag::new);
            state.dragging = state.drag.is_some();
        }
        Action::DragTo(x, y) => {
            if !state.dragging {
                return;
            }
            // 빌림이 겹치지 않게 좌표를 먼저 구한다.
            let at = state.content_at(*x, *y);
            if let (Some(drag), Some(at)) = (state.drag.as_mut(), at) {
                drag.to = at;
            }
            if let Some(drag) = state.drag {
                if !drag.is_click() {
                    // 평문은 **여기서만** 만든다. 프레임마다 만들면 대화 길이에
                    // 비례해 무거워진다 — 드래그는 사람 손 속도라 그때 만들면 된다.
                    let rows = state.rows_cache.plain();
                    let text = crate::selection::extract(&rows, &drag);
                    state.selection = (!text.is_empty()).then_some(text);
                }
            }
        }
        Action::Release => {
            state.dragging = false;
            // **범위는 남긴다.** 어디까지 골랐는지 눈으로 확인할 수 있어야 한다.
            // 클립보드로 내보내는 것은 I/O라 여기서 하지 않는다 — `run`이 한다.
            let Some(drag) = state.drag else { return };
            if drag.is_click() {
                // 움직이지 않았으면 클릭이다 — 그 줄이 작업 카드 머리면 접고 편다.
                state.drag = None;
                if let Some(&seq) = state.view_cards.get(&drag.from.0) {
                    let fold = state.folds.entry(seq).or_default();
                    fold.open = !fold.open;
                }
            }
        }
        Action::AskUp => {
            if let Some((_, a)) = &mut state.asking {
                a.up();
            }
        }
        Action::AskDown => {
            if let Some((_, a)) = &mut state.asking {
                a.down();
            }
        }
        Action::AskToggle => {
            if let Some((_, a)) = &mut state.asking {
                a.toggle();
            }
        }
        Action::AskConfirm => {
            use crate::question::{Act, RowKind};

            let Some((_, a)) = &mut state.asking else {
                return;
            };
            if a.typing {
                // 타자를 끝내는 것이 먼저다.
                a.confirm();
                return;
            }
            match a.row_at(a.cursor) {
                Some(RowKind::Option(_)) | Some(RowKind::Free) => a.toggle(),
                Some(RowKind::Action(Act::Back)) => a.back(),
                Some(RowKind::Action(Act::Next)) | Some(RowKind::Action(Act::Skip)) => a.advance(),
                Some(RowKind::Action(Act::Edit)) => a.to_edit(),
                // 답하지 않겠다고 알린다. 조용히 닫으면 상대는 계속 기다린다.
                Some(RowKind::Action(Act::Reject)) => {
                    state.asking = None;
                    state.input = Input::new();
                    state.input.insert_str("이 질문에는 답하지 않겠습니다.");
                    state.submit_now = true;
                }
                // 제출은 곧바로다. 답을 입력란에 실어 두면 I/O가 그대로 보낸다.
                Some(RowKind::Action(Act::Submit)) => {
                    if let Some((_, a)) = state.asking.take() {
                        state.input = Input::new();
                        let text = a.answer_text();
                        state.input.insert_str(if text.is_empty() {
                            "모두 건너뛰었습니다."
                        } else {
                            &text
                        });
                        state.submit_now = true;
                    }
                }
                None => {}
            }
        }
        Action::AskCancel => {
            // 타자 중이면 타자만 그만둔다. 아니면 질문을 접는다 — 답은 그냥 메시지로도 된다.
            let typing = state.asking.as_ref().is_some_and(|(_, a)| a.typing);
            if typing {
                if let Some((_, a)) = &mut state.asking {
                    a.typing = false;
                    a.input = Input::new();
                }
            } else {
                state.asking = None;
            }
        }
        Action::ClearSelection => {
            state.selection = None;
            state.drag = None;
        }
        // 다시 그리기는 화면만의 일이다. 상태가 바뀌지 않으므로 여기서는 할 일이 없다.
        Action::Repaint => {}
        // 목록을 채우는 것은 I/O 자리다. **여기서 건드리면 안 된다** — `apply`는 I/O 처리
        // 뒤에 돌므로, 여기서 자리를 잡으면 방금 받아온 목록을 도로 덮어쓴다.
        Action::OpenPicker => {}
        Action::PickUp => {
            if let Some(p) = &mut state.picker {
                p.up();
            }
        }
        Action::PickDown => {
            if let Some(p) = &mut state.picker {
                p.down();
            }
        }
        // 고른 결과의 처리(목록 이동·세션 전환·생성)는 I/O 자리가 한다.
        Action::PickConfirm => {}
        // **여기서 picker를 건드리면 안 된다.** `apply`는 I/O 처리 뒤에 도는데, I/O가
        // 세션→프로젝트로 되돌려 놓은 것을 여기서 다시 보면 "프로젝트 단계니 닫자"가
        // 되어 뒤로가기가 그냥 닫기로 변한다. 실제로 그렇게 걸렸다.
        Action::PickBack => {}
        Action::ToggleSidebar => state.sidebar_on = !state.sidebar_on,
        Action::CycleMode => state.mode = state.mode.next(),
        Action::ClearInput => {
            state.editor().take();
            state.recall = None;
        }
        // 되살리기. **대기 중인 말이 먼저다** — 그것이 아직 고칠 수 있는 유일한 말이다.
        Action::RecallOlder => {
            if let Some(text) = state.queued.pop() {
                state.input.text = text;
                state.input.end();
                // 꺼낸 순간 대기열에서 빠졌다. 더 거슬러 올라가면 지금 고치는 것을 잃으므로
                // 여기서 멈춘다 — `recall`을 세우지 않는 것이 그 뜻이다.
                state.recall = None;
                return;
            }
            if state.sent.is_empty() {
                return;
            }
            let next = match state.recall {
                Some(i) => i.saturating_sub(1),
                None => state.sent.len() - 1,
            };
            state.recall = Some(next);
            let text = state.sent[next].clone();
            state.input.text = text;
            state.input.end();
        }
        Action::RecallNewer => {
            let Some(i) = state.recall else { return };
            match state.sent.get(i + 1) {
                Some(text) => {
                    state.recall = Some(i + 1);
                    state.input.text = text.clone();
                    state.input.end();
                }
                // 맨 아래를 지나면 되살리기가 끝난다. 치던 자리로 돌아오는 셈이다.
                None => {
                    state.recall = None;
                    state.input.take();
                }
            }
        }
        Action::Approve | Action::AlwaysAllow | Action::Deny => {
            let Some(ask) = state.pending.take() else { return };
            let verdict = match action {
                Action::Approve => Verdict::Allow,
                Action::AlwaysAllow => Verdict::AllowAlways,
                _ => Verdict::Deny,
            };
            // **디렉터리째로 연다.** 남의 리포를 한 번 허락하고 그 안의 파일 하나하나를
            // 다시 묻는 것은 쓸 수 없다.
            if let (Verdict::AllowAlways, Some(path)) = (verdict, &ask.call.outside) {
                state.grants.allow_under(path);
            }
            // **이미 포기한 호출에는 답을 보내지 않는다.** 보내면 에이전트가 모르는
            // 채로 파일이 바뀐다. 허용만 기록되고 다음 호출에서 그것이 쓰인다.
            if !ask.expired {
                state.verdict_out = Some((ask.id, verdict));
            }
            state.pending = state.ask_queue.pop_front();
        }
        Action::ArmQuit => state.quit_armed_at = Some(Instant::now()),
        // 보내는 것은 I/O 자리가 한다. 여기서는 "말했다"만 적어 둔다 — 활동 줄이 그걸
        // 보여주고, 다음 Ctrl+C가 종료로 넘어갈지도 이걸로 정해진다.
        Action::Cancel => state.stopping = true,
        Action::Quit => {}
        // **등록 자체는 여기서 끊지 않는다.** 창만 닫고, 배경의 상류 폴링은 계속
        // 돌아간다 — 승인되면 `EnrollDone`이 다시 닫고, 만료되면 새 코드와 함께
        // 창이 도로 온다.
        Action::EnrollClose => state.enroll = None,
        Action::Frame(frame) => apply_frame(state, frame),
    }
}

fn apply_frame(state: &mut State, frame: &Frame) {
    match frame {
        Frame::Event { cursor, entry } => {
            // 렌더하지 않는 이벤트여도 커서는 전진한다 — 재개 위치를 놓치면 안 된다.
            state.last_cursor = Some(*cursor);
            let Some(entry) = entry else { return };
            if let EntryKind::WorkStart(_) = entry.kind {
                state.open_work = Some(entry.seq);
                state.folds.entry(entry.seq).or_default();
            }
            // 답을 기다리는 질문이 오면 곧바로 답하기 모드로 들어간다. 턴이 막혀 있으므로
            // 사람이 따로 열 이유가 없다. 이미 답이 간 질문은 다시 열지 않는다.
            if let EntryKind::Question { steps, answered } = &entry.kind {
                if *answered {
                    if state.asking.as_ref().is_some_and(|(q, _)| *q == entry.seq) {
                        state.asking = None;
                    }
                } else if state.asking.is_none() {
                    state.asking =
                        Some((entry.seq, crate::question::Answering::new(steps.clone())));
                }
            }
            // 태스크는 todo_* 도구 호출에서만 알 수 있다 — todo_change 이벤트에는 본문이 없다.
            if let EntryKind::Tool { name, todo: Some((args, result)), .. } = &entry.kind {
                state.sidebar.apply_tool(name, args, result.as_ref());
            }
            state.timeline.upsert(entry.clone());
        }
        // **카드는 저절로 접히거나 펴지지 않는다.**
        //
        // 예전에는 추론이 흐르는 동안 펴 두고 답이 시작되면 접었다. 그런데 그러면
        // 읽고 있던 화면이 스스로 움직이고, "늦게 온 추론이 도로 펴면 안 된다" 같은
        // 예외가 계속 붙는다. 접힘은 사람이 정하는 것 하나로 뒀다 — Ctrl+O다.
        Frame::Delta { kind, text } => state.timeline.push_delta(*kind, text),
        Frame::Status { running } => {
            // **턴이 끝나는 순간이 대기열을 비울 때다.** 보내는 것은 I/O라 여기서 못 한다 —
            // 깃발만 세우고 `run`이 집어 간다. `submit_now`와 같은 수법이다.
            if state.running && !*running && !state.queued.is_empty() {
                state.flush_queue = true;
            }
            // 멈춰 달라던 부탁은 이번 턴까지다. **바뀔 때만 푼다** — 도는 동안 같은
            // 상태가 여러 번 오는데 그때마다 풀면 Ctrl+C가 영영 취소만 되풀이한다.
            if state.running != *running {
                state.stopping = false;
            }
            state.running = *running;
        }
        Frame::ShellOpened { id, name } => {
            // 같은 PTY가 두 번 오면 목록이 두 벌이 된다.
            if !state.shells.iter().any(|s| s.id == *id) {
                state.shells.push(Shell { id: id.clone(), name: name.clone() });
            }
        }
        Frame::ShellClosed { id } => state.shells.retain(|s| s.id != *id),
        // **한 번에 하나만 띄운다.** 둘이 겹치면 무엇에 답하는지 알 수 없다.
        Frame::Ask(ask) => match state.pending {
            None => state.pending = Some(ask.clone()),
            Some(_) => state.ask_queue.push_back(ask.clone()),
        },
        // 창은 치우지 않고 표시만 바꾼다. 치워 버리면 사람이 무엇을 놓쳤는지 모른다.
        Frame::Expired(id) => {
            if let Some(p) = state.pending.as_mut().filter(|p| p.id == *id) {
                p.expired = true;
            }
            for waiting in state.ask_queue.iter_mut().filter(|p| p.id == *id) {
                waiting.expired = true;
            }
        }
        // **끊긴 것은 화면이 말한다.** 활동 줄이 "연결 중…"으로 바뀌고, 사유는 알림으로
        // 한 번 지나간다. 다시 붙는 것은 Runner가 하고, 붙으면 `api_rx`가 알려 준다.
        Frame::Disconnected(why) => {
            state.connected = false;
            state.set_status(state.lang.disconnected(why));
        }
        // 시각은 프레임에 싣지 않고 받는 자리에서 찍는다 — `status_at`과 같은 식이다.
        Frame::ExecStart { id, command } => {
            state.running_exec = Some((*id, command.clone(), Instant::now()));
        }
        // **끝난 것만 지운다.** 겹쳐 돌 때 뒤엣것이 앞엣것을 지우면 화면이 거짓말을 한다.
        Frame::ExecDone { id } => {
            if state.running_exec.as_ref().is_some_and(|(at, _, _)| at == id) {
                state.running_exec = None;
            }
        }
        // 등록 코드 창. 재등록이 시작되면 저절로 뜬다. 만료 뒤 새 코드가 오면
        // 내용만 갈아 끼운다 — 창은 사람이 Esc로 닫지 않는 한 유지된다.
        Frame::Enroll(view) => state.enroll = Some(view.clone()),
        Frame::EnrollPhase(phase) => {
            if let Some(view) = &mut state.enroll {
                view.phase = *phase;
            }
        }
        // 승인돼서 자격이 저장됐다. 창을 닫는다.
        Frame::EnrollDone => state.enroll = None,
        Frame::Notice(text) => state.set_status(text.clone()),
    }
}

/// 친 것이 이미 온전한 명령 이름인가. 그렇다면 목록에서 또 고를 이유가 없다.
fn typed_a_whole_command(state: &State) -> bool {
    let typed = state.input.text.trim();
    state.picker.as_ref().is_some_and(|p| p.rows.iter().any(|r| r.label == typed))
}

/// 입력란이 `/`로 시작하면 명령 목록을 띄우고, 치는 대로 좁힌다.
///
/// **목록이 안 뜨면 아무도 안 쓴다.** 무슨 명령이 있는지 알 방법이 없으면 슬래시 명령은
/// 만든 사람만 쓰는 기능이 된다.
///
/// 질문이나 승인이 열려 있으면 손대지 않는다 — 그 자리는 이미 다른 것이 쓰고 있다.
fn follow_the_slash(state: &mut State) {
    if state.asking.is_some() || state.pending.is_some() {
        return;
    }
    let typed = state.input.text.clone();
    let opening = typed.starts_with('/') && !typed.contains(char::is_whitespace);
    let showing =
        matches!(state.picker.as_ref().map(|p| &p.level), Some(crate::picker::Level::Commands));
    match (opening, showing) {
        (true, _) => {
            let mut p = crate::picker::Picker::commands(state.lang);
            p.narrow(&typed, state.lang);
            // 좁혀서 남는 것이 없으면 목록을 닫는다 — 빈 창은 고장으로 보이고,
            // `/home/...`처럼 명령이 아닌 것을 칠 때 화면을 가리기만 한다.
            state.picker = (!p.rows.is_empty()).then_some(p);
        }
        // 지웠거나 인자를 치기 시작했다. 목록은 제 할 일을 다했다.
        (false, true) => state.picker = None,
        (false, false) => {}
    }
}

/// 슬래시 명령 중 **상태만 만지면 되는 것**을 실행한다.
///
/// 서버나 디스크가 필요한 것(`/agent`·`/mcp`·`/plugin`·`/undo` …)은 여기서 손대지 않고
/// 그대로 돌려준다 — I/O 자리가 받아 마저 한다. 이 함수가 순수해야 테스트가 붙는다.
pub fn run_command(state: &mut State, text: &str) -> Option<crate::command::Command> {
    use crate::command::Command;
    let cmd = crate::command::parse(text)?;
    match &cmd {
        Command::Help => state.timeline.say(crate::command::help_text(state.lang)),
        Command::Mode(None) => {
            let said = state.lang.mode_now(state.mode.label(state.lang));
            state.timeline.say(said);
        }
        Command::Mode(Some(mode)) => {
            state.mode = *mode;
            let said = state.lang.mode_changed(mode.label(state.lang));
            state.timeline.say(said);
        }
        // **바꾸는 즉시 화면 말이 바뀐다.** 알림은 **바꾼 뒤의 말로** 낸다 — 영어로 바꿨는데
        // 확인 문구만 한국어로 오면 바뀐 건지 알 수 없다.
        // **인자 없이 치면 고르는 창을 연다.** 두 개뿐이라 목록이 곧 답이다 —
        // `/lang en`을 외우게 하는 것보다 눈으로 고르는 편이 빠르다.
        Command::Lang(None) => {
            state.picker = Some(crate::picker::Picker::languages(state.lang));
        }
        Command::Lang(Some(lang)) => {
            state.lang = *lang;
            state.timeline.say(lang.lang_changed().to_string());
        }
        Command::LangUnknown(given) => {
            let said = state.lang.lang_unknown(given);
            state.timeline.say(said);
        }
        Command::Cwd => {
            // **노드 이름도 같이 말한다.** 호스트 이름이 같은 머신이 둘이면(`arch`가
            // 흔하다) 서버의 노드 목록에서 둘을 가릴 방법이 이것뿐이다.
            // **줄 앞에 공백을 넣지 않는다.** 마크다운은 네 칸 들여쓴 줄을 코드 블록으로
            // 읽는다 — 문자열을 보기 좋게 접어 쓰면 화면에서는 상자가 되어 나온다.
            state.timeline.say(format!(
                "도구는 `{}`에서 돕니다.\n\n\
                 이 노드는 **{}**로 등록돼 있습니다 — 도구 이름은 `zyris__{}__…`입니다. \
                 `ZYRIS_NODE_NAME`으로 바꿉니다.\n\n\
                 자격은 `{}`에 있습니다.",
                state.cwd.display(),
                crate::conn::node_name(),
                crate::conn::node_slug(),
                crate::conn::credential_home(),
            ));
        }
        Command::Clear => {
            state.timeline.clear();
            // **서버의 기록은 그대로다.** 지웠다고 세션이 없어진 줄 알면 안 된다.
            state.timeline.say("화면을 지웠습니다. thread의 기록은 그대로입니다.");
        }
        Command::Grants => state.timeline.say(grants_text(&state.grants)),
        Command::GrantsClose => {
            let closed = state.grants.close_all();
            // **게이트에도 알려야 한다.** 여기서 지우고 마는 것은 화면만 닫는 것이다 —
            // 그 일은 I/O 자리가 `bridge.sync`로 한다(`finish_command`).
            state.timeline.say(match closed {
                0 => "열어 둔 곳이 없었습니다.".to_string(),
                n => format!("{n}곳을 닫았습니다. 이제 그 밖을 만지려면 다시 물어봅니다."),
            });
        }
        // 화면을 끄는 것은 I/O다. 깃발만 세운다.
        Command::Quit => state.quitting = true,
        // 모르는 것을 조용히 넘기면 다음에 또 틀린다. 무엇이 있는지 같이 알려 준다.
        Command::Unknown(what) => {
            state
                .timeline
                .say(state.lang.unknown_command(what, &crate::command::help_text(state.lang)));
        }
        // 여기서 못 하는 것들. I/O 자리가 받는다.
        Command::Mcp
        | Command::Skills
        | Command::Rules
        | Command::Agent(_)
        // 서버가 필요하다 — `finish_command`가 마저 한다.
        | Command::Project(_)
        | Command::Plugin(_)
        | Command::Changes
        | Command::Undo => {}
    }
    Some(cmd)
}

/// `/grants`가 찍는 글.
///
/// **여는 길은 승인 화면의 `a` 하나뿐이다.** 여기서 열 수 있게 하면 승인 화면을 우회하는
/// 두 번째 길이 생긴다 — 보는 것과 닫는 것만 둔다.
fn grants_text(grants: &crate::tools::gate::Grants) -> String {
    let roots = grants.roots();
    if roots.is_empty() {
        return "작업 디렉터리 밖으로 열어 둔 곳이 없습니다.\n\n\
                승인 화면에서 `a`를 누르면 그 디렉터리가 이 thread 동안 열립니다."
            .into();
    }
    let mut s = format!("작업 디렉터리 밖으로 {}곳이 열려 있습니다.\n", roots.len());
    for root in roots {
        s.push_str(&format!("\n- `{}`", root.display()));
    }
    // **끄면 잊는다는 것을 같이 말한다.** 디스크에 남는 줄 알면 필요 이상으로 겁을 낸다.
    s.push_str(
        "\n\n여기와 그 아래는 묻지 않고 만집니다. `/grants close`로 전부 닫습니다 — \
         앱을 끄면 어차피 잊습니다.",
    );
    s
}

// ---------------------------------------------------------------------------
// 여기부터가 유일한 I/O 자리다. 위쪽은 전부 순수하다.
// ---------------------------------------------------------------------------

use std::io;
use std::sync::Arc;

use crossterm::event::{Event as TermEvent, EventStream, MouseButton, MouseEventKind};
use crossterm::execute;
use futures_util::StreamExt;
use tokio::sync::mpsc;
// 트레이트가 스코프에 있어야 메서드를 부를 수 있다.
use zyris_attacca::{AttaccaApi, AttaccaApiClient};

use crate::conn::{frame_from, Session};
use crate::widgets;

/// 화면을 그리는 최소 간격. 델타가 글자 단위로 쏟아져도 프레임 단위로 합쳐 그린다.
///
/// **ssh 같은 느린 링크에서는 이 값이 곧 화면 깨짐이다.** 16ms(60fps)로 그리면 답변이
/// 빠르게 흐를 때 링크가 프레임을 다 못 실어 나르고, 덜 도착한 화면 위에 다음 프레임이
/// 겹쳐 찢어진다. 사람 눈에는 20fps면 충분히 부드러우므로 여유를 링크에 준다.
///
/// `ZYRIS_CODE_FPS`로 바꾼다 — 로컬 터미널에서 더 부드럽게 보고 싶으면 올리면 된다.
fn frame_interval() -> Duration {
    let fps: u64 = std::env::var("ZYRIS_CODE_FPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|f| (1..=120).contains(f))
        .unwrap_or(20);
    Duration::from_millis(1000 / fps)
}

/// 턴이 도는 동안 다시 그리는 최소 간격.
///
/// 재 보니 한 프레임에 선로로 나가는 바이트는 **무엇이 바뀌었느냐에 따라 천 배** 차이가
/// 난다(`tests/perf.rs::measure_bytes_on_the_wire`).
///
/// | 무엇 | 나가는 양 |
/// |---|---|
/// | 아무 변화 없음 | 32 B |
/// | 글자 하나 침 | 64 B |
/// | 스트리밍 한 조각 | 3.4 KB |
/// | 통째로 다시 그리기 | 21 KB |
///
/// 그래서 **전부를 느리게 하지 않는다.** 키 입력은 누른 자리에서 바로 그리고(64 B),
/// 답이 흘러 들어오는 동안만 이 간격으로 묶는다. 사람 손보다 스트리밍이 훨씬 빠르므로
/// 여기서 묶는 것이 눈에 띄지 않으면서 양은 절반이 된다.
const STREAM_MIN_GAP: Duration = Duration::from_millis(100);

/// 사용량과 제목을 다시 물어보는 간격. 매 프레임 물으면 서버를 두들기게 된다.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// 화면을 통째로 다시 그려 **스스로 고치는** 간격(밀리초). 0이면 끄고, 기본은 2초다.
///
/// ratatui는 **전각 글자 뒤 칸에 아무것도 쓰지 않는다.** 재 보면 선로에 이렇게 나간다
/// (`tests/perf.rs::measure_what_goes_out_behind_a_wide_character`):
///
/// ```text
/// ESC[1;1H한  ESC[1;3H글  ESC[1;5H……
///          └ 2번 칸으로 건너뛴다. 1번 칸에는 끝내 아무것도 안 나간다.
/// ```
///
/// 터미널이 전각 글자를 **두 칸에 걸쳐 칠해 준다**는 믿음 위에 선 최적화다. 그 믿음을
/// 지키지 않는 터미널 — 두 칸을 잡아 두고 글자는 한 칸에만 그리는 종류 — 에서는 뒤 칸에
/// 있던 옛 글자가 그대로 비쳐 보인다. SSH 화면에 글자 부스러기가 남는 구조적 이유다.
/// 게다가 그 칸은 우리 버퍼상 "안 바뀐 칸"이라 **영영 다시 그려지지 않는다.**
///
/// 이 모델 안에서 고칠 방법은 **지우고 처음부터 다시 그리는 것**뿐이다. 그래서 가끔 한다.
/// 사람이 Ctrl+L을 몰라도 몇 초 뒤 제자리로 온다. 화면에 아무 변화가 없었다면 새로 깨질
/// 일도 없으니 건너뛴다. 한 번에 21KB이니 자주 해도 부담은 크지 않다 — 부스러기가 거슬리면
/// `ZYRIS_CODE_HEAL_MS=300`처럼 줄이고, 깜박여 보이면 늘린다.
fn heal_interval() -> Option<Duration> {
    // **기본은 2초다.** 터미널이 자기 배경을 쓰게 두는 정책이면(`theme::page_bg`),
    // ratatui diff가 전각 글자 뒤 trailing 칸을 지워 주는 보호(`previous.bg != Reset`)가
    // 발동하지 않는다 — 그래서 그 칸은 우리 버퍼상 "안 바뀐 칸"으로 남아 영영 안
    // 지워진다. 주기적으로 화면 전체를 **덮어써서**(`force_update`) 그 잔상을 치운다.
    //
    // 예전에는 2초마다 **지우고** 다시 그렸는데, 그게 느린 SSH 링크에서 주기적인
    // 깜빡임으로 보였다. `AlwaysUpdate`는 지우지 않고 같은 내용을 다시 내보내므로
    // 깜빡이지 않는다 — 터미널이 "같은 칸을 덮어쓰는" 것을 잔상 제거로 쓴다.
    // 깜박여 보이면 `ZYRIS_CODE_HEAL_MS=4000`처럼 늘리고, 꺼 두고 싶으면 0을 준다.
    let ms: u64 = std::env::var("ZYRIS_CODE_HEAL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|ms| *ms <= 3_600_000)
        .unwrap_or(2000);
    // 너무 짧게 잡으면 지우기와 다시 그리기 사이가 벌어져 깜박여 보인다.
    (ms > 0).then(|| Duration::from_millis(ms.max(50)))
}

/// 터미널 창 제목을 바꾸는 시퀀스.
fn set_terminal_title(title: &str) {
    use std::io::Write;
    let mut out = io::stdout();
    let _ = write!(out, "\x1b]2;{}\x07", title_for_osc(title));
    let _ = out.flush();
}

/// OSC 안에 넣어도 안전한 제목.
///
/// 제목은 서버가 지어 준다 — 즉 **우리가 쓰지 않은 글자**다. 제어문자가 하나라도 섞이면
/// 시퀀스가 거기서 끊겨 나머지가 화면에 글자로 쏟아진다. 길이도 자른다.
fn title_for_osc(title: &str) -> String {
    title.chars().filter(|c| !c.is_control()).take(120).collect()
}

/// 유일한 I/O 자리.
///
/// `ratatui::init()`이 raw 모드와 대체 화면을 함께 잡는다 — 직접 또 잡지 않는다.
/// 마우스 캡처만 따로 켠다. 어떻게 끝나든 되돌리지 않으면 셸이 망가진 채 남으므로,
/// 본체를 별도 함수로 두고 결과와 무관하게 복구한다.
pub async fn run(
    api_rx: ApiRx,
    bridge: crate::tools::bridge::Bridge,
    die: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    execute!(
        io::stdout(),
        crossterm::event::EnableMouseCapture,
        // 다른 창에 갔다 오면 터미널이 화면을 되살려 주지 않는 경우가 있다. 포커스가
        // 돌아온 것을 알아야 통째로 다시 그릴 수 있다.
        crossterm::event::EnableFocusChange,
        // **줄 넘김을 끈다.** 우리가 센 글자 폭과 터미널이 그리는 폭이 한 칸이라도
        // 어긋나면(`●`·`·`·`─` 같은 글자는 터미널 설정에 따라 두 칸이 된다) 줄 끝이
        // 넘쳐 **아랫줄로 흘러내리고**, 그 아래가 통째로 밀린다. 꺼 두면 넘친 글자는
        // 그 줄에서 잘릴 뿐이라 피해가 한 줄에 머문다.
        crossterm::terminal::DisableLineWrap,
    )?;

    let result = run_inner(&mut terminal, api_rx, bridge, die).await;

    let _ = execute!(
        io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableFocusChange,
        // 줄 넘김은 셸이 쓰는 것이다. 안 되돌리면 셸에서 긴 명령이 잘려 보인다.
        crossterm::terminal::EnableLineWrap,
    );
    ratatui::restore();
    result
}

/// 지금 살아 있는 연결의 손잡이를 나르는 통로.
///
/// 연결이 끊겼다 붙으면 손잡이가 바뀐다. 앱은 매번 여기서 **최신 것을 집어** 쓴다 —
/// 처음 것을 붙들고 있으면 재연결 뒤 모든 호출이 죽은 연결로 나간다.
pub type ApiRx = tokio::sync::watch::Receiver<Option<Arc<AttaccaApiClient>>>;

/// 화면 통로에 실리는 것. `Some(세션 id)`는 그 세션의 턴 스트림에서 온 프레임이고,
/// `None`은 화면 밖(도구·다리)에서 온 것이다.
///
/// **낡은 세션의 프레임은 받는 쪽이 버린다.** 일을 걸어 두고 다른 세션으로 갈아타면
/// 앞 세션의 턴은 서버에서 계속 돌고 그 스트림도 계속 프레임을 보낸다 — 태그가 없으면
/// 지금 보는 세션의 타임라인에 앞 세션의 메시지가 섞인다. 실제로 그렇게 섞였다.
pub type AppMsg = (Option<String>, Action);

/// 화면이 지금 보고 있는 세션의 프레임인가. `None`(화면 밖에서 온 것)은 항상 통과한다.
fn frame_is_current(sid: &Option<String>, current: Option<&str>) -> bool {
    match sid {
        None => true,
        Some(id) => current == Some(id.as_str()),
    }
}

/// 지금 손잡이. 아직 안 붙었으면 `None`.
fn api_of(rx: &ApiRx) -> Option<Arc<AttaccaApiClient>> {
    rx.borrow().clone()
}

/// 화면을 지우고 **다음 프레임을 통째로** 다시 그리게 한다.
///
/// **`Terminal::clear()`를 쓰지 않는다.** 그쪽은 지우기 전에 커서가 어디 있는지 터미널에
/// 물어보고(DSR `ESC[6n`) 답을 **동기로 기다린다**. 답하지 않는 터미널에서는 거기서 멈추고,
/// 답하더라도 그 바이트를 우리 키 입력 스트림에서 훔쳐 간다. 원격 터미널은 답이 늦거나
/// 아예 없을 수 있는 자리다 — pty 스모크 테스트가 정확히 그 상황이고, 실제로 앱이 멈췄다.
///
/// `resize`는 같은 일(지우기 + 다음 프레임 전면 다시 그리기)을 아무것도 묻지 않고 한다.
fn repaint(terminal: &mut ratatui::DefaultTerminal) {
    use ratatui::backend::Backend;

    if let Ok(size) = terminal.backend().size() {
        let _ = terminal.resize(ratatui::layout::Rect::new(0, 0, size.width, size.height));
    }
}

async fn run_inner(
    terminal: &mut ratatui::DefaultTerminal,
    mut api_rx: ApiRx,
    bridge: crate::tools::bridge::Bridge,
    mut die: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut state = State::new();

    // **화면을 먼저 붙인다.** 등록 코드 창이 첫 연결 전에도 도달해야 한다 —
    // `enroll::ScreenEnroll`은 `Frame::Enroll`을 여기로 보낸다.
    let (tx, mut rx) = mpsc::unbounded_channel::<AppMsg>();

    // **여기서부터 도구가 화면에 물어볼 수 있다.** 이 전에 오는 호출은 물을 곳이 없어
    // 거부된다 — 조용히 통과시키면 승인 없이 파일이 바뀐다.
    bridge.attach(tx.clone());
    bridge.sync(state.mode, &state.grants);

    let mut keys = EventStream::new();
    let mut ticker = tokio::time::interval(frame_interval());
    let mut last_size: Option<(u16, u16)> = None;
    let mut dirty = true;

    // **첫 손잡이가 올 때까지 화면을 그리며 기다린다.** 이 동안이 첫 등록이다 —
    // "연결 중…" 위에 등록 코드 창이 뜬다. 손잡이는 `on_connect`가 보낸다.
    let api = loop {
        if let Some(api) = api_of(&api_rx) {
            break api;
        }
        if *die.borrow() {
            anyhow::bail!("연결이 끊겼습니다.");
        }
        tokio::select! {
            result = api_rx.changed() => {
                // 보내는 쪽이 사라졌다 — 러너가 끝났다. `die`가 아닌 길로도
                // 멈춰야 조용히 닫힌다.
                if result.is_err() {
                    anyhow::bail!("연결이 끊겼습니다.");
                }
            }
            _ = die.changed() => {}
            Some((sid, action)) = rx.recv() => {
                // 화면이 아직 없다(첫 등록). 이때 오는 스트림 프레임은 낡은 것이다.
                if !frame_is_current(&sid, None) {
                    continue;
                }
                apply(&mut state, &action);
                dirty = true;
            }
            Some(Ok(ev)) = keys.next() => {
                let mut quit = false;
                match ev {
                    TermEvent::Key(k) => {
                        for action in on_key(&state, k) {
                            if matches!(action, Action::Quit) {
                                quit = true;
                                break;
                            }
                            apply(&mut state, &action);
                        }
                    }
                    TermEvent::Resize(w, h) if last_size != Some((w, h)) => {
                        last_size = Some((w, h));
                        repaint(terminal);
                    }
                    _ => {}
                }
                if quit {
                    // 아직 세션도 턴도 없다 — 그냥 닫는다.
                    return Ok(());
                }
                dirty = true;
            }
            _ = ticker.tick() => {
                state.tick = state.tick.wrapping_add(1);
                // 등록 코드 창이 떠 있으면 남은 시간이 흐르므로 계속 그린다.
                if state.enroll.is_some() {
                    dirty = true;
                }
            }
        }
        if dirty {
            terminal.draw(|f| widgets::draw(f, &mut state))?;
            dirty = false;
        }
    };

    let mut api = api;
    state.connected = true;
    // **"연결 중…"이 화면에 얼어붙지 않게 한다.** 붙은 것을 반드시 다시 그린다 —
    // wait 루프가 마지막에 그린 프레임은 아직 연결 전이고, `dirty`가 그대로 꺼져
    // 있으면 아무 키나 누르기 전까지 "연결 중…"이 남는다(실제로 그렇게 남았다).
    // "연결됨"을 잠깐 보여 준 뒤 6초 후 `쉬는 중`으로 내려간다.
    dirty = true;
    state.set_status(state.lang.connected());

    // 승인한 사람이 요청보다 적게 줬을 수 있다. **그러면 목록이 조용히 빈 채로 돌아온다** —
    // 오류가 아니라 빈 결과라, 사람은 자기 계정에 에이전트가 없는 줄 안다.
    //
    // 승인 때 정해진 권한은 나중에 넓어지지 않는다. 토큰을 아무리 갱신해도 그대로다.
    // 그래서 여기서 말할 것은 "부족합니다"가 아니라 **무엇을 해야 하는가**다.
    if let Ok(me) = api.me().await {
        // 부르는 것 전부를 본다. 예전에는 셋만 봐서, `agents:read`나 `projects:read`가
        // 빠졌을 때 목록만 비고 아무 말도 없었다.
        let missing = crate::conn::missing_scopes(&me.scopes);
        if !missing.is_empty() {
            // **한 번은 다시 묻는다.** 상류에 `renew_when_scopes_missing`이 없으므로 우리가
            // 한다: 자격을 버려 두면 다음에 켤 때 화면 없이 깨끗하게 등록 코드가 뜬다.
            // 사람이 토큰을 직접 준 자리에는 버릴 자격이 없어 말로만 알린다.
            let reauth = bridge.reauth();
            let tried = reauth.as_ref().is_none_or(|r| r.spent());
            let asked_again = crate::conn::needs_reenrollment(&me.scopes, tried)
                && match &reauth {
                    Some(r) => r.discard_once().await,
                    None => false,
                };
            let said = if asked_again {
                crate::conn::scopes_will_be_asked_again(&missing)
            } else {
                crate::conn::missing_scopes_message(&missing)
            };
            state.set_status(said.clone());
            state.timeline.say(said);
        }
    }

    state.agent = crate::conn::agent_name();
    let mut agent_id = match Session::agent_id(&api).await {
        Ok(id) => id,
        Err(e) => {
            state.set_status(e.to_string());
            String::new()
        }
    };
    // 스킬 목록은 `tools::announce`가 정해 다리에 얹어 뒀다.
    let mut session = Session::new(bridge.preamble());
    // 모드가 **바뀌었는지**를 보려고 들고 있는 직전 값. `state.mode`만 보면 모드에 머무는
    // 내내 참이라, work·job 예약이 말할 때마다 되살아난다.
    let mut last_mode = state.mode;

    // 답을 기다리는 질문이 남아 있으면 그 세션으로 들어간다. 끄기 전에 답하지 않았다면
    // 서버는 아직 기다리고 있고, 사람이 손으로 찾아 들어가야 한다면 답할 길이 없는 것과
    // 같다.
    if let Some(id) = crate::conn::session_awaiting_answer(&api).await {
        if let Err(e) = switch(&api, &mut state, &mut session, id, None, &tx).await {
            state.set_status(e.to_string());
        }
    }
    let mut keys = EventStream::new();
    let mut ticker = tokio::time::interval(frame_interval());
    let mut poll = tokio::time::interval(POLL_INTERVAL);
    // 끄면 영영 안 오는 타이머로 둔다. `select!`의 가지를 늘리지 않으려는 것이다.
    let mut heal = tokio::time::interval(heal_interval().unwrap_or(Duration::from_secs(86400)));
    let healing = heal_interval().is_some();
    // 마지막 자가 치유 뒤로 화면을 건드린 적이 있는가. 안 그렸으면 새로 깨질 일도 없다.
    let mut drew_since_heal = false;
    let mut last_draw = Instant::now();
    set_terminal_title(&state.title);
    let mut shown_title = state.title.clone();
    // 종료 예고는 시간이 지나면 저절로 풀린다. 그런데 아무 입력이 없으면 다시 그릴 일이
    // 없어서 안내 글자가 화면에 남는다 — 풀린 프레임을 잡아 한 번 더 그린다.
    let mut last_quit_pending = false;
    let mut last_had_status = false;
    let mut shutdown = shutdown_signals();

    'app: loop {
        tokio::select! {
            Some(Ok(ev)) = keys.next() => {
                let actions = match ev {
                    TermEvent::Key(k) => on_key(&state, k),
                    TermEvent::Mouse(m) => match m.kind {
                        MouseEventKind::ScrollUp => vec![Action::Wheel(1)],
                        MouseEventKind::ScrollDown => vec![Action::Wheel(-1)],
                        MouseEventKind::Down(MouseButton::Left) => {
                            vec![Action::Press(m.column, m.row)]
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            vec![Action::DragTo(m.column, m.row)]
                        }
                        MouseEventKind::Up(MouseButton::Left) => vec![Action::Release],
                        _ => vec![],
                    },
                    // 포커스가 돌아오면 다시 그리되 **지우지는 않는다.** 포커스 이벤트는
                    // 모바일 SSH(Termius)에서 키보드가 열리고 닫힐 때마다 온다 — 그때마다
                    // 전체를 지우면 화면이 반짝인다. 화면이 진짜로 깨졌으면 Ctrl+L이
                    // 통째로 다시 그린다.
                    TermEvent::FocusGained => {
                        dirty = true;
                        vec![]
                    }
                    // 크기가 실제로 바뀌었을 때만 지우고 통째로 다시 그린다. 같은 크기의
                    // resize가 연달아 와도 매번 전체를 지우면 깜빡인다.
                    TermEvent::Resize(w, h) => {
                        if last_size != Some((w, h)) {
                            last_size = Some((w, h));
                            repaint(terminal);
                        }
                        dirty = true;
                        vec![]
                    }
                    _ => vec![],
                };
                for action in actions {
                    match &action {
                        // 화면 복구는 `run`이 하고, **서버에서 도는 턴은 이 밑에서 멈춘다.**
                        Action::Quit => break 'app,
                        // **일하는 중이면 여기서 안 보낸다.** `apply`가 대기열에 담고,
                        // 턴이 끝날 때 아래 `flush_queue`가 순서대로 보낸다.
                        // `apply`는 이 match 뒤에 도므로 여기서 본 `running`이 아직 옛 값이다.
                        // 슬래시 명령은 서버로 안 간다. `apply`가 `command_out`에
                        // 담고, 이 match 아래에서 실행한다.
                        Action::Submit(text) if crate::command::is_command(text) => {}
                        Action::Submit(_) if state.running => {}
                        Action::Submit(text) => {
                            if agent_id.is_empty() {
                                state.set_status("에이전트를 찾지 못해 보낼 수 없습니다.");
                            } else {
                                send_and_tell(&api, &mut state, &mut session, &agent_id, text, &tx)
                                    .await;
                            }
                        }
                        // 목록은 서버에서 받아와야 한다 — 여는 순간 채운다.
                        Action::OpenPicker => match crate::conn::projects(&api).await {
                            Ok(items) => state.picker = Some(crate::picker::Picker::projects(items, state.lang)),
                            Err(e) => {
                                state.picker = None;
                                state.set_status(e.to_string());
                            }
                        },
                        // 뒤로가기의 전부를 여기서 정한다 — 세션 단계면 프로젝트 목록으로
                        // 되돌리고, 프로젝트 단계면 닫는다.
                        Action::PickBack => {
                            let in_sessions = matches!(
                                state.picker.as_ref().map(|p| &p.level),
                                Some(crate::picker::Level::Sessions { .. })
                            );
                            if in_sessions {
                                match crate::conn::projects(&api).await {
                                    Ok(items) => {
                                        state.picker =
                                            Some(crate::picker::Picker::projects(items, state.lang))
                                    }
                                    Err(e) => {
                                        state.picker = None;
                                        state.set_status(e.to_string());
                                    }
                                }
                            } else {
                                state.picker = None;
                            }
                        }
                        Action::PickConfirm => {
                            if let Err(e) =
                                pick(&api, &mut state, &mut session, &mut agent_id, &tx).await
                            {
                                state.set_status(e.to_string());
                            }
                        }
                        Action::Cancel => {
                            if let Some(id) = session.id() {
                                let _ = api.cancel_turn(id.to_string()).await;
                            }
                        }
                        // 화면을 지우기만 한다. 바로 아래에서 다시 그린다.
                        Action::Repaint => {
                            repaint(terminal);
                        }
                        // **스크롤은 diff가 그린다.** 예전에는 전각 부스러기를 지우려고
                        // 통째로 지우고 다시 그렸는데, 이제 모든 칸에 배경이 깔려
                        // (`theme::BG`) 부스러기가 구조적으로 생기지 않으므로 지울 필요가
                        // 없다 — 지우는 순간 화면이 반짝인다.
                        Action::Wheel(_) => {}
                        _ => {}
                    }
                    apply(&mut state, &action);

                    // **판정은 여기서 도구로 돌아간다.** `apply`는 순수하므로 적어
                    // 두기만 했다 — `submit_now`·`flush_queue`와 같은 수법이다.
                    if let Some((id, verdict)) = state.verdict_out.take() {
                        bridge.answer(id, verdict);
                    }
                    // 열어 둔 디렉터리가 바뀌면 게이트가 보는 것도 같이 바뀌어야 한다.
                    // 안 옮기면 화면은 허락했는데 도구는 계속 물어본다.
                    //
                    // **모드는 여기서 안 본다.** Shift+Tab과 `/mode`가 서로 다른 길로 들어와
                    // 아래 한 자리에서 함께 처리된다 — 나뉘어 있었을 때 `/mode`가 게이트에
                    // 안 닿는 버그가 실제로 있었다.
                    if matches!(action, Action::Approve | Action::Deny | Action::AlwaysAllow) {
                        bridge.sync(state.mode, &state.grants);
                    }

                    // **마우스를 놓는 순간 고른 글이 클립보드로 나간다.** 누를 키가 따로
                    // 없다 — Ctrl+C는 멈추는 키 하나로 두는 편이 급할 때 헷갈리지 않는다.
                    // 범위는 `apply`가 정하므로 그 뒤여야 한다. 내보내는 것은 I/O라 여기다.
                    // OSC 52를 모르는 터미널은 조용히 무시하고, 손해는 없다.
                    if matches!(action, Action::Release) {
                        if let Some(text) = &state.selection {
                            crate::clipboard::export(text);
                        }
                    }

                    // 슬래시 명령. 순수한 부분은 `run_command`가 끝내고, 서버나 디스크가
                    // 필요한 것만 여기서 마저 한다.
                    if let Some(text) = state.command_out.take() {
                        finish_command(&api, &bridge, &mut state, &mut session, &mut agent_id, &text)
                            .await;
                        // `/quit`. 나가는 길은 Ctrl+C와 같다 — 도는 턴은 밑에서 멈춘다.
                        if state.quitting {
                            break 'app;
                        }
                    }

                    // **모드가 바뀐 뒤에 할 일은 여기 하나로 모은다.** 들어오는 길이 둘이라
                    // (Shift+Tab은 `apply`, `/mode`는 바로 위 `finish_command`) 각자
                    // 고치면 언젠가 한쪽만 고친다 — 실제로 `/mode`가 게이트에 안 닿고 있었다.
                    //
                    // **가장자리에서만 돈다.** 모드에 머무는 동안 계속 돌면 job 모드에서
                    // 말할 때마다 job이 하나씩 생겨, 되묻는 말에 답할 자리가 안 생긴다.
                    if state.mode != last_mode {
                        last_mode = state.mode;
                        bridge.sync(state.mode, &state.grants);
                        restage(&mut state, &mut session);
                    }

                    // 질문에서 제출을 누르면 곧바로 보낸다 — 답을 입력란에 실어 두고
                    // 사람이 Enter를 한 번 더 치게 하면 제출이 아니라 초안이 된다.
                    if std::mem::take(&mut state.submit_now) {
                        let text = state.input.take();
                        if !text.is_empty() {
                            send_and_tell(&api, &mut state, &mut session, &agent_id, &text, &tx)
                                .await;
                        }
                    }
                    // **키를 누른 자리에서 바로 그린다.** 한 프레임 64바이트라 아낄 것이
                    // 없고, 틱을 기다리면 그만큼 손이 굼떠 보인다.
                    terminal.draw(|f| widgets::draw(f, &mut state))?;
                    dirty = false;
                    drew_since_heal = true;
                    last_draw = Instant::now();
                }
            }
            Some((sid, action)) = rx.recv() => {
                // **낡은 세션의 프레임은 버린다.** 화면을 갈아탔는데 앞 세션의 메시지가
                // 계속 올라오면 안 된다 — 일을 걸어 두고 다른 세션에서 대화하는 동안
                // 그 일의 이벤트가 여기로 쏟아진다. 턴은 서버에서 계속 돌고, 그 세션으로
                // 돌아가면 다시 열 때 히스토리로 보인다.
                if !frame_is_current(&sid, session.id()) {
                    continue;
                }
                apply(&mut state, &action);
                // 턴이 끝나는 프레임이 여기로 온다 — 대기열을 비울 자리도 여기다.
                flush_queue(&api, &mut session, &agent_id, &mut state, &tx).await;
                dirty = true;
            }
            // 재연결. 손잡이를 갈아 끼우고 보던 세션의 스트림을 다시 연다.
            Ok(()) = api_rx.changed() => {
                if let Some(fresh) = api_of(&api_rx) {
                    api = fresh;
                    state.connected = true;
                    // 끊겼다 붙은 것도 화면에 보인다 — connecting → connected → idle.
                    state.set_status(state.lang.connected());
                    if let Some(id) = session.id().map(str::to_string) {
                        spawn_stream(Arc::clone(&api), id, state.last_cursor, tx.clone());
                    }
                    dirty = true;
                }
            }
            // 사용량과 제목은 이벤트로 오지 않으므로 주기적으로 물어본다.
            _ = poll.tick() => {
                if let Some(id) = session.id().map(str::to_string) {
                    if let Some(u) = crate::conn::usage(&api, &id).await {
                        if u != state.sidebar.usage {
                            state.sidebar.usage = u;
                            dirty = true;
                        }
                    }
                    let want = crate::conn::session_title(&api, &id)
                        .await
                        .unwrap_or_else(|| "Zyris Code".into());
                    if want != state.title {
                        state.title = want;
                        dirty = true;
                    }
                }
                if state.title != shown_title {
                    set_terminal_title(&state.title);
                    shown_title = state.title.clone();
                }
            }
            _ = ticker.tick() => {
                state.tick = state.tick.wrapping_add(1);
                let pending = state.quit_pending();
                // 알림이 시간이 지나 사라지는 순간에도 다시 그려야 한다. 안 그리면
                // 지워진 글이 화면에 그대로 남는다 — 종료 예고와 같은 이유다.
                let has_status = state.status().is_some();
                if has_status != last_had_status {
                    last_had_status = has_status;
                    dirty = true;
                }
                if pending != last_quit_pending {
                    last_quit_pending = pending;
                    if !pending {
                        // 예고가 풀렸으니 상태도 비워 둔다. 안 비우면 다음 판정이 또 걸린다.
                        state.quit_armed_at = None;
                    }
                    dirty = true;
                }
                // 작업 중에는 점이 깜박여야 하므로 계속 다시 그린다. 한 프레임이
                // 0.2ms대라 부담이 되지 않는다 — 그 전에는 이럴 수 없었다.
                if state.running {
                    dirty = true;
                }
                // 등록 코드 창이 떠 있으면 남은 시간이 흐르므로 계속 그린다.
                if state.enroll.is_some() {
                    dirty = true;
                }
                // 턴이 도는 동안에는 묶어서 그린다. 스트리밍 한 조각이 3.4KB라 초당
                // 스무 번이면 원격 터미널에 가장 부담이 큰 자리다 — 사람 눈에는 열 번이나
                // 스무 번이나 같지만 나가는 양은 절반이 된다.
                let held = state.running && last_draw.elapsed() < STREAM_MIN_GAP;
                if dirty && !held {
                    terminal.draw(|f| widgets::draw(f, &mut state))?;
                    dirty = false;
                    drew_since_heal = true;
                    last_draw = Instant::now();
                }
            }
            // **밖에서 끄라고 해도 끄는 것은 같다.** `kill`이나 터미널 창을 닫은
            // 것(SIGHUP)이 여기로 온다 — 그 길로 죽으면 서버에서 돌던 턴이 남는다.
            Some(()) = shutdown.recv() => break 'app,
            // **러너가 끝났다.** `main`이 이 채널에 신호를 보내면(치명적 오류) 화면도
            // 조용히 닫는다 — 터미널을 되돌린 뒤 `main`이 사유를 말한다.
            _ = die.changed(), if *die.borrow() => break 'app,
            // 스스로 고치기. 마지막 치유 뒤로 그린 적이 있을 때만 한다 — 가만히 있는
            // 화면은 깨질 일이 없고, 쉬는 세션이 SSH로 계속 바이트를 밀 이유도 없다.
            _ = heal.tick(), if healing => {
                if drew_since_heal {
                    // **지우지 않고 모든 칸을 강제로 다시 내보낸다.** clear는 깜빡임의
                    // 원인이다 — `force_update`가 diff를 우회해 전 칸을 덮어써서 전각
                    // 글자 뒤 trailing 칸의 잔상을 치운다. 다음 draw는 일반 diff로 돌아간다.
                    state.force_update = true;
                    terminal.draw(|f| widgets::draw(f, &mut state))?;
                    dirty = false;
                    drew_since_heal = false;
                }
            }
        }
    }

    // **창을 닫으면 서버에서도 멈춘다.**
    //
    // 턴은 서버에서 돈다 — 이쪽이 사라져도 저쪽은 계속 생각하고, 도구를 부를 때마다
    // 없는 노드를 찾다 실패한다. 크레딧은 그동안 계속 나간다. 닫는 사람의 뜻은
    // "그만"이지 "여기까지는 안 보고 계속 시켜라"가 아니다.
    if let Some(id) = turn_to_stop(&state, &session) {
        // 마지막 프레임 하나. 네트워크가 굼뜨면 잠시 멈춘 것처럼 보이는데, 아무 말도
        // 없으면 안 꺼지는 줄 안다. 화면은 아직 우리 것이다 — 되돌리는 것은 `run`이다.
        //
        // 종료 예고를 먼저 내린다. 활동 줄은 그것을 무엇보다 위에 두므로(`activity.rs`),
        // 안 내리면 방금 누른 Ctrl+C의 안내가 이 마지막 한 줄을 덮는다.
        state.quit_armed_at = None;
        state.set_status("진행 중인 턴을 멈추는 중입니다…");
        let _ = terminal.draw(|f| widgets::draw(f, &mut state));
        // **못 멈춰도 창은 닫힌다.** 서버가 답하지 않는데 여기서 마냥 기다리면,
        // 끄려던 사람이 끌 수도 없는 화면 앞에 남는다.
        match tokio::time::timeout(STOP_WAIT, api.cancel_turn(id)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "끄면서 턴을 멈추지 못했다"),
            Err(_) => tracing::warn!("끄면서 턴을 멈추라고 했는데 답이 없다"),
        }
    }
    Ok(())
}

/// 끌 때 멈추라고 할 세션. 도는 턴이 없으면 `None`이다.
///
/// 순수하게 떼어 둔다 — 판정이 I/O 자리에 섞여 있으면 테스트가 서버를 띄워야 한다.
fn turn_to_stop(state: &State, session: &Session) -> Option<String> {
    if !state.running {
        return None;
    }
    session.id().map(str::to_string)
}

/// 밖에서 끄라고 보내는 신호를 한 통로로 모은다.
///
/// Ctrl+C는 여기로 오지 않는다 — raw 모드라 시그널이 아니라 바이트로 들어와
/// `on_key`가 받는다. 이쪽은 `kill`(SIGTERM)과 터미널 창이 닫힐 때(SIGHUP)다.
///
/// 유닉스가 아니면 보내는 쪽이 없으니 `recv()`가 곧바로 `None`이 되고, `select!`가
/// 그 가지를 꺼 버린다 — cfg로 가지를 지우는 것보다 이쪽이 조용하다.
fn shutdown_signals() -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel(1);
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        for kind in [SignalKind::terminate(), SignalKind::hangup()] {
            let Ok(mut sig) = signal(kind) else { continue };
            let tx = tx.clone();
            tokio::spawn(async move {
                sig.recv().await;
                let _ = tx.send(()).await;
            });
        }
    }
    rx
}

/// 목록에서 고른 것을 실제로 처리한다.
async fn pick(
    api: &Arc<AttaccaApiClient>,
    state: &mut State,
    session: &mut Session,
    agent_id: &mut String,
    tx: &mpsc::UnboundedSender<AppMsg>,
) -> anyhow::Result<()> {
    use crate::picker::{Pick, Picker};

    let Some(chosen) = state.picker.as_ref().and_then(Picker::pick) else {
        return Ok(());
    };
    match chosen {
        Pick::Unavailable(why) => state.set_status(why),
        Pick::OpenProject { id, name } => {
            let items = crate::conn::sessions(api, &id).await?;
            // **들어간 순간 기억한다.** 세션을 안 고르고 Esc로 닫아도 여기서 여는
            // job·work는 이 프로젝트의 것이어야 한다.
            session.enter_project(id.clone());
            state.picker = Some(Picker::sessions(id, name, items, state.lang));
        }
        Pick::OpenSession { id, project_id } => {
            switch(api, state, session, id, Some(project_id), tx).await?;
        }
        Pick::NewSession { project_id } => {
            // **서버에 지금 만들지 않는다.** 첫 메시지를 보낼 때 만든다 — 열어만 보고 마는
            // 빈 세션이 계정에 쌓이면 안 된다. 여기서는 어느 프로젝트에 만들지만 기억한다.
            session.stage_new(project_id);
            // 새 세션은 아직 제목이 없다.
            state.title = "Zyris Code".into();
            state.sidebar.clear();
            state.timeline = Timeline::new();
            state.folds = Folds::new();
            state.asking = None;
            state.last_cursor = None;
            state.picker = None;
            state.scroll = Scroll::new();
            let _ = agent_id;
        }
        // **고르는 즉시 화면 말이 바뀐다.** 확인 문구는 바꾼 뒤의 말로 낸다 — 영어로
        // 바꿨는데 확인만 한국어로 오면 바뀐 건지 알 수 없다. 남기는 것은 여기서 한다.
        Pick::UseLang { lang } => {
            state.picker = None;
            state.lang = lang;
            crate::lang::set(lang);
            crate::lang::save(lang);
            state.timeline.say(lang.lang_changed().to_string());
        }
        Pick::UseAgent { name } => {
            state.picker = None;
            switch_agent(api, state, session, agent_id, &name).await;
        }
        // **바로 실행하지 않는다.** `/mode`처럼 인자를 받는 것이 있어서, 고른 뒤
        // 이어 칠 수 있어야 한다. 뒤에 공백을 붙여 두면 바로 인자를 칠 수 있다.
        Pick::TypeCommand { text } => {
            state.picker = None;
            state.input.take();
            state.input.insert_str(&format!("{text} "));
        }
    }
    Ok(())
}

/// 다른 세션으로 갈아탄다. 지난 기록을 되읽어 화면을 채우고 라이브 스트림을 다시 연다.
async fn switch(
    api: &Arc<AttaccaApiClient>,
    state: &mut State,
    session: &mut Session,
    id: String,
    project_id: Option<String>,
    tx: &mpsc::UnboundedSender<AppMsg>,
) -> anyhow::Result<()> {
    let events = crate::conn::history(api, &id).await?;

    // 앞 세션의 화면을 지우고 다시 쌓는다. 안 지우면 두 세션이 섞인다.
    state.timeline = Timeline::new();
    state.folds = Folds::new();
    state.asking = None;
    state.last_cursor = None;

    for event in events {
        let cursor = event.cursor;
        let entry = crate::event::entry_from(&event);
        apply(state, &Action::Frame(Frame::Event { cursor, entry }));
    }

    session.switch_to(id.clone(), project_id);
    // 제목은 폴링을 기다리지 않고 지금 맞춘다 — 세션을 바꿨는데 창 제목이 몇 초 동안
    // 앞 세션을 가리키면 어느 대화를 보고 있는지 헷갈린다.
    state.title = crate::conn::session_title(api, &id).await.unwrap_or_else(|| "Zyris Code".into());
    state.sidebar.clear();
    state.picker = None;
    state.scroll = Scroll::new(); // 바닥부터 본다.
                                  // 되읽은 곳 다음부터 이어 듣는다.
    spawn_stream(Arc::clone(api), id, state.last_cursor, tx.clone());
    Ok(())
}

/// 세션을 (없으면 만들어) 확보하고, 메시지를 보내고, 턴 스트림을 연다.
/// 턴이 끝났으면 들고 있던 말을 순서대로 보낸다.
///
/// **하나가 실패하면 거기서 멈추고 나머지는 그대로 둔다.** 다음 턴이 끝날 때 다시 시도한다 —
/// 못 보낸 것을 조용히 버리면 사람은 보냈다고 믿는다.
/// 슬래시 명령 중 **서버나 디스크가 필요한 것**을 마저 한다.
///
/// 순수한 부분은 `run_command`가 이미 끝냈다. 여기 오는 것은 넷뿐이다.
async fn finish_command(
    api: &Arc<AttaccaApiClient>,
    bridge: &crate::tools::bridge::Bridge,
    state: &mut State,
    session: &mut Session,
    agent_id: &mut String,
    text: &str,
) {
    use crate::command::Command;
    let Some(cmd) = run_command(state, text) else { return };
    match cmd {
        Command::Mcp => state.timeline.say(mcp_report_text(&bridge.mcp_report())),
        Command::Skills => {
            let skills = crate::tools::skill::Skills::discover(&state.cwd);
            state.timeline.say(skills_text(&skills.list()));
        }
        // **preamble은 안 보이는 자리다.** 조용히 안 실렸을 때 알 방법이 이것뿐이다.
        Command::Rules => {
            let found = crate::instructions::collect(&state.cwd);
            state.timeline.say(rules_text(&found));
        }
        Command::Agent(None) => match api.list_agents().await {
            Ok(agents) => {
                let rows = agents
                    .into_iter()
                    .map(|a| crate::picker::Row {
                        id: Some(a.name.clone()),
                        label: a.name,
                        note: None,
                        enabled: true,
                    })
                    .collect();
                state.picker = Some(crate::picker::Picker::agents(rows));
            }
            Err(e) => state.timeline.say(format!("에이전트 목록을 읽지 못했습니다: {e}")),
        },
        Command::Agent(Some(name)) => switch_agent(api, state, session, agent_id, &name).await,
        // 인자가 없으면 목록을 연다 — 만들기 전에 이미 있는 것을 보는 편이 낫다.
        Command::Project(None) => match crate::conn::projects(api).await {
            Ok(items) => state.picker = Some(crate::picker::Picker::projects(items, state.lang)),
            Err(e) => state.timeline.say(format!("{e}")),
        },
        Command::Project(Some(name)) => match crate::conn::create_project(api, &name).await {
            Ok((id, name)) => {
                // **만들고 그 안으로 들어간다.** 만들어 놓고 다시 골라야 하면 두 번 일이고,
                // 방금 만든 것 말고 다른 데서 일을 시작하는 사고가 난다.
                session.enter_project(id);
                session.stage_new_default();
                state.picker = None;
                let said = state.lang.project_created(&name);
                state.timeline.say(said);
            }
            Err(e) => state.timeline.say(format!("{e}")),
        },
        Command::Plugin(what) => {
            let said = run_plugin(state, what).await;
            state.timeline.say(said);
        }
        // **닫았다는 것이 게이트에도 가야 한다.** `run_command`가 상태에서 지웠을 뿐이라,
        // 여기서 옮기지 않으면 화면은 닫혔다는데 도구는 계속 통과시킨다.
        Command::GrantsClose => bridge.sync(state.mode, &state.grants),
        // **고른 언어는 두 군데로 간다.** 화면 밖(셸 알림·도구 오류)이 쓰는 전역과,
        // 다음 실행이 읽을 파일. `run_command`는 순수하므로 상태만 바꿔 두었다.
        Command::Lang(Some(lang)) => {
            crate::lang::set(lang);
            crate::lang::save(lang);
        }
        Command::Changes => {
            let said = match bridge.undo() {
                Some(undo) => changes_text(&undo.changed(), &state.cwd),
                None => "되돌림 기록을 아직 열지 못했습니다.".into(),
            };
            state.timeline.say(said);
        }
        Command::Undo => {
            let said = match bridge.undo() {
                Some(undo) => match undo.revert_last() {
                    Ok(path) => {
                        let shown = path.strip_prefix(&state.cwd).unwrap_or(&path);
                        // **에이전트에게는 알리지 않는다.** 도는 턴 한가운데에 "네가 한
                        // 걸 물렸다"를 끼워 넣으면 같은 편집을 다시 시도한다. 다음 턴에
                        // 파일을 읽으면 어차피 바뀐 것이 보인다.
                        format!("되돌렸습니다: `{}`", shown.display())
                    }
                    Err(why) => why,
                },
                None => "되돌림 기록을 아직 열지 못했습니다.".into(),
            };
            state.timeline.say(said);
        }
        _ => {}
    }
}

/// 에이전트를 바꾼다. **못 찾으면 바꾸지 않는다.**
///
/// 조용히 폴백하면 상태줄에는 새 이름이 뜨는데 전송이 `Agent not found`로 실패하는,
/// 원인을 찾기 어려운 상태가 된다 — `conn.rs`가 이미 겪은 자리다.
async fn switch_agent(
    api: &Arc<AttaccaApiClient>,
    state: &mut State,
    session: &mut Session,
    agent_id: &mut String,
    name: &str,
) {
    match Session::agent_id_named(api, name).await {
        Ok(id) => {
            *agent_id = id;
            state.agent = name.to_string();
            // **세션의 에이전트는 만들 때 정해지고 바꾸는 API가 없다**
            // (`ZNewSession.agent_id`). 그래서 새 세션을 예약한다 — 서버에는 아직
            // 아무것도 만들지 않고, 다음 메시지에서 열린다.
            session.stage_new_default();
            state.timeline.say(format!(
                "에이전트: **{name}** · 다음 메시지에서 새 thread가 열립니다. \
                 앞 thread는 ←의 목록에 그대로 있습니다."
            ));
        }
        Err(e) => state.timeline.say(format!("{e}")),
    }
}

/// `/changes`가 찍는 글.
///
/// **경로는 작업 디렉터리 기준으로 줄인다.** 절대경로 열 줄은 어느 것이 어느 것인지
/// 눈으로 가려내기 어렵고, 정작 다른 것은 뒤쪽 몇 글자뿐이다.
fn changes_text(changed: &[crate::undo::Changed], cwd: &std::path::Path) -> String {
    if changed.is_empty() {
        return "이 디렉터리에서 바꾼 파일이 없습니다.".into();
    }
    let mut s = format!("바꾼 파일 {}개입니다. 최근에 손댄 것이 위입니다.\n", changed.len());
    for c in changed {
        let shown = c.path.strip_prefix(cwd).unwrap_or(&c.path);
        // 만든 파일이라는 것이 +N보다 먼저다 — 되돌리면 지워진다는 뜻이라 무게가 다르다.
        let note = if c.created {
            " · 새로 만든 것".to_string()
        } else if c.edits > 1 {
            format!(" · {}번 고침", c.edits)
        } else {
            String::new()
        };
        s.push_str(&format!("\n- `{}`  +{} −{}{note}", shown.display(), c.added, c.removed));
    }
    // **되돌릴 수 있는 범위와 같다는 것을 말해 둔다.** 이 목록을 보고 `/undo`를 누르므로,
    // 둘의 범위가 다르면 눌러 보고서야 안다.
    s.push_str("\n\n`/undo`가 이 기록을 뒤에서부터 되돌립니다. 앱을 껐다 켜도 남습니다.");
    s
}

fn mcp_report_text(report: &[(String, Result<usize, String>)]) -> String {
    if report.is_empty() {
        return "붙은 MCP 서버가 없습니다. `.mcp.json`이나 \
                `~/.config/zyris-code/mcp.json`에 적습니다."
            .into();
    }
    let mut s = String::from("MCP 서버입니다.\n");
    for (name, outcome) in report {
        s.push_str(&match outcome {
            Ok(n) => format!("\n- `{name}` — 도구 {n}개"),
            Err(why) => format!("\n- `{name}` — 못 띄웠습니다: {why}"),
        });
    }
    s
}

/// `/plugin`. **받는 것은 남의 코드를 이 컴퓨터에 놓는 일이다** — 그 사실을 말해 준다.
async fn run_plugin(state: &mut State, what: crate::command::Plugin) -> String {
    use crate::command::Plugin as P;
    use crate::plugin;

    match what {
        P::List => plugin_list_text(&plugin::discover(&state.cwd)),
        P::Add(source) => match plugin::install(&source).await {
            Ok(p) => format!(
                "**{}**을 받았습니다.{}\n\n{}\n\n\
                 다음에 zyris-code를 다시 띄우면 붙습니다 — 도구도 스킬도 시작할 때 읽습니다.",
                p.name,
                if p.description.is_empty() { String::new() } else { format!(" {}", p.description) },
                plugin_contents_text(&p),
            ),
            Err(why) => why,
        },
        P::Remove(name) => match plugin::remove(&name) {
            Ok(()) => format!("`{name}`을 지웠습니다. 다시 띄우면 빠집니다."),
            Err(why) => why,
        },
        P::Update(name) => {
            let done = plugin::update(name.as_deref()).await;
            if done.is_empty() {
                return "받아 둔 플러그인이 없습니다.".into();
            }
            let mut s = String::from("갱신했습니다.\n");
            for (name, outcome) in done {
                s.push_str(&match outcome {
                    Ok(out) if out.contains("up to date") || out.contains("최신") => {
                        format!("\n- `{name}` — 이미 최신입니다")
                    }
                    Ok(_) => format!("\n- `{name}` — 새로 받았습니다"),
                    Err(why) => format!("\n- `{name}` — {why}"),
                });
            }
            s.push_str("\n\n다시 띄우면 반영됩니다.");
            s
        }
        P::Unknown(why) => format!(
            "`/plugin {why}`\n\n             - `/plugin` — 받아 둔 것 보기\n             - `/plugin add owner/repo` — 받기\n             - `/plugin remove 이름` — 지우기\n             - `/plugin update [이름]` — 갱신"
        ),
    }
}

/// 이 플러그인이 무엇을 얹는가. **받자마자 보여준다** — 남의 코드가 무슨 명령을 돌릴지
/// 모르는 채로 두면 안 된다.
fn plugin_contents_text(p: &crate::plugin::Plugin) -> String {
    let mut s = String::new();
    for spec in &p.mcp {
        s.push_str(&format!(
            "- MCP `{}` — 다음 실행 때 `{}`을 돌립니다\n",
            spec.slug, spec.command
        ));
    }
    if p.skills.is_some() {
        s.push_str("- 스킬이 딸려 있습니다\n");
    }
    if s.is_empty() {
        s.push_str("- 얹는 것이 없습니다\n");
    }
    s
}

fn plugin_list_text(found: &[crate::plugin::Plugin]) -> String {
    if found.is_empty() {
        return "플러그인이 없습니다. `/plugin add owner/repo`로 받습니다.".into();
    }
    let mut s = String::from("플러그인입니다.\n");
    for p in found {
        s.push_str(&format!(
            "\n- **{}**{}{}",
            p.name,
            // 받아 온 것과 손으로 둔 것을 갈라 준다 — 지울 수 있는 것은 받아 온 쪽뿐이다.
            if p.fetched() { "" } else { " (직접 둔 것)" },
            if p.description.is_empty() {
                String::new()
            } else {
                format!(" — {}", p.description)
            },
        ));
    }
    s
}

fn rules_text(found: &[crate::instructions::Found]) -> String {
    if found.is_empty() {
        return "이 디렉터리와 그 위쪽에 `CLAUDE.md`도 `AGENTS.md`도 없습니다.".into();
    }
    let mut s = String::from("이 thread에 실린 지침입니다. 아래로 갈수록 구체적입니다.\n");
    for f in found {
        // **세션을 만들 때 실린 것이지 지금 파일이 아니다.** 고쳤으면 새 세션이 필요하다.
        s.push_str(&format!("\n- `{}` — {}자", f.path.display(), f.text.chars().count()));
    }
    s.push_str("\n\n파일을 고쳤으면 `/agent`이나 ←의 목록으로 새 thread를 열어야 반영됩니다.");
    s
}

fn skills_text(skills: &[crate::tools::skill::SkillInfo]) -> String {
    if skills.is_empty() {
        return "쓸 수 있는 스킬이 없습니다. `.zyris-code/skills/`나 \
                `~/.config/zyris-code/skills/`에 둡니다."
            .into();
    }
    let mut s = String::from("쓸 수 있는 스킬입니다.\n");
    for skill in skills {
        s.push_str(&format!("\n- **{}** — {}", skill.name, skill.description));
    }
    s
}

async fn flush_queue(
    api: &Arc<AttaccaApiClient>,
    session: &mut Session,
    agent_id: &str,
    state: &mut State,
    tx: &mpsc::UnboundedSender<AppMsg>,
) {
    if !std::mem::take(&mut state.flush_queue) {
        return;
    }
    if agent_id.is_empty() {
        state.set_status("에이전트를 찾지 못해 보낼 수 없습니다.");
        return;
    }
    while let Some(text) = state.queued.first().cloned() {
        match send(api, session, agent_id, &text, state.last_cursor, state.mode, tx).await {
            Ok(announced) => {
                state.queued.remove(0);
                state.remember_sent(&text);
                // **대기열의 첫 말도 work·job을 열 수 있다.** 일하는 중에 모드를 바꿔 두고
                // 말을 담아 두면 그 말이 목표가 된다 — 열렸다는 것을 여기서도 말해야 한다.
                if let Some(said) = opened_text(state.lang, announced) {
                    state.timeline.say(said);
                }
            }
            Err(e) => {
                state.set_status(e.to_string());
                return;
            }
        }
    }
}

/// 모드가 정한 곳으로 다음 메시지가 가도록 세션 예약을 맞춘다. **모드가 바뀐 순간에만 돈다.**
///
/// **하던 대화가 있으면 모드는 그것을 가로채지 않는다.** work·job으로 바꿔도 지금
/// 세션은 그대로 이어가고, 새 work·job은 새 쓰레드에서 연다 — 예전에는 모드를
/// 바꾸는 순간 예약이 서서 다음 메시지가 조용히 새 세션을 열었다. 실제로 그렇게
/// 헷갈렸다. 사람에게는 "지금 대화는 그대로"라고 말해 준다.
///
/// 판정 자체는 `Mode::route()`가 하고 여기는 그것을 세션에 옮기고 사람에게 말할 뿐이다.
fn restage(state: &mut State, session: &mut Session) {
    let route = state.mode.route();
    // 세션이 있으면 예약하지 않는다 — `open_for`의 "예약이 없고 id가 있으면 이어
    // 붙인다"가 그대로 동작해 다음 메시지는 지금 대화로 간다.
    if session.id().is_some() {
        session.set_route(crate::mode::Route::Session);
        let said = match route {
            crate::mode::Route::Work => state.lang.mode_continues_work().to_string(),
            crate::mode::Route::Job => state.lang.mode_continues_job().to_string(),
            crate::mode::Route::Session => return,
        };
        state.timeline.say(said);
        return;
    }
    session.set_route(route);
    match route {
        crate::mode::Route::Session => {}
        crate::mode::Route::Work => state.timeline.say(state.lang.mode_opens_work().to_string()),
        crate::mode::Route::Job => state.timeline.say(state.lang.mode_opens_job().to_string()),
    }
}

/// 보내고, 새로 연 것이 있으면 그것을 말한다.
///
/// **부르는 자리가 둘이라 여기 하나로 둔다**(입력란의 Enter와 질문 카드의 제출). 한쪽만
/// 고치면 같은 일을 하는 두 길이 서로 다른 말을 하게 된다.
async fn send_and_tell(
    api: &Arc<AttaccaApiClient>,
    state: &mut State,
    session: &mut Session,
    agent_id: &str,
    text: &str,
    tx: &mpsc::UnboundedSender<AppMsg>,
) {
    match send(api, session, agent_id, text, state.last_cursor, state.mode, tx).await {
        Ok(announced) => {
            if let Some(said) = opened_text(state.lang, announced) {
                state.timeline.say(said);
            }
        }
        Err(e) => state.set_status(e.to_string()),
    }
}

/// 새로 연 것을 뭐라고 말할까. **`None`이면 아무 말도 안 한다** — 이어 붙였을 뿐인데 매
/// 메시지에 한 줄씩 붙으면 대화가 그것으로 덮인다.
///
/// 보내는 길이 둘이라(입력란, 대기열) 여기 한 자리에 둔다.
fn opened_text(
    lang: crate::lang::Lang,
    announced: Option<(crate::mode::Route, String)>,
) -> Option<String> {
    match announced? {
        (crate::mode::Route::Work, id) => Some(lang.opened_work(&id)),
        (crate::mode::Route::Job, id) => Some(lang.opened_job(&id)),
        (crate::mode::Route::Session, _) => None,
    }
}

async fn send(
    api: &Arc<AttaccaApiClient>,
    session: &mut Session,
    agent_id: &str,
    text: &str,
    after: Option<i64>,
    mode: Mode,
    tx: &mpsc::UnboundedSender<AppMsg>,
) -> anyhow::Result<Option<(crate::mode::Route, String)>> {
    let opened = session.open_for(api, agent_id, text, mode).await?;
    let id = opened.id;
    // **job·work는 여는 요청이 첫 메시지를 이미 먹었다**(`ZNewJob::message`). 여기서 또
    // 보내면 같은 말이 두 번 들어가고, job은 그것을 새 지시로 읽는다.
    if !opened.sent {
        api.send_message(id.clone(), text.to_string(), vec![])
            .await
            .map_err(|e| anyhow::anyhow!("전송하지 못했습니다: {e}"))?;
    }

    // **`after`를 비우면 안 된다.** 비우면 "지금부터의 라이브 프레임만" 오는데, 방금 보낸
    // 메시지의 `chat_user` 이벤트는 스트림을 열기 전에 이미 기록돼 있어 영영 못 본다 —
    // 보낸 말이 대화창에서 사라진다. 아직 아무것도 못 봤으면 0부터 되읽는다.
    spawn_stream(Arc::clone(api), id, Some(after.unwrap_or(0)), tx.clone());
    Ok(opened.announced)
}

/// 턴 스트림을 배경에서 읽어 액션으로 흘려보낸다.
///
/// 프레임마다 **자기 세션의 id를 태그로 실어** 보낸다. 받는 쪽은 그 태그로 낡은
/// 세션의 프레임을 버린다(`frame_is_current`) — 스트림을 끊는 대신 버리는 이유는
/// 서버에서 도는 턴을 죽이지 않기 위해서다. 그 세션으로 돌아가면 다시 열 때
/// 히스토리로 전부 보인다.
fn spawn_stream(
    api: Arc<AttaccaApiClient>,
    session_id: String,
    after: Option<i64>,
    tx: mpsc::UnboundedSender<AppMsg>,
) {
    tokio::spawn(async move {
        let tag = session_id.clone();
        match api.turn_events(session_id, after).await {
            Ok(mut stream) => {
                // `Streaming`은 head와 items로 나뉜다. head가 현재 실행 상태를 들고 온다.
                let _ =
                    tx.send((Some(tag.clone()), Action::Frame(Frame::Status { running: stream.head.running })));
                while let Some(frame) = stream.items.next().await {
                    match frame {
                        Ok(f) => {
                            if tx
                                .send((Some(tag.clone()), Action::Frame(frame_from(f))))
                                .is_err()
                            {
                                break; // 앱이 끝났다.
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "턴 스트림이 끊겼다");
                            break;
                        }
                    }
                }
                // 스트림이 끝나면 턴도 끝난 것이다. 상태줄이 "작업 중"에 얼어붙으면 안 된다.
                let _ = tx.send((Some(tag), Action::Frame(Frame::Status { running: false })));
            }
            Err(e) => tracing::error!(error = %e, "턴 스트림을 열지 못했다"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Entry, EntryKind};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn state() -> State {
        State::new()
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn ask(id: u64) -> ToolAsk {
        ToolAsk {
            id,
            call: crate::tools::gate::Call::new("code_edit", "edit", "x.rs".into())
                .leaving(Some(std::path::PathBuf::from("/home/ruma/attacca/x.rs"))),
            summary: "/home/ruma/attacca/x.rs".into(),
            expired: false,
        }
    }

    /// 승인 창 두 개가 겹치면 무엇에 답하는지 알 수 없다.
    #[test]
    fn a_second_ask_waits_behind_the_first() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Ask(ask(1))));
        apply(&mut s, &Action::Frame(Frame::Ask(ask(2))));
        assert_eq!(s.pending.as_ref().unwrap().id, 1);
        assert_eq!(s.ask_queue.len(), 1);

        apply(&mut s, &Action::Deny);
        assert_eq!(s.pending.as_ref().unwrap().id, 2, "답하면 다음 것이 올라와야 한다");
        assert!(s.ask_queue.is_empty());
    }

    /// 답은 도구 쪽으로 되돌아가야 한다 — 안 가면 그 호출이 영영 막혀 있다.
    #[test]
    fn answering_sends_the_verdict_back() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Ask(ask(7))));
        apply(&mut s, &Action::Approve);
        assert_eq!(s.verdict_out, Some((7, Verdict::Allow)));
    }

    /// **`a`는 그 디렉터리째로 연다.** 파일 하나하나를 다시 묻는 것은 쓸 수 없다.
    #[test]
    fn always_allow_opens_the_whole_directory() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Ask(ask(3))));
        apply(&mut s, &Action::AlwaysAllow);
        assert_eq!(s.verdict_out, Some((3, Verdict::AllowAlways)));
        assert!(s.grants.covers(std::path::Path::new("/home/ruma/attacca/다른것.rs")));
        assert!(!s.grants.covers(std::path::Path::new("/home/ruma/prompts/x.yml")));
    }

    /// **이미 포기한 호출에는 답을 보내지 않는다.** 보내면 에이전트가 모르는 채로
    /// 파일이 바뀐다 — 허용만 기록되고 다음 호출에서 그것이 쓰인다.
    #[test]
    fn approving_an_expired_ask_only_records_the_grant() {
        let mut s = state();
        let mut a = ask(9);
        a.expired = true;
        apply(&mut s, &Action::Frame(Frame::Ask(a)));
        apply(&mut s, &Action::AlwaysAllow);
        assert_eq!(s.verdict_out, None, "이미 포기한 호출에 답을 보내면 안 된다");
        assert!(s.grants.covers(std::path::Path::new("/home/ruma/attacca/x.rs")));
    }

    /// 마감이 지나도 **창은 남는다.** 치워 버리면 사람이 무엇을 놓쳤는지 모른다.
    #[test]
    fn a_pending_ask_can_go_stale_while_it_waits() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Ask(ask(4))));
        apply(&mut s, &Action::Frame(Frame::Expired(4)));
        assert!(s.pending.as_ref().unwrap().expired, "포기한 것을 안 알리면 헛승인을 한다");
    }

    /// 승인 창이 떠 있으면 y·n·a가 글자가 아니라 답이다.
    #[test]
    fn the_approval_keys_answer_instead_of_typing() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Ask(ask(1))));
        assert_eq!(on_key(&s, key(KeyCode::Char('y'), KeyModifiers::NONE)), vec![Action::Approve]);
        assert_eq!(on_key(&s, key(KeyCode::Char('n'), KeyModifiers::NONE)), vec![Action::Deny]);
        assert_eq!(
            on_key(&s, key(KeyCode::Char('a'), KeyModifiers::NONE)),
            vec![Action::AlwaysAllow]
        );
        // **Enter는 없다.** 다음 말을 치려고 누르던 손이 승인을 눌러 버리면 안 된다.
        assert!(on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)).is_empty());
    }

    /// 승인 창이 없을 때는 그냥 글자다.
    #[test]
    fn those_keys_are_just_letters_when_nothing_is_pending() {
        let s = state();
        assert_eq!(
            on_key(&s, key(KeyCode::Char('a'), KeyModifiers::NONE)),
            vec![Action::Insert('a')]
        );
    }

    /// 승인 창이 떠 있어도 종료는 통해야 한다. 막히면 빠져나갈 길이 없다.
    #[test]
    fn ctrl_c_still_works_while_an_approval_is_up() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Ask(ask(1))));
        assert_eq!(
            on_key(&s, key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Action::ArmQuit]
        );
    }

    fn enroll() -> EnrollView {
        EnrollView {
            code: "WXQR-7KBD".into(),
            uri: "https://attacca.example/settings/zyris/device".into(),
            expires_at: Instant::now() + Duration::from_secs(600),
            phase: EnrollPhase::Waiting,
        }
    }

    /// **등록 코드 창은 Esc로만 닫힌다.** 다른 키가 창을 치우면 코드를 보지도
    /// 못한 채 승인 단계를 지나친다 — `y`가 승인 창에 먹히지도 않아야 한다.
    #[test]
    fn the_enroll_window_closes_only_with_esc() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Enroll(enroll())));
        assert!(s.enroll.is_some());

        // 아무 키나 눌러도 창은 그대로다.
        assert_eq!(
            on_key(&s, key(KeyCode::Char('y'), KeyModifiers::NONE)),
            vec![],
            "등록 중에 y가 승인으로 먹히면 안 된다"
        );
        assert_eq!(on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)), vec![]);
        assert_eq!(on_key(&s, key(KeyCode::Up, KeyModifiers::NONE)), vec![]);

        // Esc 하나만 닫는다.
        assert_eq!(on_key(&s, key(KeyCode::Esc, KeyModifiers::NONE)), vec![Action::EnrollClose]);
        apply(&mut s, &Action::EnrollClose);
        assert!(s.enroll.is_none(), "Esc로 닫아야 한다");
    }

    /// **등록 코드 창이 떠 있어도 끄는 길은 막지 않는다.** Ctrl+C는 언제나 통한다 —
    /// 화면이 코드를 가린 채 죽은 채로 남으면 빠져나갈 길이 없다.
    #[test]
    fn ctrl_c_still_quits_while_the_enroll_window_is_up() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Enroll(enroll())));
        assert_eq!(
            on_key(&s, key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Action::ArmQuit]
        );
    }

    /// **승인이 도착하면 창이 저절로 닫힌다.** 사람이 Esc를 누르지 않아도
    /// 붙었으면 등록 창이 남아 있을 이유가 없다.
    #[test]
    fn approving_clears_the_enroll_window() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Enroll(enroll())));
        apply(&mut s, &Action::Frame(Frame::EnrollDone));
        assert!(s.enroll.is_none(), "승인됐는데 창이 남아 있다");
    }

    /// 만료·거부는 창을 닫지 않고 **사정만 바꾼다** — 새 코드가 오면 그 위에
    /// 다시 그리므로, 닫아 버리면 중간 상태를 놓친다.
    #[test]
    fn a_lapsed_or_denied_code_changes_the_phase_not_the_window() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Enroll(enroll())));
        apply(&mut s, &Action::Frame(Frame::EnrollPhase(EnrollPhase::Lapsed)));
        assert_eq!(s.enroll.as_ref().unwrap().phase, EnrollPhase::Lapsed);
        assert!(s.enroll.is_some(), "만료됐다고 창을 닫으면 안 된다");

        apply(&mut s, &Action::Frame(Frame::EnrollPhase(EnrollPhase::Denied)));
        assert_eq!(s.enroll.as_ref().unwrap().phase, EnrollPhase::Denied);
    }

    /// 앱이 마지막으로 한 말.
    fn last_system(state: &mut State) -> String {
        state
            .timeline
            .items()
            .iter()
            .rev()
            .find_map(|i| match i {
                crate::timeline::Item::System { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn typed(state: &mut State, text: &str) {
        for c in text.chars() {
            apply(state, &Action::Insert(c));
        }
    }

    /// **슬래시 명령은 서버로 안 간다.** 오타 하나가 크레딧을 쓰면 안 된다.
    #[test]
    fn a_slash_command_never_reaches_the_server() {
        let mut s = State::new();
        apply(&mut s, &Action::Submit("/cwd".into()));
        assert_eq!(s.command_out.as_deref(), Some("/cwd"));
        assert!(s.queued.is_empty(), "명령이 대기열로 샜다");
    }

    /// 일하는 중이어도 명령은 지금 돈다 — 모드를 바꾸는 일이 턴을 기다릴 이유가 없다.
    #[test]
    fn a_command_runs_even_while_a_turn_is_going() {
        let mut s = State::new();
        s.running = true;
        apply(&mut s, &Action::Submit("/mode 계획".into()));
        assert_eq!(s.command_out.as_deref(), Some("/mode 계획"));
        assert!(s.queued.is_empty(), "{:?}", s.queued);
    }

    /// **하던 대화가 있으면 모드가 그것을 가로채지 않는다.** work·job으로 바꿔도
    /// 다음 메시지는 지금 세션으로 간다 — 예전에는 모드를 바꾸는 순간 예약이 서서
    /// 조용히 새 세션을 열었다. 실제로 그렇게 헷갈렸다.
    #[test]
    fn restage_keeps_the_active_conversation() {
        let mut s = State::new();
        s.lang = crate::lang::Lang::Ko;
        let mut session = Session::new(None);
        session.switch_to("지금-세션".into(), None);
        s.mode = crate::mode::Mode::Work;
        restage(&mut s, &mut session);
        assert_eq!(session.pending_open(), None, "하던 대화를 가로채면 안 된다");
        assert!(last_system(&mut s).contains("이어갑니다"), "{}", last_system(&mut s));

        // 영어 화면도 같은 뜻을 말한다 — 번역이 빠지면 안 된다.
        let mut s = State::new();
        s.lang = crate::lang::Lang::En;
        let mut session = Session::new(None);
        session.switch_to("지금-세션".into(), None);
        s.mode = crate::mode::Mode::Work;
        restage(&mut s, &mut session);
        assert!(last_system(&mut s).contains("continues as-is"), "{}", last_system(&mut s));
    }

    /// 대화가 없으면 모드가 무엇을 열지 정한다 — 첫 메시지가 work·job이 된다.
    #[test]
    fn restage_stages_an_open_only_without_a_conversation() {
        let mut s = State::new();
        let mut session = Session::new(None);
        s.mode = crate::mode::Mode::Job;
        restage(&mut s, &mut session);
        assert_eq!(session.pending_open(), Some(crate::mode::Route::Job));
        assert!(last_system(&mut s).contains("job"), "{}", last_system(&mut s));
    }

    /// 평범한 메시지는 그대로 서버로 간다.
    #[test]
    fn a_normal_message_still_goes_to_the_server() {
        let mut s = State::new();
        apply(&mut s, &Action::Submit("안녕".into()));
        assert!(s.command_out.is_none());
    }

    /// `/mode 계획`은 모드를 바꾸고 화면에 남는다. **한국어 이름도 받는다** — 영어 화면을
    /// 쓰는 사람도 손에 익은 말로 칠 수 있어야 한다.
    #[test]
    fn the_mode_command_changes_the_mode_and_says_so() {
        let mut s = State::new();
        s.lang = crate::lang::Lang::Ko;
        run_command(&mut s, "/mode 계획");
        assert_eq!(s.mode, crate::mode::Mode::Plan);
        assert!(last_system(&mut s).contains("계획"), "{}", last_system(&mut s));

        let mut s = State::new();
        s.lang = crate::lang::Lang::En;
        run_command(&mut s, "/mode plan");
        assert_eq!(s.mode, crate::mode::Mode::Plan);
        assert!(last_system(&mut s).contains("plan"), "{}", last_system(&mut s));
    }

    /// **화면 말을 고르는 창이 뜬다.** 둘뿐이라 목록이 곧 답이다.
    #[test]
    fn the_lang_command_opens_a_list_to_pick_from() {
        let mut s = State::new();
        run_command(&mut s, "/lang");
        let picker = s.picker.as_ref().expect("목록이 떠야 한다");
        let labels: Vec<&str> = picker.rows.iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains(&"English"), "{labels:?}");
        assert!(labels.contains(&"한국어"), "{labels:?}");
    }

    /// 이름을 바로 대면 창 없이 바꾼다 — `/lang ko`가 손에 익은 사람도 있다.
    #[test]
    fn naming_a_language_changes_it_without_the_list() {
        let mut s = State::new();
        run_command(&mut s, "/lang ko");
        assert_eq!(s.lang, crate::lang::Lang::Ko);
        assert!(s.picker.is_none(), "이름을 댔으면 목록을 열 이유가 없다");
        assert!(last_system(&mut s).contains("한국어"), "{}", last_system(&mut s));
    }

    /// 모르는 명령은 무엇이 있는지 알려 준다. "모릅니다"만 하면 다음에 또 틀린다.
    #[test]
    fn an_unknown_command_lists_what_exists() {
        let mut s = State::new();
        run_command(&mut s, "/nope");
        assert!(last_system(&mut s).contains("/help"), "{}", last_system(&mut s));
    }

    /// `/clear`는 **화면만** 비운다. 지웠다고 세션이 없어진 줄 알면 안 된다.
    #[test]
    fn clearing_says_the_session_is_still_there() {
        let mut s = State::new();
        apply(&mut s, &work_start(1));
        run_command(&mut s, "/clear");
        assert!(last_system(&mut s).contains("thread"), "{}", last_system(&mut s));
        assert_eq!(s.timeline.items().len(), 1, "지운 뒤에는 안내 한 줄만 남는다");
    }

    /// 열어 둔 것이 없으면 **어떻게 열리는지**를 말해야 한다. "없습니다"만으로는
    /// 이 명령이 무엇을 보여주는 것인지 알 수 없다.
    #[test]
    fn listing_grants_with_nothing_open_says_how_they_open() {
        let mut s = State::new();
        run_command(&mut s, "/grants");
        let said = last_system(&mut s);
        assert!(said.contains('a'), "{said}");
    }

    /// **한 번 누른 `a`가 세션 내내 산다.** 무엇을 열어 뒀는지 볼 수 있어야 한다.
    #[test]
    fn listing_grants_names_every_directory_that_is_open() {
        let mut s = State::new();
        s.grants.allow_under(std::path::Path::new("/home/ruma/attacca/Cargo.toml"));
        s.grants.allow_under(std::path::Path::new("/home/ruma/prompts/x.yml"));

        run_command(&mut s, "/grants");
        let said = last_system(&mut s);
        assert!(said.contains("/home/ruma/attacca"), "{said}");
        assert!(said.contains("/home/ruma/prompts"), "{said}");
    }

    /// 닫으면 정말 닫혀야 한다 — 말만 하고 남아 있으면 최악이다.
    #[test]
    fn closing_grants_actually_closes_them() {
        let mut s = State::new();
        s.grants.allow_under(std::path::Path::new("/home/ruma/attacca/Cargo.toml"));

        run_command(&mut s, "/grants close");
        assert!(s.grants.is_empty(), "닫았다는데 남아 있다");
        assert!(last_system(&mut s).contains('1'), "{}", last_system(&mut s));
    }

    /// 목록을 보려다 닫으면 안 된다.
    #[test]
    fn listing_grants_does_not_close_them() {
        let mut s = State::new();
        s.grants.allow_under(std::path::Path::new("/home/ruma/attacca/Cargo.toml"));
        run_command(&mut s, "/grants");
        assert!(!s.grants.is_empty(), "보기만 했는데 닫혔다");
    }

    /// `/quit`는 깃발만 세운다. 끄는 것은 I/O 자리가 한다 — 거기서 도는 턴도 멈춘다.
    #[test]
    fn the_quit_command_asks_the_io_side_to_leave() {
        let mut s = State::new();
        assert!(!s.quitting);
        run_command(&mut s, "/quit");
        assert!(s.quitting);
    }

    /// 바꾼 것이 없으면 그렇게 말한다.
    #[test]
    fn the_changes_list_says_when_nothing_was_touched() {
        let said = changes_text(&[], std::path::Path::new("/home/ruma/zyris-code"));
        assert!(said.contains("없습니다"), "{said}");
    }

    /// **만든 파일은 그렇다고 말한다.** 되돌리면 지워진다는 뜻이라 무게가 다르다.
    #[test]
    fn the_changes_list_shows_counts_and_marks_new_files() {
        let cwd = std::path::Path::new("/home/ruma/zyris-code");
        let said = changes_text(
            &[
                crate::undo::Changed {
                    path: cwd.join("src/app.rs"),
                    edits: 3,
                    created: false,
                    added: 42,
                    removed: 7,
                },
                crate::undo::Changed {
                    path: cwd.join("src/enroll.rs"),
                    edits: 1,
                    created: true,
                    added: 90,
                    removed: 0,
                },
            ],
            cwd,
        );
        // 경로는 작업 디렉터리 기준으로 줄인다 — 절대경로 열 줄은 눈으로 못 가려낸다.
        assert!(said.contains("`src/app.rs`  +42 −7"), "{said}");
        assert!(said.contains("3번 고침"), "{said}");
        assert!(said.contains("새로 만든 것"), "{said}");
        assert!(!said.contains("/home/ruma"), "절대경로가 그대로 나왔다:\n{said}");
    }

    /// **`/`를 치면 목록이 뜬다.** 안 뜨면 무슨 명령이 있는지 알 방법이 없다.
    #[test]
    fn typing_a_slash_opens_the_command_list() {
        let mut s = State::new();
        typed(&mut s, "/");
        assert!(s.picker.is_some(), "목록이 안 떴다");
        assert!(matches!(
            s.picker.as_ref().map(|p| &p.level),
            Some(crate::picker::Level::Commands)
        ));
    }

    /// 치는 대로 좁혀져야 고르는 값이 있다.
    #[test]
    fn typing_narrows_the_command_list() {
        let mut s = State::new();
        typed(&mut s, "/mo");
        let rows = &s.picker.as_ref().expect("목록이 없다").rows;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].label, "/mode");
    }

    /// **목록이 떠 있어도 글자는 그대로 입력이다.** k·j가 이동키로 먹히면
    /// `/skills`를 칠 수 없다.
    #[test]
    fn letters_still_type_while_the_command_list_is_open() {
        let mut s = State::new();
        typed(&mut s, "/s");
        assert_eq!(
            on_key(&s, key(KeyCode::Char('k'), KeyModifiers::NONE)),
            vec![Action::Insert('k')]
        );
        assert_eq!(
            on_key(&s, key(KeyCode::Char('j'), KeyModifiers::NONE)),
            vec![Action::Insert('j')]
        );
    }

    /// **경로를 치면 목록이 사라져야 한다.** `/home/...`은 명령이 아니다.
    #[test]
    fn a_path_closes_the_command_list() {
        let mut s = State::new();
        typed(&mut s, "/home");
        assert!(s.picker.is_none(), "경로에 목록이 남아 있다");
    }

    /// 인자를 치기 시작하면 목록은 제 할 일을 다했다.
    #[test]
    fn the_command_list_closes_once_an_argument_starts() {
        let mut s = State::new();
        typed(&mut s, "/mode ");
        assert!(s.picker.is_none(), "인자를 치는데 목록이 화면을 가린다");
    }

    /// **다 친 명령은 Enter 한 번에 돌아야 한다.** 목록이 떠 있다고 Enter가 "고르기"로만
    /// 먹히면, 끝까지 치고 눌렀는데 같은 글이 다시 써질 뿐 아무 일도 안 일어난다.
    #[test]
    fn a_fully_typed_command_runs_on_the_first_enter() {
        let mut s = State::new();
        typed(&mut s, "/rules");
        assert!(s.picker.is_some(), "목록이 떠 있는 상황이어야 한다");
        assert_eq!(
            on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)),
            vec![Action::Submit("/rules".into())]
        );
    }

    /// 아직 다 안 친 것은 목록에서 고르는 것이 맞다.
    #[test]
    fn a_half_typed_command_still_picks_from_the_list() {
        let mut s = State::new();
        typed(&mut s, "/ru");
        assert_eq!(on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)), vec![Action::PickConfirm]);
    }

    /// 보내면 목록이 닫힌다. 안 닫으면 답이 오는 동안 화면을 가린다.
    #[test]
    fn submitting_closes_the_command_list() {
        let mut s = State::new();
        typed(&mut s, "/cwd");
        apply(&mut s, &Action::Submit("/cwd".into()));
        assert!(s.picker.is_none(), "목록이 남아 있다");
    }

    /// 다 지우면 목록도 닫힌다.
    #[test]
    fn erasing_the_slash_closes_the_list() {
        let mut s = State::new();
        typed(&mut s, "/m");
        apply(&mut s, &Action::Backspace);
        apply(&mut s, &Action::Backspace);
        assert!(s.picker.is_none(), "다 지웠는데 목록이 남았다");
    }

    fn work_start(seq: i64) -> Action {
        Action::Frame(Frame::Event {
            cursor: seq,
            entry: Some(Entry { seq, kind: EntryKind::WorkStart(String::new()) }),
        })
    }

    /// 화면이 깨졌을 때 사람이 반사적으로 누르는 키다. 다른 뜻을 주면 안 된다.
    #[test]
    fn ctrl_l_asks_for_a_full_repaint() {
        let s = state();
        assert_eq!(
            on_key(&s, key(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            vec![Action::Repaint]
        );
    }

    /// 다시 그리기는 화면만의 일이다. 상태를 건드리면 Ctrl+L이 작업을 바꿔 버린다.
    #[test]
    fn repainting_changes_no_state() {
        let mut s = state();
        s.input.insert('가');
        s.view_top = 7;
        apply(&mut s, &Action::Repaint);
        assert_eq!(s.input.text, "가");
        assert_eq!(s.view_top, 7);
    }

    /// 제목은 서버가 지어 준다 — 우리가 쓰지 않은 글자다. 제어문자가 섞이면 OSC가 거기서
    /// 끊겨 나머지가 화면에 글자로 쏟아진다.
    #[test]
    fn a_title_cannot_break_out_of_the_escape_sequence() {
        let out = title_for_osc("깨\u{7}뜨리\u{1b}기\n다음 줄");
        assert_eq!(out, "깨뜨리기다음 줄");
        assert!(!out.chars().any(char::is_control));
    }

    /// 아주 긴 제목은 자른다. 터미널마다 받아 주는 길이가 다르다.
    #[test]
    fn a_very_long_title_is_cut_short() {
        assert_eq!(title_for_osc(&"가".repeat(500)).chars().count(), 120);
    }

    #[test]
    fn enter_submits_the_typed_text() {
        let mut s = state();
        for c in "안녕".chars() {
            apply(&mut s, &Action::Insert(c));
        }
        let actions = on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(actions, vec![Action::Submit("안녕".into())]);
    }

    #[test]
    fn enter_on_an_empty_input_does_nothing() {
        let s = state();
        assert!(on_key(&s, key(KeyCode::Enter, KeyModifiers::NONE)).is_empty());
    }

    /// **추론은 처음부터 숨어 있다.** 답을 보러 온 화면을 생각 더미가 밀어내면 안 된다.
    #[test]
    fn a_work_card_starts_folded_and_stays_that_way_while_reasoning_streams() {
        let mut s = state();
        apply(&mut s, &work_start(1));
        assert!(!s.folds[&1].open, "처음부터 접혀 있어야 한다");
        apply(
            &mut s,
            &Action::Frame(Frame::Delta { kind: ZDeltaKind::Reasoning, text: "생각 중".into() }),
        );
        assert!(!s.folds[&1].open, "추론이 흘러도 저절로 펴지면 안 된다");
    }

    /// **저절로 접히지도 않는다.** 펴 놓고 읽는 중에 화면이 스스로 움직이면 안 된다.
    #[test]
    fn an_opened_card_is_never_folded_behind_the_users_back() {
        let mut s = state();
        apply(&mut s, &work_start(1));
        apply(&mut s, &Action::ToggleFold);
        assert!(s.folds[&1].open, "Ctrl+O 한 번이면 펼쳐진다");

        for kind in [ZDeltaKind::Reasoning, ZDeltaKind::Assistant] {
            apply(&mut s, &Action::Frame(Frame::Delta { kind, text: "무언가".into() }));
        }
        assert!(s.folds[&1].open, "델타가 카드를 접었다");
    }

    #[test]
    fn ctrl_o_toggles_the_latest_card() {
        let mut s = state();
        apply(&mut s, &work_start(1));
        let actions = on_key(&s, key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(actions, vec![Action::ToggleFold]);
    }

    /// 재연결에 쓸 위치를 놓치면 안 된다.
    #[test]
    fn the_last_cursor_is_remembered_for_resume() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Event { cursor: 42, entry: None }));
        assert_eq!(s.last_cursor, Some(42));
    }

    /// **낡은 세션의 프레임은 화면에 닿으면 안 된다.** 일을 걸어 두고 다른 세션으로
    /// 갈아타면 앞 세션의 턴은 서버에서 계속 돌고 그 스트림도 계속 보낸다 — 태그가
    /// 지금 세션과 다르면 버린다. 화면 밖(도구·다리)에서 온 것은 언제나 통과한다.
    #[test]
    fn a_frame_from_a_stale_session_is_dropped() {
        // 화면 밖에서 온 것(None)은 세션과 무관하게 통과한다.
        assert!(frame_is_current(&None, Some("현재")));
        assert!(frame_is_current(&None, None));
        // 자기 세션의 프레임은 통과한다.
        assert!(frame_is_current(&Some("a".into()), Some("a")));
        // **낡은 세션의 프레임은 버린다** — 화면을 갈아탔는데 앞 세션의 메시지가
        // 계속 올라오면 안 된다.
        assert!(!frame_is_current(&Some("옛 세션".into()), Some("새 세션")));
        // 세션이 아직 없는데 스트림 프레임이 오는 것도 낡은 것이다.
        assert!(!frame_is_current(&Some("어떤 세션".into()), None));
    }

    /// 휠은 마지막으로 그린 뷰포트 크기를 기준으로 움직인다 — apply는 순수해야 하므로
    /// 크기를 스스로 알 수 없고 위젯이 적어 둔 값을 읽는다.
    #[test]
    fn the_wheel_uses_the_last_drawn_viewport() {
        let mut s = state();
        s.view_total = 100;
        s.view_height = 10;
        s.scroll.on_content(100, 10);
        apply(&mut s, &Action::Wheel(1));
        assert_eq!(s.scroll.top, 87);
    }

    /// **Ctrl+C는 고른 글이 있어도 복사하지 않는다.** 멈추거나 끝내는 키 하나다 —
    /// 뜻이 겹치면 급할 때 무엇이 일어날지 알 수 없다.
    #[test]
    fn ctrl_c_never_copies_even_with_a_selection() {
        let mut s = state();
        s.selection = Some("고른 것".into());
        s.running = true;
        assert_eq!(
            on_key(&s, key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Action::Cancel]
        );
    }

    #[test]
    fn ctrl_c_cancels_the_turn_when_nothing_is_selected() {
        let mut s = state();
        s.running = true;
        assert_eq!(
            on_key(&s, key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Action::Cancel]
        );
    }

    /// **취소가 안 먹혀도 창은 닫을 수 있어야 한다.**
    ///
    /// 서버가 굳으면 `running`이 참인 채로 남는다. Ctrl+C가 그때마다 취소로만 가면
    /// 몇 번을 눌러도 같은 부탁이 다시 나갈 뿐 종료로는 못 간다.
    #[test]
    fn ctrl_c_after_a_cancel_goes_to_quitting() {
        let mut s = state();
        s.running = true;
        let k = key(KeyCode::Char('c'), KeyModifiers::CONTROL);

        let first = on_key(&s, k);
        assert_eq!(first, vec![Action::Cancel], "첫 번째는 취소여야 한다");
        apply(&mut s, &first[0]);

        let second = on_key(&s, k);
        assert_eq!(second, vec![Action::ArmQuit], "돌고 있어도 두 번째는 종료 예고다");
        apply(&mut s, &second[0]);

        assert_eq!(on_key(&s, k), vec![Action::Quit]);
    }

    /// 멈춰 달라던 부탁은 이번 턴까지다. 다음 턴에서 Ctrl+C는 다시 취소로 가야 한다.
    #[test]
    fn asking_to_stop_lasts_only_for_that_turn() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::Status { running: true }));
        apply(&mut s, &Action::Cancel);
        assert!(s.stopping);

        // 도는 동안 같은 상태가 여러 번 온다. 그때마다 풀리면 안 된다.
        apply(&mut s, &Action::Frame(Frame::Status { running: true }));
        assert!(s.stopping, "도는 중에 같은 상태가 또 왔다고 풀리면 안 된다");

        apply(&mut s, &Action::Frame(Frame::Status { running: false }));
        assert!(!s.stopping, "턴이 끝났으면 풀려야 한다");
    }

    /// 한 번은 예고, 두 번째가 종료. 실수로 한 번 눌러 꺼지면 안 된다.
    #[test]
    fn ctrl_c_needs_two_presses_to_quit() {
        let mut s = state();
        let k = key(KeyCode::Char('c'), KeyModifiers::CONTROL);

        let first = on_key(&s, k);
        assert_eq!(first, vec![Action::ArmQuit], "첫 번째는 예고여야 한다");
        apply(&mut s, &first[0]);

        assert_eq!(on_key(&s, k), vec![Action::Quit], "두 번째는 종료여야 한다");
    }

    /// 예고는 시간이 지나면 저절로 풀린다 — 한참 뒤의 Ctrl+C가 종료가 되면 놀란다.
    #[test]
    fn the_quit_warning_expires() {
        let mut s = state();
        apply(&mut s, &Action::ArmQuit);
        assert!(s.quit_pending_at(Instant::now()));

        let later = Instant::now() + QUIT_WINDOW + Duration::from_millis(1);
        assert!(!s.quit_pending_at(later), "1.5초가 지나면 풀려야 한다");
    }

    /// **알림도 시간이 지나면 사라진다.**
    ///
    /// 지나간 사정을 계속 붙들고 있으면 그 자리가 지금 무슨 일인지 말해 주지 못한다 —
    /// "Zyris로는 아직 만들 수 없습니다"가 영영 남아 있었다.
    #[test]
    fn a_notice_fades_on_its_own() {
        let mut s = state();
        s.set_status("Zyris로는 아직 만들 수 없습니다");
        assert_eq!(s.status_at(Instant::now()), Some("Zyris로는 아직 만들 수 없습니다"));

        let later = Instant::now() + STATUS_WINDOW + Duration::from_millis(1);
        assert_eq!(s.status_at(later), None, "시간이 지나면 사라져야 한다");
    }

    /// Ctrl+U는 친 것을 통째로 지운다. 어디에 커서가 있든 남는 것이 없어야 한다.
    #[test]
    fn ctrl_u_wipes_the_whole_input() {
        let mut s = state();
        for c in "지울 말".chars() {
            apply(&mut s, &Action::Insert(c));
        }
        s.input.home();
        let actions = on_key(&s, key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(actions, vec![Action::ClearInput]);
        apply(&mut s, &Action::ClearInput);
        assert_eq!(s.input.text, "");
        assert_eq!(s.input.cursor, 0);
    }

    /// 터미널이 알려 주기만 하면 Ctrl+Backspace도 같은 일을 한다.
    #[test]
    fn ctrl_backspace_wipes_it_too() {
        let s = state();
        assert_eq!(
            on_key(&s, key(KeyCode::Backspace, KeyModifiers::CONTROL)),
            vec![Action::ClearInput]
        );
    }

    /// 그냥 Backspace는 한 글자만 지운다 — 위 갈래가 이걸 삼키면 안 된다.
    #[test]
    fn a_plain_backspace_still_deletes_one_character() {
        let s = state();
        assert_eq!(
            on_key(&s, key(KeyCode::Backspace, KeyModifiers::NONE)),
            vec![Action::Backspace]
        );
    }

    fn sent(texts: &[&str]) -> State {
        let mut s = state();
        for t in texts {
            apply(&mut s, &Action::Submit(t.to_string()));
        }
        s
    }

    /// 입력란이 비었을 때 ↑는 마지막에 보낸 말을 되살린다.
    #[test]
    fn up_brings_back_the_last_thing_sent() {
        let mut s = sent(&["첫 말", "둘째 말"]);
        assert_eq!(on_key(&s, key(KeyCode::Up, KeyModifiers::NONE)), vec![Action::RecallOlder]);
        apply(&mut s, &Action::RecallOlder);
        assert_eq!(s.input.text, "둘째 말");
        assert_eq!(s.input.cursor, s.input.len_chars(), "커서가 끝에 서야 이어 칠 수 있다");
    }

    /// **두 번째 ↑에서 멈추면 안 된다.** 되살린 뒤에는 입력란이 비어 있지 않으므로
    /// "비었을 때만"이라는 조건 하나로는 한 칸밖에 못 올라간다.
    #[test]
    fn up_keeps_walking_further_back() {
        let mut s = sent(&["첫 말", "둘째 말"]);
        apply(&mut s, &Action::RecallOlder);
        assert_eq!(on_key(&s, key(KeyCode::Up, KeyModifiers::NONE)), vec![Action::RecallOlder]);
        apply(&mut s, &Action::RecallOlder);
        assert_eq!(s.input.text, "첫 말");
        // 더 위는 없다. 맨 위에서 버틴다.
        apply(&mut s, &Action::RecallOlder);
        assert_eq!(s.input.text, "첫 말");
    }

    /// ↓로 내려와 맨 아래를 지나면 입력란이 비워진다 — 치던 자리로 돌아온 셈이다.
    #[test]
    fn down_walks_back_out_of_the_history() {
        let mut s = sent(&["첫 말", "둘째 말"]);
        apply(&mut s, &Action::RecallOlder);
        apply(&mut s, &Action::RecallNewer);
        assert_eq!(s.input.text, "");
        assert!(!s.recalling());
    }

    /// 되살린 말을 고치면 되살리기에서 빠져나온다. 안 그러면 고친 것을 ↓ 한 번에 잃는다.
    #[test]
    fn editing_a_recalled_message_leaves_the_history() {
        let mut s = sent(&["보낸 말"]);
        apply(&mut s, &Action::RecallOlder);
        apply(&mut s, &Action::Insert('!'));
        assert!(!s.recalling());
        assert_eq!(on_key(&s, key(KeyCode::Down, KeyModifiers::NONE)), vec![]);
        assert_eq!(s.input.text, "보낸 말!");
    }

    /// 보낸 적이 없으면 ↑는 아무 일도 하지 않는다.
    #[test]
    fn up_does_nothing_with_an_empty_history() {
        let mut s = state();
        apply(&mut s, &Action::RecallOlder);
        assert_eq!(s.input.text, "");
    }

    /// 같은 말을 연달아 보내면 한 번만 남는다 — 되살릴 때 같은 줄을 두 번 지나면
    /// 되살리기가 고장 난 것처럼 보인다.
    #[test]
    fn the_same_message_twice_is_remembered_once() {
        let s = sent(&["같은 말", "같은 말"]);
        assert_eq!(s.sent.len(), 1);
    }

    /// **일하는 중에 친 말은 보내지 않고 들고 있는다.** 보낸 기록에도 아직 안 들어간다 —
    /// 진짜 나간 것만 "보낸 말"이어야 한다.
    #[test]
    fn typing_during_a_turn_queues_instead_of_sending() {
        let mut s = state();
        s.running = true;
        apply(&mut s, &Action::Submit("일하는 중에 친 말".into()));
        assert_eq!(s.queued, vec!["일하는 중에 친 말"]);
        assert!(s.sent.is_empty(), "아직 안 보냈는데 보낸 기록에 들어갔다");
        assert_eq!(s.input.text, "", "입력란은 비워야 한다");
    }

    /// 턴이 끝나는 순간 대기열을 비우라고 알린다. 보내는 것은 I/O 자리가 한다.
    #[test]
    fn the_end_of_a_turn_asks_for_the_queue_to_be_flushed() {
        let mut s = state();
        s.running = true;
        apply(&mut s, &Action::Submit("나중에 보낼 말".into()));
        apply(&mut s, &Action::Frame(Frame::Status { running: false }));
        assert!(s.flush_queue, "턴이 끝났는데 보내라는 신호가 없다");
    }

    /// 대기열이 비어 있으면 턴이 끝나도 보낼 것이 없다.
    #[test]
    fn an_empty_queue_asks_for_nothing() {
        let mut s = state();
        s.running = true;
        apply(&mut s, &Action::Frame(Frame::Status { running: false }));
        assert!(!s.flush_queue);
    }

    /// **↑는 대기 중인 말을 먼저 꺼낸다.** 그것이 아직 고칠 수 있는 유일한 말이다.
    #[test]
    fn up_pulls_the_queued_message_back_first() {
        let mut s = state();
        apply(&mut s, &Action::Submit("이미 보낸 말".into()));
        s.running = true;
        apply(&mut s, &Action::Submit("대기 중인 말".into()));

        apply(&mut s, &Action::RecallOlder);
        assert_eq!(s.input.text, "대기 중인 말");
        assert!(s.queued.is_empty(), "꺼냈으면 대기열에서 빠져야 한다");
        assert_eq!(s.input.cursor, s.input.len_chars());
    }

    /// 꺼내 놓고 또 ↑를 눌러 지금 고치는 것을 잃으면 안 된다.
    #[test]
    fn pulling_a_queued_message_back_stops_the_walk() {
        let mut s = state();
        apply(&mut s, &Action::Submit("이미 보낸 말".into()));
        s.running = true;
        apply(&mut s, &Action::Submit("대기 중인 말".into()));
        apply(&mut s, &Action::RecallOlder);
        assert_eq!(on_key(&s, key(KeyCode::Up, KeyModifiers::NONE)), vec![]);
    }

    /// **어떤 델타도 접힘을 건드리지 않는다.**
    ///
    /// 사용자가 겪은 "혼자 펴진다"가 여기서 났었다 — 답하다 잠깐 더 생각하면 Reasoning
    /// 델타가 뒤늦게 오고, 그때마다 카드가 도로 펴져 읽던 답이 밀려 내려갔다.
    /// 자동 접기를 걷어냈으므로 이제 그런 예외가 있을 자리가 없다.
    #[test]
    fn no_delta_ever_changes_a_fold() {
        let mut s = state();
        apply(&mut s, &work_start(1));
        for kind in [ZDeltaKind::Reasoning, ZDeltaKind::Assistant, ZDeltaKind::Reasoning] {
            apply(&mut s, &Action::Frame(Frame::Delta { kind, text: "무언가".into() }));
            assert!(!s.folds[&1].open, "델타가 카드를 폈다");
        }
    }

    /// 새 런이 시작돼도 접힌 채로 열린다. **처음부터 숨어 있다**가 여기에도 걸린다.
    #[test]
    fn a_new_work_run_also_starts_folded() {
        let mut s = state();
        apply(&mut s, &work_start(1));
        apply(&mut s, &Action::ToggleFold);
        assert!(s.folds[&1].open);

        apply(&mut s, &work_start(2));
        assert!(!s.folds[&2].open, "새 카드가 펴진 채로 열렸다");
        assert!(s.folds[&1].open, "앞 카드를 건드리면 안 된다");
    }

    // ── 도구 승인 ──────────────────────────────────────────────────────

    /// 열린 셸이 화면에 안 뜨면 유령 셸이 돈다.
    #[test]
    fn opening_and_closing_a_shell_moves_the_sidebar_list() {
        let mut s = state();
        apply(&mut s, &Action::Frame(Frame::ShellOpened { id: "p1".into(), name: "zsh".into() }));
        assert_eq!(s.shells.len(), 1);
        // 같은 것이 두 번 와도 두 벌이 되면 안 된다.
        apply(&mut s, &Action::Frame(Frame::ShellOpened { id: "p1".into(), name: "zsh".into() }));
        assert_eq!(s.shells.len(), 1);
        apply(&mut s, &Action::Frame(Frame::ShellClosed { id: "p1".into() }));
        assert!(s.shells.is_empty());
    }

    /// 실행 중일 때만 취소가 의미 있다.
    #[test]
    fn esc_cancels_only_while_a_turn_runs() {
        let mut s = state();
        assert!(on_key(&s, key(KeyCode::Esc, KeyModifiers::NONE)).is_empty());
        apply(&mut s, &Action::Frame(Frame::Status { running: true }));
        assert_eq!(on_key(&s, key(KeyCode::Esc, KeyModifiers::NONE)), vec![Action::Cancel]);
    }

    /// **창을 닫으면 서버에서도 멈춘다.** 안 그러면 이쪽이 사라진 뒤에도 저쪽은 계속
    /// 생각하고, 도구를 부를 때마다 없는 노드를 찾다 실패하며 크레딧만 나간다.
    #[test]
    fn quitting_mid_turn_stops_the_turn_on_the_server() {
        let mut s = state();
        let mut session = Session::new(None);
        session.switch_to("세션-1".into(), None);

        apply(&mut s, &Action::Frame(Frame::Status { running: true }));
        assert_eq!(turn_to_stop(&s, &session), Some("세션-1".into()));
    }

    /// 도는 것이 없으면 멈추라고 하지 않는다 — 끄는 길에 쓸데없는 왕복을 넣지 않는다.
    #[test]
    fn quitting_while_idle_says_nothing_to_the_server() {
        let mut s = state();
        let mut session = Session::new(None);
        session.switch_to("세션-1".into(), None);
        assert_eq!(turn_to_stop(&s, &session), None);

        // 아직 첫 메시지를 안 보낸 세션은 서버에 없다. 멈추라고 할 대상도 없다.
        apply(&mut s, &Action::Frame(Frame::Status { running: true }));
        assert_eq!(turn_to_stop(&s, &Session::new(None)), None);
    }
}
