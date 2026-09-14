//! Fitting text to a width without losing any of it.
//!
//! **A box that is the only copy of its text must wrap, not clip.** `Paragraph` drops whatever
//! runs past the right edge and prints no mark saying it did; an `…` is better than that but it
//! still loses the words behind it, and the boxes here — a question, a mode's description, a
//! setting's explanation — have nowhere to scroll back to. The text is the whole reason the box
//! is up.
//!
//! `widgets/enroll.rs` found this first (2026-08-14: "맨 위 안내문구 끝이 짤린다"), and its fix was
//! two rules in one: wrap the text, then **derive the box height from how many lines came out**.
//! This module is that first rule made reusable; the second one belongs to whoever draws the box.
//!
//! Three wrappers, because not every line is prose:
//! - [`words`] breaks between words, for something a person reads.
//! - [`columns`] fills by column, for text that must keep every character — pretty-printed JSON, a
//!   tool's raw output — where there is no word boundary to trust.
//! - [`line`] is the one the popups use. It splits a styled line and **keeps each span's style**,
//!   so wrapping can never repaint what it wraps.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::markdown::display_width;

/// The narrowest a wrapped column may get.
///
/// Nothing is readable below this, and it also stops a pathological width — a pane squeezed to
/// three cells — from turning every word into a column of single characters.
const MIN: usize = 8;

/// The width one character occupies, never zero: a combining mark still has to take a cell in our
/// count, or the line we measure is shorter than the one the terminal draws.
fn cell_width(ch: char) -> usize {
    display_width(&ch.to_string()).max(1)
}

/// Splits prose to fit the width, breaking between words where it can.
///
/// A word wider than a whole line is filled by column instead, so an unbroken run — a URL, a
/// base64 blob — cannot loop on one line for ever.
pub fn words(text: &str, width: usize) -> Vec<String> {
    let limit = width.max(MIN);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut used = 0usize;
    for word in text.split_whitespace() {
        let w = display_width(word);
        let gap = usize::from(!cur.is_empty());
        if used + gap + w > limit && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            used = 0;
        }
        if w > limit {
            // Longer than a line on its own — fill by column, since there is no break to find.
            for ch in word.chars() {
                let cw = cell_width(ch);
                if used + cw > limit {
                    out.push(std::mem::take(&mut cur));
                    used = 0;
                }
                cur.push(ch);
                used += cw;
            }
            continue;
        }
        if !cur.is_empty() {
            cur.push(' ');
            used += 1;
        }
        cur.push_str(word);
        used += w;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Splits by column, keeping every character — and every line break that was already there.
pub fn columns(text: &str, width: usize) -> Vec<String> {
    let limit = width.max(MIN);
    let mut out = Vec::new();
    for raw in text.lines() {
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut used = 0usize;
        for ch in raw.chars() {
            let cw = cell_width(ch);
            if used + cw > limit {
                out.push(std::mem::take(&mut cur));
                used = 0;
            }
            cur.push(ch);
            used += cw;
        }
        out.push(cur);
    }
    out
}

/// Splits a styled line into as many lines as the width needs, **keeping each span's style**.
///
/// The break prefers the last space that still fits, so a word is only cut when it could not fit
/// on a line of its own — the same order of preference [`words`] uses, carried across spans.
///
/// **A wrapped line hangs under the line it came from.** The left margin is what says which thing
/// a line belongs to — a bullet under a bullet, an option under its caret, a body under its
/// marker — and lines after the first used to start at column zero, so one long sentence stepped
/// out of the list it was part of and left the pane looking broken. The margin is measured once
/// ([`hang_width`]) and every continuation line carries it as **spaces**: the marker's own glyph
/// would read as another row if it were repeated.
///
/// **A line that already fits comes back untouched**, which is what keeps every caller's existing
/// output — a panel, a question — exactly as it was until the text is genuinely too long.
pub fn line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let limit = width.max(1);
    let total: usize = line.spans.iter().map(|s| display_width(&s.content)).sum();
    if total <= limit {
        return vec![line];
    }
    // Flattened so a break may fall inside a span; the style rides along with each character.
    let mut cells: Vec<(char, Style)> = Vec::new();
    for span in line.spans {
        for ch in span.content.chars() {
            cells.push((ch, span.style));
        }
    }
    // **The hang.** What the marker is painted in stays with it — these are spaces, so the only
    // thing it can show is a background, and a background that stops at the indent would be worse
    // than none.
    let hang_style = cells.first().map_or_else(Style::default, |(_, style)| *style);
    // Never so wide that nothing is left for the words: a continuation is still text.
    let hang = hang_width(&cells).min(limit.saturating_sub(MIN));
    let indent = || -> Vec<(char, Style)> { vec![(' ', hang_style); hang] };

    let mut whole: Vec<Vec<(char, Style)>> = Vec::new();
    let mut cur: Vec<(char, Style)> = Vec::new();
    let mut used = 0usize;
    // Where the last space we passed sits, held by index — the place to break at.
    let mut space: Option<usize> = None;
    // **Only the first line keeps the marker's own columns.** Every later one is short by the
    // hang, because it carries the indent instead.
    let mut first = true;
    for (ch, style) in cells {
        let w = cell_width(ch);
        let cap = if first { limit } else { limit.saturating_sub(hang) };
        if used + w > cap && !cur.is_empty() {
            match space.filter(|i| *i > 0) {
                Some(i) => {
                    let tail = cur.split_off(i);
                    whole.push(std::mem::take(&mut cur));
                    // `tail` starts on the space we broke at; a space is not content.
                    cur = tail.into_iter().skip_while(|(c, _)| *c == ' ').collect();
                }
                // Nowhere to break — a word wider than the line. Cut it here.
                None => whole.push(std::mem::take(&mut cur)),
            }
            first = false;
            cur = indent().into_iter().chain(cur).collect();
            used = cur.iter().map(|(c, _)| cell_width(*c)).sum();
            space = None;
        }
        if ch == ' ' {
            space = Some(cur.len());
        }
        cur.push((ch, style));
        used += w;
    }
    whole.push(cur);

    whole
        .into_iter()
        .map(|cells| {
            // Consecutive characters that share a style go back into one span.
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (ch, style) in cells {
                match spans.last_mut() {
                    Some(last) if last.style == style => last.content.to_mut().push(ch),
                    _ => spans.push(Span::styled(ch.to_string(), style)),
                }
            }
            Line::from(spans)
        })
        .collect()
}

