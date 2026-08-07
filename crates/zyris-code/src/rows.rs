//! Lays items out into screen lines. **The single source of truth for the conversation screen.**
//!
//! What the widget draws, the scroll-window math, and the folded-card heights all use the same
//! `Vec<Line>` produced here. If the drawing code and the measuring code diverge, scrolling quietly drifts.

use std::collections::HashMap;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::markdown;
use crate::theme;
use crate::timeline::{Item, Part};

/// Horizontal padding of the conversation.
///
/// **The markers use this padding.** `▌`·`│`·`●`·`▸` stand in the padding and the text starts inside
/// it — so the text is tidy while the markers visibly stick out.
pub const PAD: u16 = 2;

fn pad() -> Span<'static> {
    Span::styled(" ".repeat(PAD as usize), Style::default().fg(theme::text()))
}

/// The width text can actually use.
///
/// Only the left padding is subtracted — **the right padding is given by `transcript` shrinking the drawing area
/// itself**. Subtracting again here would double the gap on the right only.
fn body_width(width: u16) -> u16 {
    width.saturating_sub(PAD)
}

/// One card's fold state.
///
/// **The default is folded.** Reasoning is for skimming, not reading, and a wall of thinking must not
/// push away the screen the person came to for answers. **Even folded, the tool rows show** — only the reasoning
/// body is hidden. Hiding what was done would leave the person not knowing what the agent is up to.
/// Opening is done by the person with Ctrl+O.
///
/// **Nothing changes by itself.** It used to unfold during reasoning and fold when the answer started, but that
/// made the screen being read move on its own and kept sprouting exceptions like "late reasoning must not
/// unfold again". That algorithm was removed wholesale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fold {
    pub open: bool,
}

pub type Folds = HashMap<i64, Fold>;

/// The rendered result. Along with the lines it gives **which line is the head of which work card**.
///
/// Folding a card by click requires knowing what a given screen line is, and the only place that knows
/// is here, where the lines are made. If the widget counted separately, it would drift from what's drawn.
#[derive(Debug, Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    /// Row index → the seq of the card that clicking that row folds and unfolds.
    pub cards: HashMap<usize, i64>,
    /// The links on each line, in that line's display columns.
    pub links: Vec<Vec<crate::markdown::Link>>,
}

impl Rendered {
    /// Plain text of each line. Used by selection extraction.
    pub fn plain(&self) -> Vec<String> {
        self.lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }
}

/// The question currently being answered. Only that card gets the cursor and checkmark.
pub struct Active<'a> {
    pub seq: i64,
    pub answering: &'a crate::question::Answering,
}

pub fn rows(items: &[Item], width: u16, folds: &Folds, lang: crate::lang::Lang) -> Rendered {
    rows_with(items, width, folds, None, lang)
}

/// Lays out all items at once. Used by tests and small screens.
///
/// Real drawing goes through `Cache` — as the conversation grows, this function rebuilds everything
/// every time and blows the frame budget.
pub fn rows_with(
    items: &[Item],
    width: u16,
    folds: &Folds,
    active: Option<Active<'_>>,
    lang: crate::lang::Lang,
) -> Rendered {
    let mut cache = Cache::new();
    cache.layout(items, width, folds, active.map(|a| a.seq), lang);
    let total = cache.total();
    Rendered {
        lines: cache.window(0, total),
        cards: cache.cards().clone(),
        links: cache.window_links(0, total),
    }
}

/// The result of rendering one item.
#[derive(Debug, Clone)]
struct Made {
    lines: Vec<Line<'static>>,
    /// The links on each line, in that line's display columns. Parallel to `lines`.
    links: Vec<Vec<crate::markdown::Link>>,
    /// The lines that fold/unfold when clicked. (which line within this item, which seq).
    ///
    /// There's one work-card head, plus one per tool row inside it —
    /// **each tool unfolds separately.**
    heads: Vec<(usize, i64)>,
}

/// Every fold state that affects how this item is drawn. The cache comparison looks at this.
///
/// Looking only at the item's own would make the cache think "nothing changed" when one tool row
/// is unfolded, and the screen would stay put.
type Affecting = Vec<(i64, Fold)>;

fn affecting(item: &Item, folds: &Folds) -> Affecting {
    let at = |seq: i64| (seq, folds.get(&seq).copied().unwrap_or_default());
    let mut out = vec![at(item.seq())];
    if let Item::Work { parts, .. } = item {
        out.extend(parts.iter().filter_map(|p| match p {
            Part::Step(s) => Some(at(s.seq)),
            Part::Think(_) | Part::Text(_) => None,
        }));
    }
    out
}

/// Where an item sits. `begin` is the start **including the separator blank line before it**.
#[derive(Debug, Clone, Copy)]
struct Slot {
    seq: i64,
    begin: usize,
    /// Whether a separator blank line precedes it. Only the first item lacks one.
    lead_blank: bool,
    len: usize,
}

impl Slot {
    fn end(&self) -> usize {
        self.begin + self.lead_blank as usize + self.len
    }
    /// The item's first real line.
    fn first_line(&self) -> usize {
        self.begin + self.lead_blank as usize
    }
}

/// Holds made lines per item and remakes **only what changed**.
///
/// It used to re-parse every item's markdown each frame. As the conversation grew, the cost rose
/// linearly, and around 16 turns one frame hit 1.7× the 20fps budget — the next frame stamped on top
/// before drawing finished, garbling text. `tests/perf.rs` measures those numbers.
#[derive(Debug, Default)]
pub struct Cache {
    width: u16,
    made: HashMap<i64, (Item, Affecting, Made)>,
    slots: Vec<Slot>,
    total: usize,
    cards: HashMap<usize, i64>,
    /// How many items were actually redrawn. Tests use this to confirm the cache really works.
    renders: u64,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn cards(&self) -> &HashMap<usize, i64> {
        &self.cards
    }

    pub fn renders(&self) -> u64 {
        self.renders
    }

    /// What the line at `line` belongs to: `(item seq, how far into that item)`.
    ///
    /// **This is what makes the viewport survive a relayout.** `Scroll.top` is an absolute index
    /// into a line list that `layout` rebuilds from scratch, so a width change (every wrap point
    /// moves) or a fold toggle above the viewport silently changes what that index points at —
    /// and always toward older text, since the clamp in `Scroll::on_content` can only move it
    /// down. Remembering the item instead means the same words stay under the eye.
    ///
    /// `None` when there is nothing laid out, or `line` is past the end.
    pub fn anchor_at(&self, line: usize) -> Option<(i64, usize)> {
        let slot = self.slots.iter().find(|s| line < s.end())?;
        // A line in the leading blank counts as the item's first line — the blank belongs to the
        // separation between items, not to either of them.
        Some((slot.seq, line.saturating_sub(slot.first_line())))
    }

    /// Where `(seq, offset)` sits now. The inverse of `anchor_at`, after a relayout.
    ///
    /// The offset is clamped to the item's current length: rewrapping narrower makes an item
    /// longer and wider makes it shorter, and an offset past the end would otherwise skip into
    /// the item below.
    pub fn line_of(&self, seq: i64, offset: usize) -> Option<usize> {
        let slot = self.slots.iter().find(|s| s.seq == seq)?;
        Some(slot.first_line() + offset.min(slot.len.saturating_sub(1)))
    }

