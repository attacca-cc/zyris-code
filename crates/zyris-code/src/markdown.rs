//! Turns markdown into screen lines.
//!
//! `pulldown-cmark` does the parsing and **the layout is done by hand** — no crate gives
//! width-based wrapping + fullwidth-2-columns + code-fence borders in one go.
//!
//! **No partial parser for incomplete streaming markdown.** On every delta,
//! the accumulated string is reparsed whole. If the code fence isn't closed, pulldown-cmark
//! treats the rest as code, so an "open code block" just comes out naturally.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::buffer::CellWidth;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme;

/// The number of columns it occupies on screen. Fullwidth is 2 columns.
///
/// **Measured the way ratatui draws it**: by grapheme cluster, with ratatui's own cell width. It
/// used to be counted by `char`, and wrapping, the cursor and selection each forced every `char` to
/// at least one column — while the buffer puts a whole cluster in one cell. NFD Hangul (`ᄒ ᅡ ᆫ`,
/// how macOS spells file names) counted four columns and drew two, and an emoji with a
/// skin tone or a joiner counted twice what it drew, so lines wrapped early and the cursor stood
/// off to the right of what was typed. A cluster holding a control character counts nothing,
/// because the buffer drops it.
pub fn display_width(s: &str) -> usize {
    // Printable ASCII is one column a byte, and it is most of what is measured.
    if s.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return s.len();
    }
    s.graphemes(true).map(cluster_width).sum()
}

/// One cluster's columns — see [`display_width`].
fn cluster_width(cluster: &str) -> usize {
    if cluster.contains(char::is_control) {
        0
    } else {
        cluster.cell_width() as usize
    }
}

