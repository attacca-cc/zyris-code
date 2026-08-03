//! 입력창 아래 한 줄 — 모드와 에이전트.
//!
//! **연결 상태는 여기 없다.** 잘 붙어 있다는 것을 화면 구석에 늘 띄워 둘 이유가 없어서
//! 뺐다. 지금 무슨 일이 벌어지는지는 입력란 위 `activity` 줄이 말한다.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;

use crate::theme;

pub fn draw(frame: &mut Frame, area: Rect, state: &State) {
    // 맨 왼쪽에 붙인다 — 바로 위 상태 줄의 점과 열을 맞춘다.
    //
    // 가운뎃점에 공백을 두지 않는다. 모드와 에이전트는 **한 벌로 읽히는 것**이라
    // 사이가 벌어지면 서로 다른 두 정보처럼 보인다 — 붙여 두면 눈이 한 번에 집는다.
    let mut spans = vec![
        Span::styled(state.mode.label(state.lang), Style::default().fg(state.mode.color())),
        Span::styled("·", Style::default().fg(theme::BORDER_LIGHT)),
        Span::styled(
            if state.agent.is_empty() { "-" } else { state.agent.as_str() }.to_string(),
            Style::default().fg(theme::TEXT_MUTED),
        ),
    ];

    // **아직 안 보낸 말이 있으면 반드시 말한다.** 들고 있는 것을 안 알리면 보냈다고 믿는다.
    if !state.queued.is_empty() {
        spans.push(Span::styled(" · ", Style::default().fg(theme::BORDER_LIGHT)));
        spans.push(Span::styled(
            state.lang.queued(state.queued.len()),
            Style::default().fg(theme::WARNING),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