    /// Decides which item lands on which line. Only redraws changed items.
    ///
    /// `skip` is the seq of the question currently being answered in the lower panel — drawing it again
    /// in the transcript would show the same question twice.
    pub fn layout(
        &mut self,
        items: &[Item],
        width: u16,
        folds: &Folds,
        skip: Option<i64>,
        lang: crate::lang::Lang,
    ) {
        // A width change moves every wrap point. Throw it all away.
        if self.width != width {
            self.width = width;
            self.made.clear();
        }
        self.slots.clear();
        self.cards.clear();

        let mut pos = 0usize;
        for item in items {
            let seq = item.seq();
            if skip == Some(seq) {
                continue;
            }
            let now = affecting(item, folds);
            let fresh = match self.made.get(&seq) {
                Some((was, had, _)) => was == item && *had == now,
                None => false,
            };
            if !fresh {
                let made = make(item, width, folds, lang);
                self.made.insert(seq, (item.clone(), now, made));
                self.renders += 1;
            }
            let made = &self.made[&seq].2;
            let slot =
                Slot { seq, begin: pos, lead_blank: !self.slots.is_empty(), len: made.lines.len() };
            pos = slot.end();
            for (rel, at) in &made.heads {
                self.cards.insert(slot.first_line() + rel, *at);
            }
            self.slots.push(slot);
        }
        self.total = pos;

        // No reason to keep lines for items that have left the screen.
        if self.made.len() > self.slots.len() * 2 + 8 {
            let live: std::collections::HashSet<i64> = self.slots.iter().map(|s| s.seq).collect();
            self.made.retain(|seq, _| live.contains(seq));
        }
    }

    /// Produces only the lines in `[from, to)`. **Doing only what's visible is the point.**
    pub fn window(&self, from: usize, to: usize) -> Vec<Line<'static>> {
        let to = to.min(self.total);
        if from >= to {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(to - from);
        for slot in &self.slots {
            if slot.end() <= from {
                continue;
            }
            if slot.begin >= to {
                break;
            }
            let made = &self.made[&slot.seq].2;
            for i in slot.begin.max(from)..slot.end().min(to) {
                let inner = i - slot.begin;
                out.push(match (slot.lead_blank, inner) {
                    (true, 0) => blank(),
                    (true, n) => made.lines[n - 1].clone(),
                    (false, n) => made.lines[n].clone(),
                });
            }
        }
        out
    }

    /// The links on the lines `window` returns, in the same order. The blank separator
    /// line between items has no links — the drawing side wraps link cells in OSC 8
    /// (Ctrl+click) using these, so they must match `window` line for line.
    pub fn window_links(&self, from: usize, to: usize) -> Vec<Vec<crate::markdown::Link>> {
        let to = to.min(self.total);
        if from >= to {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(to - from);
        for slot in &self.slots {
            if slot.end() <= from {
                continue;
            }
            if slot.begin >= to {
                break;
            }
            let made = &self.made[&slot.seq].2;
            for i in slot.begin.max(from)..slot.end().min(to) {
                let inner = i - slot.begin;
                out.push(match (slot.lead_blank, inner) {
                    (true, 0) => Vec::new(),
                    (true, n) => made.links[n - 1].clone(),
                    (false, n) => made.links[n].clone(),
                });
            }
        }
        out
    }

    /// Plain text of all lines. **Only called when copying a selection** — never per frame.
    pub fn plain(&self) -> Vec<String> {
        self.window(0, self.total)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }
}

fn blank() -> Line<'static> {
    Line::from(Span::styled("", Style::default().fg(theme::text())))
}