/// `url` if it is safe to hand to the terminal (OSC 8) and to the OS opener, else `None`.
///
/// **A link's destination is text we did not write** — the agent's answer, and through it any web
/// page or file it read. Two things are refused:
///
/// - **Any control character.** The URL goes inside an OSC 8 sequence, and an `ESC \` in it ends
///   the hyperlink early and hands the rest to the terminal as commands: a clipboard write, a
///   title, anything. CommonMark's `<...>` form lets such bytes through.
/// - **Any scheme but `http`, `https` and `mailto`.** Ctrl+click hands the URL to `open`,
///   `xdg-open` or the Windows shell, and `file:`, `javascript:`, `ms-msdt:` and a bare relative
///   path all mean something to those.
///
/// A refused link keeps its text; it is only no longer a link.
pub fn safe_url(url: &str) -> Option<&str> {
    if url.chars().any(char::is_control) {
        return None;
    }
    let scheme = url.split_once(':')?.0.to_ascii_lowercase();
    matches!(scheme.as_str(), "http" | "https" | "mailto").then_some(url)
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

/// What `render_rich` produced: the lines plus, per line, the links on it and **how much of the
/// line's front the renderer drew**.
///
/// `prefix` is the gutter this module adds — the rule down a code block's left (`│ `), the margin a
/// wrapped line hangs under — and nothing else. It exists because the caller prints its own margin
/// in front (`rows::PAD`, a marker) and the selection has to know where the author's text starts:
/// counting it as each is drawn beats reading the drawn characters back, which cannot tell a
/// renderer's indent from text somebody indented by hand (2026-09-15 report).
#[derive(Debug, Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub links: Vec<Vec<Link>>,
    pub prefix: Vec<u16>,
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
    // The room there is, however little: widened to a floor, every line of a narrow pane ran past
    // its edge and was cut.
    let width = width.max(1) as usize;
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut out_links: Vec<Vec<Link>> = Vec::new();
    // Where each line's own text starts, parallel to `out` (see [`Rendered::prefix`]).
    let mut out_prefix: Vec<u16> = Vec::new();
    let mut buf: Vec<Piece> = Vec::new();
    let mut style = Style::default().fg(theme::text());
    // **What each open inline or block tag found, restored when it closes.** `End(Strong)` used to
    // reset to plain text, so the rest of a heading lost its weight and the rest of a quote its
    // colour after the first bold word.
    let mut styles: Vec<Style> = Vec::new();
    let mut in_code = false;
    // The next number of each open list, innermost last; `None` for a bulleted one.
    let mut lists: Vec<Option<u64>> = Vec::new();
    // The indent of each open item, innermost last — the margin its own text hangs at.
    let mut items: Vec<String> = Vec::new();
    // **The indent a list item's own text sits at**, so a wrapped line stays inside the item it
    // belongs to. It is set when the item opens and handed to every paragraph of that item; the
    // first paragraph's first line does not take it, because the bullet is already standing there.
    let mut item_indent = String::new();
    let mut item_first_para = true;
    // How many block quotes are open. Each draws a `│ ` down the left of every line inside it.
    let mut quote = 0usize;
    let mut table: Option<Table> = None;
    // The link currently being read. Its text carries this URL. `None` outside a link.
    let mut cur_link: Option<String> = None;

    // Ends the buffered text with the item margin `$indent` on wrapped lines — and on the first
    // one too when `$first` — inside the bars of any quote it sits in.
    macro_rules! flush_at {
        ($indent:expr, $first:expr) => {{
            let bars = "│ ".repeat(quote);
            let indent: &str = $indent;
            let first = if $first { format!("{bars}{indent}") } else { bars.clone() };
            flush(
                &mut out,
                &mut out_links,
                &mut out_prefix,
                &mut buf,
                width,
                &format!("{bars}{indent}"),
                &first,
            )
        }};
    }

    let parser = Parser::new_ext(src, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    for event in parser {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                styles.push(style);
                style = Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD);
            }
            Event::End(TagEnd::Heading(_)) => {
                flush_at!("", false);
                style = styles.pop().unwrap_or(style);
            }
            Event::Start(Tag::Emphasis) => {
                styles.push(style);
                style = style.add_modifier(Modifier::ITALIC);
            }
            Event::Start(Tag::Strong) => {
                styles.push(style);
                style = style.fg(theme::text_heading()).add_modifier(Modifier::BOLD);
            }
            // **Struck through and dimmed**: the strike alone is dropped by the Linux console and
            // several multiplexers, and an old value would then read as the current one.
            Event::Start(Tag::Strikethrough) => {
                styles.push(style);
                style = style.fg(theme::text_muted()).add_modifier(Modifier::CROSSED_OUT);
            }
            Event::End(TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough) => {
                style = styles.pop().unwrap_or(style);
            }
            // **A quote wears a bar down its left**, not only a dimmer colour — muted text alone
            // read as ordinary prose.
            Event::Start(Tag::BlockQuote(_)) => {
                flush_at!(&item_indent, false);
                styles.push(style);
                style = Style::default().fg(theme::text_muted());
                quote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush_at!("", false);
                quote = quote.saturating_sub(1);
                style = styles.pop().unwrap_or(style);
            }
            Event::Start(Tag::List(start)) => {
                // **A nested list starts on its own line.** A tight list has no paragraph to end
                // the line, so without this the inner bullet is glued onto the end of the outer
                // item's sentence: `∙ 바깥  ∙ 안쪽`. The flush carries the outer item's margin,
                // which is what the line it closes belongs to.
                flush_at!(&item_indent, false);
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                // **An ordered list keeps its numbers**: "see step 3" has to find a 3.
                let marker = match lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}. ", *n - 1)
                    }
                    _ => "∙ ".to_string(),
                };
                let base = items.last().cloned().unwrap_or_default();
                buf.push(Piece {
                    span: Span::styled(
                        format!("{base}{marker}"),
                        Style::default().fg(theme::accent()),
                    ),
                    url: None,
                });
                // **The marker and its text are one margin wide**, so the item's own indent is the
                // whole of it: the enclosing item's, plus what the marker fills.
                item_indent = format!("{base}{}", " ".repeat(display_width(&marker)));
                items.push(item_indent.clone());
                item_first_para = true;
            }
            Event::End(TagEnd::Item) => {
                // **A tight list item ends here and nowhere else.** With no paragraph of its own
                // (that is what "tight" means) the item's text is still in the buffer, and this is
                // the only flush it gets — so this is where its margin has to be passed.
                flush_at!(&item_indent, false);
                // **Back out one level, not to nothing.** An item inside an item leaves the outer
                // item's margin behind it, which is where its own remaining text belongs.
                items.pop();
                item_indent = items.last().cloned().unwrap_or_default();
                item_first_para = false;
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                flush_at!("", false);
                let lang = match &kind {
                    CodeBlockKind::Fenced(l) if !l.is_empty() => l.to_string(),
                    _ => String::new(),
                };
                out.push(Line::from(Span::styled(
                    truncate_to(&format!("┌─ {lang} "), width),
                    Style::default().fg(theme::border_light()),
                )));
                out_links.push(Vec::new());
                // The frame is dropped from a selection whole (`selection::is_code_fence`).
                out_prefix.push(0);
                in_code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                out.push(Line::from(Span::styled(
                    "└─",
                    Style::default().fg(theme::border_light()),
                )));
                out_links.push(Vec::new());
                out_prefix.push(0);
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
            // **A code line wider than the screen wraps by column** under a dotted rule, which
            // says the row goes on from the one above. It used to run past the edge and be cut
            // with no sign, and a drag copied the cut code. The wrapped rows are recorded as
            // joined, so a copy gives the line back whole. Tabs become spaces first: the screen
            // draws a tab as nothing.
            Event::Text(t) if in_code => {
                let gutter = display_width("│ ");
                for raw in crate::wrap::expand_tabs(&t).lines() {
                    let mut parts = crate::wrap::columns(raw, width.saturating_sub(gutter));
                    // A blank line of code is still a line of the block.
                    if parts.is_empty() {
                        parts.push(String::new());
                    }
                    for (i, part) in parts.into_iter().enumerate() {
                        out.push(Line::from(vec![
                            Span::styled(
                                if i == 0 { "│ " } else { "┊ " },
                                Style::default().fg(theme::border_light()),
                            ),
                            Span::styled(part, Style::default().fg(theme::text())),
                        ]));
                        out_links.push(Vec::new());
                        // The rule and the space after it; the code's own indent follows.
                        let joined = if i == 0 { 0 } else { crate::selection::JOINED };
                        out_prefix.push(gutter as u16 | joined);
                    }
                }
            }
            // A link. Its text renders styled (underlined) and **its URL rides along** so the
            // drawing side can wrap the cells in OSC 8 — that is what makes Ctrl+click open it.
            Event::Start(Tag::Link { dest_url, .. }) => {
                cur_link = safe_url(&dest_url).map(str::to_string);
                // Restored when the link ends, so a link inside a quote leaves the rest of the
                // quote in the quote's colour.
                styles.push(style);
                // A refused destination (`safe_url`) leaves plain text, not a link that does
                // nothing when clicked.
                if cur_link.is_some() {
                    style = Style::default().fg(theme::link()).add_modifier(Modifier::UNDERLINED);
                }
            }
            Event::End(TagEnd::Link) => {
                cur_link = None;
                style = styles.pop().unwrap_or(style);
            }
            // **HTML is text to a terminal reader.** pulldown-cmark reads `<String>`, `<T>`,
            // `<path>` — any `<word>` in prose — as inline HTML, and dropping it turned "use
            // Vec<String> here" into "use Vec here" without a sign anything was missing.
            Event::InlineHtml(t) => match &mut table {
                Some(tb) => tb.cell.push_str(&t),
                None => buf.push(Piece { span: Span::styled(t.to_string(), style), url: None }),
            },
            Event::Html(t) => {
                for raw in t.lines() {
                    buf.push(Piece { span: Span::styled(raw.to_string(), style), url: None });
                    flush_at!(&item_indent, !item_first_para);
                }
            }
            // --- table ---------------------------------------------------------
            // Cell text is collected, then drawn once with widths aligned when the table ends. Column
            // widths need every row, so they can't be drawn midway.
            Event::Start(Tag::Table(_)) => {
                flush_at!("", false);
                table = Some(Table::default());
            }
            Event::End(TagEnd::Table) => {
                if let Some(t) = table.take() {
                    for line in t.render(width) {
                        out.push(line);
                        out_links.push(Vec::new());
                        // A table's border is part of it and stays selectable.
                        out_prefix.push(0);
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
            Event::End(TagEnd::Paragraph) => {
                // **A list item's paragraph carries the item's margin into its wrapped lines.** A
                // second paragraph of the same item is indented from its own first line, because
                // the bullet is not in front of it — the item's first paragraph is the one line the
                // bullet stands on.
                flush_at!(&item_indent, !item_first_para);
                item_first_para = false;
            }
            Event::Rule => {
                out.push(Line::from(Span::styled(
                    "─".repeat(width),
                    Style::default().fg(theme::border()),
                )));
                out_links.push(Vec::new());
                out_prefix.push(0);
            }
            _ => {}
        }
    }
    flush_at!("", false);
    Rendered { lines: out, links: out_links, prefix: out_prefix }
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
    // **Pipes inside a code fence are code**: a shell line `| sort` at the end of a streaming
    // snippet got an invented `|---|` row under it.
    if block.is_empty() || fenced(&lines[..start]).is_some() {
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
    // The fence the line is inside, if any. `| grep x || true` in a code block is code, and it
    // was split into two lines for good.
    let mut fence: Option<char> = None;
    let out: Vec<String> = src
        .lines()
        .map(|line| {
            fence = step_fence(fence, line);
            if fence.is_some() || !line.trim_start().starts_with('|') || !line.contains("||") {
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

/// The fence open after `line`, given the one open before it: a line of three backticks or
/// tildes opens one, and a line of the same character closes it.
fn step_fence(open: Option<char>, line: &str) -> Option<char> {
    let t = line.trim_start();
    let mark = ['`', '~'].into_iter().find(|c| t.starts_with(&c.to_string().repeat(3)));
    match (open, mark) {
        (None, Some(c)) => Some(c),
        (Some(o), Some(c)) if o == c => None,
        (open, _) => open,
    }
}

/// The fence still open at the end of `lines`, if any.
fn fenced(lines: &[&str]) -> Option<char> {
    lines.iter().fold(None, |open, line| step_fence(open, line))
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
        // **Too narrow for even one column a cell**, the grid would be cut at the right border
        // with whole cells behind it. Each row is written out as a short list instead.
        if width < chrome + cols {
            return self.as_list(width);
        }
        let budget = width.saturating_sub(chrome);
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
            out.extend(row_lines(&self.head, &w, theme::text_heading(), true));
            out.push(border("├", "┼", "┤", &w));
        }
        for r in &self.rows {
            out.extend(row_lines(r, &w, theme::text(), false));
        }
        out.push(border("└", "┴", "┘", &w));
        out
    }

    /// Every row as a few lines of `head: cell`, for a width the grid cannot fit in.
    fn as_list(&self, width: usize) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                let mut spans = vec![Span::styled(
                    if i == 0 { "∙ " } else { "  " },
                    Style::default().fg(theme::accent()),
                )];
                if let Some(head) = self.head.get(i).filter(|h| !h.is_empty()) {
                    spans.push(Span::styled(
                        format!("{head}: "),
                        Style::default().fg(theme::text_heading()),
                    ));
                }
                spans.push(Span::styled(cell.clone(), Style::default().fg(theme::text())));
                out.extend(crate::wrap::line(Line::from(spans), width));
            }
        }
        out
    }
}

/// One row, as many lines as its tallest cell. **A cell wraps inside its column** rather than being
/// cut with `…`: the table is the only copy of what the cell says, and a drag copies the screen.
fn row_lines(
    cells: &[String],
    w: &[usize],
    fg: ratatui::style::Color,
    bold: bool,
) -> Vec<Line<'static>> {
    let bar = Style::default().fg(theme::border_light());
    let mut text = Style::default().fg(fg);
    if bold {
        text = text.add_modifier(Modifier::BOLD);
    }
    let wrapped: Vec<Vec<String>> = w
        .iter()
        .enumerate()
        .map(|(i, target)| {
            crate::wrap::words(cells.get(i).map(String::as_str).unwrap_or(""), *target)
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(0).max(1);
    (0..height)
        .map(|k| {
            let mut spans = Vec::new();
            for (target, lines) in w.iter().zip(&wrapped) {
                spans.push(Span::styled("│ ", bar));
                let shown = lines.get(k).cloned().unwrap_or_default();
                let pad = target.saturating_sub(display_width(&shown));
                spans.push(Span::styled(shown, text));
                spans.push(Span::styled(" ".repeat(pad + 1), text));
            }
            spans.push(Span::styled("│", bar));
            Line::from(spans)
        })
        .collect()
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
    for g in s.graphemes(true) {
        // Leave one column for the `…`.
        if display_width(&out) + display_width(g) > limit.saturating_sub(1) {
            break;
        }
        out.push_str(g);
    }
    out.push('…');
    out
}

/// Folds accumulated spans into lines at the width. Fullwidth counts as 2 columns.
///
/// Along with each line it records the links on it — a word that carries a URL opens a
/// range at its column, and the range closes when the next word has a different URL (or none).
///
/// **`indent` is the margin of what was wrapped.** It opens every line but the first, which opens
/// with `first`. A list item's first line already carries its bullet, which fills exactly that
/// margin, so it takes only a quote's bars there; a second paragraph of the same item carries
/// nothing, so its own first line takes the whole margin too. Before this, a wrapped line started
/// at column zero and stepped out of the list it belonged to. `width` is the whole room and the
/// margin is counted inside it, so nothing is drawn past the edge.
fn flush(
    out: &mut Vec<Line<'static>>,
    out_links: &mut Vec<Vec<Link>>,
    out_prefix: &mut Vec<u16>,
    buf: &mut Vec<Piece>,
    width: usize,
    indent: &str,
    first: &str,
) {
    if buf.is_empty() {
        return;
    }
    // **The margin comes out of the width, not on top of it.** A line that fits is a line that
    // fits with its indent.
    let limit = width.max(1);
    let mut f = Fill {
        out,
        out_links,
        out_prefix,
        indent,
        indent_w: display_width(indent),
        line: Vec::new(),
        links: Vec::new(),
        used: 0,
        margin_w: 0,
        words: 0,
        open: None,
        joined: false,
    };
    f.open_with(first);
    // The widest word a fresh line can take whole.
    let room = limit.saturating_sub(f.indent_w).max(1);

    for piece in buf.drain(..) {
        let style = piece.span.style;
        for word in split_keeping_spaces(&piece.span.content) {
            let w = display_width(&word);
            let blank = word.trim().is_empty();
            // **A word wider than any line is filled by column**, the way `wrap::words` does it.
            // Kept whole, a long URL, path or hash ran past the right edge and ratatui cut it with
            // no mark — and a drag copied only the part left on screen. Each break inside it is
            // recorded as joined, so a copy puts it back together.
            if w > room && !blank {
                if f.words > 0 {
                    f.break_line(false);
                }
                let mut chunk = String::new();
                let mut cw = 0usize;
                for g in word.graphemes(true) {
                    let gw = display_width(g);
                    if f.used + cw + gw > limit && cw > 0 {
                        f.place(std::mem::take(&mut chunk), cw, style, &piece.url);
                        cw = 0;
                        f.break_line(true);
                    }
                    chunk.push_str(g);
                    cw += gw;
                }
                f.place(chunk, cw, style, &piece.url);
                continue;
            }
            if f.used + w > limit && f.words > 0 {
                f.break_line(false);
                // Leading spaces carried onto a new line are dropped — they'd look like indentation.
                if blank {
                    continue;
                }
            }
            f.place(word, w, style, &piece.url);
        }
    }
    f.finish();
}

/// One run of [`flush`]: the line being filled and where the finished ones go.
struct Fill<'a> {
    out: &'a mut Vec<Line<'static>>,
    out_links: &'a mut Vec<Vec<Link>>,
    out_prefix: &'a mut Vec<u16>,
    indent: &'a str,
    indent_w: usize,
    line: Vec<Span<'static>>,
    links: Vec<Link>,
    used: usize,
    /// **How much of this line's front is the margin**, which is what the caller records the
    /// line's own text starting after (see [`Rendered::prefix`]). Zero when no margin was drawn.
    margin_w: usize,
    /// **Words, not columns.** A line holding only the margin is not a line: a break must not be
    /// decided on one, or the first word of every item would be pushed a line down on its own.
    words: usize,
    /// The link currently being placed on this line, and the column it started at.
    open: Option<(usize, String)>,
    /// Whether this line continues the one above in the middle of a word
    /// (`selection::JOINED`).
    joined: bool,
}

impl Fill<'_> {
    /// **The margin opens a line.** Without it the wrapped part would sit against the left edge
    /// it was told to stay away from.
    fn margin(&mut self) {
        self.open_with(self.indent);
    }

    /// Opens the line with `margin`, which the selection counts as layout, not text.
    fn open_with(&mut self, margin: &str) {
        let w = display_width(margin);
        if w > 0 {
            self.line
                .push(Span::styled(margin.to_string(), Style::default().fg(theme::border_light())));
            self.used = w;
            self.margin_w = w;
        }
    }

    /// Closes the link open on this line at the column reached so far.
    fn close_link(&mut self) {
        if let Some((start, url)) = self.open.take() {
            self.links.push(Link { start, end: self.used, url });
        }
    }

    /// Ends this line and opens the next with the margin. `joined` says the next one carries on
    /// in the middle of a word.
    fn break_line(&mut self, joined: bool) {
        // Close the open link at the end of this line; the next word reopens it.
        self.close_link();
        self.emit();
        self.joined = joined;
        self.margin();
    }

    fn emit(&mut self) {
        self.out.push(Line::from(std::mem::take(&mut self.line)));
        self.out_links.push(std::mem::take(&mut self.links));
        let joined = if self.joined { crate::selection::JOINED } else { 0 };
        self.out_prefix.push(self.margin_w as u16 | joined);
        self.used = 0;
        self.words = 0;
        self.margin_w = 0;
        self.joined = false;
    }

    /// Puts `text`, `w` columns wide, at the end of this line.
    fn place(&mut self, text: String, w: usize, style: Style, url: &Option<String>) {
        match (&self.open, url) {
            // Same link continues. Nothing to record.
            (Some((_, u)), Some(wurl)) if u == wurl => {}
            // Opening a link at this word's column.
            (None, Some(wurl)) => self.open = Some((self.used, wurl.clone())),
            // A different link starts — close the previous one first.
            (Some(_), Some(wurl)) => {
                self.close_link();
                self.open = Some((self.used, wurl.clone()));
            }
            // Non-link text closes any open link.
            (_, None) => self.close_link(),
        }
        self.used += w;
        if !text.trim().is_empty() {
            self.words += 1;
        }
        self.line.push(Span::styled(text, style));
    }

    fn finish(mut self) {
        self.close_link();
        if !self.line.is_empty() {
            self.emit();
        }
    }
}

