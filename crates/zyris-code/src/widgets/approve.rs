//! 승인 창 — **작업 디렉터리 밖으로 나갈 때만 뜬다.**
//!
//! 입력란 자리를 차지한다. 질문 화면과 같은 자리이고 둘이 겹치면 사람이 어디에 답하는지
//! 알 수 없으므로 승인이 먼저다 — 도구 하나가 답을 기다리며 멈춰 있고 저쪽에는 마감이 있다.
//!
//! **묻는 것은 "무엇을 하느냐"가 아니라 "어디를 만지느냐"다.** 안쪽 일은 아무것도 묻지
//! 않으므로, 이 창이 떴다는 것 자체가 곧 "여기는 밖이다"라는 뜻이다. 그래서 화면의 가운데를
//! 차지하는 것은 도구 이름이 아니라 **그 경로**다.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{State, ToolAsk};
use crate::theme;

/// 머리말·경로·빈 줄·키 안내. 이 넷은 언제나 있다.
const CHROME: u16 = 4;

pub fn height(state: &State, cap: u16) -> u16 {
    let Some(ask) = &state.pending else { return 0 };
    // 마감이 지났으면 그 사정을 두 줄로 말한다 — 한 줄에 우겨넣으면 좁은 화면에서 잘린다.
    let want = CHROME + 2 * ask.expired as u16 + !state.ask_queue.is_empty() as u16;
    want.min(cap.max(3))
}

pub fn draw(frame: &mut Frame, area: Rect, state: &State) {
    let Some(ask) = &state.pending else { return };
    let mut lines = head(ask, state.lang);

    lines.push(Line::from(vec![
        Span::styled("  ", Style::default().fg(theme::TEXT)),
        Span::styled(
            ask.summary.clone(),
            Style::default().fg(theme::WARNING).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default().fg(theme::TEXT)),
        Span::styled(
            format!("{} · ", short(&ask.call.capability, &ask.call.tool)),
            Style::default().fg(theme::TOOL),
        ),
        Span::styled(
            state.lang.approve_root(&state.cwd.display().to_string()),
            Style::default().fg(theme::TEXT_MUTED),
        ),
    ]));

    if !state.ask_queue.is_empty() {
        lines.push(Line::from(Span::styled(
            state.lang.approve_more_waiting(state.ask_queue.len()),
            Style::default().fg(theme::TEXT_MUTED),
        )));
    }
    lines.push(keys(state.lang));
    frame.render_widget(Paragraph::new(lines), area);
}

/// 머리말. 마감이 지났으면 **창을 치우지 않고 사정만 바꿔 말한다** — 치워 버리면
/// 사람이 무엇을 놓쳤는지 모른다.
fn head(ask: &ToolAsk, lang: crate::lang::Lang) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(Span::styled(
        lang.approve_head(),
        Style::default().fg(theme::WARNING).add_modifier(Modifier::BOLD),
    ))];
    if ask.expired {
        out.push(Line::from(Span::styled(
            lang.approve_gave_up(),
            Style::default().fg(theme::TEXT_MUTED),
        )));
        out.push(Line::from(Span::styled(
            lang.approve_next_time(),
            Style::default().fg(theme::TEXT_MUTED),
        )));
    }
    out
}

fn keys(lang: crate::lang::Lang) -> Line<'static> {
    Line::from(Span::styled(lang.approve_keys(), Style::default().fg(theme::BORDER_LIGHT)))
}

/// 화면에 보이는 도구 이름. 대화의 도구 줄과 같은 모양이다.
fn short(capability: &str, tool: &str) -> String {
    format!("{capability}.{tool}")
}
