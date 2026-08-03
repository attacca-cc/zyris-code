//! 오른쪽 사이드바 — 위에 사용량, 아래에 태스크.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;
use crate::markdown::display_width;
use crate::sidebar::compact;
use crate::theme;

/// 사이드바 폭. 좁으면 숫자가 잘리고 넓으면 대화가 좁아진다.
pub const WIDTH: u16 = 30;

pub fn draw(frame: &mut Frame, area: Rect, state: &State) {
    let inner = Rect { x: area.x + 2, width: area.width.saturating_sub(2), ..area };
    let mut lines: Vec<Line<'static>> = Vec::new();

    // **도구가 어디서 도는지 맨 위에 둔다.** 도구 줄의 `src/app.rs`가 어느 리포의
    // 것인지는 이것을 봐야 안다. 머리말을 붙이지 않는다 — 경로는 보면 경로다.
    let here = short_path(&state.cwd, inner.width as usize);
    if !here.is_empty() {
        lines.push(Line::from(Span::styled(here, Style::default().fg(theme::TEXT_MUTED))));
        lines.push(Line::from(""));
    }

    lines.push(head(state.lang.usage()));
    let u = &state.sidebar.usage;
    // **값의 왼쪽 끝을 맞춘다.** 이름 길이가 제각각이라(크레딧 6칸, 컨텍스트 8칸) 그냥
    // 붙이면 숫자가 계단처럼 어긋나 한눈에 비교가 안 된다.
    let rows = [
        (state.lang.credits(), u.credits_used.clone().unwrap_or_else(|| "-".into())),
        (state.lang.context(), context_text(u)),
        (state.lang.total_tokens(), u.total_tokens.map(compact).unwrap_or_else(|| "-".into())),
    ];
    let col = rows.iter().map(|(k, _)| display_width(k)).max().unwrap_or(0) + 2;
    for (key, value) in rows {
        lines.push(kv(key, value, col));
    }
    // **열린 셸은 있을 때만 절을 연다.** 좁은 사이드바에서 빈 절은 자리만 먹는다.
    if !state.shells.is_empty() {
        lines.push(Line::from(""));
        lines.push(head(state.lang.shells()));
        for shell in &state.shells {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(theme::SUCCESS)),
                Span::styled(
                    truncate(&shell.name, inner.width.saturating_sub(2) as usize),
                    Style::default().fg(theme::TEXT),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(head(state.lang.tasks()));

    // 남는 높이만큼만 태스크를 보여준다.
    let room = (inner.height as usize).saturating_sub(lines.len()).max(1);
    let tasks = state.sidebar.visible_tasks(room);
    if tasks.is_empty() {
        lines.push(Line::from(Span::styled(
            state.lang.none(),
            Style::default().fg(theme::TEXT_MUTED),
        )));
    }
    for t in tasks {
        let colour = match t.state {
            crate::sidebar::TaskState::Done => theme::TEXT_MUTED,
            crate::sidebar::TaskState::Running => theme::ACCENT,
            crate::sidebar::TaskState::Pending => theme::TEXT,
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", t.state.mark()), Style::default().fg(colour)),
            Span::styled(
                truncate(&t.text, inner.width.saturating_sub(2) as usize),
                Style::default().fg(colour),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// 세로 가름선. 대화 영역과 사이드바 사이에 선다.
pub fn draw_divider(frame: &mut Frame, area: Rect) {
    let lines: Vec<Line<'static>> = (0..area.height)
        .map(|_| Line::from(Span::styled("│", Style::default().fg(theme::BORDER))))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn head(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
    ))
}

/// 컨텍스트는 **쓴 양 / 담을 수 있는 양**으로 보여준다. 숫자 하나만으로는 그것이 여유로운
/// 것인지 꽉 찬 것인지 알 수 없다. 한계를 모르는 모델이면 쓴 양만 적는다.
fn context_text(u: &crate::sidebar::Usage) -> String {
    let Some(used) = u.context_tokens else { return "-".into() };
    match crate::sidebar::context_limit(u.model.as_deref()) {
        Some(max) => format!("{} / {}", compact(used), compact(max)),
        None => compact(used),
    }
}

/// 이름을 `col`칸까지 채워 값의 왼쪽 끝을 맞춘다. 전각이 섞이므로 글자 수가 아니라 칸 수다.
fn kv(key: &str, value: String, col: usize) -> Line<'static> {
    let pad = col.saturating_sub(display_width(key));
    Line::from(vec![
        Span::styled(format!("{key}{}", " ".repeat(pad)), Style::default().fg(theme::TEXT_MUTED)),
        Span::styled(value, Style::default().fg(theme::TEXT)),
    ])
}

/// 좁은 사이드바에 경로를 넣는다.
///
/// 들어가면 그대로 두고, 넘치면 **끝의 두 조각만** 남긴다 — 앞을 자르면 어느 리포인지
/// 알려 주는 마지막 이름이 먼저 사라진다. 사람이 알아야 하는 것은 그쪽이다.
fn short_path(p: &std::path::Path, limit: usize) -> String {
    let full = p.to_string_lossy();
    if display_width(&full) <= limit {
        return full.into_owned();
    }
    let tail: Vec<String> = p
        .components()
        .rev()
        .take(2)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let joined = tail.into_iter().rev().collect::<Vec<_>>().join("/");
    truncate(&format!("…/{joined}"), limit)
}

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