/// Splits by word without dropping spaces — dropping them glues sentences together.
fn split_keeping_spaces(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    // By cluster, so a cut never falls between a letter and its mark or inside an emoji sequence.
    for g in s.graphemes(true) {
        if g == " " {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.push(" ".to_string());
        } else {
            cur.push_str(g);
            // Fullwidth has no word boundaries — it must be cut per character to stay within width.
            if g.len() > 1 && display_width(&cur) >= 2 {
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
    /// **Columns are counted the way the buffer fills cells**: a cluster at a time. Counted by
    /// `char` these came out wider than they draw, and everything measured with them drifted.
    #[test]
    fn width_is_counted_per_cluster_as_ratatui_draws_it() {
        // NFD Hangul: three scalars, one syllable, two columns.
        assert_eq!(display_width("\u{1112}\u{1161}\u{11ab}"), 2);
        // A letter and its combining accent share one cell.
        assert_eq!(display_width("e\u{301}"), 1);
        // Emoji with a joiner, a skin tone, a variation selector.
        assert_eq!(display_width("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}"), 2);
        assert_eq!(display_width("\u{1f44d}\u{1f3fd}"), 2);
        // Control characters are dropped by the buffer, so they take nothing.
        assert_eq!(display_width("a\tb\r"), 2);
        // And the same total as ratatui's own count of a drawn line.
        for s in ["한글 abc", "e\u{301}x", "\u{1f44d}\u{1f3fd}!"] {
            assert_eq!(display_width(s), ratatui::text::Line::raw(s).width(), "{s:?}");
        }
    }

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

    /// **Only a web or mail link is a link.** A control character would end the OSC 8 sequence it
    /// rides in and hand the rest to the terminal; any other scheme is handed to the OS opener.
    #[test]
    fn only_safe_destinations_become_links() {
        assert_eq!(safe_url("https://example.com/?a=1&b"), Some("https://example.com/?a=1&b"));
        assert_eq!(safe_url("HTTP://x"), Some("HTTP://x"));
        assert_eq!(safe_url("mailto:a@b.c"), Some("mailto:a@b.c"));
        for bad in [
            "https://x/\x1b\\\x1b]52;c;aGk=\x07",
            "https://x/\u{9c}",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ms-msdt:/id",
            "notes.md",
        ] {
            assert_eq!(safe_url(bad), None, "{bad:?} was let through");
        }
        let r = render_rich("[bad](<https://x/\u{1b}]52;c;aGk=\u{7}>) [ok](https://ok)", 40);
        let urls: Vec<&str> = r.links.iter().flatten().map(|l| l.url.as_str()).collect();
        assert_eq!(urls, ["https://ok"]);
        assert!(plain(&r.lines).join("").contains("bad"), "the text of a refused link stays");
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

    /// **A wrapped list item stays in its list.** The bullet is one margin wide, and every line
    /// after the first is indented by exactly that much — without it the tail of a long item sat
    /// against the left edge and read as ordinary text that had escaped the list.
    #[test]
    fn a_wrapped_list_item_keeps_its_margin() {
        let out = plain(&render(
            "- 이 항목의 설명이 아주 길어서 좁은 폭에서는 반드시 여러 줄로 접힌다",
            24,
        ));
        assert!(out.len() > 1, "nothing wrapped: {out:?}");
        assert!(out[0].starts_with("∙ "), "{out:?}");
        for line in &out[1..] {
            assert!(line.starts_with("  "), "the tail did not hang under the bullet: {out:?}");
            assert!(!line.trim_start().starts_with('∙'), "the bullet was repeated: {out:?}");
        }
        assert!(out.iter().all(|l| display_width(l) <= 24), "{out:?}");
        // **Not a character was lost on the way out.** Korean is cut per column rather than at
        // word boundaries, so the check is on the characters, not on the words.
        let squashed = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        let src = "- 이 항목의 설명이 아주 길어서 좁은 폭에서는 반드시 여러 줄로 접힌다";
        assert_eq!(squashed(&out.concat()), squashed(&src.replace('-', "∙")), "{out:?}");
    }

    /// **A nested item hangs under its own bullet**, not under the outer one — the margin grows
    /// with the depth, exactly as the bullet's does.
    #[test]
    fn a_nested_item_hangs_under_its_own_bullet() {
        let out = plain(&render(
            "- 바깥 항목\n  - 안쪽 항목의 설명이 길어서 좁은 폭에서 여러 줄로 접힌다",
            26,
        ));
        let inner = out.iter().position(|l| l.contains("안쪽 항목")).expect("{out:?}");
        // **It starts on its own line.** Inside the outer item's sentence it read as more of it.
        assert!(out[inner].starts_with("  ∙ "), "{out:?}");
        for line in &out[inner + 1..] {
            assert!(line.starts_with("    "), "the nested tail did not hang: {out:?}");
        }
    }

    /// A table is drawn to the width it was given and is never re-wrapped by a margin that is not
    /// its own — nothing about a list item's margin may leak into one drawn after it.
    #[test]
    fn a_table_after_a_list_keeps_its_own_width() {
        let out = plain(&render("- 항목\n\n| 이름 | 값 |\n|---|---|\n| 가나다 | 1 |", 24));
        let border = out.iter().find(|l| l.starts_with('┌')).expect("{out:?}");
        assert!(display_width(border) <= 24, "{out:?}");
        for line in out.iter().filter(|l| l.contains('│')) {
            assert!(display_width(line) <= 24, "{out:?}");
        }
    }

    /// **A word wider than the line is split across lines, not cut at the edge.** Long URLs,
    /// paths and hashes used to run past the right border and lose their tail, on screen and in a
    /// copy. The rows after the first are marked joined so a copy gives the word back whole.
    #[test]
    fn a_word_wider_than_the_line_is_split_and_marked_joined() {
        let token = format!("https://example.com/{}", "a1b2c3d4".repeat(22));
        let r = render_rich(&format!("see {token} end"), 40);
        let out = plain(&r.lines);
        assert!(out.len() > 3, "{out:?}");
        assert!(out.iter().all(|l| display_width(l) <= 40), "{out:?}");
        let squashed: String = out.concat().split_whitespace().collect();
        assert_eq!(squashed, format!("see{token}end"));
        let joined = r.prefix.iter().filter(|p| **p & crate::selection::JOINED != 0).count();
        assert_eq!(joined, out.len() - 2, "{:?}", r.prefix);
        // A link keeps its range on every row it was split over.
        let r = render_rich(&format!("[{token}]({token})"), 40);
        assert!(r.links.iter().all(|l| l.len() == 1), "{:?}", r.links);
    }

    /// **A code line wider than the screen wraps under a dotted rule**, and a tab is drawn as the
    /// spaces it stands for rather than as nothing.
    #[test]
    fn a_long_code_line_wraps_and_a_tab_keeps_its_indent() {
        let long = "x".repeat(50);
        let r = render_rich(&format!("```\n\tif a {{\n{long}\n\n```"), 24);
        let out = plain(&r.lines);
        assert_eq!(out[1], "│     if a {", "{out:?}");
        assert!(out.iter().all(|l| display_width(l) <= 24), "{out:?}");
        assert!(out[2].starts_with("│ ") && out[3].starts_with("┊ "), "{out:?}");
        assert_eq!(
            out.iter().filter(|l| l.contains('x')).map(|l| &l[4..]).collect::<String>(),
            long
        );
        assert_eq!(out[out.len() - 2], "│ ", "a blank code line went missing: {out:?}");
    }

    /// **A cell wraps in its column instead of losing its tail to `…`**, and a table too narrow
    /// for its grid becomes a list rather than being cut at the right border.
    #[test]
    fn a_table_cell_wraps_and_a_too_narrow_table_becomes_a_list() {
        let md = "| name | note |\n|---|---|\n| a | one two three four five six seven |";
        let out = plain(&render(md, 24));
        assert!(out.iter().all(|l| display_width(l) <= 24), "{out:?}");
        let all = out.join(" ");
        for word in ["one", "two", "three", "four", "five", "six", "seven"] {
            assert!(all.contains(word), "{word} was cut: {out:?}");
        }
        assert!(!all.contains('…'), "{out:?}");

        let wide = "| a | b | c | d |\n|---|---|---|---|\n| 1 | 2 | 3 | long cell |";
        let out = plain(&render(wide, 12));
        assert!(out.iter().all(|l| display_width(l) <= 12), "{out:?}");
        assert!(out.join(" ").contains("long"), "{out:?}");
        assert!(out[0].starts_with("∙ a: 1"), "{out:?}");
    }

    /// **Angle-bracketed words are text.** Parsed as inline HTML they were dropped: "use
    /// Vec<String> here" read "use Vec here".
    #[test]
    fn html_in_prose_is_kept_as_text() {
        let out = plain(&render("use Vec<String> here and <T>\n\n<div>block</div>", 60));
        assert_eq!(out, ["use Vec<String> here and <T>", "<div>block</div>"]);
    }

    /// Strikethrough strikes and dims, an ordered list keeps its numbers, a quote draws its bar,
    /// and a bold word gives the heading or quote around it back its own style.
    #[test]
    fn strike_numbers_quotes_and_nested_styles_render() {
        let r = render("~~old~~ new", 40);
        let old = &r[0].spans[0];
        assert_eq!(old.content, "old");
        assert!(old.style.add_modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(old.style.fg, Some(theme::text_muted()));

        let out = plain(&render("3. third\n4. fourth", 40));
        assert_eq!(out, ["3. third", "4. fourth"]);

        let out = plain(&render("> quoted words that wrap onto a second line here", 20));
        assert!(out.len() > 1 && out.iter().all(|l| l.starts_with("│ ")), "{out:?}");

        let r = render("# Title **b** rest", 40);
        let rest = r[0].spans.iter().find(|s| s.content.contains("rest")).expect("{r:?}");
        assert!(rest.style.add_modifier.contains(Modifier::BOLD), "{r:?}");
        let r = render("> a **b** c", 40);
        let c = r[0].spans.iter().find(|s| s.content == "c").expect("{r:?}");
        assert_eq!(c.style.fg, Some(theme::text_muted()), "{r:?}");
    }

    /// **Pipes inside a code fence are code**: no table is invented under `| sort`, and
    /// `|| true` is not split into two rows.
    #[test]
    fn pipes_inside_a_code_fence_are_left_alone() {
        assert_eq!(optimistic_table("```sh\nls\n| sort"), None);
        assert_eq!(split_glued_rows("```\n| grep x || true\n```"), None);
        // Outside a fence both still do their job.
        assert!(split_glued_rows("```\n```\n| a || b |").is_some());
    }

    #[test]
    fn a_bullet_list_is_marked() {
        let out = plain(&render("- 첫째\n- 둘째", 40));
        assert_eq!(out.len(), 2);
        assert!(out[0].contains("첫째"));
        assert!(out[0].trim_start().starts_with('∙') || out[0].trim_start().starts_with('-'));
    }
}