/// Lays one item out into lines. This is the only place markdown is parsed.
///
/// Alongside the lines it records the links on each line (shifted by the marker prefix), so the
/// drawing side can wrap link cells in OSC 8 — Ctrl+click then opens them. `links` is kept in
/// lockstep with `out`: **every** line pushed must push a link entry, empty unless the line's
/// text came from a link.
fn make(item: &Item, width: u16, folds: &Folds, lang: crate::lang::Lang) -> Made {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut links: Vec<Vec<crate::markdown::Link>> = Vec::new();
    let mut heads: Vec<(usize, i64)> = Vec::new();
    let fold = folds.get(&item.seq()).copied().unwrap_or_default();
    match item {
        Item::User { text, .. } => {
            for (i, raw) in text.lines().enumerate() {
                // **Typed-in answers look different.** The fact that the answer wasn't among the options is itself
                // information, and if it blends in with the picked ones that distinction disappears.
                // Answer lines arrive as `  - {free_mark}…` — the list marker must be stripped
                // first for the prefix to show.
                let bare = raw.trim_start().trim_start_matches("- ").trim_start();
                let typed = bare.starts_with(lang.free_mark());
                let body =
                    if typed { bare.trim_start_matches(lang.free_mark()).trim() } else { raw };
                let style = if typed {
                    Style::default().fg(theme::accent_hover()).add_modifier(Modifier::ITALIC)
                } else {
                    Style::default().fg(theme::text())
                };
                // **The bar stands on every line.** Set only on the first line, the second line onward
                // wouldn't be distinguishable from an answer — the longer the question, the longer that stretch.
                let _ = i;
                let mut spans = vec![Span::styled("▌ ", Style::default().fg(theme::accent()))];
                if typed {
                    spans.push(Span::styled("✎ ", Style::default().fg(theme::accent_hover())));
                }
                let prefix_w =
                    spans.iter().map(|s| markdown::display_width(&s.content)).sum::<usize>();
                let rendered = markdown::render_rich(body, body_width(width));
                for (li, line) in rendered.lines.into_iter().enumerate() {
                    let mut row = spans.clone();
                    row.extend(line.spans.into_iter().map(|sp| {
                        if typed {
                            Span::styled(sp.content.to_string(), style)
                        } else {
                            sp
                        }
                    }));
                    // **The background rides on the line, not the spans.** Painted on a span it breaks at glyph widths
                    // into blotches, and padding with spaces to fill the width lets those spaces
                    // travel through `plain()` into the clipboard. Stretching to the screen width is
                    // done where it draws (`widgets::transcript::stretch`).
                    out.push(Line::from(row).style(Style::default().bg(theme::user_bg())));
                    links.push(shift_links(&rendered.links[li], prefix_w));
                    spans = vec![Span::styled("▌ ", Style::default().fg(theme::accent()))];
                }
            }
        }
        Item::Agent { text, .. } => {
            // The body starts inside the padding. The lines carrying markers and the text must stand in the same
            // column for the eye's comfort — only markers come out beyond the padding.
            //
            // **The marker goes on the first line only.** On every line it reads like a blockquote, and since an answer is
            // the longest chunk in the conversation, that effect would cover the screen. With no marker at all,
            // only the answer lacks a sign — not "clean by default" but simply undifferentiated.
            let rendered = markdown::render_rich(text, body_width(width));
            for (i, line) in rendered.lines.into_iter().enumerate() {
                let mut spans = match i {
                    0 => vec![Span::styled("◆ ", Style::default().fg(theme::accent()))],
                    _ => vec![pad()],
                };
                let prefix_w =
                    spans.iter().map(|s| markdown::display_width(&s.content)).sum::<usize>();
                spans.extend(line.spans);
                out.push(Line::from(spans));
                links.push(shift_links(&rendered.links[i], prefix_w));
            }
        }
        Item::Error { message, .. } => {
            out.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(theme::danger())),
                Span::styled(message.clone(), Style::default().fg(theme::danger())),
            ]));
            links.push(Vec::new());
        }
        // The question being answered doesn't come here — `layout` filters it out.
        Item::Question { steps, answered, .. } => {
            let rows = question_rows(steps, *answered, width, lang);
            let n = rows.len();
            out.extend(rows);
            links.extend(std::iter::repeat_with(Vec::new).take(n));
        }
        // What the app said. It's a third voice that is neither person nor agent, so it gets its own marker.
        Item::System { text, .. } => {
            let rendered = markdown::render_rich(text, body_width(width));
            for (i, line) in rendered.lines.into_iter().enumerate() {
                let mut spans = vec![Span::styled(
                    if i == 0 { "◈ " } else { "  " },
                    Style::default().fg(theme::text_muted()),
                )];
                let prefix_w =
                    spans.iter().map(|s| markdown::display_width(&s.content)).sum::<usize>();
                spans.extend(line.spans);
                out.push(Line::from(spans));
                links.push(shift_links(&rendered.links[i], prefix_w));
            }
        }
        Item::Subagent { summary, .. } => {
            out.push(Line::from(vec![
                Span::styled("└ ", Style::default().fg(theme::text_muted())),
                Span::styled(summary.clone(), Style::default().fg(theme::text_muted())),
            ]));
            links.push(Vec::new());
        }
        Item::Work { seq, title, parts } => {
            let marker = if fold.open { "▾ " } else { "▸ " };
            // The title is empty when the run starts and a small model fills it in later.
            let head = if title.is_empty() { lang.working() } else { title.as_str() };
            // Pressing this line folds and unfolds this card. Which screen line it is
            // is decided by `layout`, which knows the layout.
            heads.push((0, *seq));
            let tools = parts.iter().filter(|p| matches!(p, Part::Step(_))).count();
            let mut card = vec![
                Span::styled(marker, Style::default().fg(theme::accent())),
                Span::styled(
                    head.to_string(),
                    Style::default().fg(theme::text_muted()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  ·  {}", lang.tool_count(tools)),
                    Style::default().fg(theme::text_muted()),
                ),
            ];
            // **Even folded, what changed is visible.** The numbers only live on tool rows, which need the card
            // unfolded, so when the turn ends and the card folds, the trace would vanish entirely.
            // Sticking `+0 −0` on a card that changed nothing is its own kind of noise.
            let (added, removed) = parts.iter().fold((0u32, 0u32), |(a, r), p| match p {
                Part::Step(s) => match &s.diff {
                    Some(d) => (a + d.added, r + d.removed),
                    None => (a, r),
                },
                _ => (a, r),
            });
            if added + removed > 0 {
                card.extend(counts(added, removed));
            }
            out.push(Line::from(card));
            links.push(Vec::new());

            // **Tool rows show even when folded.** Folding a card hides reasoning, not what was
            // done — tool use is part of the conversation's flow, and buried under thinking
            // the person wouldn't know what the agent is doing. Unfolded, the
            // thinking lines interleave in place and the "think → tool → think → tool" order
            // shows as is.
            for part in parts {
                match part {
                    Part::Think(text) if fold.open => {
                        // **Thinking lines can fold and unfold the card by click too.** The same action
                        // as Ctrl+O — a click toggles the card fold, it doesn't open tool detail.
                        let rendered = markdown::render_rich(text, body_width(width));
                        for (li, line) in rendered.lines.into_iter().enumerate() {
                            heads.push((out.len(), *seq));
                            let mut spans =
                                vec![Span::styled("┊ ", Style::default().fg(theme::border_light()))];
                            let prefix_w = spans
                                .iter()
                                .map(|s| markdown::display_width(&s.content))
                                .sum::<usize>();
                            // The whole body is repainted dim. At the same brightness as the answer,
                            // the conclusion wouldn't be visible — losing emphasis and inline
                            // code colours inside reasoning is the price paid for that.
                            spans.extend(line.spans.into_iter().map(|s| {
                                Span::styled(s.content.to_string(), s.style.fg(theme::text_muted()))
                            }));
                            out.push(Line::from(spans));
                            links.push(shift_links(&rendered.links[li], prefix_w));
                        }
                    }
                    Part::Text(text) => {
                        // **A run-time message.** It shows even when folded — folding a card hides reasoning,
                        // not the conversation flow. Same policy as tool rows. Not a click target — unlike thinking lines,
                        // a message is content, not a handle.
                        let rendered = markdown::render_rich(text, body_width(width));
                        for (li, line) in rendered.lines.into_iter().enumerate() {
                            let mut spans = vec![match li {
                                0 => Span::styled("◆ ", Style::default().fg(theme::accent())),
                                _ => pad(),
                            }];
                            let prefix_w = spans
                                .iter()
                                .map(|s| markdown::display_width(&s.content))
                                .sum::<usize>();
                            spans.extend(line.spans);
                            out.push(Line::from(spans));
                            links.push(shift_links(&rendered.links[li], prefix_w));
                        }
                    }
                    Part::Step(step) => {
                        let dot = if step.failed { theme::danger() } else { theme::success() };
                        // Only rows with something to unfold are clickable. Pressing one and having
                        // nothing happen looks like a bug.
                        let can_open = !step.detail.is_empty() || step.diff.is_some();
                        // **Tools of a folded card can't open their detail** — folding the card means
                        // folding everything. A row that does nothing while folded is
                        // not a click target.
                        let open =
                            fold.open && can_open && folds.get(&step.seq).is_some_and(|f| f.open);
                        if fold.open && can_open {
                            heads.push((out.len(), step.seq));
                        }
                        // **Name and summary are painted separately.** Reasoning fills the screen in a dim colour; if tools
                        // shared that colour, "what was done" would be buried in the thinking pile.
                        // When skimming, the eye catches this name.
                        let mut head = vec![
                            Span::styled("● ", Style::default().fg(dot)),
                            Span::styled(
                                step.name.clone(),
                                Style::default().fg(theme::tool()).add_modifier(Modifier::BOLD),
                            ),
                        ];
                        if !step.note.is_empty() {
                            head.push(Span::styled(
                                format!("  {}", step.note),
                                Style::default().fg(theme::tool_arg()),
                            ));
                        }
                        // **Even folded, how much changed is visible.** If it took unfolding to know,
                        // a skim alone wouldn't tell what happened.
                        if let Some(d) = &step.diff {
                            head.extend(counts(d.added, d.removed));
                        }
                        // Signals that it can be unfolded. Not knowing, nobody would click.
                        head.push(Span::styled(
                            match (fold.open && can_open, open) {
                                (false, _) => "",
                                (true, false) => "  ▸",
                                (true, true) => "  ▾",
                            },
                            Style::default().fg(theme::border_light()),
                        ));
                        out.push(Line::from(head));
                        links.push(Vec::new());
                        if open {
                            match &step.diff {
                                // **When a diff exists, only it is shown.** Layering the same content as JSON once more
                                // just doubles the screen length, and what the person reads is the diff.
                                Some(d) => {
                                    for line in &d.lines {
                                        out.push(diff_line(line, body_width(width) as usize, lang));
                                        links.push(Vec::new());
                                    }
                                }
                                // Detail is drawn **per section**. `event::tool_detail` assembles it with
                                // the "Args/Output/Result/Error" heads, so they're set apart by colour and markers — if JSON dumps and shell output
                                // looked like one lump, the eye would have to re-read every time
                                // what's an argument and what's a result.
                                None => {
                                    let detail = tool_detail_lines(
                                        &step.detail,
                                        body_width(width),
                                        step.failed,
                                        lang,
                                    );
                                    let n = detail.len();
                                    out.extend(detail);
                                    links.extend(std::iter::repeat_with(Vec::new).take(n));
                                }
                            }
                        }
                    }
                    Part::Think(_) => {}
                }
            }
        }
    }
    Made { lines: out, links, heads }
}

