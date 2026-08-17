//! The conversation area. Draws the lines `rows` built, clipped to the scroll window —
//! counting and drawing share the same `Vec<Line>`, so they cannot drift apart.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::State;
use crate::markdown::display_width;
use std::collections::HashMap;

/// Where in the breath a waiting dot is at `ms`, in the whole steps [`crate::rows::Turn::pulse`]
/// takes — 0 at full colour, [`crate::rows::PULSE_STEPS`] at the background.
///
/// **A triangle, not a square.** The dot used to be on for half a second and off for the next,
/// which reads as flicker and pulls the eye away from the words being read; going out and coming
/// back smoothly says the same thing without asking for attention. The page this app is modelled
/// on does it with `opacity` on a 1.6s ease; this is the same period.
///
/// **It never goes all the way out.** A dot that disappears reads as one that finished, and half of
/// what this is for is saying that something is still waiting.
///
/// Pure and taking its own clock, so a test can walk it rather than sleep through it.
pub fn pulse_at(ms: u64) -> u8 {
    const PERIOD_MS: u64 = 1600;
    const DEEPEST: u32 = 11; // of PULSE_STEPS — far enough to read as receding, not as gone.
    let half = PERIOD_MS / 2;
    let into = ms % PERIOD_MS;
    // Out for the first half of the period, back for the second.
    let travelled = if into < half { into } else { PERIOD_MS - into };
    ((travelled as u32 * DEEPEST) / half as u32) as u8
}

/// The lines a node's body occupies, given where every node's head sits.
///
/// **A body runs from just under its own head to just above the next one.** `heads` is the map the
/// row cache already keeps for click-to-fold — absolute line index to the seq of the node that
/// starts there — so nothing new has to be tracked to know what a fold revealed.
///
/// Pure, and the reason the fade can be applied to lines that are already built and cached: only
/// their colour changes over those few frames, so the cache never has to be told about it.
pub fn body_of(heads: &HashMap<usize, i64>, seq: i64, total: usize) -> std::ops::Range<usize> {
    let Some(&head) = heads.iter().find(|(_, s)| **s == seq).map(|(line, _)| line) else {
        return 0..0;
    };
    let next = heads.keys().copied().filter(|line| *line > head).min().unwrap_or(total);
    (head + 1).min(total)..next.min(total)
}

/// Moves every span on `line` `amount` of the way to the background.
fn faded(line: Line<'static>, amount: f64) -> Line<'static> {
    let style = line.style;
    let spans = line
        .spans
        .into_iter()
        .map(|mut span| {
            // **Only what has a colour is faded.** A span with none is drawing in the terminal's
            // own foreground, and picking a colour for it here would change what it looks like
            // for good rather than for a moment.
            if let Some(fg) = span.style.fg {
                span.style = span.style.fg(crate::theme::fade(fg, amount));
            }
            span
        })
        .collect::<Vec<_>>();
    Line::from(spans).style(style)
}

pub fn draw(frame: &mut Frame, area: Rect, state: &mut State) {
    // The question being answered in the panel below is not drawn again inside the conversation.
    let skip = state.asking.as_ref().map(|(seq, _)| *seq);

    // How far a waiting tool's dot has receded right now. Wall-clock based, so it moves without
    // anything having to be stored between frames.
    let pulse = pulse_at(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    );

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
        let State { timeline, rows_cache, folds, running, lang, .. } = &mut *state;
        let turn = crate::rows::Turn { running: *running, pulse };
        rows_cache.layout(timeline.items(), area.width, folds, skip, turn, *lang);
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
    state.view_open = state.rows_cache.open_states().clone();

    state.scroll.on_content(total, height);
    let (start, end) = state.scroll.window(total, height);
    state.view_top = start;

    // **Build only the visible lines.** Building all of them would grow with the conversation length and blow the frame budget.
    let mut shown = state.rows_cache.window(start, end);
    // **A body arrives rather than appearing.** The lines a fold just revealed are drawn washed
    // toward the background and brought up over `FADE_IN`, so the eye is led to what opened instead
    // of the screen changing under it in one step. Applied to the copies handed back here, so the
    // cache is untouched — see `body_of`.
    for (seq, amount) in state.fading_in() {
        let body = body_of(state.rows_cache.cards(), seq, total);
        for line in body.start.max(start)..body.end.min(end) {
            if let Some(row) = shown.get_mut(line - start) {
                *row = faded(std::mem::take(row), amount);
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **The dot breathes; it does not flicker.** Walking one period, the recession has to rise to
    /// its deepest and come back — a square wave (what this replaced) would only ever read two
    /// values, and that hard on/off is what pulled the eye off the words being read.
    #[test]
    fn a_waiting_dot_fades_out_and_back_rather_than_switching() {
        let seen: Vec<u8> = (0..1600).step_by(50).map(pulse_at).collect();
        let deepest = *seen.iter().max().expect("a period has samples");
        assert!(seen.iter().collect::<std::collections::HashSet<_>>().len() > 4, "{seen:?}");
        assert_eq!(pulse_at(0), 0, "the period starts at full colour");
        assert_eq!(pulse_at(800), deepest, "the middle of the period is the deepest");
        assert_eq!(pulse_at(1600), 0, "and it comes back to where it started");

        // **Never all the way to the background.** A dot that vanishes reads as one that finished,
        // and saying something is still waiting is the whole job.
        assert!(deepest < crate::rows::PULSE_STEPS, "the dot goes out entirely: {deepest}");
    }

    /// It repeats, so a long wait looks the same at the end as at the start.
    #[test]
    fn the_breath_repeats() {
        for ms in [0u64, 137, 799, 1200] {
            assert_eq!(pulse_at(ms), pulse_at(ms + 1600), "the period does not close at {ms}");
        }
    }

    /// **A body is the lines between its own head and the next one.** This is what lets a fold's
    /// reveal be faded without the row cache knowing anything about it — the heads are already
    /// mapped for click-to-fold, so the extent comes out of arithmetic rather than new state.
    #[test]
    fn a_nodes_body_runs_from_under_its_head_to_the_next_one() {
        let heads = HashMap::from([(0usize, 1i64), (4, 2), (9, 3)]);
        assert_eq!(body_of(&heads, 1, 12), 1..4, "the card's body stops at the next head");
        assert_eq!(body_of(&heads, 2, 12), 5..9);
        assert_eq!(body_of(&heads, 3, 12), 10..12, "the last body runs to the end");
        assert_eq!(body_of(&heads, 99, 12), 0..0, "a node that is not there has no body");
    }

    /// **A head with nothing under it has an empty body, not a backwards one.** A folded node sits
    /// directly above the next head, and a range that starts after it ends would panic on slicing.
    #[test]
    fn a_node_with_nothing_under_it_has_an_empty_body() {
        let heads = HashMap::from([(0usize, 1i64), (1, 2)]);
        let body = body_of(&heads, 1, 2);
        assert!(body.start >= body.end, "an empty body came back as {body:?}");
        assert!(body_of(&heads, 2, 2).end <= 2);
    }
}
