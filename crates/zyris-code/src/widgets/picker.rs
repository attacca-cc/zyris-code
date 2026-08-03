//! 프로젝트/세션 목록. 화면 가운데에 겹쳐 띄운다.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::markdown::display_width;
use crate::picker::{Picker, Slot};
use crate::theme;

pub fn draw(frame: &mut Frame, area: Rect, picker: &Picker, lang: crate::lang::Lang) {
    // 화면 가운데 상자. 목록 길이에 맞추되 화면을 넘지 않는다.
    // 가름선이 한 줄을 더 쓰므로 그것까지 세어야 다 들어간다.
    let rule = picker.is_create(0) && picker.rows.len() > 1;
    let want_h = (picker.rows.len() as u16).saturating_add(4 + rule as u16).max(6);
    let h = want_h.min(area.height.saturating_sub(2)).max(3);
    let w = 64.min(area.width.saturating_sub(4)).max(20);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    // 뒤를 지우지 않으면 대화가 비쳐 보인다.
    frame.render_widget(Clear, box_area);
    // **경계에 걸친 전각 글자를 마저 지운다.** 상자 왼쪽 바로 바깥에 전각 글자의 앞
    // 절반이 남아 있으면 그 글자가 상자 안쪽으로 번져 테두리가 깨진다. `Clear`는 상자
    // 안만 지우므로 이 반쪽은 우리가 치워야 한다.
    scrub_left_edge(frame, box_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(Span::styled(
            format!(" {} ", picker.title(lang)),
            Style::default().fg(theme::TEXT_HEADING).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if picker.loading {
        lines
            .push(Line::from(Span::styled("불러오는 중…", Style::default().fg(theme::TEXT_MUTED))));
    }

    // 어느 줄을 어디에 놓을지는 **순수한 쪽이 정한다**(`picker::slots`). 여기는 그린다.
    let width = inner.width as usize;
    let body_h = inner.height.saturating_sub(1) as usize;
    for slot in crate::picker::slots(&picker.rows, picker.cursor, body_h) {
        lines.push(match slot {
            Slot::Row(i) => {
                row_line(&picker.rows[i], i == picker.cursor, picker.is_create(i), width)
            }
            Slot::Rule => {
                Line::from(Span::styled("─".repeat(width), Style::default().fg(theme::BORDER)))
            }
            Slot::More { count, up } => Line::from(Span::styled(
                format!("  {} {count}개 더", if up { "↑" } else { "↓" }),
                Style::default().fg(theme::TEXT_MUTED),
            )),
        });
    }

    // 단계에 따라 ← 의 뜻이 다르다. 그대로 말해 준다.
    let back = match picker.level {
        crate::picker::Level::Projects => "← 닫기",
        crate::picker::Level::Sessions { .. } => "← 뒤로",
        // 이 둘은 프로젝트 계층 바깥이라 뒤로 갈 곳이 없다.
        crate::picker::Level::Agents
        | crate::picker::Level::Commands
        | crate::picker::Level::Languages => "Esc 닫기",
    };
    lines.push(Line::from(Span::styled(
        format!("↑↓ 이동 · Enter 고르기 · {back}"),
        Style::default().fg(theme::TEXT_MUTED),
    )));

    frame.render_widget(Paragraph::new(lines), inner);
}

/// 목록의 한 줄.
///
/// **"새로 만들기"는 다른 색이다.** 세션 목록에 섞여 있으면 세션 하나로 읽히는데,
/// 그것은 고르는 것이 아니라 만드는 것이다.
fn row_line(row: &crate::picker::Row, on: bool, create: bool, width: usize) -> Line<'static> {
    let fg = match (row.enabled, create, on) {
        (false, _, _) => theme::BORDER_LIGHT,
        (true, true, _) => theme::ACCENT,
        (true, false, true) => theme::TEXT_HEADING,
        (true, false, false) => theme::TEXT,
    };
    let (label, note) = split(width, &row.label, row.note.as_deref());
    let mut spans = vec![
        Span::styled(if on { "❯ " } else { "  " }, Style::default().fg(theme::ACCENT)),
        Span::styled(label.clone(), Style::default().fg(fg)),
    ];
    if let Some(note) = note {
        let used = 2 + display_width(&label);
        let pad = width.saturating_sub(used + display_width(&note));
        spans.push(Span::styled(" ".repeat(pad), Style::default().fg(theme::TEXT_MUTED)));
        spans.push(Span::styled(note, Style::default().fg(theme::TEXT_MUTED)));
    }
    Line::from(spans)
}

/// 설명이 아무리 짧아도 이만큼은 보여줄 값어치가 있다. 이보다 좁으면 통째로 뺀다.
const NOTE_MIN: usize = 8;

/// 한 줄을 (이름, 설명)으로 나눈다. **이름이 먼저다.**
///
/// 설명에 자리를 먼저 주면 이름이 깎인다 — 실제로 `/agent`이 `/a…`로 잘려 목록에서
/// 무슨 명령인지 알 수 없었다. **이름은 신원이고 설명은 곁들임이다**: 이름이 잘리면
/// 그 줄을 고를 이유 자체가 사라지지만, 설명은 없어도 이름으로 짐작할 수 있다.
///
/// 그래도 이름은 반드시 자른다 — 세션 제목은 길이가 제멋대로라 그대로 두면 상자를
/// 뚫고 나가 화면이 무너진다.
fn split(width: usize, label: &str, note: Option<&str>) -> (String, Option<String>) {
    let label = truncate(label, width.saturating_sub(2));
    let Some(note) = note else {
        return (label, None);
    };
    // 이름과 설명 사이에 최소 두 칸은 띄운다. 붙어 있으면 한 낱말로 읽힌다.
    let room = width.saturating_sub(2 + display_width(&label) + 2);
    if room < NOTE_MIN {
        return (label, None);
    }
    (label, Some(truncate(note, room)))
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

/// 상자 왼쪽 바깥에 걸친 전각 글자의 앞 절반을 공백으로 바꾼다.
///
/// 등록 코드 창(`enroll.rs`)도 같은 일을 한다 — 겹쳐 띄우는 창은 다 이 길을 쓴다.
pub(crate) fn scrub_left_edge(frame: &mut Frame, box_area: Rect) {
    if box_area.x == 0 {
        return;
    }
    let x = box_area.x - 1;
    let buf = frame.buffer_mut();
    for y in box_area.y..box_area.y.saturating_add(box_area.height) {
        if !buf.area.contains((x, y).into()) {
            continue;
        }
        if display_width(buf[(x, y)].symbol()) > 1 {
            buf[(x, y)].set_symbol(" ");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **이름은 절대 안 깎인다.** 실제로 `/agent`이 `/a…`로 잘려 목록에서 무슨
    /// 명령인지 알 수 없었다 — 설명이 길다는 이유였다.
    #[test]
    fn a_long_note_never_eats_into_the_name() {
        let (label, _) =
            split(62, "/agent", Some("에이전트를 고릅니다. 다음 메시지에서 새 thread가 열립니다"));
        assert_eq!(label, "/agent");
    }

    /// 설명이 들어갈 자리가 없으면 설명을 뺀다. 반 토막 설명은 읽을 수 없다.
    #[test]
    fn a_note_is_dropped_rather_than_squeezed_to_nothing() {
        // 이름이 딱 들어가고 설명이 설 자리는 안 남는 폭.
        let (label, note) = split(14, "가나다라마", Some("설명"));
        assert_eq!(label, "가나다라마", "이름이 깎였다");
        assert!(note.is_none(), "{note:?}");
    }

    /// 그래도 이름은 자른다 — 세션 제목이 상자를 뚫고 나가면 화면이 무너진다.
    #[test]
    fn a_very_long_name_is_still_cut_to_fit() {
        let (label, _) = split(20, &"가".repeat(40), None);
        assert!(display_width(&label) <= 18, "{}칸: {label}", display_width(&label));
        assert!(label.ends_with('…'), "잘렸다는 표시가 없다: {label}");
    }

    /// 둘 다 들어가면 둘 다 보인다.
    #[test]
    fn both_fit_when_there_is_room() {
        let (label, note) = split(40, "/cwd", Some("도구가 도는 자리"));
        assert_eq!(label, "/cwd");
        assert_eq!(note.as_deref(), Some("도구가 도는 자리"));
    }
}