/// Shifts link columns by the marker prefix width so they land in the output line's columns.
fn shift_links(links: &[crate::markdown::Link], shift: usize) -> Vec<crate::markdown::Link> {
    links
        .iter()
        .map(|l| crate::markdown::Link {
            start: l.start + shift,
            end: l.end + shift,
            url: l.url.clone(),
        })
        .collect()
}

/// The `  +12 −3` attached to summary lines.
///
/// **The two numbers are painted separately.** Painted one colour, the eye has to read once more
/// which side grew. The minus is U+2212, not a hyphen, so it has the same width as the plus — the numbers line up.
fn counts(added: u32, removed: u32) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("  +{added}"), Style::default().fg(theme::diff_add())),
        Span::styled(format!(" −{removed}"), Style::default().fg(theme::diff_del())),
    ]
}

/// One diff line. **Only colour, no background** — a background would fight the selection.
/// `pub(crate)` because the preview and the record must draw the same way.
///
/// `width` is the width text can use (`body_width`); the two-space indent is subtracted here.
pub(crate) fn diff_line(
    line: &crate::tools::diff::DiffLine,
    width: usize,
    lang: crate::lang::Lang,
) -> Line<'static> {
    use crate::tools::diff::DiffLine;
    let (text, colour) = match line {
        DiffLine::Add(s) => (format!("+{s}"), theme::diff_add()),
        DiffLine::Del(s) => (format!("-{s}"), theme::diff_del()),
        DiffLine::Keep(s) => (format!(" {s}"), theme::text_muted()),
        DiffLine::Skip(n) => (lang.diff_skip(*n), theme::border_light()),
    };
    Line::from(vec![
        Span::styled("  ", Style::default().fg(theme::border_light())),
        Span::styled(clip_to(text, width.saturating_sub(2)), Style::default().fg(colour)),
    ])
}

/// Clips when wider than the limit. **No wrapping** — if one code line grew into several screen lines,
/// skimming what changed gets hard, and the wrapped tail reads like the next line's `+`/`-`.
fn clip_to(text: String, width: usize) -> String {
    let limit = width.max(8);
    if markdown::display_width(&text) <= limit {
        return text;
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = markdown::display_width(&ch.to_string()).max(1);
        if used + w > limit - 1 {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Wraps to fit the width. **Cuts only by column count** — tool detail is JSON or raw text, so it
/// must not be parsed as markdown.
fn wrap_plain(text: &str, width: u16) -> Vec<String> {
    let limit = (width as usize).max(8);
    let mut out = Vec::new();
    for raw in text.lines() {
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut used = 0usize;
        for ch in raw.chars() {
            let w = markdown::display_width(&ch.to_string()).max(1);
            if used + w > limit {
                out.push(std::mem::take(&mut cur));
                used = 0;
            }
            cur.push(ch);
            used += w;
        }
        out.push(cur);
    }
    out
}

/// Draws the tool detail ("Args\n…\n\nOutput\n…") **as a box**.
///
/// Laid out as plain text, JSON dumps and shell output wouldn't be told apart. The whole detail is
/// wrapped in a border to separate it from the space below the tool row, and the
/// "Args/Output/Result/Error" heads are
/// painted separately in colour and markers. A failed tool is danger-coloured down to its border. Not parsing
/// it as markdown is as before — if JSON's `*`·`_` got eaten as emphasis, the original text would break.
fn tool_detail_lines(
    detail: &str,
    width: u16,
    failed: bool,
    lang: crate::lang::Lang,
) -> Vec<Line<'static>> {
    let border = if failed { theme::danger() } else { theme::border_light() };
    // Inner width after removing the "│ " gutter. Together with the borders it equals the screen width.
    let inner = width.saturating_sub(2).max(8);
    let mut out: Vec<Line<'static>> = Vec::new();
    // Top border.
    out.push(Line::from(Span::styled(
        format!("┌{}┐", "─".repeat(inner as usize)),
        Style::default().fg(border),
    )));
    let mut section: Option<&'static str> = None;
    // A head is only recognized after a blank line (or at the first line) — so a same word in the
    // body doesn't mix in.
    let mut prev_blank = true;
    for raw in detail.lines() {
        let trimmed = raw.trim();
        // A head picks a `lang.tool_sections()` element verbatim — storing a slice borrowed from
        // `detail` would let `section` outlive `detail`.
        let head = lang.tool_sections().iter().copied().find(|s| *s == trimmed);
        if let (Some(head), true) = (head, prev_blank) {
            section = Some(head);
            let (mark, color) = match head {
                h if h == lang.detail_args() => {
                    (format!("⎿ {}", lang.detail_args()), theme::tool_arg())
                }
                h if h == lang.detail_output() => {
                    (format!("⎿ {}", lang.detail_output()), theme::accent())
                }
                h if h == lang.detail_result() => {
                    (format!("⎿ {}", lang.detail_result()), theme::border_light())
                }
                _ => (format!("⎿ {}", lang.detail_error()), theme::danger()),
            };
            out.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(border)),
                Span::styled(mark, Style::default().fg(color).add_modifier(Modifier::BOLD)),
            ]));
            prev_blank = false;
            continue;
        }
        let color = match section {
            Some(h) if h == lang.detail_error() => theme::danger(),
            Some(h) if h == lang.detail_output() => theme::text(),
            _ => theme::text_muted(),
        };
        for line in wrap_plain(raw, inner).into_iter() {
            out.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(border)),
                Span::styled(line, Style::default().fg(color)),
            ]));
        }
        prev_blank = trimmed.is_empty();
    }
    // Bottom border.
    out.push(Line::from(Span::styled(
        format!("└{}┘", "─".repeat(inner as usize)),
        Style::default().fg(border),
    )));
    out
}

