//! 모드 — 에이전트가 이 컴퓨터에 손댈 수 있는가, 그리고 내 말이 어디로 가는가.
//!
//! **서버로 보내지 않는다. 보낼 이유가 없다.** 도구를 실제로 돌리는 것이 이 노드이므로
//! 돌릴지 말지도 여기서 정한다 — attacca는 이 결정을 모르고 알 필요도 없다.
//! 뜻을 주는 곳은 `tools::gate::decide`이고, 게이트까지 나르는 것은 `tools::bridge`다.
//!
//! **묻는 모드는 없앴다**(2026-08-02 사용자 결정). 도구를 쓸 때마다 승인을 받는 것이
//! 실제로는 흐름을 끊기만 했다.
//!
//! 모드가 정하는 것은 **둘**이다. 오래도록 하나뿐이었다가 2026-08-03에 하나가 늘었다.
//!
//! ```text
//!         gate::decide        Route
//!         (도구를 돌릴까)     (내 말이 어디로)
//! job     통과                create_job  → job 세션      ← 기본값
//! plan    쓰기 거부           지금 대화 그대로
//! work    통과                create_work → planner 세션
//! ```
//!
//! **`Plan`만 지금 대화에 머문다.** 하던 얘기에 잠깐 거는 것이 계획 모드가 쓸모 있는
//! 유일한 방식이라, 여기서 새 것을 열면 그 쓸모가 사라진다. 나머지 둘은 **서버에 새 것을
//! 만든다** — 그것도 첫 메시지 한 번뿐이고, 그 뒤는 열린 세션에 이어 붙는다.

use ratatui::style::Color;

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// 묻지 않고 실행한다. **기본값이고, 여는 것은 job이다.**
    ///
    /// 한때 이 자리에 맨 세션을 여는 `Normal`이 따로 있었다. 갈라 둘 이유가 없었다 —
    /// job도 세션 하나로 도는 대화이고(`ZJob::session_id`), 그러면서 attacca 쪽 목록과
    /// 상태를 갖는다. 맨 세션은 그 모든 것이 없는 job일 뿐이다.
    #[default]
    Job,
    /// 실행하지 않고 무엇을 할지 먼저 내놓는다.
    Plan,
    /// 다음 메시지를 목표로 삼아 attacca에 work를 연다. 관문 둘과 태스크 그래프가 붙는다.
    Work,
}

/// 다음에 여는 것이 무엇인가. **모드가 뜻을 얻는 두 번째 자리다.**
///
/// 셋 다 끝에는 **평범한 세션 id**가 나온다(`ZJob::session_id`,
/// `ZWork::planner_session_id`). 그래서 화면·타임라인·접힘은 무엇을 열었든 모르고 지나간다 —
/// 그리는 쪽을 하나도 안 건드린 이유가 이것이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Route {
    /// 지금 세션에 이어 붙인다. 세션이 없으면 그때 하나 만든다.
    #[default]
    Session,
    /// `create_work`. 첫 메시지가 목표가 된다.
    Work,
    /// `create_job`. 첫 메시지가 시킬 일이 된다.
    Job,
}

