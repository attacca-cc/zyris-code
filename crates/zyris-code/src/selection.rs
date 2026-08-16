//! Text selection anywhere on the screen.
//!
//! Coordinates are **(row, screen column)**. A column is cells, not characters, so a full-width
//! glyph takes 2 — because the mouse reports in cells. Handling characters instead would skew
//! the selection on lines that contain Hangul.

use crate::markdown::display_width;

/// The range being dragged. The start and the current point; their order can be reversed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drag {
    pub from: (usize, usize),
    pub to: (usize, usize),
}

impl Drag {
    pub fn new(at: (usize, usize)) -> Self {
        Self { from: at, to: at }
    }

    /// (Start, end) sorted top-to-bottom.
    pub fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.from <= self.to {
            (self.from, self.to)
        } else {
            (self.to, self.from)
        }
    }

    /// Has it not moved a single cell? Then it is a click, not a selection.
    pub fn is_click(&self) -> bool {
        self.from == self.to
    }
}

/// Extracts the selected text. Lines are joined with `\n`.
///
/// **The cell the drag ends on is part of the selection.** Ranges here are half-open, so the end
/// column has one added to it: without that, letting go on a character left that character out and
/// a line ending in `)` came back without its `)` (reported 2026-08-17). Nobody drags one past what
/// they mean to take, because there is nothing there to aim at.
pub fn extract(rows: &[String], drag: &Drag) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let ((r0, c0), (r1, c1)) = drag.ordered();
    let last = rows.len() - 1;
    let (r0, r1) = (r0.min(last), r1.min(last));

    // **A selection starts at the text, never in the margin.** Reaching left past the words — which
    // is what selecting whole lines looks like — used to take the markers with it.
    let from = |row: &str, at: usize| at.max(body_start(row));

    if r0 == r1 {
        let (a, b) = (c0.min(c1), c0.max(c1) + 1);
        return slice_cols(&rows[r0], from(&rows[r0], a), b);
    }

    let mut out = vec![slice_cols(&rows[r0], from(&rows[r0], c0), usize::MAX)];
    for row in &rows[r0 + 1..r1] {
        out.push(slice_cols(row, body_start(row), usize::MAX));
    }
    out.push(slice_cols(&rows[r1], body_start(&rows[r1]), c1 + 1));
    out.join("\n")
}

/// The per-row column spans the drag highlights, as `(row, from, to)` with `to` exclusive. Empty
/// when the drag never moved — that is a click, not a selection.
///
/// **This is text selection, not a box.** It mirrors `extract`, so the highlight shows exactly
/// the text that gets copied: a single line spans from its start column to its end column; a
/// multi-line drag takes the first line from its start column, the whole middle lines, and the
/// last line up to its end column. The blank cells before the start (and after the end on the
/// last line) stay untouched — a solid rectangle of colour around the words would read as
/// "box the text", not "select these letters".
///
/// `rows` is the band of the screen the highlight may occupy, and `moved` is how far down the
/// screen its text has travelled since — the conversation scrolling under a highlight moves the
/// highlight with it, so the colour stays on the words it was drawn around.
///
/// **What falls outside `rows` is dropped, not folded onto the edge.** A highlight can be scrolled
/// past either end now, and squashing what fell off the top onto the first visible line would
/// leave a stripe of colour over text nobody selected.
///
/// **A clipped end is covered to the edge.** If the selection carries on above or below what can
/// be seen, the visible line at that end is selected the whole way — stopping at the column the
/// drag happened to start or finish at would draw a ragged edge where the text is not ragged.
pub fn row_spans(
    drag: &Drag,
    width: u16,
    rows: std::ops::Range<u16>,
    moved: isize,
) -> Vec<(u16, u16, u16)> {
    if drag.is_click() || rows.is_empty() || width == 0 {
        return Vec::new();
    }
    let ((r0, c0), (r1, c1)) = drag.ordered();
    let (first, limit) = (rows.start as isize, rows.end as isize);
    let w = width as usize;
    let (r0, r1) = (r0 as isize + moved, r1 as isize + moved);
    if r0 >= limit || r1 < first {
        return Vec::new();
    }

    // Cut at the top: what is left starts at the beginning of the first line still on screen. The
    // column the drag began at belongs to a line that is no longer there.
    let (r0, c0) = if r0 < first { (first, 0) } else { (r0, c0) };
    // Cut at the bottom: the selection runs past the last line on screen, so that line is covered
    // to the edge rather than up to where the drag stopped.
    let cut = r1 >= limit;
    let r1 = if cut { limit - 1 } else { r1 };
    // The end column has one added to it for the same reason `extract` does — the cell the drag
    // ends on is inside the selection — and both are clamped to the width so the last column of a
    // line can be reached at all.
    let (c0, c1) = (c0.min(w.saturating_sub(1)), (c1 + 1).min(w));

    let (r0, r1) = (r0 as u16, r1 as u16);
    if r0 == r1 {
        let (from, to) = if cut { (c0, w) } else { (c0.min(c1), c0.max(c1)) };
        return vec![(r0, from as u16, to as u16)];
    }
    let mut out = vec![(r0, c0 as u16, w as u16)];
    for r in (r0 + 1)..r1 {
        out.push((r, 0, w as u16));
    }
    out.push((r1, 0, if cut { w as u16 } else { c1 as u16 }));
    out
}

