//! 대화 영역. `rows`가 만든 줄을 스크롤 창만큼 잘라 그린다 —
//! 세는 것과 그리는 것이 같은 `Vec<Line>`이라 어긋날 수 없다.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;
use crate::markdown::display_width;
use crate::selection;

pub fn draw(frame: &mut Frame, area: Rect, state: &mut State) {
    // 지금 아래 패널에서 답하고 있는 질문은 대화 안에 또 그리지 않는다.
    let skip = state.asking.as_ref().map(|(seq, _)| *seq);
    {
        // 필드를 따로 빌린다 — `timeline`과 `rows_cache`를 동시에 잡아야 한다.
        let State { timeline, rows_cache, folds, .. } = &mut *state;
        rows_cache.layout(timeline.items(), area.width, folds, skip);
    }

    let total = state.rows_cache.total();
    let height = area.height as usize;
    // 휠 처리가 읽을 뷰포트 크기를 남긴다 — `apply`는 순수해서 이걸 스스로 알 수 없다.
    state.view_total = total;
    state.view_height = height;
    state.view_origin = (area.x, area.y);
    state.view_cards = state.rows_cache.cards().clone();

    state.scroll.on_content(total, height);
    let (start, end) = state.scroll.window(total, height);
    state.view_top = start;

    // **보이는 줄만 만든다.** 전부 만들면 대화 길이에 비례해 무거워져 프레임을 넘긴다.
    let mut shown = state.rows_cache.window(start, end);
    // 배경이 얹힌 줄을 화면 끝까지 늘린다. **반전보다 먼저** 한다 — 여러 줄에 걸친
    // 선택이 한 덩어리로 보여야 하고, 늘린 자리도 그 덩어리에 들어가야 한다.
    for line in shown.iter_mut() {
        if line.style.bg.is_some() {
            *line = stretch(std::mem::take(line), area.width as usize);
        }
    }
    // 고른 구간을 반전시켜 보여준다. **열 단위로 자른다** — 줄 통째로 반전하면 고른 것과
    // 보이는 것이 달라진다.
    if let Some(drag) = state.drag.filter(|d| !d.is_click()) {
        for (i, line) in shown.iter_mut().enumerate() {
            if let Some((from, to)) = selection::highlight_span(&drag, start + i) {
                *line = reverse_cols(line, from, to);
            }
        }
    }
    frame.render_widget(Paragraph::new(shown), area);
}

/// 배경이 얹힌 줄을 화면 폭까지 늘린다.
///
/// **여기서 늘린다.** `rows`에서 늘리면 그 공백이 `Rendered::plain()`을 타고 클립보드로
/// 나가 붙여넣은 코드가 망가진다. 그리는 자리만 폭을 알면 되고, 세는 자리는 몰라도 된다.
fn stretch(line: Line<'static>, width: usize) -> Line<'static> {
    let used: usize = line.spans.iter().map(|s| display_width(&s.content)).sum();
    if used >= width {
        return line;
    }
    let bg = line.style.bg;
    let mut spans = line.spans;
    // **fg를 반드시 준다.** 안 주면 터미널 자체의 기본 전경색이 새어 나오고,
    // 그 자리가 반전되면 엉뚱한 색이 뜬다(`theme.rs`의 규칙).
    let mut style = Style::default().fg(crate::theme::TEXT);
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    spans.push(Span::styled(" ".repeat(width - used), style));
    Line::from(spans).style(line.style)
}

/// 화면 열 `[from, to)`만 반전시킨 줄. 경계에 걸친 span은 쪼갠다.
///
/// 원래 색을 잃지 않으려고 span을 다시 칠하지 않고 나눠서 modifier만 얹는다.
fn reverse_cols(line: &Line<'static>, from: usize, to: usize) -> Line<'static> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;

    for span in &line.spans {
        let mut chunk = String::new();
        let mut chunk_on = None::<bool>;

        for ch in span.content.chars() {
            let w = display_width(&ch.to_string()).max(1);
            let on = col >= from && col < to;
            if chunk_on != Some(on) && !chunk.is_empty() {
                out.push(styled(&chunk, span.style, chunk_on == Some(true)));
                chunk.clear();
            }
            chunk_on = Some(on);
            chunk.push(ch);
            col += w;
        }
        if !chunk.is_empty() {
            out.push(styled(&chunk, span.style, chunk_on == Some(true)));
        }
    }
    Line::from(out)
}

fn styled(text: &str, base: ratatui::style::Style, on: bool) -> Span<'static> {
    let style = if on { base.add_modifier(Modifier::REVERSED) } else { base };
    Span::styled(text.to_string(), style)
}
