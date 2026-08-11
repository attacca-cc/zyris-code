//! Lays items out into screen lines. **The single source of truth for the conversation screen.**
//!
//! What the widget draws, the scroll-window math, and the folded-card heights all use the same
//! `Vec<Line>` produced here. If the drawing code and the measuring code diverge, scrolling quietly drifts.

use std::collections::HashMap;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::markdown;
use crate::theme;
use crate::timeline::{Item, Part, Step};

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

/// One node's fold state.
///
/// **The default is folded.** Reasoning is for skimming, not reading, and a wall of thinking must not
/// push away the screen the person came to for answers. **Even folded, the tool rows show** — only the reasoning
/// body is hidden. Hiding what was done would leave the person not knowing what the agent is up to.
/// Opening is done by the person with Ctrl+O.
///
/// **Topics open and fold with the run.** A topic renders open while the turn is working and folds
/// itself when the turn ends — so the person watches the sections form live, then is left with a
/// folded summary. That auto behaviour stops the moment the person touches the node: once
/// `user_touched` is set, only the person decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fold {
    pub open: bool,
    /// Set when the person explicitly opened/closed this node. Auto-open/auto-fold then leaves it alone.
    pub user_touched: bool,
}

/// The kind of a foldable node.
///
/// **Cards and chips follow the run; tool rows never do.** A tool's detail is something the person
/// went looking for, so opening every one of them mid-run would bury the card in output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Card,
    Chip,
    Tool,
}

/// The open state rendering should actually show for `kind`. **The one place this rule lives** —
/// the cache's change detection and the renderer must agree, or the cache would think nothing
/// changed when the run ends and the screen would keep the run's shape forever.
fn effective_open(kind: NodeKind, f: &Fold, running: bool) -> bool {
    match kind {
        // **A card is open while it is being worked on and folds itself when it is done.** The
        // answer is no longer inside it — the agent speaking is what ends the card — so what is
        // left once it finishes is the working out, and a finished turn should leave a line
        // saying "Done", not a screenful of thoughts already read.
        NodeKind::Card => {
            if f.user_touched {
                f.open
            } else {
                running
            }
        }
        // **A chip's body stays folded until it is asked for, running or not.** What is worth
        // watching live is the subject and the tools under it; the reasoning itself is the
        // model talking itself round — "actually", "but wait" — and left open it fills the
        // screen with second thoughts while saying nothing about what is being done.
        NodeKind::Chip => f.open,
        NodeKind::Tool => f.open,
    }
}

fn fold_of(folds: &Folds, seq: i64) -> Fold {
    folds.get(&seq).copied().unwrap_or_default()
}

/// What a reasoning chip is called.
///
/// **The server's title wins.** `agent_runtime.rs` labels each block with a small model and updates
/// the event in place, so an untitled chip re-titles itself when that lands. Until then the first
/// sentence stands in — clipped at a sentence end where one is near, so the heading doesn't read
/// as a fragment.
fn chip_title(t: &crate::timeline::Think, lang: crate::lang::Lang) -> String {
    if let Some(title) = t.title.as_ref().filter(|s| !s.trim().is_empty()) {
        return title.clone();
    }
    let first = t
        .text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .trim_start_matches(['#', '-', '*', '•', '>', ' '])
        .trim();
    if first.is_empty() {
        return lang.thinking().to_string();
    }
    // **Prefer a sentence end inside the budget over a hard cut.** A title cut mid-word reads as
    // broken; a whole first sentence reads as a heading. Fullwidth stops are listed too — the
    // model writes Korean and Japanese with those.
    //
    // The first stop is not always the right one: reasoning that opens with "Great!" would give a
    // one-word heading that says nothing. So a break is only taken once there is enough of a
    // sentence to be worth reading, and the last one inside the budget wins.
    let mut cut = 0usize;
    let mut used = 0usize;
    for (i, ch) in first.char_indices() {
        used += markdown::display_width(&ch.to_string()).max(1);
        if used > CHIP_TITLE_WIDTH {
            break;
        }
        if used >= CHIP_TITLE_FLOOR && matches!(ch, '.' | '!' | '?' | '。' | '！' | '？') {
            cut = i + ch.len_utf8();
        }
    }
    if cut > 0 {
        return first[..cut].to_string();
    }
    clip_to(first.to_string(), CHIP_TITLE_WIDTH)
}

/// How wide a fallback chip title may get before it is cut.
const CHIP_TITLE_WIDTH: usize = 56;
/// How much of a sentence there must be before a stop is worth breaking at. Below this the
/// "heading" is an interjection — "Great!" — which says nothing about what follows.
const CHIP_TITLE_FLOOR: usize = 20;

pub type Folds = HashMap<i64, Fold>;

