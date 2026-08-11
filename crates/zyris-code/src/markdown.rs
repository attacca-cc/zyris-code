//! Turns markdown into screen lines.
//!
//! `pulldown-cmark` does the parsing and **the layout is done by hand** — no crate gives
//! width-based wrapping + fullwidth-2-columns + code-fence borders in one go.
//!
//! **No partial parser for incomplete streaming markdown.** On every delta,
//! the accumulated string is reparsed whole. If the code fence isn't closed, pulldown-cmark
//! treats the rest as code, so an "open code block" just comes out naturally.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme;

/// The number of columns it occupies on screen. Fullwidth is 2 columns.
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// A link on one output line, in that line's display columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// First display column, inclusive.
    pub start: usize,
    /// Last display column, exclusive.
    pub end: usize,
    pub url: String,
}

/// What `render_rich` produced: the lines plus, per line, the links on it.
#[derive(Debug, Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub links: Vec<Vec<Link>>,
}

pub fn render(src: &str, width: u16) -> Vec<Line<'static>> {
    render_rich(src, width).lines
}

/// One span plus the link it belongs to, if any. `flush` needs the URL to record
/// link ranges as it wraps words into lines.
struct Piece {
    span: Span<'static>,
    url: Option<String>,
}

pub fn render_rich(src: &str, width: u16) -> Rendered {
    let owned;
    let src = match optimistic_table(src) {
        Some(fixed) => {
            owned = fixed;
            owned.as_str()
        }
        None => src,
    };
    let width = width.max(8) as usize;
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut out_links: Vec<Vec<Link>> = Vec::new();
    let mut buf: Vec<Piece> = Vec::new();
    let mut style = Style::default().fg(theme::text());
    let mut in_code = false;
    let mut list_depth: usize = 0;
    let mut table: Option<Table> = None;
    // The link currently being read. Its text carries this URL. `None` outside a link.
    let mut cur_link: Option<String> = None;
    // The style to restore when the current link ends. Links cannot nest (CommonMark),
    // so one slot is enough. Without it, a link inside a blockquote would leave the rest
    // of the quote in plain `TEXT`.
    let mut link_saved: Option<Style> = None;

    let parser = Parser::new_ext(src, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    for event in parser {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                style = Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD);
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(&mut out, &mut out_links, &mut buf, width, "");
                style = Style::default().fg(theme::text());
            }
            Event::Start(Tag::Emphasis) => style = style.add_modifier(Modifier::ITALIC),
            Event::End(TagEnd::Emphasis) => style = style.remove_modifier(Modifier::ITALIC),
            Event::Start(Tag::Strong) => {
                style = style.fg(theme::text_heading()).add_modifier(Modifier::BOLD)
            }
            Event::End(TagEnd::Strong) => style = Style::default().fg(theme::text()),
            Event::Start(Tag::BlockQuote(_)) => style = Style::default().fg(theme::text_muted()),
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(&mut out, &mut out_links, &mut buf, width, "│ ");
                style = Style::default().fg(theme::text());
            }
            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::End(TagEnd::List(_)) => list_depth = list_depth.saturating_sub(1),
            Event::Start(Tag::Item) => {
                buf.push(Piece {
                    span: Span::styled(
                        format!("{}· ", "  ".repeat(list_depth.saturating_sub(1))),
                        Style::default().fg(theme::accent()),
                    ),
                    url: None,
                });
            }
            Event::End(TagEnd::Item) => flush(&mut out, &mut out_links, &mut buf, width, "  "),
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(&mut out, &mut out_links, &mut buf, width, "");
                let lang = match &kind {
                    CodeBlockKind::Fenced(l) if !l.is_empty() => l.to_string(),
                    _ => String::new(),
                };
                out.push(Line::from(Span::styled(
                    format!("┌─ {lang} "),
                    Style::default().fg(theme::border_light()),
                )));
                out_links.push(Vec::new());
                in_code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                out.push(Line::from(Span::styled(
                    "└─",
                    Style::default().fg(theme::border_light()),
                )));
                out_links.push(Vec::new());
                in_code = false;
            }
            Event::Code(t) => {
                // In a table it's a cell; otherwise it goes to the body.
                if let Some(tb) = &mut table {
                    tb.cell.push_str(&t);
                } else {
                    buf.push(Piece {
                        span: Span::styled(t.to_string(), Style::default().fg(theme::accent())),
                        url: cur_link.clone(),
                    });
                }
            }
            Event::Text(t) if in_code => {
                for raw in t.lines() {
                    out.push(Line::from(vec![
                        Span::styled("│ ", Style::default().fg(theme::border_light())),
                        Span::styled(raw.to_string(), Style::default().fg(theme::text())),
                    ]));
                    out_links.push(Vec::new());
                }
            }
            // A link. Its text renders styled (underlined) and **its URL rides along** so the
            // drawing side can wrap the cells in OSC 8 — that is what makes Ctrl+click open it.
            Event::Start(Tag::Link { dest_url, .. }) => {
                cur_link = Some(dest_url.to_string());
                link_saved = Some(style);
                style = Style::default().fg(theme::link()).add_modifier(Modifier::UNDERLINED);
            }
            Event::End(TagEnd::Link) => {
                cur_link = None;
                style = link_saved.take().unwrap_or(Style::default().fg(theme::text()));
            }
            // --- table ---------------------------------------------------------
            // Cell text is collected, then drawn once with widths aligned when the table ends. Column
            // widths need every row, so they can't be drawn midway.
            Event::Start(Tag::Table(_)) => {
                flush(&mut out, &mut out_links, &mut buf, width, "");
                table = Some(Table::default());
            }
            Event::End(TagEnd::Table) => {
                if let Some(t) = table.take() {
                    for line in t.render(width) {
                        out.push(line);
                        out_links.push(Vec::new());
                    }
                }
            }
            Event::Start(Tag::TableHead) => {
                if let Some(t) = &mut table {
                    t.in_head = true;
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(t) = &mut table {
                    t.end_row();
                    t.in_head = false;
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(t) = &mut table {
                    t.end_row();
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(t) = &mut table {
                    t.end_cell();
                }
            }
            Event::Text(t) if table.is_some() => {
                if let Some(tb) = &mut table {
                    tb.cell.push_str(&t);
                }
            }
            Event::Text(t) => {
                buf.push(Piece { span: Span::styled(t.to_string(), style), url: cur_link.clone() })
            }
            Event::SoftBreak | Event::HardBreak => {
                buf.push(Piece { span: Span::styled(" ", style), url: cur_link.clone() })
            }
            Event::End(TagEnd::Paragraph) => flush(&mut out, &mut out_links, &mut buf, width, ""),
            Event::Rule => {
                out.push(Line::from(Span::styled(
                    "─".repeat(width),
                    Style::default().fg(theme::border()),
                )));
                out_links.push(Vec::new());
            }
            _ => {}
        }
    }
    flush(&mut out, &mut out_links, &mut buf, width, "");
    Rendered { lines: out, links: out_links }
}

