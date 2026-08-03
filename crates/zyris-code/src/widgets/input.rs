//! 입력란. 하단 바 바로 위에 고정되고 내용에 따라 자란다.
//!
//! **진짜 터미널 커서를 세운다.** 반전 셀로 가짜 커서를 그리는 방법도 있지만, 그러면
//! `ratatui::init()`이 숨겨 둔 진짜 커서가 화면 어딘가에 남고 **IME가 조합 창을 그 자리에
//! 띄운다** — 한글을 칠 때 조합 중인 글자가 엉뚱한 곳에 뜨는 원인이다. 진짜 커서를 제자리에
//! 두면 조합 창이 따라오고, 깜박임도 터미널이 native로 해 준다(우리가 셀 때보다 정확하다).
//!
//! 조합 중인 글자 자체는 우리 손에 없다. fcitx5가 자기 오버레이에 그리고 완성된 글자만
//! 보내므로, 그 글자의 폰트나 크기는 앱이 정할 수 없다.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;
use crate::theme;

/// 프롬프트 `"> "`가 차지하는 칸.
const PROMPT_WIDTH: u16 = 2;

/// 가름선 한 줄.
///
/// 입력란은 **위아래로 같은 선에 물린다.** 아래를 빈 줄로 두면 하단 바가 입력란의 꼬리처럼
/// 읽히는데, 그건 모드를 눈에서 지운다. 선이 하나 그어져 있으면 "여기까지가 입력란"이
/// 한눈에 보인다.
pub fn rule(frame: &mut Frame, area: Rect) {
    let line = Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(theme::BORDER),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

pub fn draw(frame: &mut Frame, area: Rect, state: &State) {
    let inner = area.width.saturating_sub(PROMPT_WIDTH);
    let (wrapped, (crow, ccol)) = state.input.wrapped(inner);

    // 자리가 모자라면 **커서가 있는 줄이 보이게** 뒤쪽을 남긴다. 앞을 남기면 치는
    // 글자가 화면 밖에서 자란다.
    let rows = area.height.saturating_sub(1).max(1) as usize;
    let start = (crow as usize + 1).saturating_sub(rows).min(wrapped.len().saturating_sub(1));

    let mut lines = vec![Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(theme::BORDER),
    ))];
    for (i, text) in wrapped[start..].iter().take(rows).enumerate() {
        // 프롬프트는 첫 줄에만. 이어지는 줄은 같은 열에서 시작해 글이 가지런하다.
        let head = if start + i == 0 { "> " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(head, Style::default().fg(theme::ACCENT)),
            Span::styled(text.clone(), Style::default().fg(theme::TEXT)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);

    // 커서 자리. 전각을 2칸으로 세어 잡는다 — 글자 수로 세면 한글 앞에서 왼쪽으로 밀린다.
    let col = PROMPT_WIDTH + ccol;
    let usable = area.width.saturating_sub(1);
    let y = area.y + 1 + (crow as usize).saturating_sub(start) as u16;
    frame.set_cursor_position((area.x + col.min(usable), y.min(area.y + area.height - 1)));
}