/// What the turn is doing right now, as far as drawing is concerned.
///
/// **Two flags that always travel together.** `running` decides whether cards and chips are open
/// (see [`effective_open`]) and `blink` is the phase a pending tool's dot is drawn at; splitting
/// them across signatures only made every layer carry two more parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Turn {
    pub running: bool,
    /// The bright half of the pending dot's blink. Driven by the frame counter, not a clock, so
    /// drawing stays pure.
    pub blink: bool,
}

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
    rows_with(items, width, folds, None, lang, Turn::default())
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
    turn: Turn,
) -> Rendered {
    let mut cache = Cache::new();
    cache.layout(items, width, folds, active.map(|a| a.seq), turn, lang);
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
/// Looking only at the item's own would make the cache think "nothing changed" when one node
/// unfolds, and the screen would stay put. It carries the **effective** open (after the running-flag
/// rule) of the card's head plus every topic, subtopic and tool node, so both a manual fold and the
/// run ending invalidate the cache.
type Affecting = Vec<(i64, bool)>;

fn affecting(item: &Item, folds: &Folds, running: bool) -> Affecting {
    let at = |seq: i64, k: NodeKind| (seq, effective_open(k, &fold_of(folds, seq), running));
    let Item::Work { seq, parts, .. } = item else {
        // Nothing else folds; its own seq stands in so the vector is never empty.
        return vec![(item.seq(), true)];
    };
    let mut out = vec![at(*seq, NodeKind::Card)];
    for part in parts {
        match part {
            Part::Think(t) => out.push(at(t.seq, NodeKind::Chip)),
            Part::Step(s) => out.push(at(s.seq, NodeKind::Tool)),
        }
    }
    out
}

/// The card that is being worked on right now, if any.
///
/// **It is the last item, not merely the last card.** Once the agent has spoken, its answer stands
/// after the card and the card is finished — reading "the last `Work` in the list" would keep the
/// card it just closed open until a new one started.
fn live_card(items: &[Item], running: bool) -> Option<i64> {
    match items.last() {
        Some(Item::Work { seq, .. }) if running => Some(*seq),
        _ => None,
    }
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
    /// The **effective** open state of every node that has a head, as it was last drawn.
    ///
    /// A click toggles from what is on screen, not from what is stored. A card with no fold state
    /// draws open while its stored `Fold` is `open: false`; flipping the stored value there set
    /// `open: true` and the card did not move.
    open: HashMap<i64, bool>,
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

    /// The effective open state of every node with a head, as last drawn.
    pub fn open_states(&self) -> &HashMap<i64, bool> {
        &self.open
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
        turn: Turn,
        lang: crate::lang::Lang,
    ) {
        // A width change moves every wrap point. Throw it all away.
        if self.width != width {
            self.width = width;
            self.made.clear();
        }
        self.slots.clear();
        self.cards.clear();
        self.open.clear();

        // **Only one card is running: the one at the end.** Passing the turn's flag to every card
        // would re-open every card of the conversation the moment a new turn started.
        let live = live_card(items, turn.running);

        let mut pos = 0usize;
        for item in items {
            let seq = item.seq();
            if skip == Some(seq) {
                continue;
            }
            let turn = Turn { running: live == Some(seq), blink: turn.blink };
            // `affecting` is already exactly "(node, effective open)" for every node in this
            // item, so the click handler reads it from here rather than recomputing the rule.
            let now = affecting(item, folds, turn.running);
            self.open.extend(now.iter().copied());
            let fresh = match self.made.get(&seq) {
                Some((was, had, _)) => was == item && *had == now,
                None => false,
            };
            if !fresh {
                let made = make(item, width, folds, turn, lang);
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
fn make(item: &Item, width: u16, folds: &Folds, turn: Turn, lang: crate::lang::Lang) -> Made {
    let Turn { running, blink } = turn;
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut links: Vec<Vec<crate::markdown::Link>> = Vec::new();
    let mut heads: Vec<(usize, i64)> = Vec::new();
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
            // ── Card head ────────────────────────────────────────────────────────────────────
            // The run's title, the whole card's tool count, and everything it changed. It folds
            // the card away entirely, so it carries a marker of its own.
            let card_open = effective_open(NodeKind::Card, &fold_of(folds, *seq), running);
            // **The head says where the work stands, not what it was called.** While the run goes
            // it carries the server's latest `work_summary` — the line that keeps rewriting itself
            // to say what is happening — and once the agent has spoken it is simply done. Holding
            // the last title there would leave a stale "writing the report" above a finished turn.
            let head = match (running, title.is_empty()) {
                (false, _) => lang.run_done(),
                (true, true) => lang.thinking(),
                (true, false) => title.as_str(),
            };
            let steps = || {
                parts.iter().filter_map(|p| match p {
                    Part::Step(s) => Some(s),
                    _ => None,
                })
            };
            let total = steps().count();
            let (add, rem) = steps().fold((0, 0), |(a, r), s| (a + s.counts().0, r + s.counts().1));
            // **`✻`, not `◆`.** `◆` is what an answer wears, and the head is the opposite of an
            // answer — it is the working out gathered behind one line. A chevron won't do either:
            // it is exactly what the reasoning chips under it use, and the head stopped reading as
            // the thing they hang from. The fold marker goes on the end, where the tool rows put
            // theirs.
            let mut card = vec![
                Span::styled("✻ ", Style::default().fg(theme::topic())),
                Span::styled(
                    head.to_string(),
                    Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
                ),
            ];
            if total > 0 {
                card.push(Span::styled(
                    format!("  ·  {}", lang.tool_count(total)),
                    Style::default().fg(theme::text_muted()),
                ));
            }
            if add + rem > 0 {
                card.extend(counts(add, rem));
            }
            card.push(Span::styled(
                if card_open { "  ▾" } else { "  ▸" },
                Style::default().fg(theme::border_light()),
            ));
            heads.push((out.len(), *seq));
            out.push(Line::from(card));
            links.push(Vec::new());
            if !card_open {
                return Made { lines: out, links, heads };
            }

            // ── Children, in arrival order ───────────────────────────────────────────────────
            // Reasoning chips and tool rows stand at **the same level**. The order they arrived in
            // is what says "thought this, then did that"; regrouping them would lose it.
            for part in parts {
                match part {
                    Part::Think(t) => {
                        let open = effective_open(NodeKind::Chip, &fold_of(folds, t.seq), running);
                        let title = chip_title(t, lang);
                        // **The title alone.** A chip used to carry the tools that ran under it
                        // and what they changed, but the tool rows are right there below it
                        // saying the same thing — and the card head already totals the run
                        // (decided with the user, 2026-08-11).
                        let spans = vec![
                            pad(),
                            Span::styled(
                                if open { "▾ " } else { "▸ " },
                                Style::default().fg(theme::topic()),
                            ),
                            // Not bold: the card head is the heading, and a chip is one step down.
                            Span::styled(title.clone(), Style::default().fg(theme::topic())),
                        ];
                        heads.push((out.len(), t.seq));
                        out.push(Line::from(spans));
                        links.push(Vec::new());
                        // **The body is only drawn when it says more than the title.** With no
                        // server title, one short sentence of reasoning becomes both, and the
                        // chip printed the same line twice.
                        let body = t.text.trim();
                        if !open || body.is_empty() || body == title {
                            continue;
                        }
                        // The reasoning body, dim so the answer beside it keeps the eye.
                        let rendered =
                            markdown::render_rich(&t.text, body_width(width).saturating_sub(2));
                        for (li, line) in rendered.lines.into_iter().enumerate() {
                            let mut spans = vec![
                                pad(),
                                pad(),
                                Span::styled("┊ ", Style::default().fg(theme::border_light())),
                            ];
                            let prefix_w = spans
                                .iter()
                                .map(|s| markdown::display_width(&s.content))
                                .sum::<usize>();
                            spans.extend(line.spans.into_iter().map(|s| {
                                Span::styled(s.content.to_string(), s.style.fg(theme::text_muted()))
                            }));
                            out.push(Line::from(spans));
                            links.push(shift_links(&rendered.links[li], prefix_w));
                        }
                    }
                    Part::Step(step) => {
                        let open = effective_open(NodeKind::Tool, &fold_of(folds, step.seq), false);
                        let (rows, clickable) = tool_row(step, open, blink, width, lang);
                        if clickable {
                            heads.push((out.len(), step.seq));
                        }
                        links.extend(std::iter::repeat_with(Vec::new).take(rows.len()));
                        out.extend(rows);
                    }
                }
            }
        }
    }
    Made { lines: out, links, heads }
}

/// One tool row: the status dot, the short name, what it was run against, how much it changed,
/// and — when the person opened it — its detail.
/// Returns the row's lines, and whether the row is a fold target — a tool with nothing to expand
/// does nothing when pressed, so it must not look pressable.
fn tool_row(
    step: &Step,
    open: bool,
    blink: bool,
    width: u16,
    lang: crate::lang::Lang,
) -> (Vec<Line<'static>>, bool) {
    use crate::tool_view::{Detail, ToolState};
    let mut out: Vec<Line<'static>> = Vec::new();
    let can_open = !matches!(step.detail, Detail::None);
    // **The dot reads the call, not the turn.** Pending is yellow and blinks, a failure is red, a
    // return is green. Painting "the last tool of a running turn" yellow instead would call every
    // other in-flight call finished.
    let dot = match step.state {
        ToolState::Failed => Span::styled("● ", Style::default().fg(theme::danger())),
        ToolState::Ok => Span::styled("● ", Style::default().fg(theme::success())),
        ToolState::Pending => {
            let style = Style::default().fg(theme::warning());
            Span::styled("● ", if blink { style } else { style.add_modifier(Modifier::DIM) })
        }
    };
    let mut head = vec![
        pad(),
        dot,
        Span::styled(
            step.name.clone(),
            Style::default().fg(theme::tool()).add_modifier(Modifier::BOLD),
        ),
    ];
    if !step.action.is_empty() {
        head.push(Span::styled(
            format!("  {}", step.action),
            Style::default().fg(theme::tool_arg()),
        ));
    }
    let (add, rem) = step.counts();
    if add + rem > 0 {
        head.extend(counts(add, rem));
    }
    head.push(Span::styled(
        match (can_open, open) {
            (false, _) => "",
            (true, false) => "  ▸",
            (true, true) => "  ▾",
        },
        Style::default().fg(theme::border_light()),
    ));
    out.push(Line::from(head));
    if open {
        out.extend(detail_lines(&step.detail, width, step.state, lang));
    }
    (out, can_open)
}

/// The indent a tool's detail sits at. Deep enough to read as belonging to the row above,
/// shallow enough that a diff still has room.
const DETAIL_PAD: &str = "    ";

/// Draws an opened tool row's detail. **Per shape, not one flat dump** — a shell log, a diff and a
/// match list are read three different ways, and a JSON dump of any of them is read none.
fn detail_lines(
    detail: &crate::tool_view::Detail,
    width: u16,
    state: crate::tool_view::ToolState,
    lang: crate::lang::Lang,
) -> Vec<Line<'static>> {
    use crate::tool_view::{Detail, ToolState};
    let failed = state == ToolState::Failed;
    let inner = body_width(width).saturating_sub(DETAIL_PAD.len() as u16).max(8);
    let mut out: Vec<Line<'static>> = Vec::new();
    // The gutter that ties the detail to its row. Red when the call failed, so a failure is
    // visible without reading a word of it.
    let rail = if failed { theme::danger() } else { theme::border_light() };
    let row = |mark: &str, spans: Vec<Span<'static>>| {
        let mut line = vec![
            Span::styled(DETAIL_PAD, Style::default()),
            Span::styled(mark.to_string(), Style::default().fg(rail)),
        ];
        line.extend(spans);
        Line::from(line)
    };
    let plain = |text: &str, colour: ratatui::style::Color| {
        wrap_plain(text, inner)
            .into_iter()
            .map(|l| vec![Span::styled(l, Style::default().fg(colour))])
            .collect::<Vec<_>>()
    };

    match detail {
        Detail::None => {}
        // Only the diff is shown. The same content as JSON beside it would double the height and
        // the diff is what gets read.
        Detail::Diff(d) => {
            for line in &d.lines {
                out.push(diff_line(line, inner as usize, lang));
            }
        }
        Detail::Exec { exit, timed_out, out: stdout, err } => {
            // The headline first: whether it finished, and how. A quiet success still says so —
            // an empty detail reads as a broken tool.
            let (label, colour) = if *timed_out {
                (lang.detail_timed_out().to_string(), theme::danger())
            } else {
                match exit {
                    Some(0) | None => (lang.detail_ok().to_string(), theme::success()),
                    Some(c) => (lang.detail_exit_code(*c), theme::danger()),
                }
            };
            out.push(row("⎿ ", vec![Span::styled(label, Style::default().fg(colour))]));
            if stdout.trim().is_empty() && err.trim().is_empty() {
                out.push(row(
                    "  ",
                    vec![Span::styled(
                        lang.detail_no_output().to_string(),
                        Style::default().fg(theme::text_muted()),
                    )],
                ));
            }
            for spans in plain(stdout.trim_end(), theme::text()) {
                out.push(row("  ", spans));
            }
            if !err.trim().is_empty() {
                out.push(row(
                    "⎿ ",
                    vec![Span::styled(
                        "stderr".to_string(),
                        Style::default().fg(theme::danger()).add_modifier(Modifier::BOLD),
                    )],
                ));
                for spans in plain(err.trim_end(), theme::danger()) {
                    out.push(row("  ", spans));
                }
            }
        }
        Detail::Hits { scanned, hits, truncated } => {
            out.push(row(
                "⎿ ",
                vec![Span::styled(
                    lang.detail_hits(hits.len(), *scanned),
                    Style::default().fg(theme::accent()),
                )],
            ));
            for h in hits {
                // `path:line` first, then the matched text — the eye scans down the left column.
                let place = format!("{}:{}", h.path, h.line);
                let room = (inner as usize).saturating_sub(markdown::display_width(&place) + 2);
                out.push(row(
                    "  ",
                    vec![
                        Span::styled(place, Style::default().fg(theme::tool_arg())),
                        Span::styled(
                            format!("  {}", clip_to(h.text.clone(), room.max(8))),
                            Style::default().fg(theme::text_muted()),
                        ),
                    ],
                ));
            }
            if *truncated {
                out.push(row(
                    "  ",
                    vec![Span::styled(
                        lang.detail_truncated().to_string(),
                        Style::default().fg(theme::border_light()),
                    )],
                ));
            }
        }
        Detail::Paths { paths, truncated } => {
            out.push(row(
                "⎿ ",
                vec![Span::styled(
                    lang.detail_found(paths.len()),
                    Style::default().fg(theme::accent()),
                )],
            ));
            for p in paths {
                out.push(row(
                    "  ",
                    vec![Span::styled(
                        clip_to(p.clone(), inner as usize),
                        Style::default().fg(theme::text_muted()),
                    )],
                ));
            }
            if *truncated {
                out.push(row(
                    "  ",
                    vec![Span::styled(
                        lang.detail_truncated().to_string(),
                        Style::default().fg(theme::border_light()),
                    )],
                ));
            }
        }
        Detail::Body { label, text } => {
            if !label.is_empty() {
                out.push(row(
                    "⎿ ",
                    vec![Span::styled(
                        label.clone(),
                        Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
                    )],
                ));
            }
            for spans in plain(text, theme::text()) {
                out.push(row("  ", spans));
            }
        }
        // The fallback. **Not parsed as markdown** — JSON's `*` and `_` eaten as emphasis would
        // break the original text.
        Detail::Json { args, result } => {
            for (head, body, colour) in [
                (lang.detail_args(), args, theme::tool_arg()),
                (
                    if failed { lang.detail_error() } else { lang.detail_result() },
                    result,
                    if failed { theme::danger() } else { theme::accent() },
                ),
            ] {
                if body.trim().is_empty() {
                    continue;
                }
                out.push(row(
                    "⎿ ",
                    vec![Span::styled(
                        head.to_string(),
                        Style::default().fg(colour).add_modifier(Modifier::BOLD),
                    )],
                ));
                let base = if failed { theme::danger() } else { theme::text_muted() };
                for line in wrap_plain(body, inner) {
                    out.push(row("  ", json_line(&line, base)));
                }
            }
        }
    }
    out
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
/// `width` is the width the text itself may use — the caller has already taken the indent off.
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
        Span::styled(DETAIL_PAD, Style::default().fg(theme::border_light())),
        Span::styled(clip_to(text, width), Style::default().fg(colour)),
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

/// Colours one pretty-printed JSON line: a `"key"` in front of a colon stands out in `tool_arg`,
/// the rest keeps `base`. Values and non-key lines stay `base` — enough to scan an args/result
/// block without parsing it.
fn json_line(raw: &str, base: ratatui::style::Color) -> Vec<Span<'static>> {
    let trimmed = raw.trim_start();
    let indent = raw.len() - trimmed.len();
    let mut spans = vec![Span::styled(" ".repeat(indent), Style::default().fg(base))];
    if let Some(after_quote) = trimmed.strip_prefix('"') {
        if let Some(end) = after_quote.find('"') {
            // **The opening quote goes back on.** Slicing after `strip_prefix` dropped it, so
            // every key was drawn as `command":` — it read like broken JSON.
            let key = format!("\"{}", &after_quote[..=end]);
            spans.push(Span::styled(key, Style::default().fg(theme::tool_arg())));
            spans.push(Span::styled(after_quote[end + 1..].to_string(), Style::default().fg(base)));
            return spans;
        }
    }
    spans.push(Span::styled(trimmed.to_string(), Style::default().fg(base)));
    spans
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
            action: "viewport".into(),
            state: crate::tool_view::ToolState::Ok,
            detail: crate::tool_view::Detail::Json {
                args: "{\"pattern\": \"viewport\"}".into(),
                result: "{\"hits\": 1}".into(),
            },
        }
    }

    fn think_at(seq: i64, title: &str, text: &str) -> Part {
        Part::Think(crate::timeline::Think { seq, title: Some(title.into()), text: text.into() })
    }

    fn work_at(seq: i64) -> Item {
        Item::Work {
            seq,
            title: "스크롤 계산 위치를 찾는 중".into(),
            parts: vec![
                think_at(seq * 10, "먼저 구조를 본다", "rows.rs가 정본이므로 거기부터 본다"),
                Part::Step(step_at(seq * 100, "grep")),
            ],
        }
    }

    fn work() -> Item {
        work_at(1)
    }

    /// Folds that open the card and every chip in it — the way a person clicking them would — so
    /// the reasoning shows even when no turn is running.
    fn open_all(item: &Item) -> Folds {
        let Item::Work { seq, parts, .. } = item else {
            return Folds::new();
        };
        let mut f = Folds::new();
        f.insert(*seq, Fold { open: true, user_touched: true });
        for p in parts {
            if let Part::Think(t) = p {
                f.insert(t.seq, Fold { open: true, user_touched: true });
            }
        }
        f
    }

    /// Draws with the last card being worked on right now — the state most of these assertions are
    /// about, since a finished stretch folds itself away.
    fn live(items: &[Item], width: u16, folds: &Folds, lang: crate::lang::Lang) -> Rendered {
        rows_with(items, width, folds, None, lang, Turn { running: true, blink: false })
    }

    /// The first chip's fold key, so a test can open a single chip.
    fn chip_key(item: &Item) -> i64 {
        let Item::Work { parts, .. } = item else {
            panic!("not a work item");
        };
        parts
            .iter()
            .find_map(|p| match p {
                Part::Think(t) => Some(t.seq),
                _ => None,
            })
            .expect("the work item has no chip")
    }

    /// **A folded chip hides thinking, not what was done.** Tool use is part of the flow; buried
    /// under a fold the person can't tell what the agent is doing.
    #[test]
    fn a_folded_chip_hides_thinking_but_shows_tools() {
        let shut = Folds::from([(chip_key(&work()), Fold { open: false, user_touched: true })]);
        let out = plain(&live(&[work()], 40, &shut, crate::lang::Lang::Ko));
        assert!(out[0].contains("스크롤 계산 위치를 찾는 중"), "no card head: {out:?}");
        assert!(out.iter().any(|l| l.contains("먼저 구조를 본다")), "no chip title: {out:?}");
        assert!(out.iter().any(|l| l.contains("grep")), "the tool row must show: {out:?}");
        assert!(
            !out.iter().any(|l| l.contains('┊')),
            "the reasoning body must stay folded: {out:?}"
        );
        assert!(out.iter().any(|l| l.contains("도구 1개")), "the head still counts tools: {out:?}");
    }

    /// Unfolded, thinking interleaves between tools — in the order it came.
    #[test]
    fn an_open_card_interleaves_thinking_with_tools() {
        let folds = open_all(&work());
        let out = plain(&rows(&[work()], 40, &folds, crate::lang::Lang::Ko));
        let think = out.iter().position(|l| l.contains("rows.rs가 정본")).expect("no thought");
        let tool = out.iter().position(|l| l.contains("grep")).expect("no tool");
        assert!(think < tool, "the thought must come before the tool: {out:?}");
    }

    #[test]
    fn an_open_card_shows_reasoning_and_steps() {
        let folds = open_all(&work());
        let out = plain(&rows(&[work()], 40, &folds, crate::lang::Lang::Ko));
        assert!(out.iter().any(|l| l.contains("rows.rs가 정본")), "{out:?}");
        assert!(out.iter().any(|l| l.contains("grep")), "{out:?}");
    }

    /// **The head line says how many times tools were used.** What's counted is `Part::Step`, i.e. tool calls,
    /// so the "steps" suffix doesn't point at them.
    #[test]
    fn a_card_head_counts_tools_not_steps() {
        let out = plain(&live(&[work()], 60, &Folds::new(), crate::lang::Lang::Ko));
        assert!(out[0].contains("도구 1개"), "{out:?}");
        assert!(!out[0].contains("단계"), "{out:?}");
    }

    /// **A finished stretch of working folds itself into one line.** What the agent said now
    /// stands outside the card, so nothing is taken away by folding it — and a turn that left its
    /// whole working out on screen buried the answer the person came back to read.
    #[test]
    fn a_finished_run_folds_its_working_away() {
        let out = plain(&rows(&[work()], 40, &Folds::new(), crate::lang::Lang::Ko));
        assert_eq!(out.len(), 1, "a finished card must be one line: {out:?}");
        assert!(out[0].contains(crate::lang::Lang::Ko.run_done()), "{out:?}");
        assert!(out[0].contains("도구 1개"), "the head still says what it did: {out:?}");
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
        let folds = open_all(&items[0]);
        let out = plain(&rows(&items, 60, &folds, crate::lang::Lang::Ko));
        let row = out.iter().find(|l| l.contains("grep")).expect("{out:?}");
        assert!(!row.contains("zyris__"), "the raw wire name is shown: {row:?}");
    }

    /// **Tools must be a different colour from reasoning.** In an open card, reasoning fills the screen;
    /// if tools are also dim, "what was done" gets buried in the thinking pile.
    #[test]
    fn a_tool_row_stands_out_from_the_reasoning_around_it() {
        let items = [work_at(1)];
        let folds = open_all(&items[0]);
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
        let folds = open_all(&items[0]);
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
    fn a_card_head_shows_how_much_changed() {
        let d = crate::tools::diff::Diff::parse("-a\n+b\n", "src/app.rs", 12, 3).unwrap();
        let items = [Item::Work {
            seq: 1,
            title: "고치는 중".into(),
            parts: vec![Part::Step(Step {
                seq: 100,
                name: "edit".into(),
                action: "src/app.rs".into(),
                state: crate::tool_view::ToolState::Ok,
                detail: crate::tool_view::Detail::Diff(d),
            })],
        }];
        let out = plain(&live(&items, 60, &Folds::new(), crate::lang::Lang::Ko));
        assert!(out[0].contains("+12"), "the header row carries no counts: {out:?}");
        assert!(out[0].contains("−3"), "the header row carries no counts: {out:?}");
        assert!(
            out.iter().any(|l| l.contains("src/app.rs")),
            "the tool row must show what it changed: {out:?}"
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
        let folds = open_all(&work());
        let out = plain(&rows(&[work()], 40, &folds, crate::lang::Lang::Ko));
        let think = out
            .iter()
            .find(|l| l.contains("rows.rs가 정본") && l.contains('┊'))
            .expect("no reasoning body row");
        assert!(
            think.starts_with("    ┊"),
            "reasoning must use its own gutter, not the code block's: {think:?}"
        );
    }

    /// Reasoning must be dimmer than the answer. At the same brightness, the conclusion isn't visible.
    #[test]
    fn reasoning_is_dimmer_than_the_answer() {
        let folds = open_all(&work());
        let r = rows(&[work()], 40, &folds, crate::lang::Lang::Ko);
        let joined =
            |l: &Line<'static>| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() };
        let line = r
            .lines
            .iter()
            .find(|l| joined(l).contains("rows.rs가 정본") && joined(l).contains('┊'))
            .expect("no reasoning body row");
        assert!(
            line.spans
                .iter()
                .skip(3)
                .filter(|s| !s.content.trim().is_empty())
                .all(|s| s.style.fg == Some(theme::text_muted())),
            "the reasoning body is not dimmed: {:?}",
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
            r.lines
                .iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.style.fg == Some(crate::theme::danger())),
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
        for folds in [Folds::new(), Folds::from([(2, Fold { open: true, user_touched: true })])] {
            let want = rows(&items, 40, &folds, crate::lang::Lang::Ko);
            let mut cache = Cache::new();
            cache.layout(
                &items,
                40,
                &folds,
                None,
                Turn { running: false, blink: false },
                crate::lang::Lang::Ko,
            );

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
        cache.layout(
            &items,
            40,
            &folds,
            None,
            Turn { running: false, blink: false },
            crate::lang::Lang::Ko,
        );
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

        cache.layout(
            &items,
            40,
            &folds,
            None,
            Turn { running: false, blink: false },
            crate::lang::Lang::Ko,
        );
        let first = cache.renders();
        assert_eq!(first, items.len() as u64, "everything is drawn the first time");

        cache.layout(
            &items,
            40,
            &folds,
            None,
            Turn { running: false, blink: false },
            crate::lang::Lang::Ko,
        );
        assert_eq!(cache.renders(), first, "unchanged, not a single row is drawn again");

        // A delta was appended to the answer — only that item should be redrawn.
        if let Item::Agent { text, .. } = &mut items[2] {
            text.push_str("| c | 3 |\n");
        }
        cache.layout(
            &items,
            40,
            &folds,
            None,
            Turn { running: false, blink: false },
            crate::lang::Lang::Ko,
        );
        assert_eq!(cache.renders(), first + 1, "only the changed one is drawn again");
    }

    /// Opening one chip redraws only the card it is in.
    ///
    /// **The cache compares every fold that affects an item, not just the item's own.** Looking
    /// only at the item's own would make it think nothing changed, and the screen would stay put.
    #[test]
    fn opening_a_chip_redraws_only_that_card() {
        let items = mixed();
        let mut cache = Cache::new();
        cache.layout(
            &items,
            40,
            &Folds::new(),
            None,
            Turn { running: false, blink: false },
            crate::lang::Lang::Ko,
        );
        let before = cache.renders();

        let chip = chip_key(&items[1]);
        let open = Fold { open: true, user_touched: true };
        let opened = Folds::from([(chip, open), (items[1].seq(), open)]);
        cache.layout(
            &items,
            40,
            &opened,
            None,
            Turn { running: false, blink: false },
            crate::lang::Lang::Ko,
        );
        assert_eq!(cache.renders(), before + 1, "only the card holding that chip is drawn again");
        assert!(
            cache.plain().iter().any(|l| l.contains("rows.rs가 정본")),
            "once opened, the reasoning must show"
        );
    }

    /// A width change moves every wrap point — everything must be redrawn.
    #[test]
    fn a_width_change_redraws_everything() {
        let items = mixed();
        let folds = Folds::new();
        let mut cache = Cache::new();
        cache.layout(
            &items,
            40,
            &folds,
            None,
            Turn { running: false, blink: false },
            crate::lang::Lang::Ko,
        );
        let before = cache.renders();

        cache.layout(
            &items,
            80,
            &folds,
            None,
            Turn { running: false, blink: false },
            crate::lang::Lang::Ko,
        );
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
        cache.layout(
            &items,
            40,
            &Folds::new(),
            Some(2),
            Turn { running: false, blink: false },
            crate::lang::Lang::Ko,
        );
        assert!(
            !cache.plain().iter().any(|l| l.contains("어느 쪽으로 갈까요")),
            "the question being answered must not appear twice: {:?}",
            cache.plain()
        );

        cache.layout(
            &items,
            40,
            &Folds::new(),
            None,
            Turn { running: false, blink: false },
            crate::lang::Lang::Ko,
        );
        assert!(
            cache.plain().iter().any(|l| l.contains("어느 쪽으로 갈까요")),
            "답이 끝나면 대화에 남아야 한다"
        );
    }

    /// A tool starts as one line; unfolded, its args and result come out.
    #[test]
    fn a_tool_row_starts_as_one_line_and_opens_into_its_detail() {
        let items = [work_at(1)];
        let card_open = open_all(&items[0]);

        let shut = plain(&rows(&items, 60, &card_open, crate::lang::Lang::Ko));
        assert!(shut.iter().any(|l| l.contains("grep")), "{shut:?}");
        assert!(
            !shut.iter().any(|l| l.contains("viewport\"}")),
            "the detail shows while folded: {shut:?}"
        );
        assert!(
            shut.iter().any(|l| l.contains("grep") && l.contains('▸')),
            "nothing says the row can be opened: {shut:?}"
        );

        let mut both = card_open.clone();
        both.insert(100, Fold { open: true, user_touched: true });
        let open = plain(&rows(&items, 60, &both, crate::lang::Lang::Ko));
        assert!(open.iter().any(|l| l.contains("인자")), "the detail is not shown: {open:?}");
        assert!(open.iter().any(|l| l.contains("viewport")), "{open:?}");
    }

    /// A tool row must be openable by click — `cards` tells where to click.
    #[test]
    fn a_tool_row_is_clickable_on_its_own() {
        let items = [work_at(1)];
        let folds = open_all(&items[0]);
        let r = rows(&items, 60, &folds, crate::lang::Lang::Ko);

        // The tool row must itself be a fold target.
        assert_eq!(
            r.cards.values().filter(|s| **s == 100).count(),
            1,
            "the tool row must be clickable"
        );

        // The tool row's index must actually be that tool row — off by one and the wrong thing unfolds.
        let lines = r.plain();
        let row = r.cards.iter().find(|(_, s)| **s == 100).map(|(r, _)| *r).unwrap();
        assert!(lines[row].contains("grep"), "{:?}", lines[row]);
    }

    /// **Nothing inside a folded card is clickable** — its rows aren't on screen, so a click
    /// target there would point at whatever happens to be drawn at that row instead.
    #[test]
    fn a_folded_card_has_nothing_clickable_inside_it() {
        let items = [work_at(1)];
        let folds = Folds::from([(1, Fold { open: false, user_touched: true })]);
        let r = rows(&items, 60, &folds, crate::lang::Lang::Ko);
        let by_seq: Vec<i64> = r.cards.values().copied().collect();
        assert_eq!(by_seq, vec![1], "only the card head is clickable when it is folded");
    }

    /// **A folded card hides open tool details too** — folding the card means folding everything
    /// under it. (The symptom was that folding a card left the tool details below fully expanded.)
    #[test]
    fn a_folded_card_hides_open_tool_details() {
        let items = [work_at(1)];
        let folds = Folds::from([
            (1, Fold { open: false, user_touched: true }),
            (100, Fold { open: true, user_touched: true }),
        ]);
        let out = plain(&rows(&items, 60, &folds, crate::lang::Lang::Ko));
        assert!(!out.iter().any(|l| l.contains("grep")), "a folded card hides its rows: {out:?}");
        assert!(!out.iter().any(|l| l.contains("인자")), "tool details are visible: {out:?}");
    }

    /// **What the agent said survives the card folding away.** It is a message of its own, not one
    /// of the card's parts, so folding the working out never takes the answer with it.
    #[test]
    fn folding_a_card_never_hides_what_was_said() {
        let items = [
            Item::Work {
                seq: 1,
                title: "커밋하는 중".into(),
                parts: vec![Part::Step(step_at(100, "exec"))],
            },
            Item::Agent { seq: 2, text: "이제 커밋합니다".into() },
        ];
        let folds = Folds::new();
        let out = plain(&rows(&items, 60, &folds, crate::lang::Lang::Ko));
        assert!(!out.iter().any(|l| l.contains("exec")), "a finished card must fold: {out:?}");
        assert!(out.iter().any(|l| l.contains("이제 커밋합니다")), "{out:?}");
    }

    /// **Thinking lines must be clickable too** — clicking folds and unfolds the card (the same
    /// action as Ctrl+O). It doesn't open tool detail.
    #[test]
    fn a_thinking_line_maps_to_the_card_fold() {
        let items = [work_at(1)];
        let folds = open_all(&items[0]);
        let r = rows(&items, 60, &folds, crate::lang::Lang::Ko);
        let lines = r.plain();
        // The reasoning body is content, not a handle — no reasoning line is a fold target.
        for (i, l) in lines.iter().enumerate() {
            if l.contains("rows.rs가 정본") && l.contains("┊") {
                assert!(!r.cards.contains_key(&i), "reasoning content is clickable: {l:?}");
            }
        }
    }

    /// **A detail is drawn per shape, not as one flat dump.** A shell log, a diff and a match list
    /// are read three different ways, and a JSON dump of any of them is read none.
    #[test]
    fn an_exec_detail_leads_with_how_it_finished_then_its_output() {
        use crate::tool_view::{Detail, ToolState};
        let d = Detail::Exec {
            exit: Some(0),
            timed_out: false,
            out: "Up to date".into(),
            err: String::new(),
        };
        let out = detail_lines(&d, 60, ToolState::Ok, crate::lang::Lang::Ko);
        let plain: Vec<String> =
            out.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(plain[0].contains("완료"), "no headline saying it finished: {plain:?}");
        assert!(plain.iter().any(|l| l.contains("Up to date")), "no output body: {plain:?}");
        assert!(!plain.iter().any(|l| l.contains("exit_code")), "raw JSON keys show: {plain:?}");
    }

    /// A failing exit code is the one thing that must not be lost — 0 and 3 read the same otherwise.
    #[test]
    fn a_failed_exec_detail_says_its_exit_code_in_the_danger_colour() {
        use crate::tool_view::{Detail, ToolState};
        let d = Detail::Exec {
            exit: Some(3),
            timed_out: false,
            out: String::new(),
            err: "error[E0308]".into(),
        };
        let out = detail_lines(&d, 60, ToolState::Failed, crate::lang::Lang::Ko);
        let plain: Vec<String> =
            out.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(plain.iter().any(|l| l.contains("종료 코드 3")), "{plain:?}");
        assert!(plain.iter().any(|l| l.contains("E0308")), "{plain:?}");
        assert!(
            out.iter().flat_map(|l| &l.spans).any(|s| s.style.fg == Some(theme::danger())),
            "a failure must be visible without reading it"
        );
    }

    /// A quiet success still has to say something. An empty detail reads as a broken tool.
    #[test]
    fn a_silent_command_still_says_something() {
        use crate::tool_view::{Detail, ToolState};
        let d = Detail::Exec {
            exit: Some(0),
            timed_out: false,
            out: String::new(),
            err: String::new(),
        };
        let out = detail_lines(&d, 60, ToolState::Ok, crate::lang::Lang::Ko);
        let plain: Vec<String> =
            out.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(plain.iter().any(|l| l.contains("출력 없음")), "{plain:?}");
    }

    /// Matches are drawn `path:line` first, so the eye scans down the left column.
    #[test]
    fn a_grep_detail_puts_the_place_before_the_matched_text() {
        use crate::tool_view::{Detail, Hit, ToolState};
        let d = Detail::Hits {
            scanned: 42,
            hits: vec![Hit {
                path: "src/rows.rs".into(),
                line: 88,
                text: "fn row_line() {".into(),
            }],
            truncated: false,
        };
        let out = detail_lines(&d, 60, ToolState::Ok, crate::lang::Lang::Ko);
        let plain: Vec<String> =
            out.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(plain[0].contains("42"), "no scanned count: {plain:?}");
        let row = plain.iter().find(|l| l.contains("fn row_line")).expect("no match row");
        assert!(
            row.find("src/rows.rs:88").unwrap() < row.find("fn row_line").unwrap(),
            "the place must come first: {row:?}"
        );
    }

    /// **"nothing more matched" and "we stopped looking" must not read the same.**
    #[test]
    fn a_cut_short_result_says_so() {
        use crate::tool_view::{Detail, ToolState};
        let d = Detail::Paths { paths: vec!["a.rs".into()], truncated: true };
        let out = detail_lines(&d, 60, ToolState::Ok, crate::lang::Lang::Ko);
        let plain: Vec<String> =
            out.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(plain.iter().any(|l| l.contains("여기까지만")), "{plain:?}");
    }

    /// **A JSON key keeps both its quotes.** Slicing after `strip_prefix('"')` dropped the opening
    /// one, so every key was drawn as `command":` and the block read like broken JSON.
    #[test]
    fn a_json_key_keeps_its_opening_quote() {
        use crate::tool_view::{Detail, ToolState};
        let d = Detail::Json {
            args: "{\n  \"command\": \"git push\"\n}".into(),
            result: String::new(),
        };
        let out = detail_lines(&d, 60, ToolState::Ok, crate::lang::Lang::Ko);
        let plain: Vec<String> =
            out.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(
            plain.iter().any(|l| l.contains("\"command\":")),
            "the opening quote was eaten: {plain:?}"
        );
    }

    /// The fallback still labels its two halves — the eye must tell an argument from a result.
    #[test]
    fn a_json_detail_labels_its_sections() {
        use crate::tool_view::{Detail, ToolState};
        let d = Detail::Json {
            args: "{\n  \"cmd\": \"git push\"\n}".into(),
            result: "{\n  \"ok\": true\n}".into(),
        };
        let out = detail_lines(&d, 60, ToolState::Ok, crate::lang::Lang::Ko);
        let plain: Vec<String> =
            out.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(plain.iter().any(|l| l.contains("⎿ 인자")), "no arguments heading: {plain:?}");
        assert!(plain.iter().any(|l| l.contains("⎿ 결과")), "no result heading: {plain:?}");
        assert!(plain.iter().any(|l| l.contains("git push")), "no arguments body: {plain:?}");
    }

    /// A tool with nothing to unfold should do nothing when pressed, so it doesn't pretend to be clickable.
    #[test]
    fn a_tool_with_nothing_to_show_is_not_clickable() {
        let item = Item::Work {
            seq: 1,
            title: "런".into(),
            parts: vec![Part::Step(Step {
                seq: 100,
                name: "todo".into(),
                action: "정리".into(),
                state: crate::tool_view::ToolState::Ok,
                detail: crate::tool_view::Detail::None,
            })],
        };
        let folds = open_all(&item);
        let r = rows(&[item], 60, &folds, crate::lang::Lang::Ko);
        assert!(
            !r.cards.values().any(|s| *s == 100),
            "a tool with no detail was taken as clickable"
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
        let card_open = open_all(&items[0]);
        let mut cache = Cache::new();
        cache.layout(
            &items,
            60,
            &card_open,
            None,
            Turn { running: false, blink: false },
            crate::lang::Lang::Ko,
        );
        let before = cache.renders();

        let mut both = card_open.clone();
        both.insert(100, Fold { open: true, user_touched: true });
        cache.layout(
            &items,
            60,
            &both,
            None,
            Turn { running: false, blink: false },
            crate::lang::Lang::Ko,
        );
        assert_eq!(cache.renders(), before + 1, "a tool was unfolded but nothing was redrawn");
        assert!(cache.plain().iter().any(|l| l.contains("인자")), "{:?}", cache.plain());
    }

    /// A stretch whose title hasn't arrived yet must still hold its place — the server writes the
    /// first `work_summary` a moment after the run starts.
    #[test]
    fn a_work_card_without_a_title_yet_says_it_is_thinking() {
        let items = [Item::Work { seq: 1, title: String::new(), parts: vec![] }];
        let out = plain(&live(&items, 40, &Folds::new(), crate::lang::Lang::Ko));
        assert!(out[0].contains(crate::lang::Lang::Ko.thinking()), "{out:?}");
    }

    /// Found against a real session: reasoning that opened with "Great!" became the whole heading.
    /// **A stop only breaks a title once there is a sentence worth reading.**
    #[test]
    fn an_interjection_is_not_taken_as_the_whole_title() {
        let t = crate::timeline::Think {
            seq: 1,
            title: None,
            text: "Great! Zyris Agent has three boards. I will fetch all of them.".into(),
        };
        let got = chip_title(&t, crate::lang::Lang::En);
        assert_ne!(got, "Great!", "a one-word interjection says nothing about what follows");
        assert!(got.starts_with("Great!"), "{got:?}");
        assert!(got.len() > "Great!".len(), "{got:?}");
    }

    /// The server's title always wins — it is what the small model wrote for this block.
    #[test]
    fn a_server_title_wins_over_the_first_sentence() {
        let t = crate::timeline::Think {
            seq: 1,
            title: Some("파일을 훑는 중".into()),
            text: "먼저 rows.rs를 본다.".into(),
        };
        assert_eq!(chip_title(&t, crate::lang::Lang::Ko), "파일을 훑는 중");
    }

    /// Found against a real session: with no server title, one short sentence of reasoning became
    /// both the title and the body, and the chip printed the same line twice.
    #[test]
    fn a_chip_does_not_repeat_a_body_its_title_already_says() {
        let item = Item::Work {
            seq: 1,
            title: "런".into(),
            parts: vec![Part::Think(crate::timeline::Think {
                seq: 2,
                title: None,
                text: "All done. Let me summarize the result for the user.".into(),
            })],
        };
        let folds = open_all(&item);
        let out = plain(&rows(&[item], 78, &folds, crate::lang::Lang::En));
        let said = out.iter().filter(|l| l.contains("All done.")).count();
        assert_eq!(said, 1, "the title and the body said the same thing twice: {out:?}");
    }

    /// A body that says more than its title is still drawn.
    #[test]
    fn a_chip_still_shows_a_body_that_says_more() {
        let item = Item::Work {
            seq: 1,
            title: "런".into(),
            parts: vec![Part::Think(crate::timeline::Think {
                seq: 2,
                title: Some("파일을 훑는 중".into()),
                text: "rows.rs가 정본이므로 거기부터 본다.".into(),
            })],
        };
        let folds = open_all(&item);
        let out = plain(&rows(&[item], 78, &folds, crate::lang::Lang::Ko));
        assert!(out.iter().any(|l| l.contains('┊') && l.contains("rows.rs가 정본")), "{out:?}");
    }

    /// **The four things on screen must not look alike.** The stretch of working, the reasoning
    /// under it, the tools it ran and the agent's own words were all chevrons-or-nothing at two
    /// indents, so the one line that keeps changing to say what is happening read as just another
    /// chip — and what was said to the person read as one more thought.
    #[test]
    fn the_head_a_chip_a_tool_and_what_was_said_each_get_their_own_marker() {
        let items = [
            Item::Work {
                seq: 1,
                title: "카반 데이터를 모으는 중".into(),
                parts: vec![
                    think_at(2, "먼저 보드를 센다", "보드가 셋이다"),
                    Part::Step(step_at(3, "exec")),
                ],
            },
            Item::Agent {
                seq: 4, text: "다 됐습니다! 셋을 하나로 묶었습니다.".into()
            },
        ];
        // The card is open, so its own children are on screen to be told apart from.
        let out = plain(&rows(&items, 78, &open_all(&items[0]), crate::lang::Lang::Ko));
        let find = |needle: &str| {
            out.iter().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle}: {out:?}"))
        };
        assert!(out[0].starts_with("✻ "), "{:?}", out[0]);
        // The chip is open here, so its chevron points down; folded it is `▸`. Either way the
        // chevron is the chip's own mark, and it is indented under the head.
        assert!(find("먼저 보드를 센다").starts_with("  ▾ "), "{:?}", find("먼저 보드를 센다"));
        assert!(find("exec").starts_with("  ● "), "{:?}", find("exec"));
        assert!(find("다 됐습니다").starts_with("◆ "), "{:?}", find("다 됐습니다"));
    }

    /// The card head still folds — the marker moved to the end, it did not go away.
    #[test]
    fn the_card_head_still_says_whether_it_is_folded() {
        let items = [work_at(1)];
        let open = Folds::from([(1, Fold { open: true, user_touched: true })]);
        assert!(plain(&rows(&items, 78, &open, crate::lang::Lang::Ko))[0].ends_with('▾'));
        let shut = Folds::from([(1, Fold { open: false, user_touched: true })]);
        assert!(plain(&rows(&items, 78, &shut, crate::lang::Lang::Ko))[0].ends_with('▸'));
    }

    /// **A stretch that is over says so.** Holding the last `work_summary` there would leave a
    /// stale "writing the report" standing above a turn that finished minutes ago.
    #[test]
    fn a_finished_stretch_of_working_reads_as_done_not_as_its_last_title() {
        let items = [Item::Work { seq: 1, title: "보고서 작성 중".into(), parts: vec![] }];
        let head = plain(&rows(&items, 78, &Folds::new(), crate::lang::Lang::Ko)).remove(0);
        assert!(head.contains(crate::lang::Lang::Ko.run_done()), "{head:?}");
        assert!(!head.contains("보고서"), "the head kept a title from the middle of it: {head:?}");

        let running = plain(&rows_with(
            &items,
            78,
            &Folds::new(),
            None,
            crate::lang::Lang::Ko,
            Turn { running: true, blink: false },
        ))
        .remove(0);
        assert!(running.contains("보고서 작성 중"), "{running:?}");
    }

    /// **Every `work_summary` of a turn is one card, not one each.** The server writes one whenever
    /// the subject changes, and a card apiece chopped a single turn into a column of near-empty
    /// heads that made the conversation read as far longer than it was.
    #[test]
    fn a_turn_that_changed_its_subject_three_times_is_still_one_card() {
        let mut t = crate::timeline::Timeline::new();
        let at = |seq, kind| crate::event::Entry { seq, kind };
        t.upsert(at(1, crate::event::EntryKind::WorkStart("노드 재시도".into())));
        t.upsert(at(2, crate::event::EntryKind::WorkStart("보고서 작성 중".into())));
        t.upsert(at(3, crate::event::EntryKind::WorkStart("결과를 보고 중".into())));
        let items = t.items().to_vec();
        assert_eq!(items.len(), 1, "{items:?}");
        let out = plain(&rows_with(
            &items,
            78,
            &Folds::new(),
            None,
            crate::lang::Lang::Ko,
            Turn { running: true, blink: false },
        ));
        assert_eq!(out[0].trim_end_matches([' ', '▾']), "✻ 결과를 보고 중", "{out:?}");
    }

    /// **The card follows the run; the chips inside it never do.** A running card shows what is
    /// being done — the subject and the tools — and the reasoning behind each step stays folded
    /// until it is asked for (decided with the user, 2026-08-11). Watching the model talk itself
    /// noise, and it pushes the tool rows off the screen.
    #[test]
    fn a_run_opens_the_card_but_never_the_reasoning_inside_it() {
        let items = [work_at(1)];
        let mut cache = Cache::new();
        let draw = |cache: &mut Cache, running: bool| {
            cache.layout(
                &items,
                60,
                &Folds::new(),
                None,
                Turn { running, blink: false },
                crate::lang::Lang::Ko,
            );
            cache.plain()
        };
        let running = draw(&mut cache, true);
        assert!(running.iter().any(|l| l.contains("먼저 구조를 본다")), "{running:?}");
        assert!(running.iter().any(|l| l.contains("grep")), "{running:?}");
        assert!(!running.iter().any(|l| l.contains('┊')), "the reasoning body shows: {running:?}");
        // Idle: the whole card folds itself.
        let idle = draw(&mut cache, false);
        assert_eq!(idle.len(), 1, "{idle:?}");
    }

    /// **A chip is its title and nothing else.** It used to carry the tools that ran under it
    /// and what they changed, but the tool rows are right below it saying the same thing, and
    /// the card head already totals the run.
    #[test]
    fn a_chip_carries_no_counts_of_its_own() {
        let items = [work_at(1)];
        let out = plain(&live(&items, 60, &Folds::new(), crate::lang::Lang::Ko));
        let chip = out.iter().find(|l| l.contains("먼저 구조를 본다")).expect("{out:?}");
        assert_eq!(chip.trim_end(), "  ▸ 먼저 구조를 본다", "{chip:?}");
        assert!(out[0].contains("도구 1개"), "the head still totals the run: {out:?}");
    }

    /// A chip the person opened by hand stays open — running or not, it is their choice.
    #[test]
    fn a_user_opened_chip_stays_open() {
        let items = [work_at(1)];
        let mut folds = Folds::from([(1, Fold { open: true, user_touched: true })]);
        folds.insert(chip_key(&items[0]), Fold { open: true, user_touched: true });
        let mut cache = Cache::new();
        cache.layout(&items, 60, &folds, None, Turn::default(), crate::lang::Lang::Ko);
        assert!(cache.plain().iter().any(|l| l.contains('┊')), "{:?}", cache.plain());
    }
}