/// **Pre-draws as a table** one whose delimiter row hasn't arrived yet during streaming.
///
/// Markdown can only know it's a table once the `|---|---|` line arrives. So until that line
/// the pipes show as plain text, then suddenly flip to a table — jarring in a streaming answer.
/// If only pipe-starting lines have piled up at the end, an invented delimiter is inserted to make it a table early.
///
/// If a real delimiter follows, this function does nothing and the original is parsed as is.
fn optimistic_table(src: &str) -> Option<String> {
    let joined;
    let src = match split_glued_rows(src) {
        Some(fixed) => {
            joined = fixed;
            joined.as_str()
        }
        None => src,
    };
    let lines: Vec<&str> = src.lines().collect();
    // Counts from the end how many lines start with a pipe.
    let start = lines.iter().rposition(|l| !l.trim_start().starts_with('|')).map_or(0, |i| i + 1);
    let block = &lines[start..];
    if block.is_empty() {
        return None;
    }

    let is_delim = |l: &str| {
        let t = l.trim().trim_matches('|');
        !t.is_empty() && t.chars().all(|c| matches!(c, '-' | ':' | '|' | ' '))
    };

    // The header is the first line that isn't a delimiter.
    let header = block[0];
    if is_delim(header) {
        return None; // Only a delimiter arrived. No grounds yet to treat it as a table.
    }
    // Before any text enters the first cell (`| `), there's nothing to draw. An empty table is worse.
    let inner = header.trim().trim_matches('|');
    if inner.trim().is_empty() {
        return None;
    }
    let cols = inner.split('|').count();

    // If the following delimiter **matches the header's column count**, leave it alone. If not (still arriving),
    // drop that line and insert a proper one — left as is, it falls out of the table into text and
    // flickers between table and plain text.
    let delim_ok = block
        .get(1)
        .is_some_and(|l| is_delim(l) && l.trim().trim_matches('|').split('|').count() == cols);

    // **A blank line must precede the table.** Glued right onto the previous sentence, pulldown-cmark
    // eats that pipe line as a paragraph continuation and it isn't a table — whether a blank line comes decides table↔text.
    let needs_gap = start > 0 && !lines[start - 1].trim().is_empty();

    if delim_ok && !needs_gap {
        return None;
    }

    let body: Vec<&str> = block[1..].iter().copied().filter(|l| !is_delim(l)).collect();
    let _ = &body;
    let delim = format!("|{}", "---|".repeat(cols));

    let mut out: Vec<String> = lines[..start].iter().map(|s| s.to_string()).collect();
    if needs_gap {
        out.push(String::new());
    }
    out.push(header.to_string());
    // If a real delimiter already came properly, use it.
    out.push(if delim_ok { block[1].to_string() } else { delim });
    out.extend(body.iter().map(|s| s.to_string()));
    Some(out.join("\n"))
}