/// How far a wrapped line's continuation is indented.
///
/// **The left margin of the line is what the eye reads as "which thing is this"**, so it is what a
/// wrap has to carry. Two kinds of column make it up: plain spaces, and a marker glyph followed by
/// a space. Both count. Counting a marker costs it nothing, because the continuation draws spaces
/// in its place ([`line`]) — `❯ 모드` and `  모드` therefore hang by the same two columns, and no
/// second bullet appears.
///
/// The scan stops at the first text character: the hang is a margin, not the whole prefix, and a
/// line is not indented by its own sentence.
fn hang_width(cells: &[(char, Style)]) -> usize {
    let mut w = 0usize;
    let mut i = 0usize;
    loop {
        while i < cells.len() && cells[i].0 == ' ' {
            w += cell_width(cells[i].0);
            i += 1;
        }
        // **A marker counts only when a space follows it.** Without one it is the text itself:
        // `─────` is a rule and `+12` is a count, and neither is a margin.
        if i + 1 < cells.len() && is_marker(cells[i].0) && cells[i + 1].0 == ' ' {
            w += cell_width(cells[i].0) + 1;
            i += 2;
            continue;
        }
        return w;
    }
}

/// The glyphs these panes use to mark a line — a caret, a bullet, a chip's chevron, `?` for a
/// question.
///
/// **A marker is not text.** It stands in the left margin, outside the sentence, which is exactly
/// why a wrapped line hangs under it instead of starting at column zero.
fn is_marker(ch: char) -> bool {
    matches!(
        ch,
        '❯' | '▸'
            | '▾'
            | '●'
            | '○'
            | '✓'
            | '✎'
            | '✕'
            | '→'
            | '∙'
            | '•'
            | '‒'
            | '◆'
            | '✻'
            | '┊'
            | '│'
            | '◈'
            | '└'
            | '?'
            | '-'
            | '*'
            | '+'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    /// **Not one word is allowed to go missing**, which is the whole point of the module.
    #[test]
    fn wrapping_loses_no_words() {
        let src = "the words here are ordinary but there are a great many of them in this sentence";
        let out = words(src, 20);
        assert!(out.len() > 1, "nothing wrapped: {out:?}");
        assert_eq!(out.join(" "), src);
        assert!(out.iter().all(|l| display_width(l) <= 20), "{out:?}");
    }

    /// A word wider than the line is cut by column rather than dropped — and the cut is not
    /// silent, because the pieces are still on screen.
    #[test]
    fn a_word_wider_than_the_line_is_filled_by_column() {
        let long = "a".repeat(37);
        let out = words(&long, 10);
        assert_eq!(out.concat(), long);
        assert!(out.iter().all(|l| display_width(l) <= 10), "{out:?}");
    }

    /// **A line that fits is handed straight back.** Every caller in the app kept drawing exactly
    /// what it drew before this module arrived, until the text really was too long.
    #[test]
    fn a_line_that_fits_is_unchanged() {
        let line = Line::from(vec![
            Span::styled("hello ", Style::default().fg(Color::Red)),
            Span::styled("world", Style::default().fg(Color::Blue)),
        ]);
        let out = line.clone().spans;
        let got = super::line(line, 80);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].spans, out);
    }

    /// Each span's own colour survives the split — a wrapped line must not be repainted.
    #[test]
    fn wrapping_keeps_every_span_its_own_colour() {
        let line = Line::from(vec![
            Span::styled("❯ ", Style::default().fg(Color::Green)),
            Span::styled(
                "a description that is far too long to fit",
                Style::default().fg(Color::Red),
            ),
        ]);
        let out = super::line(line, 16);
        assert!(out.len() > 1, "{:?}", text(&out));
        // The marker stays on the first line only, in its own colour.
        assert_eq!(out[0].spans[0].content.as_ref(), "❯ ");
        assert_eq!(out[0].spans[0].style.fg, Some(Color::Green));
        let joined = text(&out).join(" ");
        // **The words keep the description's colour.** The hang is spaces and carries the
        // marker's — spaces show no foreground, so nothing is repainted by them.
        for span in out.iter().flat_map(|l| &l.spans).filter(|s| !s.content.trim().is_empty()) {
            let first = span.content.as_ref() == "❯ ";
            assert_eq!(
                span.style.fg,
                Some(if first { Color::Green } else { Color::Red }),
                "{joined:?}"
            );
        }
        // Not one word went missing, and the marker did not come back on the next line.
        for word in "a description that is far too long to fit".split(' ') {
            assert!(joined.contains(word), "{word:?} went missing: {joined:?}");
        }
        assert_eq!(joined.matches('❯').count(), 1, "the marker was repeated: {joined:?}");
    }

    /// **A wrapped line hangs under the line it came from.** A list whose cursor row begins with
    /// `❯ ` and whose other rows begin with two spaces must wrap to the same shape — the marker
    /// stands in the same two columns as that margin, so it is measured the same way.
    #[test]
    fn a_wrapped_line_hangs_under_its_own_margin() {
        let long = "one two three four five six seven eight";
        let bordered = |margin: &'static str| {
            text(&super::line(
                Line::from(vec![
                    Span::styled(margin, Style::default().fg(Color::Green)),
                    Span::styled(long, Style::default().fg(Color::Red)),
                ]),
                14,
            ))
        };
        let marker = bordered("❯ ");
        let spaces = bordered("  ");
        assert!(marker.len() > 1, "nothing wrapped: {marker:?}");
        for line in &marker[1..] {
            assert!(line.starts_with("  "), "the wrapped part did not hang: {marker:?}");
        }
        assert_eq!(marker.len(), spaces.len(), "{marker:?} {spaces:?}");
        for (a, b) in marker.iter().zip(&spaces) {
            // **Columns, not bytes.** `❯` is three bytes wide and one column, and the whole point
            // of the margin is that the two shapes line up on screen.
            assert_eq!(
                display_width(a),
                display_width(b),
                "the two margins do not line up: {marker:?} {spaces:?}"
            );
        }
        // **The margin is inside the width, not on top of it.**
        assert!(marker.iter().all(|l| display_width(l) <= 14), "{marker:?}");
    }

    /// The break lands on a space when there is one, so words are not sliced in half for nothing.
    #[test]
    fn a_break_prefers_a_space() {
        let out = super::line(Line::from("alpha beta gamma"), 11);
        assert_eq!(text(&out), vec!["alpha beta", "gamma"]);
    }

    /// `columns` keeps every character, including a line break that was already there.
    ///
    /// The width is floored at [`MIN`], so a request narrower than that gets eight columns —
    /// never one column of single characters.
    #[test]
    fn columns_keeps_every_character() {
        let out = columns("abcdefghij\nsecond", 8);
        assert_eq!(out, vec!["abcdefgh", "ij", "second"]);
    }
}
