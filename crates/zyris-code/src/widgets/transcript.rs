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

/// How long one out-and-back of the breath takes.
///
/// **A period, and the drawing side reads it as a clock** (`breath_at` is handed `state.breath_ms`,
/// not a frame number): a tempo stepped by frames runs at whatever rate the frame timer happens to
/// be set to.
pub const BREATH_PERIOD_MS: u64 = 1600;

/// How many steps the breath is drawn in over that period.
///
/// **A tempo, not a frame count** — the same lesson as the activity dot's blink
/// (`activity::BLINK_HALF_MS`), learned from the other end. The fade itself is continuous, so
/// every frame carries a slightly different colour and every frame is a *different picture*: drawn
/// on every tick that is sixty frames a second spent on a 1.6s fade, and a frame is not free —
/// measured at 211×58 in a debug build, 12-20ms against a 16ms tick, so the loop was saturated for
/// as long as a turn ran. Twenty steps over the period is a step every 80ms, which the eye reads as
/// the same fade, at a fifth of the frames.
pub const BREATH_STEPS: u64 = 20;

/// Which step of the breath `ms` falls in — the clock rounded to what the eye is shown.
///
/// Read by the frame loop to decide whether a tick owes a frame (`tick_draws_for_the_breath`).
/// **`breath_at` keeps taking the continuous time**: a frame drawn for another reason should carry
/// the breath where it actually is, not where the last step left it.
pub fn breath_step(ms: u64) -> u64 {
    (ms % BREATH_PERIOD_MS) / (BREATH_PERIOD_MS / BREATH_STEPS)
}

/// How far toward the background the breath has gone at `ms` — `0.0` at full colour, [`DEEPEST`]
/// at its faintest.
///
/// **A triangle, not a square.** The dot used to be on for half a second and off for the next,
/// which reads as flicker and pulls the eye away from the words being read; going out and coming
/// back smoothly says the same thing without asking for attention. The page this app is modelled
/// on does it with `opacity` on a 1.6s ease; this is the same period.
///
/// **It never goes all the way out.** A dot that disappears reads as one that finished, and half of
/// what this is for is saying that something is still waiting.
///
/// **Not quantised.** It used to come back in sixteenths, because the value was baked into a
/// cached line and every distinct one cost a rebuild. Eleven steps over the sixteen frames of a
/// half-period is 0.6875 of a step per frame, so the increments ran `0,1,1,1,0,1,0,1,1,…` — a limp
/// that no frame rate could smooth, since raising it only added frames that repeated the value.
/// The fade is applied to the drawn copy now and costs nothing, so it can simply be continuous.
///
/// Pure and taking its own clock, so a test can walk it rather than sleep through it.
pub fn breath_at(ms: u64) -> f64 {
    let half = BREATH_PERIOD_MS / 2;
    let into = ms % BREATH_PERIOD_MS;
    // Out for the first half of the period, back for the second.
    let travelled = if into < half { into } else { BREATH_PERIOD_MS - into };
    travelled as f64 / half as f64 * DEEPEST
}

/// How far toward the background the faintest part of the breath sits. **Not all the way**: this
/// is a line somebody is reading, and text that goes out is worse than text that does not move.
pub const DEEPEST: f64 = 0.6875;

/// Which line a node's head sits on, if it is on screen at all.
pub fn head_of(heads: &HashMap<usize, i64>, seq: i64) -> Option<usize> {
    heads.iter().find(|(_, s)| **s == seq).map(|(line, _)| *line)
}

/// The lines a node's body occupies: from just under its head to whichever comes first — the next
/// node's head, or `stop`, the end of the item the node belongs to.
///
/// **`stop` is what keeps the fade to what actually opened.** The head map only knows about things
/// that fold, so a tool or a chip that is the last one inside its card has no head after it, and
/// reaching for "the next head" ran past the end of the card and washed the agent's answer — and
/// everything under it — along with the body that had just been revealed (reported 2026-08-18).
///
/// Pure, and the reason the fade can be applied to lines that are already built and cached: only
/// their colour changes over those few frames, so the cache never has to be told about it.
pub fn body_of(heads: &HashMap<usize, i64>, head: usize, stop: usize) -> std::ops::Range<usize> {
    let next = heads.keys().copied().filter(|line| *line > head).min().unwrap_or(stop);
    (head + 1).min(stop)..next.min(stop)
}