impl Mode {
    /// 화면에 보이는 이름. 문구는 `lang.rs`가 들고 있다.
    pub fn label(self, lang: crate::lang::Lang) -> &'static str {
        match self {
            Mode::Job => lang.mode_job(),
            Mode::Plan => lang.mode_plan(),
            Mode::Work => lang.mode_work(),
        }
    }

    /// 하단 바의 모드 색.
    ///
    /// **셋 다 색을 가진다.** 기본 모드를 흐린 회색으로 두면 하단 바가 통째로 배경처럼
    /// 읽혀, 정작 "지금 무슨 모드인가"가 눈에 안 들어온다 — 그 줄에서 색이 있는 것은
    /// 모드뿐이므로 그것이 눈이 잡는 자리다.
    ///
    /// **순환에서 이웃한 둘은 서로 먼 색이어야 한다.** 한 번에 하나만 보이므로 눈이
    /// 견주는 것은 방금 전 색이다: 초록 → 주황 → 파랑 → 초록.
    pub fn color(self) -> Color {
        match self {
            // 도구가 그냥 도는 기본 상태. 초록이 "가도 된다"로 읽힌다.
            Mode::Job => theme::SUCCESS,
            Mode::Plan => theme::ACCENT,
            Mode::Work => theme::TOOL,
        }
    }

    /// 내 말이 어디로 가는가.
    pub fn route(self) -> Route {
        match self {
            // **계획 모드만 지금 대화에 머문다.** 하던 얘기에 잠깐 거는 것이 이 모드가
            // 쓸모 있는 유일한 방식이라, 여기서 새 것을 열면 그 쓸모가 사라진다.
            Mode::Plan => Route::Session,
            Mode::Work => Route::Work,
            Mode::Job => Route::Job,
        }
    }

    /// Shift+Tab이 도는 차례.
    ///
    /// **기본값에서 한 칸이 계획이다.** 제일 자주 오가는 둘을 붙여 둔다 — 하던 얘기에
    /// 계획을 걸었다 푸는 것이 이 키를 쓰는 가장 흔한 이유다.
    pub fn next(self) -> Mode {
        match self {
            Mode::Job => Mode::Plan,
            Mode::Plan => Mode::Work,
            Mode::Work => Mode::Job,
        }
    }

    /// 셋 전부. 테스트와 `/mode` 목록이 쓴다 — **빠뜨린 모드가 생기지 않게 한 자리에서 센다.**
    pub const ALL: [Mode; 3] = [Mode::Job, Mode::Plan, Mode::Work];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **기본값은 job이다.** 맨 세션을 여는 `Normal`은 2026-08-03에 job으로 합쳤다 —
    /// job도 세션 하나로 도는 대화인데 attacca 쪽 목록과 상태를 갖는다.
    #[test]
    fn the_default_mode_opens_a_job() {
        assert_eq!(Mode::default(), Mode::Job);
        assert_eq!(Mode::default().route(), Route::Job);
    }

    /// 한 바퀴 돌면 제자리다. **모드를 더하고 `next`를 안 고치면 여기서 걸린다.**
    #[test]
    fn cycling_through_every_mode_comes_back() {
        let mut seen = vec![Mode::default()];
        let mut m = Mode::default();
        for _ in 0..Mode::ALL.len() - 1 {
            m = m.next();
            assert!(!seen.contains(&m), "{m:?}를 두 번 지난다 — 한 바퀴가 모드 수보다 짧다");
            seen.push(m);
        }
        assert_eq!(m.next(), Mode::default(), "한 바퀴를 돌면 기본으로 온다");
        assert_eq!(seen.len(), Mode::ALL.len(), "순환이 모든 모드를 지나야 한다");
    }

    #[test]
    fn every_mode_has_a_label_and_a_colour() {
        for m in Mode::ALL {
            for lang in [crate::lang::Lang::Ko, crate::lang::Lang::En] {
                assert!(!m.label(lang).is_empty(), "{m:?}의 이름이 비어 있다");
            }
            let _ = m.color();
        }
    }

    /// 색이 겹치면 하단 바만 보고는 무슨 모드인지 알 수 없다.
    #[test]
    fn no_two_modes_share_a_colour() {
        for (i, a) in Mode::ALL.iter().enumerate() {
            for b in &Mode::ALL[i + 1..] {
                assert_ne!(a.color(), b.color(), "{a:?}와 {b:?}가 같은 색이다");
            }
        }
    }

    /// **계획 모드만 하던 대화에 머문다.** 이것이 깨지면 대화 도중에 계획 모드를 켜는
    /// 순간 얘기가 끊겨, 이 모드를 쓸 이유가 사라진다.
    #[test]
    fn only_planning_stays_in_the_conversation() {
        assert_eq!(Mode::Plan.route(), Route::Session);
        for m in Mode::ALL {
            if m != Mode::Plan {
                assert_ne!(m.route(), Route::Session, "{m:?}가 새로 안 연다");
            }
        }
    }

    #[test]
    fn work_and_job_each_open_their_own_thing() {
        assert_eq!(Mode::Work.route(), Route::Work);
        assert_eq!(Mode::Job.route(), Route::Job);
    }
}