/// Splits table rows that got glued onto one line.
///
/// Deltas sometimes deliver the newline late. Then two rows join onto one line like `| 가 | 1 || 나 | 2 |`,
/// and a very wide table with way too many columns is drawn — when the newline arrives it returns
/// to normal and repeats on the next row, making the table flicker.
///
/// A genuine empty cell is written with a space inside like `| |`, so a space-less `||` is taken as a glued spot.
fn split_glued_rows(src: &str) -> Option<String> {
    if !src.contains("||") {
        return None;
    }
    let mut changed = false;
    let out: Vec<String> = src
        .lines()
        .map(|line| {
            if !line.trim_start().starts_with('|') || !line.contains("||") {
                return line.to_string();
            }
            changed = true;
            line.split("||")
                .map(|part| {
                    let p = part.trim_end();
                    let p = p.strip_prefix('|').unwrap_or(p);
                    let p = p.strip_suffix('|').unwrap_or(p);
                    format!("|{p}|")
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    changed.then(|| out.join("\n"))
}

/// State held until one whole table is collected.
///
/// Column widths need **every row**, so it can't be drawn midway. Cell text is just accumulated and
/// drawn once at `End(Table)`.
#[derive(Default)]
struct Table {
    head: Vec<String>,
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    in_head: bool,
}

impl Table {
    fn end_cell(&mut self) {
        self.row.push(std::mem::take(&mut self.cell).trim().to_string());
    }

    fn end_row(&mut self) {
        let row = std::mem::take(&mut self.row);
        if row.is_empty() {
            return;
        }
        if self.in_head {
            self.head = row;
        } else {
            self.rows.push(row);
        }
    }

    /// Draws with aligned column widths. If width is short, columns shrink **proportionally**, and overflowing cells are cut with `…`.
    fn render(&self, width: usize) -> Vec<Line<'static>> {
        let cols = self.head.len().max(self.rows.iter().map(Vec::len).max().unwrap_or(0));
        if cols == 0 {
            return Vec::new();
        }

        // Each column's desired width.
        let mut w = vec![0usize; cols];
        for r in std::iter::once(&self.head).chain(self.rows.iter()) {
            for (i, cell) in r.iter().enumerate().take(cols) {
                w[i] = w[i].max(display_width(cell));
            }
        }

        // Columns eaten by separators and padding: "│ " per column + "│" at the end
        let chrome = cols * 3 + 1;
        let budget = width.saturating_sub(chrome).max(cols);
        let total: usize = w.iter().sum();
        if total > budget {
            // Shave from the widest column. Leave at least one column.
            let mut over = total - budget;
            while over > 0 {
                let Some(i) = (0..cols).max_by_key(|&i| w[i]) else {
                    break;
                };
                if w[i] <= 1 {
                    break;
                }
                w[i] -= 1;
                over -= 1;
            }
        }

        let mut out = Vec::new();
        let border = |l: &str, m: &str, r: &str, w: &[usize]| {
            let mid: Vec<String> = w.iter().map(|n| "─".repeat(n + 2)).collect();
            Line::from(Span::styled(
                format!("{l}{}{r}", mid.join(m)),
                Style::default().fg(theme::border_light()),
            ))
        };

        out.push(border("┌", "┬", "┐", &w));
        if !self.head.is_empty() {
            out.push(row_line(&self.head, &w, theme::text_heading(), true));
            out.push(border("├", "┼", "┤", &w));
        }
        for r in &self.rows {
            out.push(row_line(r, &w, theme::text(), false));
        }
        out.push(border("└", "┴", "┘", &w));
        out
    }
}

/// One row. Cells are cut to the width and padded right with spaces.
fn row_line(cells: &[String], w: &[usize], fg: ratatui::style::Color, bold: bool) -> Line<'static> {
    let mut spans = Vec::new();
    let bar = Style::default().fg(theme::border_light());
    let mut text = Style::default().fg(fg);
    if bold {
        text = text.add_modifier(Modifier::BOLD);
    }
    for (i, target) in w.iter().enumerate() {
        spans.push(Span::styled("│ ", bar));
        let cell = cells.get(i).map(String::as_str).unwrap_or("");
        let shown = truncate_to(cell, *target);
        let pad = target.saturating_sub(display_width(&shown));
        spans.push(Span::styled(shown, text));
        spans.push(Span::styled(" ".repeat(pad + 1), text));
    }
    spans.push(Span::styled("│", bar));
    Line::from(spans)
}

/// Cuts to not exceed the width. When cut, a `…` goes at the end — being cut must be visible.
///
/// Counted in **display columns**, so fullwidth text is cut where it actually reaches the limit
/// rather than where its bytes or chars run out.
pub fn truncate_to(s: &str, limit: usize) -> String {
    if display_width(s) <= limit {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        // Leave one column for the `…`.
        if display_width(&out) + display_width(&ch.to_string()) > limit.saturating_sub(1) {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// Folds accumulated spans into lines at the width. Fullwidth counts as 2 columns.
///
/// Along with each line it records the links on it — a word that carries a URL opens a
/// range at its column, and the range closes when the next word has a different URL (or none).
fn flush(
    out: &mut Vec<Line<'static>>,
    out_links: &mut Vec<Vec<Link>>,
    buf: &mut Vec<Piece>,
    width: usize,
    indent: &str,
) {
    if buf.is_empty() {
        return;
    }
    let indent_w = display_width(indent);
    let limit = width.saturating_sub(indent_w).max(1);
    let mut line: Vec<Span<'static>> = Vec::new();
    let mut links: Vec<Link> = Vec::new();
    let mut used = 0usize;
    // The link currently being placed on this line, and the column it started at.
    let mut open: Option<(usize, String)> = None;

    for piece in buf.drain(..) {
        for word in split_keeping_spaces(&piece.span.content) {
            let w = display_width(&word);
            if used + w > limit && used > 0 {
                // Close the open link at the end of this line; the next line reopens it.
                if let Some((start, url)) = open.take() {
                    links.push(Link { start, end: used, url });
                }
                out.push(Line::from(std::mem::take(&mut line)));
                out_links.push(std::mem::take(&mut links));
                used = 0;
                // Leading spaces carried onto a new line are dropped — they'd look like indentation.
                if word.trim().is_empty() {
                    continue;
                }
            }
            match (&open, &piece.url) {
                // Same link continues. Nothing to record.
                (Some((_, u)), Some(wurl)) if u == wurl => {}
                // Opening a link at this word's column.
                (None, Some(wurl)) => open = Some((used, wurl.clone())),
                // A different link starts — close the previous one first.
                (Some(_), Some(wurl)) => {
                    if let Some((start, url)) = open.take() {
                        links.push(Link { start, end: used, url });
                    }
                    open = Some((used, wurl.clone()));
                }
                // Non-link text closes any open link.
                (_, None) => {
                    if let Some((start, url)) = open.take() {
                        links.push(Link { start, end: used, url });
                    }
                }
            }
            used += w;
            line.push(Span::styled(word, piece.span.style));
        }
    }
    if let Some((start, url)) = open.take() {
        links.push(Link { start, end: used, url });
    }
    if !line.is_empty() {
        out.push(Line::from(line));
        out_links.push(links);
    }
}

/// Splits by word without dropping spaces — dropping them glues sentences together.
fn split_keeping_spaces(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch == ' ' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.push(" ".to_string());
        } else {
            cur.push(ch);
            // Fullwidth has no word boundaries — it must be cut per character to stay within width.
            if ch.len_utf8() > 1 && display_width(&cur) >= 2 {
                out.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    /// A link's URL rides along so the drawing side can wrap the cells in OSC 8.
    #[test]
    fn a_link_records_its_range_and_url() {
        let r = render_rich("[문서](https://example.com/x) 끝", 40);
        let text = plain(&r.lines).join("\n");
        assert!(text.contains("문서"), "{text:?}");
        let links = &r.links[0];
        assert_eq!(links.len(), 1, "{links:?}");
        let l = &links[0];
        assert_eq!(l.url, "https://example.com/x");
        // The range is in display columns; walk the text by display width to compare.
        let mut col = 0usize;
        let mut covered = String::new();
        for ch in text.chars() {
            let w = display_width(&ch.to_string());
            if col >= l.start && col + w <= l.end {
                covered.push(ch);
            }
            col += w;
        }
        assert_eq!(covered, "문서", "the range must cover the link text");
    }

    /// Two links on one line are separate ranges with their own URLs.
    #[test]
    fn two_links_on_one_line_are_separate_ranges() {
        let r = render_rich("[a](https://a) [b](https://b)", 40);
        let links = &r.links[0];
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].url, "https://a");
        assert_eq!(links[1].url, "https://b");
        assert!(links[0].end <= links[1].start);
    }

    /// A link split across a wrap still marks both parts.
    #[test]
    fn a_link_wrapping_over_lines_marks_each_line() {
        let r = render_rich("[abcdefghijklmnop](https://x)", 8);
        let ranges: Vec<usize> = r.links.iter().map(|l| l.len()).collect();
        assert!(ranges.iter().any(|&n| n > 0), "no line carries the link: {ranges:?}");
        for links in &r.links {
            for l in links {
                assert_eq!(l.url, "https://x");
            }
        }
    }

    /// A bare URL in plain text is not a link — only `[text](url)` and autolinks are.
    #[test]
    fn a_bare_url_in_plain_text_is_not_a_link() {
        let r = render_rich("가 https://example.com 나", 40);
        assert!(r.links.iter().all(|l| l.is_empty()), "{r:?}");
    }

    /// Fullwidth is 2 columns. Counting by bytes or chars goes wrong for Korean.
    #[test]
    fn korean_counts_as_two_columns() {
        assert_eq!(display_width("한글"), 4);
        assert_eq!(display_width("ab"), 2);
        assert_eq!(display_width("한a"), 3);
    }

    #[test]
    fn a_paragraph_wraps_at_the_given_width() {
        let out = plain(&render("hello world foo bar", 11));
        assert_eq!(out, vec!["hello world", "foo bar"]);
    }

    /// A Korean paragraph must never exceed the width.
    #[test]
    fn a_korean_paragraph_never_exceeds_the_width() {
        for line in render("가나다라마바사아자차카타파하", 10) {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(display_width(&text) <= 10, "it exceeded the width: {text:?}");
        }
    }

    /// During streaming the code fence isn't closed yet. It must still render as code.
    #[test]
    fn an_unclosed_code_fence_still_renders_as_code() {
        let out = plain(&render("```rust\nfn main() {\n", 40));
        assert!(
            out.iter().any(|l| l.contains("fn main() {")),
            "the code body must be visible: {out:?}"
        );
        assert!(
            out.iter().any(|l| l.contains("rust")),
            "the language label must be visible: {out:?}"
        );
    }

    /// A span without a colour lets the terminal's default foreground bleed through.
    #[test]
    fn every_span_has_a_foreground_colour() {
        let lines = render("# 제목\n\n본문 **강조** 와 `코드`\n\n- 목록", 40);
        for line in &lines {
            for span in &line.spans {
                assert!(span.style.fg.is_some(), "a span with no colour: {:?}", span.content);
            }
        }
    }

    const TABLE: &str = "\
| 이름 | 값 |
|---|---|
| 가나다 | 1 |
| ab | 22 |";

    #[test]
    fn a_table_renders_every_cell() {
        let out = plain(&render(TABLE, 40));
        let all = out.join("\n");
        for cell in ["이름", "값", "가나다", "1", "ab", "22"] {
            assert!(all.contains(cell), "{cell:?} is missing:\n{all}");
        }
    }

    /// Columns must line up vertically to read as a table. Without fullwidth widths, Korean columns drift.
    #[test]
    fn table_columns_line_up_even_with_wide_characters() {
        let out = plain(&render(TABLE, 40));
        let rows: Vec<&String> = out.iter().filter(|l| l.contains('│')).collect();
        assert!(rows.len() >= 3, "the table is missing rows: {out:?}");

        let cols: Vec<Vec<usize>> = rows
            .iter()
            .map(|l| {
                let mut acc = Vec::new();
                let mut w = 0;
                for ch in l.chars() {
                    if ch == '│' {
                        acc.push(w);
                    }
                    w += display_width(&ch.to_string());
                }
                acc
            })
            .collect();
        for c in &cols {
            assert_eq!(
                c,
                &cols[0],
                "the separator sits at a different column per row:\n{}",
                out.join("\n")
            );
        }
    }

    #[test]
    fn a_table_never_exceeds_the_width() {
        for line in render(TABLE, 24) {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(display_width(&text) <= 24, "it exceeded the width: {text:?}");
        }
    }

    /// It must render as a table before the delimiter arrives — otherwise the pipes show as text, then
    /// suddenly flip to a table, jarring in a streaming answer.
    #[test]
    fn a_table_renders_before_its_delimiter_row_arrives() {
        let out = plain(&render("| 이름 | 값 |", 30));
        assert!(out.iter().any(|l| l.contains('┌')), "the table has no border: {out:?}");
        assert!(out.iter().any(|l| l.contains("이름")), "{out:?}");
    }

    /// Each new row must attach to the table immediately.
    #[test]
    fn streaming_rows_appear_one_by_one() {
        let one = plain(&render("| 이름 | 값 |\n|---|---|\n| 가 | 1 |", 30));
        let two = plain(&render("| 이름 | 값 |\n|---|---|\n| 가 | 1 |\n| 나 | 2 |", 30));
        assert!(one.iter().any(|l| l.contains('가')));
        assert!(!one.iter().any(|l| l.contains('나')), "a row that has not arrived is visible");
        assert!(two.iter().any(|l| l.contains('나')), "the new row was not appended");
    }

    /// A real delimiter row is not invented around.
    #[test]
    fn a_real_delimiter_row_is_left_alone() {
        let md = "| 이름 | 값 |\n|---|---|\n| 가 | 1 |";
        let out = plain(&render(md, 30));
        let borders = out.iter().filter(|l| l.contains('├')).count();
        assert_eq!(borders, 1, "the separator row was doubled: {out:?}");
    }

    /// Ordinary text with a single pipe is not a table.
    #[test]
    fn a_lone_pipe_is_not_a_table() {
        let out = plain(&render("a | b 는 논리 연산", 30));
        assert!(!out.iter().any(|l| l.contains('┌')), "{out:?}");
    }

    /// **It must be a table the whole time it streams in character by character.** Falling to plain text midway makes
    /// it flicker between table and text — that's really what was seen.
    #[test]
    fn a_streaming_table_never_falls_back_to_plain_text() {
        let full = "| 이름 | 값 |\n|---|---|\n| 가 | 1 |\n| 나 | 2 |";
        let chars: Vec<char> = full.chars().collect();
        // From after the first pipe and one character, it must stay a table.
        for n in 4..=chars.len() {
            let partial: String = chars[..n].iter().collect();
            let out: Vec<String> = render(&partial, 30)
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            assert!(
                out.iter().any(|l| l.contains('┌') || l.contains('│')),
                "{n}자에서 평문으로 떨어졌다:\n{}",
                out.join("\n")
            );
        }
    }

    /// A half-written delimiter row must keep the table.
    #[test]
    fn a_half_written_delimiter_row_keeps_the_table() {
        for partial in ["| 이름 | 값 |\n|", "| 이름 | 값 |\n|--", "| 이름 | 값 |\n|---|"] {
            let out: Vec<String> = render(partial, 30)
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            assert!(
                out.iter().any(|l| l.contains('┌')),
                "{partial:?} did not render as a table: {out:?}"
            );
        }
    }

    /// **A table glued right onto the previous line must still be a table.** Without a blank line, the parser eats
    /// it as a paragraph continuation — whether a blank line comes made it flicker between table and text.
    #[test]
    fn a_table_glued_to_the_previous_line_still_renders() {
        let out = plain(&render("다음은 표입니다:\n| 이름 | 값 |\n|---|---|\n| 가 | 1 |", 40));
        assert!(out.iter().any(|l| l.contains('┌')), "not a table:\n{}", out.join("\n"));
        assert!(out.iter().any(|l| l.contains("다음은 표입니다")), "the earlier text disappeared");
    }

    /// Even streaming in character by character with preceding text attached, it must stay a table.
    #[test]
    fn a_glued_streaming_table_never_flickers() {
        let full = "설명:\n| 이름 | 값 |\n|---|---|\n| 가 | 1 |\n| 나 | 2 |";
        let chars: Vec<char> = full.chars().collect();
        // From after text enters the first cell — before that there's nothing to draw as a table.
        let head = "설명:\n| 이".chars().count();
        for n in head..=chars.len() {
            let partial: String = chars[..n].iter().collect();
            let out: Vec<String> = render(&partial, 40)
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            assert!(
                out.iter().any(|l| l.contains('┌') || l.contains('│')),
                "{n}자에서 표가 아니다:\n{}",
                out.join("\n")
            );
        }
    }

    /// **Rows glued onto one line must not inflate the columns.**
    ///
    /// When a delta delivers the newline late, two rows join onto one line. Left as is, the table gets
    /// a very wide set of columns, and when the newline arrives it returns to normal and flickers.
    #[test]
    fn rows_glued_onto_one_line_are_split_apart() {
        let out = plain(&render("설명:\n\n| 이름 | 값 || 가 | 1 |", 44));
        let cols =
            out.iter().find(|l| l.contains('┌')).map(|l| l.matches('┬').count()).expect("no table");
        assert_eq!(cols, 1, "the columns grew (separator count {cols}):\n{}", out.join("\n"));
        assert!(out.iter().any(|l| l.contains("가")), "a row that had arrived disappeared");
    }

    /// A genuine empty cell with a space inside is left alone.
    #[test]
    fn a_genuine_empty_cell_is_left_alone() {
        let out = plain(&render("| a | b |\n|---|---|\n| 1 | |", 30));
        assert!(out.iter().any(|l| l.contains('1')), "{out:?}");
        let rows = out.iter().filter(|l| l.starts_with('│')).count();
        assert_eq!(rows, 2, "the row was split: {out:?}");
    }

    #[test]
    fn a_bullet_list_is_marked() {
        let out = plain(&render("- 첫째\n- 둘째", 40));
        assert_eq!(out.len(), 2);
        assert!(out[0].contains("첫째"));
        assert!(out[0].trim_start().starts_with('·') || out[0].trim_start().starts_with('-'));
    }
}
