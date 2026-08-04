//! 새 프로젝트 양식. 화면 가운데에 겹쳐 띄운다 — 목록 위에 얹히므로 Esc로 닫으면
//! 그대로 목록이 보인다.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::markdown::display_width;
use crate::newproject::{Field, Form};
use crate::theme;

pub fn draw(frame: &mut Frame, area: Rect, form: &Form, lang: crate::lang::Lang) {
    // 이름·설명 두 칸 + 안내 줄 + (오류 줄) + 테두리 두 줄.
    let h = 5u16.saturating_add(form.error.is_some() as u16);
    let h = h.min(area.height.saturating_sub(2)).max(5);
    let w = 60.min(area.width.saturating_sub(4)).max(24);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    // 뒤를 지우지 않으면 목록이 비쳐 보인다.
    frame.render_widget(Clear, box_area);
    // **경계에 걸친 전각 글자를 마저 지운다.** 목록의 글이 상자 왼쪽 바깥에 앞 절반만
    // 남아 있으면 테두리가 깨진다.
    crate::widgets::picker::scrub_left_edge(frame, box_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(Span::styled(
            format!(" {} ", lang.project_form_title()),
            Style::default().fg(theme::TEXT_HEADING).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let width = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(field_line(
        lang.project_name(),
        lang.project_name_placeholder(),
        &form.name,
        form.field == Field::Name,
        width,
    ));
    lines.push(field_line(
        lang.project_description(),
        lang.project_description_placeholder(),
        &form.description,
        form.field == Field::Description,
        width,
    ));
    if let Some(err) = &form.error {
        lines.push(Line::from(Span::styled(err.clone(), Style::default().fg(theme::DANGER))));
    }
    lines.push(Line::from(Span::styled(
        lang.project_form_keys().to_string(),
        Style::default().fg(theme::TEXT_MUTED),
    )));

    frame.render_widget(Paragraph::new(lines), inner);
}

/// 양식의 한 줄: 칸 이름 + 값. **활성 칸에는 커서가 선다.**
///
/// 값이 비면 무엇을 쓰는 자리인지 흐리게 말해 준다 — 빈 칸은 이유를 모르면 고장으로
/// 보인다. 값은 폭에 맞춰 자르고, 칸이 활성이면 끝에 커서를 붙인다.
fn field_line(
    label: &'static str,
    placeholder: &'static str,
    input: &crate::input::Input,
    on: bool,
    width: usize,
) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label} "), Style::default().fg(theme::TEXT_MUTED))];
    spans.push(Span::styled("> ", Style::default().fg(theme::ACCENT)));
    if input.text.is_empty() {
        spans.push(Span::styled(placeholder, Style::default().fg(theme::BORDER_LIGHT)));
    } else {
        // 붙여넣기로 줄바꿈이 들어올 수 있다 — 한 줄 칸이므로 공백으로 보여준다.
        let shown = truncate(&input.text.replace('\n', " "), width.saturating_sub(3));
        spans.push(Span::styled(shown, Style::default().fg(theme::TEXT)));
    }
    if on {
        spans.push(Span::styled("▎", Style::default().fg(theme::ACCENT)));
    }
    Line::from(spans)
}

/// 칸 수에 맞춰 자른다. 자르면 `…`를 붙여 잘렸다는 것을 보인다.
fn truncate(s: &str, limit: usize) -> String {
    if display_width(s) <= limit {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        if display_width(&out) + display_width(&ch.to_string()) > limit.saturating_sub(1) {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}
