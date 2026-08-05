//! The conversation area. Draws the lines `rows` built, clipped to the scroll window —
//! counting and drawing share the same `Vec<Line>`, so they cannot drift apart.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;
use crate::markdown::display_width;
use crate::selection;

pub fn draw(frame: &mut Frame, area: Rect, state: &mut State) {
    // The question being answered in the panel below is not drawn again inside the conversation.
    let skip = state.asking.as_ref().map(|(seq, _)| *seq);
    {
        // Borrow the fields separately — `timeline` and `rows_cache` must be held at the same time.
        let State { timeline, rows_cache, folds, lang, .. } = &mut *state;
        rows_cache.layout(timeline.items(), area.width, folds, skip, *lang);
    }

    let total = state.rows_cache.total();
    let height = area.height as usize;
    // Leave the viewport size for wheel handling to read — `apply` is pure and cannot know it itself.
    state.view_total = total;
    state.view_height = height;
    state.view_origin = (area.x, area.y);
    state.view_cards = state.rows_cache.cards().clone();

    state.scroll.on_content(total, height);
    let (start, end) = state.scroll.window(total, height);
    state.view_top = start;

    // **Build only the visible lines.** Building all of them would grow with the conversation length and blow the frame budget.
    let mut shown = state.rows_cache.window(start, end);
    // Stretch lines with a background to the screen edge. **Do this before inverting** — a
    // selection spanning multiple lines must look like one block, with the stretched space in it.
    for line in shown.iter_mut() {
        if line.style.bg.is_some() {
            *line = stretch(std::mem::take(line), area.width as usize);
        }
    }
    // Show the selected span inverted. **Clip by column** — inverting whole lines would make
    // what was selected differ from what is shown.
    if let Some(drag) = state.drag.filter(|d| !d.is_click()) {
        for (i, line) in shown.iter_mut().enumerate() {
            if let Some((from, to)) = selection::highlight_span(&drag, start + i) {
                *line = reverse_cols(line, from, to);
            }
        }
    }
    frame.render_widget(Paragraph::new(shown), area);
}

/// Stretches a line with a background to the screen width.
///
/// **Stretch here.** Stretching in `rows` would carry that padding through `Rendered::plain()`
/// to the clipboard and break pasted code. Only the drawing side needs the width, not the counting side.
fn stretch(line: Line<'static>, width: usize) -> Line<'static> {
    let used: usize = line.spans.iter().map(|s| display_width(&s.content)).sum();
    if used >= width {
        return line;
    }
    let bg = line.style.bg;
    let mut spans = line.spans;
    // **Must set fg.** Without it, the terminal's own default foreground bleeds through, and
    // inverting that space shows the wrong color (the rule in `theme.rs`).
    let mut style = Style::default().fg(crate::theme::TEXT);
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    spans.push(Span::styled(" ".repeat(width - used), style));
    Line::from(spans).style(line.style)
}

/// A line with only screen columns `[from, to)` inverted. Spans crossing the boundary are split.
///
/// To keep original colors, spans are split rather than repainted and only a modifier is applied.
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