/// The glyphs the screen draws in a row's left margin, which are furniture rather than text.
///
/// `▌` marks what a person said, `✻` a stretch of working, `▸`/`▾` a fold, `●` a tool row and `◆`
/// an answer; `│` runs down the left of a code block and `┊` down an opened reasoning body.
const MARGIN_GLYPHS: [char; 8] = ['▌', '✻', '▸', '▾', '●', '◆', '│', '┊'];

/// The column a row's own text starts at, past whatever the screen drew in its margin.
///
/// **The margin is drawn, not written.** Dragging across a code block came back with `  │ ` on the
/// front of every line, and a tool row with its `●` — decoration nobody selected and nothing can
/// paste (reported 2026-08-17). `rows::PAD` is the margin the markers stand in, and a code block
/// adds its rule and one space on top of that.
///
/// **What comes after is left exactly as it is**, indentation included: the space the renderer put
/// after `│` is furniture, and every space after that belongs to the code.
pub fn body_start(row: &str) -> usize {
    let mut col = 0usize;
    let mut chars = row.chars().peekable();
    // The margin: at most `rows::PAD` columns, and blank ones at that.
    //
    // **Counted rather than assumed.** Not every row on screen has a margin — the bottom bar and
    // the status line start hard against the left edge — so taking `PAD` off every row would eat
    // two characters of those. A row whose first cell holds text has no margin to skip.
    while col < crate::rows::PAD as usize && chars.peek() == Some(&' ') {
        chars.next();
        col += 1;
    }
    // A marker standing in that margin, or a rule at the head of the body — a code block, an
    // opened reasoning body — together with the single space the renderer puts after it. What
    // comes after that is the row's own, indentation included.
    if chars.peek().is_some_and(|c| MARGIN_GLYPHS.contains(c)) {
        let c = chars.next().expect("just peeked at it");
        col += display_width(&c.to_string()).max(1);
        if chars.peek() == Some(&' ') {
            col += 1;
        }
    }
    col
}

