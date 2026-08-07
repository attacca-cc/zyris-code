//! The conversation area. Draws the lines `rows` built, clipped to the scroll window —
//! counting and drawing share the same `Vec<Line>`, so they cannot drift apart.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;
use crate::markdown::display_width;

pub fn draw(frame: &mut Frame, area: Rect, state: &mut State) {
    // The question being answered in the panel below is not drawn again inside the conversation.
    let skip = state.asking.as_ref().map(|(seq, _)| *seq);

    // **What the viewport was looking at, taken before the relayout.** `Scroll.top` is an
    // absolute line index and `layout` rebuilds the line list from scratch, so a width change or
    // a fold opening above the viewport moves the text out from under that index — always toward
    // older content, because the clamp in `on_content` can only push it down. That is the
    // "scrolled up, came back, and it was showing old chat" report: nothing scrolled, the lines
    // moved. Sticking to the bottom needs no anchor; the bottom is its own anchor.
    let anchor =
        (!state.scroll.stick).then(|| state.rows_cache.anchor_at(state.scroll.top)).flatten();

    {
        // Borrow the fields separately — `timeline` and `rows_cache` must be held at the same time.
        let State { timeline, rows_cache, folds, lang, .. } = &mut *state;
        rows_cache.layout(timeline.items(), area.width, folds, skip, *lang);
    }

    // Put the view back on the same words. When nothing was relaid out this resolves to the line
    // it already held, so it costs a lookup and changes nothing.
    if let Some((seq, offset)) = anchor {
        if let Some(line) = state.rows_cache.line_of(seq, offset) {
            state.scroll.top = line;
        }
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
    // The links on those same lines, in the same order. `widgets::draw` wraps the link cells
    // in OSC 8 (Ctrl+click) using these — they are in **display columns of the line as drawn**,
    // so the injection needs no further mapping beyond `view_origin`.
    state.view_links = state.rows_cache.window_links(start, end);
    // Stretch lines with a background to the screen edge. The selection highlight is applied
    // over the whole frame after every widget drew (`widgets::draw`), so it covers this
    // stretched space in the same block.
    for line in shown.iter_mut() {
        if line.style.bg.is_some() {
            *line = stretch(std::mem::take(line), area.width as usize);
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
    let mut style = Style::default().fg(crate::theme::text());
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    spans.push(Span::styled(" ".repeat(width - used), style));
    Line::from(spans).style(line.style)
}