/// Question card. While awaiting an answer it can be chosen; after the answer it's read-only.
fn question_rows(
    steps: &[crate::question::Step],
    answered: bool,
    width: u16,
    lang: crate::lang::Lang,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let Some(step) = steps.first() else {
        return out;
    };

    let mark = if answered { "✓" } else { "?" };
    let head_colour = if answered { theme::text_muted() } else { theme::accent() };
    let mut head = vec![Span::styled(format!("{mark} "), Style::default().fg(head_colour))];
    if let Some(h) = &step.header {
        head.push(Span::styled(format!("[{h}] "), Style::default().fg(theme::text_muted())));
    }
    head.push(Span::styled(
        step.question.clone(),
        Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
    ));
    if steps.len() > 1 {
        head.push(Span::styled(
            format!("  ·  {}", lang.step_count(steps.len())),
            Style::default().fg(theme::text_muted()),
        ));
    }
    out.push(Line::from(head));

    let _ = width;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{Item, Step};

    fn plain(r: &Rendered) -> Vec<String> {
        r.plain()
    }

    /// One tool row. **Must not collide with the card's seq** — the cache key is the seq.
    fn step_at(seq: i64, label: &str) -> Step {
        Step {
            seq,
            name: label.into(),
            note: "viewport".into(),
            failed: false,
            detail: "인자\n{\"pattern\": \"viewport\"}".into(),
            diff: None,
        }
    }

    fn work_at(seq: i64) -> Item {
        Item::Work {
            seq,
            title: "스크롤 계산 위치를 찾는 중".into(),
            parts: vec![
                Part::Think("먼저 구조를 보자".into()),
                Part::Step(step_at(seq * 100, "grep")),
            ],
        }
    }

    fn work() -> Item {
        work_at(1)
    }

    /// **Even folded, the tool rows show.** Folding a card hides reasoning, not what was
    /// done — tool use is part of the conversation's flow, and buried under thinking the
    /// person wouldn't know what the agent is doing.
    #[test]
    fn a_folded_card_hides_thinking_but_shows_tools() {
        let folds = Folds::from([(1, Fold::default())]);
        let out = plain(&rows(&[work()], 40, &folds, crate::lang::Lang::Ko));
        assert!(out[0].contains("스크롤 계산 위치를 찾는 중"), "no header row: {out:?}");
        assert!(
            out.iter().any(|l| l.contains("grep")),
            "tools stay visible even when folded: {out:?}"
        );
        assert!(
            !out.iter().any(|l| l.contains("먼저 구조를 보자")),
            "추론은 접혀 있어야 한다: {out:?}"
        );
    }

    /// Unfolded, thinking interleaves between tools — in the order it came.
    #[test]
    fn an_open_card_interleaves_thinking_with_tools() {
        let folds = Folds::from([(1, Fold { open: true })]);
        let out = plain(&rows(&[work()], 40, &folds, crate::lang::Lang::Ko));
        let think = out.iter().position(|l| l.contains("먼저 구조를 보자")).expect("no thought");
        let tool = out.iter().position(|l| l.contains("grep")).expect("no tool");
        assert!(think < tool, "the thought must come before the tool: {out:?}");
    }

    #[test]
    fn an_open_card_shows_reasoning_and_steps() {
        let folds = Folds::from([(1, Fold { open: true })]);
        let out = plain(&rows(&[work()], 40, &folds, crate::lang::Lang::Ko));
        assert!(out.iter().any(|l| l.contains("먼저 구조를 보자")), "{out:?}");
        assert!(out.iter().any(|l| l.contains("grep")), "{out:?}");
    }

    /// **The head line says how many times tools were used.** What's counted is `Part::Step`, i.e. tool calls,
    /// so the "steps" suffix doesn't point at them.
    #[test]
    fn a_card_head_counts_tools_not_steps() {
        let out = plain(&rows(&[work()], 60, &Folds::new(), crate::lang::Lang::Ko));
        assert!(out[0].contains("도구 1개"), "{out:?}");
        assert!(!out[0].contains("단계"), "{out:?}");
    }

    /// A card with no fold state must be folded — the default is a quiet screen.
    /// (Tool rows still stand on that screen.)
    #[test]
    fn a_card_with_no_fold_state_defaults_to_folded() {
        let out = plain(&rows(&[work()], 40, &Folds::new(), crate::lang::Lang::Ko));
        assert!(out[0].contains("스크롤 계산 위치를 찾는 중"), "{out:?}");
        assert!(
            !out.iter().any(|l| l.contains("먼저 구조를 보자")),
            "reasoning is visible: {out:?}"
        );
        assert!(out.iter().any(|l| l.contains("grep")), "the tool is not visible: {out:?}");
    }

    #[test]
    fn a_user_message_is_marked_with_the_accent_bar() {
        let out = plain(&rows(
            &[Item::User { seq: 1, text: "안녕".into() }],
            40,
            &Folds::new(),
            crate::lang::Lang::Ko,
        ));
        assert!(out[0].starts_with('▌'), "{out:?}");
    }

    /// **Tool rows don't use the wire name as is.** Left as `zyris__arch__terminal__exec`, it alone
    /// eats a whole line, and every row starting with the same prefix hides the part that differs.
    #[test]
    fn a_tool_row_shows_the_short_name() {
        let items = [work_at(1)];
        let folds = Folds::from([(1, Fold { open: true })]);
        let out = plain(&rows(&items, 60, &folds, crate::lang::Lang::Ko));
        let row = out.iter().find(|l| l.contains("grep")).expect("{out:?}");
        assert!(!row.contains("zyris__"), "the raw wire name is shown: {row:?}");
    }

    /// **Tools must be a different colour from reasoning.** In an open card, reasoning fills the screen;
    /// if tools are also dim, "what was done" gets buried in the thinking pile.
    #[test]
    fn a_tool_row_stands_out_from_the_reasoning_around_it() {
        let items = [work_at(1)];
        let folds = Folds::from([(1, Fold { open: true })]);
        let r = rows(&items, 60, &folds, crate::lang::Lang::Ko);
        let row = r
            .lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("grep")))
            .expect("no tool row");
        let name = row.spans.iter().find(|s| s.content.contains("grep")).unwrap();
        assert_eq!(name.style.fg, Some(theme::tool()));
        assert_ne!(
            name.style.fg,
            Some(theme::text_muted()),
            "it must not share the reasoning colour"
        );
    }

    /// The name and its summary are **coloured separately** — one span can't split colours.
    #[test]
    fn the_name_and_its_summary_are_coloured_apart() {
        let items = [work_at(1)];
        let folds = Folds::from([(1, Fold { open: true })]);
        let r = rows(&items, 60, &folds, crate::lang::Lang::Ko);
        let row = r
            .lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("grep")))
            .expect("no tool row");
        let note = row.spans.iter().find(|s| s.content.contains("viewport")).expect("no summary");
        assert_eq!(note.style.fg, Some(theme::tool_arg()));
    }

    /// **Even folded, how much changed is visible.** Both the head line and tool rows carry the numbers.
    #[test]
    fn a_folded_card_still_shows_how_much_changed() {
        let d = crate::tools::diff::Diff::parse("-a\n+b\n", "src/app.rs", 12, 3).unwrap();
        let items = [Item::Work {
            seq: 1,
            title: "고치는 중".into(),
            parts: vec![Part::Step(Step {
                seq: 100,
                name: "edit".into(),
                note: "src/app.rs".into(),
                failed: false,
                detail: String::new(),
                diff: Some(d),
            })],
        }];
        let out = plain(&rows(&items, 60, &Folds::new(), crate::lang::Lang::Ko));
        assert!(out[0].contains("+12"), "the header row carries no counts: {out:?}");
        assert!(out[0].contains("−3"), "the header row carries no counts: {out:?}");
        assert!(
            out.iter().any(|l| l.contains("src/app.rs")),
            "접힌 상태에도 도구 줄은 보여야 한다: {out:?}"
        );
    }

    /// Sticking `+0 −0` on a card that changed nothing is its own kind of noise.
    #[test]
    fn a_card_that_changed_nothing_gets_no_counts() {
        let out = plain(&rows(&[work()], 60, &Folds::new(), crate::lang::Lang::Ko));
        assert!(!out[0].contains('+'), "{out:?}");
    }

    /// **Trailing spaces must not leak into the copy.** If the padding used to paint the background seeps
    /// into the clipboard, pasted code breaks. Padding is only done when `transcript` draws.
    #[test]
    fn the_band_never_leaks_trailing_spaces_into_the_copy() {
        let items = [Item::User { seq: 1, text: "안녕".into() }];
        for line in plain(&rows(&items, 60, &Folds::new(), crate::lang::Lang::Ko)) {
            assert_eq!(line, line.trim_end(), "trailing spaces were added: {line:?}");
        }
    }

    /// The background must ride on the line, not the spans — painted on a span, it breaks at glyph widths.
    #[test]
    fn the_user_band_rides_on_the_line_not_the_spans() {
        let items = [Item::User { seq: 1, text: "안녕".into() }];
        let r = rows(&items, 60, &Folds::new(), crate::lang::Lang::Ko);
        assert_eq!(r.lines[0].style.bg, Some(theme::user_bg()));
        assert!(r.lines[0].spans.iter().all(|s| s.style.bg.is_none()), "a span got a background");
    }

    /// **The bar runs down every line.** Only on the first line, the second line onward wouldn't be distinguishable from an answer.
    #[test]
    fn the_user_bar_runs_down_every_line() {
        let items = [Item::User { seq: 1, text: "첫 줄\n둘째 줄".into() }];
        let out = plain(&rows(&items, 40, &Folds::new(), crate::lang::Lang::Ko));
        assert!(out.len() >= 2, "{out:?}");
        assert!(out.iter().all(|l| l.starts_with('▌')), "{out:?}");
    }

    /// The bar must also stand on wrapped continuation lines — when one long sentence becomes several lines.
    #[test]
    fn the_user_bar_also_runs_down_wrapped_lines() {
        let long = "아주 긴 문장을 하나 적어서 좁은 폭에서 반드시 여러 줄로 접히게 만든다";
        let items = [Item::User { seq: 1, text: long.into() }];
        let out = plain(&rows(&items, 24, &Folds::new(), crate::lang::Lang::Ko));
        assert!(out.len() >= 2, "it did not fold: {out:?}");
        assert!(out.iter().all(|l| l.starts_with('▌')), "{out:?}");
    }

    /// **Answers need a marker too.** Everything else has one; an answer without one isn't "clean by default"
    /// — it's just not differentiated.
    #[test]
    fn an_agent_answer_is_marked_too() {
        let items = [Item::Agent { seq: 1, text: "그건 rows.rs가 정합니다.".into() }];
        let out = plain(&rows(&items, 40, &Folds::new(), crate::lang::Lang::Ko));
        assert!(out[0].starts_with('◆'), "{out:?}");
        assert!(out[0].contains("rows.rs가 정합니다"), "{out:?}");
    }

    /// The marker only on the first line. On every line it reads like a blockquote.
    #[test]
    fn only_the_first_line_of_an_answer_is_marked() {
        let items = [Item::Agent { seq: 1, text: "첫 줄\n\n둘째 문단".into() }];
        let out = plain(&rows(&items, 40, &Folds::new(), crate::lang::Lang::Ko));
        assert_eq!(out.iter().filter(|l| l.starts_with('◆')).count(), 1, "{out:?}");
    }

    /// **Reasoning uses a different gutter from code blocks.** If both used `│ `, they'd read as the same —
    /// code blocks inside answers use `│ ` in `markdown.rs`.
    #[test]
    fn reasoning_does_not_use_the_code_block_gutter() {
        let folds = Folds::from([(1, Fold { open: true })]);
        let out = plain(&rows(&[work()], 40, &folds, crate::lang::Lang::Ko));
        let think = out.iter().find(|l| l.contains("먼저 구조를 보자")).expect("no reasoning row");
        assert!(think.starts_with('┊'), "{think:?}");
    }

    /// Reasoning must be dimmer than the answer. At the same brightness, the conclusion isn't visible.
    #[test]
    fn reasoning_is_dimmer_than_the_answer() {
        let folds = Folds::from([(1, Fold { open: true })]);
        let r = rows(&[work()], 40, &folds, crate::lang::Lang::Ko);
        // Markdown splits words into several spans — searching a single span won't find it.
        let joined =
            |l: &Line<'static>| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() };
        let line = r
            .lines
            .iter()
            .find(|l| joined(l).contains("먼저 구조를 보자"))
            .expect("no reasoning row");
        // The gutter (`┊`) is a line, not text, so it stays BORDER_LIGHT. Only the body is checked.
        assert!(
            line.spans
                .iter()
                .skip(1)
                .filter(|s| !s.content.trim().is_empty())
                .all(|s| s.style.fg == Some(theme::text_muted())),
            "추론 본문이 흐리지 않다: {:?}",
            line.spans.iter().map(|s| (s.content.clone(), s.style.fg)).collect::<Vec<_>>()
        );
    }

    /// An error must never pass quietly.
    #[test]
    fn an_error_is_always_visible_and_red() {
        let items = [Item::Error { seq: 1, message: "크레딧이 부족합니다".into() }];
        let r = rows(&items, 40, &Folds::new(), crate::lang::Lang::Ko);
        assert!(plain(&r).iter().any(|l| l.contains("크레딧이 부족합니다")));
        assert!(
            r.lines.iter().flat_map(|l| &l.spans).any(|s| s.style.fg == Some(crate::theme::danger())),
            "오류는 빨간색이어야 한다"
        );
    }

    /// A conversation mixing what actually stands on screen. **`seq`s must differ** —
    /// a property `Timeline` guarantees via its BTreeMap keys; overlapping ones would make the cache evict each other.
    fn mixed() -> Vec<Item> {
        vec![
            Item::User { seq: 1, text: "표 좀 그려 줘".into() },
            work_at(2),
            Item::Agent {
                seq: 3,
                text: "| 경로 | 크기 |\n|---|---|\n| a | 1 |\n| b | 2 |\n\n끝입니다.".into(),
            },
            Item::Error { seq: 4, message: "크레딧이 부족합니다".into() },
            Item::Subagent { seq: 5, summary: "하위 에이전트가 끝났다".into() },
        ]
    }

    /// **The cache must draw exactly like the plain path.** Faster but different is useless.
    #[test]
    fn the_cache_draws_exactly_what_the_plain_path_draws() {
        let items = mixed();
        for folds in [Folds::new(), Folds::from([(2, Fold { open: true })])] {
            let want = rows(&items, 40, &folds, crate::lang::Lang::Ko);
            let mut cache = Cache::new();
            cache.layout(&items, 40, &folds, None, crate::lang::Lang::Ko);

            assert_eq!(cache.total(), want.lines.len(), "the row counts must match");
            assert_eq!(cache.plain(), want.plain(), "the contents must match");
            assert_eq!(cache.cards(), &want.cards, "the card head must sit at the same place");
        }
    }

    /// A window gives back only that slice. Building past the screen gets heavier with conversation length.
    #[test]
    fn a_window_gives_back_only_that_slice() {
        let items = mixed();
        let folds = Folds::new();
        let all = rows(&items, 40, &folds, crate::lang::Lang::Ko).plain();

        let mut cache = Cache::new();
        cache.layout(&items, 40, &folds, None, crate::lang::Lang::Ko);
        for (from, to) in [(0usize, 3usize), (2, 5), (1, cache.total()), (0, cache.total())] {
            let got: Vec<String> = cache
                .window(from, to)
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            assert_eq!(got, all[from..to], "the {from}..{to} range is off");
        }
        assert!(cache.window(3, 3).is_empty(), "an empty range gives an empty result");
        assert_eq!(cache.window(0, 9999).len(), cache.total(), "past the end it must still clamp");
    }

    /// **Only the changed item is drawn again.** If this breaks, it's slow again proportional to conversation length.
    #[test]
    fn only_the_changed_item_is_drawn_again() {
        let mut items = mixed();
        let folds = Folds::new();
        let mut cache = Cache::new();

        cache.layout(&items, 40, &folds, None, crate::lang::Lang::Ko);
        let first = cache.renders();
        assert_eq!(first, items.len() as u64, "everything is drawn the first time");

        cache.layout(&items, 40, &folds, None, crate::lang::Lang::Ko);
        assert_eq!(cache.renders(), first, "unchanged, not a single row is drawn again");

        // A delta was appended to the answer — only that item should be redrawn.
        if let Item::Agent { text, .. } = &mut items[2] {
            text.push_str("| c | 3 |\n");
        }
        cache.layout(&items, 40, &folds, None, crate::lang::Lang::Ko);
        assert_eq!(cache.renders(), first + 1, "only the changed one is drawn again");
    }

    /// Folding or unfolding redraws only that card.
    #[test]
    fn folding_a_card_redraws_only_that_card() {
        let items = mixed();
        let mut cache = Cache::new();
        cache.layout(&items, 40, &Folds::new(), None, crate::lang::Lang::Ko);
        let before = cache.renders();

        let opened = Folds::from([(2, Fold { open: true })]);
        cache.layout(&items, 40, &opened, None, crate::lang::Lang::Ko);
        assert_eq!(cache.renders(), before + 1, "only the unfolded card is drawn again");
        assert!(
            cache.plain().iter().any(|l| l.contains("먼저 구조를 보자")),
            "펼쳤으면 추론이 보여야 한다"
        );
    }

    /// A width change moves every wrap point — everything must be redrawn.
    #[test]
    fn a_width_change_redraws_everything() {
        let items = mixed();
        let folds = Folds::new();
        let mut cache = Cache::new();
        cache.layout(&items, 40, &folds, None, crate::lang::Lang::Ko);
        let before = cache.renders();

        cache.layout(&items, 80, &folds, None, crate::lang::Lang::Ko);
        assert_eq!(cache.renders(), before + items.len() as u64);
        assert_eq!(cache.plain(), rows(&items, 80, &folds, crate::lang::Lang::Ko).plain());
    }

    /// The question being answered isn't drawn in the transcript — it's in the lower panel.
    #[test]
    fn the_question_being_answered_is_left_out_of_the_transcript() {
        let steps = vec![crate::question::Step {
            header: None,
            question: "어느 쪽으로 갈까요".into(),
            options: vec![],
            multi: false,
        }];
        let items = vec![
            Item::User { seq: 1, text: "골라 줘".into() },
            Item::Question { seq: 2, steps, answered: false },
        ];
        let mut cache = Cache::new();
        cache.layout(&items, 40, &Folds::new(), Some(2), crate::lang::Lang::Ko);
        assert!(
            !cache.plain().iter().any(|l| l.contains("어느 쪽으로 갈까요")),
            "답하는 중인 질문이 두 벌로 보이면 안 된다: {:?}",
            cache.plain()
        );

        cache.layout(&items, 40, &Folds::new(), None, crate::lang::Lang::Ko);
        assert!(
            cache.plain().iter().any(|l| l.contains("어느 쪽으로 갈까요")),
            "답이 끝나면 대화에 남아야 한다"
        );
    }

    /// A tool starts as one line; unfolded, its args and result come out.
    #[test]
    fn a_tool_row_starts_as_one_line_and_opens_into_its_detail() {
        let items = [work_at(1)];
        let card_open = Folds::from([(1, Fold { open: true })]);

        let shut = plain(&rows(&items, 60, &card_open, crate::lang::Lang::Ko));
        assert!(shut.iter().any(|l| l.contains("grep")), "{shut:?}");
        assert!(
            !shut.iter().any(|l| l.contains("viewport\"}")),
            "안 폈는데 상세가 보인다: {shut:?}"
        );
        assert!(
            shut.iter().any(|l| l.contains("grep") && l.contains('▸')),
            "펼 수 있다는 표시가 없다: {shut:?}"
        );

        let mut both = card_open.clone();
        both.insert(100, Fold { open: true });
        let open = plain(&rows(&items, 60, &both, crate::lang::Lang::Ko));
        assert!(open.iter().any(|l| l.contains("인자")), "the detail is not shown: {open:?}");
        assert!(open.iter().any(|l| l.contains("viewport")), "{open:?}");
    }

    /// A tool row must be openable by click — `cards` tells where to click.
    #[test]
    fn a_tool_row_is_clickable_on_its_own() {
        let items = [work_at(1)];
        let folds = Folds::from([(1, Fold { open: true })]);
        let r = rows(&items, 60, &folds, crate::lang::Lang::Ko);

        // Since thinking lines also toggle the card fold, the same seq spans several lines — what we check is
        // "which seqs are clickable", not the number of lines.
        let mut by_seq: Vec<i64> = r.cards.values().copied().collect();
        by_seq.sort();
        by_seq.dedup();
        assert_eq!(by_seq, vec![1, 100], "both the card head and the tool row must be clickable");

        // The tool row's index must actually be that tool row — off by one and the wrong thing unfolds.
        let lines = r.plain();
        let row = r.cards.iter().find(|(_, s)| **s == 100).map(|(r, _)| *r).unwrap();
        assert!(lines[row].contains("grep"), "{:?}", lines[row]);
    }

    /// **Tool rows of a folded card aren't clickable** — detail only opens with the card unfolded, so
    /// a row that does nothing when clicked while folded is not a click target.
    #[test]
    fn folded_tool_rows_are_not_clickable() {
        let items = [work_at(1)];
        let r = rows(&items, 60, &Folds::new(), crate::lang::Lang::Ko);
        let mut by_seq: Vec<i64> = r.cards.values().copied().collect();
        by_seq.sort();
        assert_eq!(by_seq, vec![1], "on a folded card only the head may be clickable");
    }

    /// **In a folded card, open tool details aren't visible either** — folding the card means
    /// folding everything. (The symptom users saw was that clicking reasoning to fold the card left the
    /// tool details below fully expanded.)
    #[test]
    fn a_folded_card_hides_open_tool_details() {
        let items = [work_at(1)];
        // The tool detail is open, but the card is folded.
        let folds = Folds::from([(100, Fold { open: true })]);
        let out = plain(&rows(&items, 60, &folds, crate::lang::Lang::Ko));
        assert!(out.iter().any(|l| l.contains("grep")), "the tool row must be visible: {out:?}");
        assert!(
            !out.iter().any(|l| l.contains("인자")),
            "tool details are visible on a folded card: {out:?}"
        );
    }

    /// **Run-time messages show even when folded** — folding a card hides reasoning, not the whole
    /// conversation flow.
    #[test]
    fn a_text_part_shows_even_when_the_card_is_folded() {
        let items = [Item::Work {
            seq: 1,
            title: "커밋하는 중".into(),
            parts: vec![Part::Text("이제 커밋합니다".into()), Part::Step(step_at(100, "exec"))],
        }];
        let out = plain(&rows(&items, 60, &Folds::new(), crate::lang::Lang::Ko));
        assert!(
            out.iter().any(|l| l.contains("이제 커밋합니다")),
            "접혀도 메시지는 보여야 한다: {out:?}"
        );
        assert!(out.iter().any(|l| l.contains("exec")), "{out:?}");
    }

    /// **Run-time messages aren't click targets** — unlike thinking lines, which fold the card when
    /// pressed, a message is content.
    #[test]
    fn a_text_part_is_not_clickable() {
        let items = [Item::Work {
            seq: 1,
            title: "커밋하는 중".into(),
            parts: vec![Part::Text("이제 커밋합니다".into()), Part::Step(step_at(100, "exec"))],
        }];
        let r = rows(&items, 60, &Folds::new(), crate::lang::Lang::Ko);
        let lines = r.plain();
        let row = lines.iter().position(|l| l.contains("이제 커밋합니다")).expect("message row");
        assert!(
            !r.cards.contains_key(&row),
            "a message row was taken as clickable: {:?}",
            lines[row]
        );
    }

    /// **Messages, thinking, and tools stand in the order they came.** In an open card the three must interleave
    /// to show what was thought, what was said, and what was done.
    #[test]
    fn text_thinking_and_tools_interleave_in_the_card() {
        let items = [Item::Work {
            seq: 1,
            title: "커밋하는 중".into(),
            parts: vec![
                Part::Think("먼저 상태를 보자".into()),
                Part::Text("이제 커밋합니다".into()),
                Part::Step(step_at(100, "exec")),
            ],
        }];
        let folds = Folds::from([(1, Fold { open: true })]);
        let out = plain(&rows(&items, 60, &folds, crate::lang::Lang::Ko));
        let think = out.iter().position(|l| l.contains("먼저 상태를 보자")).unwrap();
        let text = out.iter().position(|l| l.contains("이제 커밋합니다")).unwrap();
        let tool = out.iter().position(|l| l.contains("exec")).unwrap();
        assert!(think < text && text < tool, "the arrival order is wrong: {out:?}");
    }

    /// **Thinking lines must be clickable too** — clicking folds and unfolds the card (the same
    /// action as Ctrl+O). It doesn't open tool detail.
    #[test]
    fn a_thinking_line_maps_to_the_card_fold() {
        let items = [work_at(1)];
        let r = rows(&items, 60, &Folds::from([(1, Fold { open: true })]), crate::lang::Lang::Ko);
        let lines = r.plain();
        let row = lines.iter().position(|l| l.contains("먼저 구조를 보자")).expect("reasoning row");
        let seq = r.cards.get(&row).copied().expect("a reasoning line must be clickable");
        assert_eq!(seq, 1, "clicking a reasoning line must fold and unfold the card");
    }

    /// **Open tool detail is set apart in a box.** The "Args/Output/Result/Error" heads stand out in
    /// colour and markers, with the body below. The eye must read at a glance what's an argument and what's a result.
    #[test]
    fn tool_detail_lines_label_their_sections() {
        let out = tool_detail_lines(
            "인자\n{\"cmd\": \"git push\"}\n\n출력\nUp to date",
            60,
            false,
            crate::lang::Lang::Ko,
        );
        let plain: Vec<String> =
            out.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(plain[0].starts_with('┌'), "there must be a top border: {plain:?}");
        assert!(plain.last().unwrap().starts_with('└'), "there must be a bottom border: {plain:?}");
        assert!(plain.iter().any(|l| l.contains("⎿ 인자")), "no arguments heading: {plain:?}");
        assert!(plain.iter().any(|l| l.contains("⎿ 출력")), "no output heading: {plain:?}");
        assert!(plain.iter().any(|l| l.contains("git push")), "no arguments body: {plain:?}");
        assert!(plain.iter().any(|l| l.contains("Up to date")), "no output body: {plain:?}");

        // The error section is painted in the danger colour.
        let err = tool_detail_lines("오류\nboom", 60, false, crate::lang::Lang::Ko);
        assert!(
            err.iter().any(|l| {
                l.spans.iter().any(|s| s.content == "boom" && s.style.fg == Some(theme::danger()))
            }),
            "오류 본문이 위험색이 아니다: {err:?}"
        );
    }

    /// **A failed tool's detail is danger-coloured down to its border.** It must look different from a successful
    /// one so a skim catches what went wrong.
    #[test]
    fn a_failed_tools_detail_box_is_red() {
        let out = tool_detail_lines("인자\n{}", 60, true, crate::lang::Lang::Ko);
        assert!(
            out[0].spans[0].style.fg == Some(theme::danger()),
            "the top border is not in the danger colour"
        );
        assert!(
            out.last().unwrap().spans[0].style.fg == Some(theme::danger()),
            "아래 테두리가 위험색이 아니다"
        );
    }

    /// A tool with nothing to unfold should do nothing when pressed, so it doesn't pretend to be clickable.
    #[test]
    fn a_tool_with_nothing_to_show_is_not_clickable() {
        let items = [Item::Work {
            seq: 1,
            title: "런".into(),
            parts: vec![Part::Step(Step {
                seq: 100,
                name: "todo".into(),
                note: "정리".into(),
                failed: false,
                detail: String::new(),
                diff: None,
            })],
        }];
        let folds = Folds::from([(1, Fold { open: true })]);
        let r = rows(&items, 60, &folds, crate::lang::Lang::Ko);
        assert_eq!(
            r.cards.values().copied().collect::<Vec<_>>(),
            vec![1],
            "상세가 없는 도구가 눌리는 줄로 잡혔다"
        );
        assert!(
            !r.plain().iter().any(|l| l.contains('▸') && l.contains("todo")),
            "{:?}",
            r.plain()
        );
    }

    /// **Unfolding one tool must redraw that item.**
    /// Looking only at the item's own fold would make the cache think "nothing changed" and the screen stays put.
    #[test]
    fn opening_a_tool_row_redraws_the_card() {
        let items = [work_at(1)];
        let card_open = Folds::from([(1, Fold { open: true })]);
        let mut cache = Cache::new();
        cache.layout(&items, 60, &card_open, None, crate::lang::Lang::Ko);
        let before = cache.renders();

        let mut both = card_open.clone();
        both.insert(100, Fold { open: true });
        cache.layout(&items, 60, &both, None, crate::lang::Lang::Ko);
        assert_eq!(cache.renders(), before + 1, "a tool was unfolded but nothing was redrawn");
        assert!(cache.plain().iter().any(|l| l.contains("인자")), "{:?}", cache.plain());
    }

    /// A card whose title hasn't arrived yet must still hold its place.
    #[test]
    fn a_work_card_without_a_title_yet_says_it_is_working() {
        let items = [Item::Work { seq: 1, title: String::new(), parts: vec![] }];
        let out = plain(&rows(&items, 40, &Folds::new(), crate::lang::Lang::Ko));
        assert!(out[0].contains("작업 중"), "{out:?}");
    }
}