/// The characters in screen columns `[from, to)`. Full-width glyphs count as 2 cells.
fn slice_cols(row: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    for ch in row.chars() {
        let w = display_width(&ch.to_string()).max(1);
        if col >= to {
            break;
        }
        if col >= from {
            out.push(ch);
        }
        col += w;
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<String> {
        vec![
            "안녕하세요 반갑습니다".to_string(),
            "second line".to_string(),
            "세 번째 줄".to_string(),
        ]
    }

    #[test]
    fn a_drag_that_never_moved_is_a_click() {
        assert!(Drag::new((2, 3)).is_click());
        let mut d = Drag::new((2, 3));
        d.to = (2, 4);
        assert!(!d.is_click());
    }

    /// Selecting within one line yields just that span. Full-width is 2 cells, so column 4 is where
    /// the third character starts — and letting go on a character takes it.
    #[test]
    fn selecting_within_one_line_takes_that_span() {
        let d = Drag { from: (0, 0), to: (0, 4) };
        assert_eq!(extract(&rows(), &d), "안녕하");
    }

    #[test]
    fn selecting_backwards_gives_the_same_text() {
        let forward = Drag { from: (0, 0), to: (0, 4) };
        let backward = Drag { from: (0, 4), to: (0, 0) };
        assert_eq!(extract(&rows(), &forward), extract(&rows(), &backward));
    }

    /// Selecting across lines keeps the middle rows whole and clips the two ends.
    #[test]
    fn selecting_across_lines_keeps_the_middle_whole() {
        let d = Drag { from: (0, 10), to: (2, 4) };
        let got = extract(&rows(), &d);
        let lines: Vec<&str> = got.lines().collect();
        assert_eq!(lines.len(), 3, "{got:?}");
        assert_eq!(lines[1], "second line", "a middle line must be selected whole");
        // That glyph is full-width (columns 3–4), so dragging to column 4 covers it and includes it.
        assert_eq!(lines[2], "세 번", "the last line runs to the glyph covering column 4");
    }

    /// Pointing off-screen must not crash — the mouse can go anywhere.
    #[test]
    fn dragging_past_the_end_is_clamped() {
        let d = Drag { from: (0, 0), to: (99, 999) };
        let got = extract(&rows(), &d);
        assert!(got.ends_with("세 번째 줄"), "{got:?}");
    }

    /// A drag that moved selects text, not a box: the first row starts at its start column,
    /// the middle row is whole, and the last runs to its end column — the blank cells before
    /// the start and after the end stay untouched.
    #[test]
    fn row_spans_follow_the_text_not_a_rectangle() {
        let d = Drag { from: (1, 3), to: (3, 7) };
        assert_eq!(row_spans(&d, 80, 0..24, 0), vec![(1, 3, 80), (2, 0, 80), (3, 0, 8)]);
    }

    /// A single-line drag spans just its two ends.
    #[test]
    fn row_spans_on_one_line_span_the_two_ends() {
        let d = Drag { from: (0, 2), to: (0, 6) };
        assert_eq!(row_spans(&d, 80, 0..24, 0), vec![(0, 2, 7)]);
    }

    /// Dragging backwards gives the same spans as forwards.
    #[test]
    fn row_spans_are_the_same_either_way_around() {
        let down = Drag { from: (1, 3), to: (3, 7) };
        let up = Drag { from: (3, 7), to: (1, 3) };
        assert_eq!(row_spans(&down, 80, 0..24, 0), row_spans(&up, 80, 0..24, 0));
    }

    /// A drag that never moved is a click and highlights nothing.
    #[test]
    fn row_spans_is_empty_for_a_click() {
        assert_eq!(row_spans(&Drag::new((2, 3)), 80, 0..24, 0), Vec::<(u16, u16, u16)>::new());
    }

    /// The mouse can go past the edge of the terminal; the spans are clipped to it. The last
    /// visible line is covered to the edge, because the selection carries on below it.
    #[test]
    fn row_spans_are_clipped_to_the_terminal() {
        let d = Drag { from: (0, 0), to: (99, 999) };
        let spans = row_spans(&d, 80, 0..24, 0);
        assert_eq!(spans.len(), 24);
        assert_eq!(spans.first(), Some(&(0, 0, 80)));
        assert_eq!(spans.last(), Some(&(23, 0, 80)));
    }

    /// **The highlight rides the conversation.** Scrolling moves the text under it, so the colour
    /// moves the same distance and stays on the same words. Before this it was simply dropped, and
    /// a scroll of one notch looked like the selection had been lost — copied text and all.
    #[test]
    fn a_highlight_moves_with_the_text_it_was_drawn_around() {
        let d = Drag { from: (5, 3), to: (7, 7) };
        let still = row_spans(&d, 80, 0..24, 0);
        let moved = row_spans(&d, 80, 0..24, -4);
        assert_eq!(still, vec![(5, 3, 80), (6, 0, 80), (7, 0, 8)]);
        assert_eq!(moved, vec![(1, 3, 80), (2, 0, 80), (3, 0, 8)], "it did not follow the text");
    }

    /// Scrolled off the top, what is left starts at the beginning of the first line still on
    /// screen — the column the drag began at is on a line that is no longer there, and starting
    /// the colour at that column would indent a highlight whose text is not indented.
    #[test]
    fn a_highlight_scrolled_past_the_top_keeps_only_what_is_left() {
        let d = Drag { from: (5, 3), to: (9, 7) };
        assert_eq!(row_spans(&d, 80, 0..24, -7), vec![(0, 0, 80), (1, 0, 80), (2, 0, 8)]);
    }

    /// Scrolled away entirely, it draws nothing. Folding it onto the edge instead would leave a
    /// stripe of colour over text nobody selected.
    #[test]
    fn a_highlight_scrolled_out_of_sight_draws_nothing() {
        let d = Drag { from: (5, 3), to: (7, 7) };
        assert!(row_spans(&d, 80, 0..24, -8).is_empty(), "it was folded onto the top edge");
        assert!(row_spans(&d, 80, 0..24, 20).is_empty(), "it was folded onto the bottom edge");
    }

    /// **The band is the conversation, not the screen.** Below it are the activity line, the input
    /// and the bars, and those do not scroll — a highlight sliding down over them would sit on
    /// text that never moved.
    #[test]
    fn a_highlight_never_slides_over_what_does_not_scroll() {
        let d = Drag { from: (5, 3), to: (7, 7) };
        let spans = row_spans(&d, 80, 0..9, 2);
        assert_eq!(spans, vec![(7, 3, 80), (8, 0, 80)], "it was drawn past the conversation");
    }

    /// **The character the drag ends on is taken.** Ranges here are half-open, so without the one
    /// added to the end column, letting go on a character left it out — a line ending in `)` came
    /// back without its `)` (reported 2026-08-17). Nobody drags one past what they mean to take,
    /// because past the last character there is nothing to aim at.
    #[test]
    fn the_character_the_drag_ends_on_is_copied() {
        let rows = vec!["do_it(x)".to_string()];
        // Column 7 is the `)`.
        let d = Drag { from: (0, 0), to: (0, 7) };
        assert_eq!(extract(&rows, &d), "do_it(x)");
        // And backwards over the same cells.
        let back = Drag { from: (0, 7), to: (0, 0) };
        assert_eq!(extract(&rows, &back), "do_it(x)");
    }

    /// The same on the last line of a selection that runs over several.
    #[test]
    fn the_last_line_keeps_the_character_the_drag_stopped_on() {
        let rows = vec!["first".to_string(), "do_it(x)".to_string()];
        let d = Drag { from: (0, 0), to: (1, 7) };
        assert_eq!(extract(&rows, &d), "first\ndo_it(x)");
    }

    /// **What is highlighted is what is copied.** The two are read off different functions, so
    /// nothing but a test keeps them saying the same thing — and a highlight that stops one cell
    /// short of the clipboard is how the missing `)` looked on screen.
    #[test]
    fn the_highlight_covers_exactly_what_is_copied() {
        let rows = vec!["do_it(x)".to_string()];
        let d = Drag { from: (0, 2), to: (0, 7) };
        let spans = row_spans(&d, 80, 0..24, 0);
        assert_eq!(spans.len(), 1);
        let (_, from, to) = spans[0];
        assert_eq!(extract(&rows, &d), "_it(x)");
        assert_eq!(
            (to - from) as usize,
            extract(&rows, &d).chars().count(),
            "colour and text differ"
        );
    }

    /// **The margin is furniture, not text.** Dragging a code block came back with `  │ ` on the
    /// front of every line, and a tool row with its `●` — decoration nobody selected and nothing
    /// can paste. The code's own indentation is not margin and stays.
    #[test]
    fn the_margin_the_screen_draws_is_not_part_of_the_selection() {
        assert_eq!(body_start("  │     ret = pthread_create();"), 4, "the rule and one space");
        assert_eq!(body_start("● read  src/rows.rs"), 2, "just the marker's own margin");
        assert_eq!(body_start("  plain conversation text"), 2, "an unmarked row still has one");
        // The bottom bar and the status line start hard against the edge and have no margin.
        assert_eq!(body_start("job·Main Agent"), 0, "it ate the start of an unmargined row");
        assert_eq!(body_start(""), 0, "a blank row has nothing to skip");
        assert_eq!(body_start(" "), 1, "and neither has a short one");
    }

    /// Reaching left past the words — which is what selecting whole lines looks like — must not
    /// take the markers with it, on any line of the selection.
    #[test]
    fn dragging_across_a_code_block_copies_the_code_and_not_the_rule() {
        let rows = vec![
            "  │     ret = call();".to_string(),
            "  │     if (ret != 0) {".to_string(),
            "  │         return 1;".to_string(),
        ];
        let d = Drag { from: (0, 0), to: (2, 30) };
        let got = extract(&rows, &d);
        assert_eq!(got, "    ret = call();\n    if (ret != 0) {\n        return 1;");
        assert!(!got.contains('│'), "the rule came along:\n{got}");
    }

    /// Starting the drag inside the text keeps it there — the margin rule only ever pushes right.
    #[test]
    fn a_drag_that_starts_inside_the_text_starts_where_it_was_put() {
        let rows = vec!["  │     ret = call();".to_string()];
        // Two spaces, the rule, five more spaces, then `ret = call();` — so columns 10 to 12 are
        // the end of `ret` and the `=` after it.
        let d = Drag { from: (0, 10), to: (0, 12) };
        assert_eq!(extract(&rows, &d), "t =".to_string());
    }

    #[test]
    fn selecting_nothing_gives_an_empty_string() {
        assert_eq!(extract(&[], &Drag::new((0, 0))), "");
    }
}
