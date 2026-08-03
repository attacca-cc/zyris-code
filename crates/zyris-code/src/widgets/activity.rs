//! 입력란 바로 위 한 줄 — **지금 무슨 일이 벌어지고 있는가.**
//!
//! 연결 상태를 화면 구석에 늘 띄워 두는 것보다 이쪽이 낫다. 사람이 알고 싶은 것은
//! "연결됐나"가 아니라 "지금 내 차례인가, 기다려야 하나"이기 때문이다. 연결은
//! 끊겼을 때만 말한다 — 잘 되고 있는 것을 계속 알릴 이유가 없다.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;
use crate::markdown::display_width;
use crate::theme;

/// 점이 켜져 있는가. 20fps 기준 여덟 프레임(0.4초)마다 뒤집힌다.
///
/// 깜박임을 시계가 아니라 프레임 수로 정하는 이유는 **테스트가 시간을 안 기다려도
/// 되기 때문**이다. 그리는 쪽이 순수해진다.
pub fn blink_on(tick: u64) -> bool {
    (tick / 8).is_multiple_of(2)
}

/// 이 줄에 나올 (점 색, 글, 도움말). 순수하다 — 테스트가 이걸 본다.
pub fn parts(state: &State) -> (ratatui::style::Color, String, &'static str) {
    parts_at(state, std::time::Instant::now())
}

/// 시각을 받는 판. 테스트가 경과 시간을 정해 놓고 본다.
pub fn parts_at(
    state: &State,
    now: std::time::Instant,
) -> (ratatui::style::Color, String, &'static str) {
    // 지금 알아야 할 것이 위다. 종료 예고가 무엇보다 먼저다.
    if state.quit_pending() {
        return (theme::WARNING, "한 번 더 Ctrl+C를 누르면 종료합니다".into(), "");
    }
    // 알림은 시간이 지나면 저절로 사라진다 — `State::status`가 그 판정을 한다.
    if let Some(s) = state.status() {
        return (theme::WARNING, s.to_string(), "");
    }
    if !state.connected {
        return (theme::WARNING, state.lang.connecting().to_string(), "");
    }
    // **"작업 중…"보다 구체적이다.** 명령은 완료될 때 한 번만 결과를 주므로, 무엇이
    // 도는지 여기서 말하지 않으면 사람은 최대 55초를 눈뜬장님으로 기다린다.
    // **멈춰 달라고 말한 뒤가 먼저다.** 서버가 답할 때까지는 "작업 중"이 그대로라,
    // 그걸 계속 보여주면 Ctrl+C가 안 먹은 줄 알고 또 누른다.
    if state.running && state.stopping {
        return (theme::WARNING, state.lang.stopping().to_string(), state.lang.ctrl_c_quits());
    }
    if let Some((_, command, since)) = &state.running_exec {
        let secs = now.saturating_duration_since(*since).as_secs();
        return (theme::ACCENT, format!("▶ {command}  ·  {secs}초"), "Esc 정지");
    }
    if state.running {
        return (theme::ACCENT, "작업 중…".into(), "Esc 정지");
    }
    if state.asking.is_some() {
        return (theme::WARNING, "대기 중 — 답을 고르세요".into(), "↑↓ 이동 · Enter 고르기");
    }
    // 쉬는 중에는 도움말을 붙이지 않는다. 늘 떠 있는 안내는 곧 안 읽힌다.
    (theme::TEXT_MUTED, "쉬는 중".into(), "")
}

pub fn draw(frame: &mut Frame, area: Rect, state: &State) {
    let (colour, label, hint) = parts(state);

    // 작업 중일 때만 점이 깜박인다. 멈춰 있는 점은 "돌아가고 있다"를 말해 주지 못한다.
    let lit = !state.running || blink_on(state.tick);
    let dot = Style::default().fg(if lit { colour } else { theme::BORDER_LIGHT });

    // **점은 맨 왼쪽에 붙인다.** 대화의 여백과 맞추지 않는다 — 이 줄은 대화가 아니라
    // 화면 자체의 상태이고, 왼쪽 끝에 있어야 눈이 늘 같은 자리에서 찾는다.
    let mut spans = vec![
        Span::styled("● ", dot),
        Span::styled(label.clone(), Style::default().fg(colour).add_modifier(Modifier::BOLD)),
    ];

    // 도움말은 오른쪽 끝에 붙인다. 좁으면 통째로 뺀다 — 상태가 먼저다.
    // 사이드바 쪽 여백은 이 영역이 이미 좁혀진 채로 오므로 여기서 또 빼지 않는다.
    let used = 2 + display_width(&label);
    let room = area.width as usize;
    if !hint.is_empty() && used + display_width(hint) + 2 <= room {
        let gap = room - used - display_width(hint);
        spans.push(Span::styled(" ".repeat(gap), Style::default().fg(theme::TEXT)));
        spans.push(Span::styled(hint, Style::default().fg(theme::BORDER_LIGHT)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
