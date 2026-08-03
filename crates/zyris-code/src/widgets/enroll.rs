//! 등록 코드 창. 재등록이 시작되면 화면 가운데에 뜬다.
//!
//! 예전에는 코드가 stdout으로 나가 화면에 가려 못 봤다. 이제 `EnrollmentUi` 훅이
//! 코드를 `Frame::Enroll`로 실어 보내므로(`enroll::ScreenEnroll`), 이 창이 그
//! 자리를 차지한다 — 만료·거부 사정도 `EnrollPhase`로 여기 그린다.
//!
//! **Esc로만 닫힌다.** 등록은 배경에서 계속 돌므로 닫아도 승인이 도착하면
//! `EnrollDone`이 창을 닫는다.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{EnrollPhase, EnrollView};
use crate::theme;

pub fn draw(frame: &mut Frame, area: Rect, view: &EnrollView, lang: crate::lang::Lang) {
    // 화면 가운데 상자. 코드가 크게 보여야 하므로 목록 창보다 넉넉히 준다.
    let h = 10.min(area.height.saturating_sub(2)).max(5);
    let w = 56.min(area.width.saturating_sub(4)).max(30);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    // 뒤를 지우지 않으면 대화가 비쳐 보인다.
    frame.render_widget(Clear, box_area);
    // 경계에 걸친 전각 글자를 마저 지운다 — picker와 같은 이유.
    crate::widgets::picker::scrub_left_edge(frame, box_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(Span::styled(
            format!(" {} ", lang.enroll_title()),
            Style::default().fg(theme::TEXT_HEADING).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let mut lines: Vec<Line<'static>> = Vec::new();

    match view.phase {
        EnrollPhase::Waiting => {
            lines.push(Line::from(Span::styled(
                lang.enroll_steps(),
                Style::default().fg(theme::TEXT),
            )));
            lines.push(Line::from(""));
            // 코드는 크고 뚜렷하게. 하이픈을 그대로 두어 더블클릭으로 통째로
            // 선택되게 한다 — 상류 상자와 같은 규칙이다.
            lines.push(Line::from(Span::styled(
                format!("   {}   ", view.code),
                Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                view.uri.clone(),
                Style::default().fg(theme::TOOL),
            )));
            lines.push(Line::from(""));
            let remaining =
                view.expires_at.saturating_duration_since(std::time::Instant::now());
            lines.push(Line::from(Span::styled(
                lang.enroll_expires(remaining.as_secs()),
                Style::default().fg(theme::TEXT_MUTED),
            )));
        }
        EnrollPhase::Lapsed => {
            lines.push(Line::from(Span::styled(
                lang.enroll_lapsed(),
                Style::default().fg(theme::WARNING),
            )));
        }
        EnrollPhase::Denied => {
            lines.push(Line::from(Span::styled(
                lang.enroll_denied(),
                Style::default().fg(theme::DANGER),
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        lang.enroll_keys(),
        Style::default().fg(theme::BORDER_LIGHT),
    )));

    frame.render_widget(Paragraph::new(lines), inner);
}