/// The lines a node put on screen, which is what a fade is applied to.
///
/// **A card's is its whole card, chips and all.** [`body_of`] stops at the next head, which is
/// right for a chip — the head after it is its sibling — and wrong for the card wrapping them,
/// whose first chip is the very next head. The wrapper's reveal was therefore the nothing between
/// the two, and expanding it read as no animation at all (reported 2026-08-18).
///
/// `owns_the_item` is whether this head is the item's own, which is how the two are told apart.
pub fn revealed(
    heads: &HashMap<usize, i64>,
    head: usize,
    end: usize,
    owns_the_item: bool,
) -> std::ops::Range<usize> {
    match owns_the_item {
        true => (head + 1).min(end)..end,
        false => body_of(heads, head, end),
    }
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

    // **Driven by the clock, not by a frame count.** A timer fire is not a draw: the streaming
    // gate drops some, a keystroke and the healing repaint draw extra frames between two of them,
    // and a stalled loop fires several back to back. Stepping an animation by that count is
    // fictional time, and the eye reads the difference as the breath speeding up and stalling.
    // Where the breath actually is depends on nothing but how long the turn has been going.
    let breath = if state.running { breath_at(state.breath_ms()) } else { 0.0 };

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
        let turn = crate::rows::Turn { running: *running };
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
    // **Where each drawn line's own text starts.** The layout is the only thing that knows what it
    // put in a line's margin, so the record rides along with the lines `window` just handed out and
    // the selection starts there instead of guessing from the characters (`rows::furniture_width`).
    state.view_body = state.rows_cache.window_body(start, end);

    // **Build only the visible lines.** Building all of them would grow with the conversation length and blow the frame budget.
    let mut shown = state.rows_cache.window(start, end);
    // **A body arrives rather than appearing.** The lines a fold just revealed are drawn washed
    // toward the background and brought up over `FADE_IN`, so the eye is led to what opened instead
    // of the screen changing under it in one step. Applied to the copies handed back here, so the
    // cache is untouched — see `body_of`.
    for (seq, amount) in state.fading_in() {
        let Some(head) = head_of(state.rows_cache.cards(), seq) else { continue };
        // **A card's body is its whole card, chips and all.** `body_of` stops at the next head,
        // which is right for a chip — the head after it is its sibling — and wrong for the card
        // wrapping them, whose first chip is the very next head. The wrapper's reveal therefore
        // covered the nothing between the two and read as no animation at all (reported
        // 2026-08-18). An item's own head is told apart by its seq being the item's own.
        let owns_the_item = state.rows_cache.anchor_at(head).is_some_and(|(item, _)| item == seq);
        let end = state.rows_cache.item_end(head);
        let body = revealed(state.rows_cache.cards(), head, end, owns_the_item);
        for line in body.start.max(start)..body.end.min(end) {
            if let Some(row) = shown.get_mut(line - start) {
                *row = faded(std::mem::take(row), amount);
            }
        }
    }
    // **The breath, applied to the copies rather than baked into the cache.** This is the whole
    // reason it moves: a card is rebuilt only when its content changes, so a head coloured at
    // build time stood perfectly still through a silent tool call and then lurched the moment
    // output arrived. Here it costs one span rewrite per breathing thing per frame, and it lands
    // wherever the clock says regardless of what was rebuilt.
    if breath > 0.0 {
        for (row, span) in state.rows_cache.breathing() {
            if *row < start || *row >= end {
                continue;
            }
            let Some(line) = shown.get_mut(row - start) else { continue };
            let Some(target) = line.spans.get_mut(*span) else { continue };
            if let Some(colour) = target.style.fg {
                target.style = target.style.fg(crate::theme::fade(colour, breath));
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
        assert_eq!(breath_at(0), 0.0, "the period starts at full colour");
        assert_eq!(breath_at(800), DEEPEST, "the middle of the period is the deepest");
        assert_eq!(breath_at(1600), 0.0, "and it comes back to where it started");

        // **Never all the way to the background.** A dot that vanishes reads as one that finished,
        // and saying something is still waiting is the whole job.
        assert!(breath_at(800) < 1.0, "the dot goes out entirely: {DEEPEST}");
    }

    /// **Every frame moves it, at any frame rate.** The value used to come back in sixteenths, so
    /// eleven steps had to cover the sixteen frames of a half-period — 0.6875 of a step each,
    /// which is an irregular run of holds and moves that reads as a limp however fast frames come.
    /// Raising the rate made it worse: at 60fps, four frames in five repeated the one before.
    #[test]
    fn no_two_frames_in_a_row_land_on_the_same_breath() {
        for frame_ms in [50u64, 33, 16] {
            let seen: Vec<f64> = (0..1600).step_by(frame_ms as usize).map(breath_at).collect();
            for pair in seen.windows(2) {
                assert_ne!(pair[0], pair[1], "the breath stood still at {frame_ms}ms: {seen:?}");
            }
        }
    }

    /// It repeats, so a long wait looks the same at the end as at the start.
    #[test]
    fn the_breath_repeats() {
        for ms in [0u64, 137, 799, 1200] {
            assert_eq!(breath_at(ms), breath_at(ms + 1600), "the period does not close at {ms}");
        }
    }

    /// **The wrapper reveals its whole card, not the gap above its first chip.** Its chips are
    /// heads of their own, so stopping at "the next head" left the fade nothing to touch and
    /// expanding a Thinking card looked like it had no animation at all.
    #[test]
    fn a_card_reveals_everything_inside_it_and_a_chip_only_its_own_body() {
        // A card head at 0, chips at 2 and 6, the card ending at 10.
        let heads = HashMap::from([(0usize, 1i64), (2, 2), (6, 3)]);
        assert_eq!(
            revealed(&heads, 0, 10, true),
            1..10,
            "the wrapper's reveal stopped at its first chip",
        );
        // A chip inside it still stops where its sibling begins.
        assert_eq!(revealed(&heads, 2, 10, false), 3..6);
        assert_eq!(revealed(&heads, 6, 10, false), 7..10);
        // And an empty card is an empty range, not a backwards one.
        assert!(revealed(&HashMap::from([(4usize, 1i64)]), 4, 4, true).is_empty());
    }

    /// **A body is the lines between its own head and the next one — and never past its item.**
    /// The head map only knows about things that fold, so the last tool inside a card has no head
    /// after it; reaching for "the next head" ran off the end of the card and washed the agent's
    /// answer and everything below it along with the reveal.
    #[test]
    fn a_body_stops_at_the_next_head_or_at_the_end_of_its_item() {
        // A card at 0 holding tools at 4 and 9; the card itself ends at 12, and lines 12..20 are
        // the answer that followed it and whatever came after that.
        let heads = HashMap::from([(0usize, 1i64), (4, 2), (9, 3)]);
        assert_eq!(body_of(&heads, 0, 12), 1..4, "the card's body stops at the first tool");
        assert_eq!(body_of(&heads, 4, 12), 5..9);
        assert_eq!(
            body_of(&heads, 9, 12),
            10..12,
            "the last tool's body must stop where its card does, not run on into the answer",
        );
    }

    /// **A head with nothing under it has an empty body, not a backwards one.** A folded node sits
    /// directly above the next head, and a range that starts after it ends would panic on slicing.
    #[test]
    fn a_node_with_nothing_under_it_has_an_empty_body() {
        let heads = HashMap::from([(0usize, 1i64), (1, 2)]);
        let body = body_of(&heads, 0, 2);
        assert!(body.start >= body.end, "an empty body came back as {body:?}");
        assert!(body_of(&heads, 1, 2).end <= 2);
    }

    /// The head map is how a seq is found at all; a node that is not on screen has no head.
    #[test]
    fn a_node_that_is_not_on_screen_has_no_head() {
        let heads = HashMap::from([(0usize, 1i64), (4, 2)]);
        assert_eq!(head_of(&heads, 2), Some(4));
        assert_eq!(head_of(&heads, 99), None);
    }
}
