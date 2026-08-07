//! The session timeline. **`seq` is the identity and `cursor` is the progress.**
//!
//! attacca updates durable events in place, issuing a fresh `cursor` and re-broadcasting to live
//! subscribers (`cursor = nextval(...)` in `session_event_repo.rs`).
//! So this is always an upsert — appending would make work cards balloon every turn.

use std::collections::BTreeMap;

use zyris_attacca::ZDeltaKind;

use crate::event::{Entry, EntryKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The seq of this tool-call event. **It is the fold-state key** — each tool row folds independently.
    pub seq: i64,
    /// The short name shown on screen — `exec`, not `zyris__arch__terminal__exec`.
    pub name: String,
    /// A piece shown dimly next to the name — what the action was against.
    pub note: String,
    pub failed: bool,
    /// What to show when expanded. Empty means there's nothing to expand, so pressing does nothing.
    pub detail: String,
    /// Attached when the tool changed files. If present, the screen draws this instead of JSON.
    pub diff: Option<crate::tools::diff::Diff>,
}

/// A piece inside a work card. **It lays out in the exact order it arrived.**
///
/// If all reasoning were gathered and tools stacked below it, "what was thought and what was done" would
/// disappear. The model actually did think → tool → think → tool, so show it exactly that way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Think(String),
    /// What the agent said during a run. It stands in arrival order with tools and reasoning.
    Text(String),
    Step(Step),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    User {
        seq: i64,
        text: String,
    },
    Agent {
        seq: i64,
        text: String,
    },
    Work {
        seq: i64,
        title: String,
        parts: Vec<Part>,
    },
    Error {
        seq: i64,
        message: String,
    },
    Subagent {
        seq: i64,
        summary: String,
    },
    /// What the agent asked. It stands **outside**, not inside the work card — it's something to answer.
    Question {
        seq: i64,
        steps: Vec<crate::question::Step>,
        answered: bool,
    },
    /// What the app said. Slash-command results and revert notices land here.
    ///
    /// **Different from `Frame::Notice`.** That one is a status line that disappears after 6 seconds; this is meant
    /// to remain while being read — you can't ask someone to read `/mcp`'s list within 6 seconds.
    System {
        seq: i64,
        text: String,
    },
}

impl Item {
    pub fn seq(&self) -> i64 {
        match self {
            Item::User { seq, .. }
            | Item::Agent { seq, .. }
            | Item::Work { seq, .. }
            | Item::Error { seq, .. }
            | Item::Subagent { seq, .. }
            | Item::System { seq, .. }
            | Item::Question { seq, .. } => *seq,
        }
    }
}

#[derive(Debug, Default)]
pub struct Timeline {
    entries: BTreeMap<i64, Entry>,
    /// A snippet of answer text not yet solidified as durable. Stacked in anchor order.
    live_text: Vec<LiveText>,
    /// The reasoning delta of the open work card.
    live_reasoning: String,
    /// The items built so far. **If nothing changed, they are not rebuilt.**
    ///
    /// It used to rebuild every frame, which cloned every event's strings each time.
    /// The longer the conversation, the more the cost grew linearly, blowing the frame budget —
    /// this is what `tests/perf.rs` measures.
    cache: Vec<Item>,
    dirty: bool,
    /// How many times it actually rebuilt. Tests use this to confirm the cache really works.
    rebuilds: u64,
    /// What the app said. It stands mixed with server events in time order.
    said: Vec<Said>,
    /// The seq for the next app item. **It's negative** — see the explanation below.
    next_said: i64,
}

/// A snippet of answer text that streamed in.
///
/// Text that flowed between durable events, so it's not in `entries` yet. It stands after the anchor (`after`),
/// before the next event. When a durable `chat_agent` arrives, it takes over that spot and this disappears.
#[derive(Debug, Clone)]
struct LiveText {
    /// The last durable seq when this snippet started flowing.
    after: i64,
    text: String,
}

/// Whose words a locally-made item carries.
///
/// **The app's line and the user's echo share one list.** Both are said here rather than by the
/// server, both have to stand exactly where they were said, and both take their identity from the
/// same negative seq counter — two vectors would mean two placement passes that could disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Voice {
    /// The app speaking — slash-command results and revert notices.
    App,
    /// The user's own words, put up the instant they were submitted. Retired when the server's
    /// copy of the same message arrives.
    Echo,
}

/// One thing that was said here rather than by the server.
#[derive(Debug, Clone)]
struct Said {
    /// How far the server events had arrived when it was said. It sits after this, before the next event.
    after: i64,
    /// Who is speaking. It decides which `Item` this becomes and whether the server can retire it.
    voice: Voice,
    /// The screen item's identity. **Negative, so it never collides with server seqs.**
    ///
    /// Since fold state (`Folds`) and the row cache (`rows::Cache`) key on seq, a collision would make
    /// two items fight over one spot. Screen order is decided by the vector `build` produces, not by seq,
    /// so being negative doesn't push them upward.
    seq: i64,
    text: String,
}

impl Timeline {
    pub fn new() -> Self {
        Self { dirty: true, next_said: -1, ..Default::default() }
    }

    /// The app says something. Slash-command results and revert notices land here.
    pub fn say(&mut self, text: impl Into<String>) {
        self.push_said(Voice::App, text.into());
    }

    /// The user's own words, on screen the instant they were submitted.
    ///
    /// **Nothing else puts them there.** `Item::User` is otherwise built only from the server's
    /// `chat_user` event (`event.rs::entry_from`), so until the round trip completes the person
    /// sees no trace of what they just sent. In 일/작업 mode it is worse than slow: the first
    /// message never goes through `send_message` at all — it rides inside `ZNewJob::message` /
    /// `ZNewWork::message` (`conn::open_job`) — so it may never come back at all, and the words
    /// would be gone for good.
    ///
    /// It shares the negative seq space and the placement rule with `say`, so it stands exactly
    /// where it was typed. `upsert` takes it away when the server's copy lands — the same words
    /// must never be on screen twice.
    pub fn echo(&mut self, text: impl Into<String>) {
        self.push_said(Voice::Echo, text.into());
    }

    /// The one place a locally-made item is born. **Both voices go through it** — the anchor rule
    /// and the seq allocation must not exist in two copies.
    fn push_said(&mut self, voice: Voice, text: String) {
        let after = self.entries.keys().next_back().copied().unwrap_or(0);
        self.said.push(Said { after, voice, seq: self.next_said, text });
        self.next_said -= 1;
        self.dirty = true;
    }

    /// Empties the on-screen conversation. **The server's record stays intact** — `/clear` calls this.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.said.clear();
        self.live_text.clear();
        self.live_reasoning.clear();
        self.dirty = true;
    }

    pub fn upsert(&mut self, entry: Entry) {
        // When a new run opens, the previous run's reasoning must not carry over.
        if let EntryKind::WorkStart(_) = &entry.kind {
            self.live_reasoning.clear();
        }
        // **The server's copy takes over from the echo.** Both carry the same words, so leaving
        // the echo up would put the message on screen twice — the same reason a durable
        // `chat_agent` retires the streamed snippet it solidifies.
        if let EntryKind::User(text) = &entry.kind {
            self.retire_echo(text);
        }
        self.entries.insert(entry.seq, entry);
        self.dirty = true;
    }

    /// Retires the **oldest** outstanding echo carrying these words.
    ///
    /// Oldest, because the same words sent twice leave two echoes standing, and the server records
    /// them in the order they went out — retiring the newest would leave the pair on screen in the
    /// wrong order once the second copy lands.
    ///
    /// **Finding nothing is normal.** A history replay is all server copies with no echo
    /// outstanding, and a job's first message may produce no `chat_user` at all — there the echo
    /// is the only copy there will ever be, and it has to stay.
    fn retire_echo(&mut self, text: &str) {
        // Compared trimmed: Enter submits the input verbatim while the picker path trims it, and
        // the server may hand back either.
        let at = self
            .said
            .iter()
            .position(|s| s.voice == Voice::Echo && s.text.trim() == text.trim());
        if let Some(at) = at {
            self.said.remove(at);
            self.dirty = true;
        }
    }

    pub fn push_delta(&mut self, kind: ZDeltaKind, text: &str) {
        match kind {
            ZDeltaKind::Assistant => {
                // The snippet's anchor is "after the last durable event so far". If no durable event
                // intervened, it merges into the same snippet and reads as one answer;
                // if a tool call intervenes, it becomes a new snippet that stands **before** that tool.
                let after = self.entries.keys().next_back().copied().unwrap_or(0);
                match self.live_text.last_mut() {
                    Some(seg) if seg.after == after => seg.text.push_str(text),
                    _ => self.live_text.push(LiveText { after, text: text.to_string() }),
                }
            }
            ZDeltaKind::Reasoning => self.live_reasoning.push_str(text),
        }
        self.dirty = true;
    }

    /// How many times the items have been rebuilt so far. A test-only observation window.
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// The items to draw, in seq order. If nothing changed, the previously built ones are returned as-is.
    ///
    /// The work card's boundaries follow what the server defined — thinking/tool/todo events after the
    /// `work_summary`'s `seq` and before the next `work_summary` belong to that card.
    /// Same as attacca's `WorkSummaryState.seq` being "the lower bound of events in this run".
    pub fn items(&mut self) -> &[Item] {
        if self.dirty {
            self.cache = self.build();
            self.dirty = false;
            self.rebuilds += 1;
        }
        &self.cache
    }

    fn build(&mut self) -> Vec<Item> {
        let mut out: Vec<Item> = Vec::new();
        let mut open_work: Option<usize> = None;
        // Streaming text snippets. Insert them in place between events — appended at the end,
        // they'd be pushed below the tool rows that came after, making the screen look out of order (it
        // actually did). When a durable `chat_agent` arrives and solidifies, it takes over the spot and this is removed.
        let mut live: Vec<LiveText> = std::mem::take(&mut self.live_text);
        // Up to the snippets already placed. Within one build, each snippet is placed only once.
        let mut flushed = 0usize;

        for entry in self.entries.values() {
            let seq = entry.seq;
            // Once a durable answer arrives, earlier snippets have done their part — the same text
            // must not appear twice.
            if matches!(entry.kind, EntryKind::Agent(_)) {
                live.retain(|s| s.after >= seq);
            }
            // Insert the snippets that belong before this event into the currently open card.
            flush_live(&mut out, &mut open_work, &live, seq, &mut self.next_said, &mut flushed);
            match &entry.kind {
                EntryKind::User(text) => {
                    open_work = None;
                    out.push(Item::User { seq, text: text.clone() });
                }
                EntryKind::Agent(text) => {
                    // **Answers that arrive mid-run stand inside the card** — if messages between tools were pushed
                    // out of the card, what was said while doing what would scatter. With no card,
                    // it's an ordinary answer.
                    if let Some(at) = open_work {
                        if let Item::Work { parts, .. } = &mut out[at] {
                            parts.push(Part::Text(text.clone()));
                            continue;
                        }
                    }
                    out.push(Item::Agent { seq, text: text.clone() });
                }
                EntryKind::Error(message) => {
                    open_work = None;
                    out.push(Item::Error { seq, message: message.clone() });
                }
                EntryKind::Subagent(summary) => {
                    out.push(Item::Subagent { seq, summary: summary.clone() });
                }
                EntryKind::Question { steps, answered } => {
                    // A question must not be buried in a card. Close the run and stand it outside.
                    open_work = None;
                    out.push(Item::Question { seq, steps: steps.clone(), answered: *answered });
                }
                EntryKind::WorkStart(title) => {
                    open_work = Some(out.len());
                    out.push(Item::Work { seq, title: title.clone(), parts: Vec::new() });
                }
                EntryKind::Thinking(text) => {
                    let at = card_for(&mut out, &mut open_work, seq);
                    if let Item::Work { parts, .. } = &mut out[at] {
                        push_think(parts, text);
                    }
                }
                EntryKind::Tool { name, summary, failed, detail, diff, .. } => {
                    let at = card_for(&mut out, &mut open_work, seq);
                    if let Item::Work { parts, .. } = &mut out[at] {
                        parts.push(Part::Step(Step {
                            seq,
                            name: short_name(name),
                            // For a tool that changed files, **the changed file** is the summary — the argument's
                            // `path` may be relative, and the result's is authoritative.
                            note: match diff {
                                Some(d) => d.path.clone(),
                                None => summary.clone(),
                            },
                            failed: *failed,
                            detail: detail.clone(),
                            diff: diff.clone(),
                        }));
                    }
                }
                EntryKind::Todo(text) => {
                    let at = card_for(&mut out, &mut open_work, seq);
                    if let Item::Work { parts, .. } = &mut out[at] {
                        parts.push(Part::Step(Step {
                            seq,
                            name: "todo".into(),
                            note: text.clone(),
                            failed: false,
                            detail: String::new(),
                            diff: None,
                        }));
                    }
                }
            }
        }

        // Snippets left at the end — appended to the open card's end or as a standalone answer.
        flush_live(&mut out, &mut open_work, &live, i64::MAX, &mut self.next_said, &mut flushed);

        // Lay the not-yet-durable reasoning delta onto the **end** of the open card. If it's thinking again
        // after using a tool, it must attach below that tool for the order to be right.
        if !self.live_reasoning.is_empty() {
            if let Some(Item::Work { parts, .. }) = open_work.map(|i| &mut out[i]) {
                push_think(parts, &self.live_reasoning);
            }
        }

        // Snippets meant to sit after future events (anchors not yet arrived) are carried over.
        self.live_text = live;
        self.weave_in_what_was_said_here(out)
    }

    /// Weaves what was said here into its place among the server events.
    ///
    /// **Sweep once and merge.** Inserting one by one would let earlier inserts shift later positions,
    /// flipping the order when two or more sayings share a spot.
    ///
    /// Position is **"right after the after item"**, not a seq comparison — like the implicit work card,
    /// even when an item's seq is outside the positive range, the said spot doesn't shift.
    fn weave_in_what_was_said_here(&self, items: Vec<Item>) -> Vec<Item> {
        if self.said.is_empty() {
            return items;
        }
        let mut out = Vec::with_capacity(items.len() + self.said.len());
        let mut said = self.said.iter().peekable();
        let mine = |s: &Said| match s.voice {
            Voice::App => Item::System { seq: s.seq, text: s.text.clone() },
            // **Drawn exactly like the server's copy.** If the echo looked different, the line
            // would change shape under the reader's eyes the moment the server's copy arrived,
            // for no reason they could see.
            Voice::Echo => Item::User { seq: s.seq, text: s.text.clone() },
        };
        // What was said when no events existed stands at the very front.
        while said.peek().is_some_and(|s| s.after == 0) {
            out.push(mine(said.next().expect("just looked at it")));
        }
        let mut pending: Vec<&Said> = Vec::new();
        for item in items {
            // The sayings that go after this item — those whose after equals this item's seq.
            while said.peek().is_some_and(|s| s.after == item.seq()) {
                pending.push(said.next().expect("just looked at it"));
            }
            out.push(item);
            if !pending.is_empty() {
                out.extend(pending.drain(..).map(mine));
            }
        }
        out.extend(said.map(mine));
        out
    }
}

/// The tool name shown on screen.
///
/// attacca builds node tools as `zyris__{node}__{capability}__{tool}`. Left as-is,
/// it alone eats a whole line, and every row starts with the same prefix, so **the part that differs is invisible**.
/// Server built-ins (`todo_add`·`web_search`) have no `__`, so they stay as-is.
///
/// Two can share a name (`file_io.read` and `terminal.read`), but the summary beside them tells them apart —
/// one is a path and the other a PTY.
fn short_name(name: &str) -> String {
    name.rsplit("__").next().unwrap_or(name).to_string()
}

/// The item seq of an implicit work card. Made a very distant negative so it can't collide with server seqs
/// (positive), app sayings (small negative), or live answers — a collision would make fold state and the row
/// cache treat two items as one.
fn implicit_seq(first: i64) -> i64 {
    i64::MIN + first
}

/// Pushes streaming snippets whose anchor is before `up_to` into their current spot.
///
/// With a card open, inside it (before the first tool, or at the end); without one, as a standalone answer. Snippets
/// stack in anchor order, so they're examined in order from the front. **Don't remove them** — until durable
/// arrives, this snippet is the only home of that text, and since placement starts over from scratch each time,
/// it can't appear twice either.
fn flush_live(
    out: &mut Vec<Item>,
    open_work: &mut Option<usize>,
    live: &[LiveText],
    up_to: i64,
    next_seq: &mut i64,
    from: &mut usize,
) {
    // Snippets stack in anchor order, and `from` is how many this build has already placed.
    // Without it, the same snippet would re-enter on every event and appear twice.
    while *from < live.len() {
        let seg = &live[*from];
        if seg.after >= up_to {
            break;
        }
        match open_work {
            Some(at) => {
                if let Item::Work { parts, .. } = &mut out[*at] {
                    splice_text(parts, seg.after, &seg.text);
                }
            }
            None => {
                // Standalone answers share the negative seq space with app sayings — they must not collide.
                out.push(Item::Agent { seq: *next_seq, text: seg.text.clone() });
                *next_seq -= 1;
            }
        }
        *from += 1;
    }
}

/// Splices a snippet into its place in the card — **before** the first tool after the anchor (`after`), or
/// at the end if there is none. Even wedged between reasoning rows, arrival order is preserved.
fn splice_text(parts: &mut Vec<Part>, after: i64, text: &str) {
    let at = parts
        .iter()
        .position(|p| matches!(p, Part::Step(s) if s.seq > after))
        .unwrap_or(parts.len());
    parts.insert(at, Part::Text(text.to_string()));
}

/// Appends reasoning. Reasoning that continues with no tool in between is merged into one block —
/// if nothing happened in between, splitting it would only hurt readability.
/// The currently open work card. **Creates one if there isn't.**
///
/// attacca creates a `work_summary` when a run starts, but there are turns where the first reasoning delta
/// arrives before it, and turns with no `work_summary` at all (short answers that use no tools). Previously
/// reasoning and tool calls were **silently dropped** then — to the person it looks like "I sent a prompt
/// and nothing appeared on screen". It's not folded; it's simply absent.
///
/// So when there's nowhere to go, an untitled card is opened. The title attaches even if `work_summary`
/// arrives late — events are reordered by seq, so by then this spot becomes the real card.
fn card_for(out: &mut Vec<Item>, open_work: &mut Option<usize>, seq: i64) -> usize {
    if let Some(at) = *open_work {
        return at;
    }
    let at = out.len();
    // **Make the implicit card's fold key not collide with the first part's seq.** If they collided,
    // expanding the card would also expand the first tool's detail under the same key (it actually
    // looked that way — in tool-only turns with no reasoning, pressing the card opened the first tool's args and result).
    out.push(Item::Work { seq: implicit_seq(seq), title: String::new(), parts: Vec::new() });
    *open_work = Some(at);
    at
}

fn push_think(parts: &mut Vec<Part>, text: &str) {
    match parts.last_mut() {
        Some(Part::Think(prev)) => {
            prev.push('\n');
            prev.push_str(text);
        }
        _ => parts.push(Part::Think(text.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Entry, EntryKind};

    fn e(seq: i64, kind: EntryKind) -> Entry {
        Entry { seq, kind }
    }

    fn texts(t: &mut Timeline) -> Vec<String> {
        t.items()
            .iter()
            .map(|i| match i {
                Item::User { text, .. } | Item::Agent { text, .. } => text.clone(),
                Item::System { text, .. } => format!("앱: {text}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// **What the app says stands where it was said.** Pushed to the very top or bottom,
    /// you couldn't tell what it was answering.
    #[test]
    fn what_the_app_says_lands_where_it_was_said() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("안녕".into())));
        t.say("작업 디렉터리는 /home/ruma입니다");
        t.upsert(e(2, EntryKind::Agent("네".into())));
        assert_eq!(
            texts(&mut t),
            vec!["안녕", "앱: 작업 디렉터리는 /home/ruma입니다", "네"],
            "말한 자리에 안 섰다"
        );
    }

    /// Two sayings in the same spot must keep **one order**.
    #[test]
    fn two_things_said_in_a_row_keep_their_order() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("안녕".into())));
        t.say("첫째");
        t.say("둘째");
        t.upsert(e(2, EntryKind::Agent("네".into())));
        assert_eq!(texts(&mut t), vec!["안녕", "앱: 첫째", "앱: 둘째", "네"]);
    }

    /// **App seqs must not collide with server events.** Fold state and the row cache key on seq.
    #[test]
    fn what_the_app_says_never_collides_with_a_server_seq() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("안녕".into())));
        t.say("하나");
        t.say("둘");
        let seqs: Vec<i64> = t.items().iter().map(|i| i.seq()).collect();
        let unique: std::collections::HashSet<i64> = seqs.iter().copied().collect();
        assert_eq!(seqs.len(), unique.len(), "seq values collide: {seqs:?}");
        assert!(seqs.iter().filter(|s| **s < 0).count() == 2, "{seqs:?}");
    }

    fn users(t: &mut Timeline) -> Vec<String> {
        t.items()
            .iter()
            .filter_map(|i| match i {
                Item::User { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// **What was typed is on screen before the server has said anything.** `Item::User` is
    /// otherwise built only from a `chat_user` event, so without the echo a person's own words
    /// are missing for the whole round trip.
    #[test]
    fn a_submitted_message_is_on_screen_before_the_server_echoes_it() {
        let mut t = Timeline::new();
        t.echo("안녕하세요");
        assert_eq!(users(&mut t), vec!["안녕하세요".to_string()]);
    }

    /// The durable copy takes the spot over instead of standing beside it — the same words twice
    /// reads as having sent it twice.
    #[test]
    fn the_servers_copy_replaces_the_echo_instead_of_showing_it_twice() {
        let mut t = Timeline::new();
        t.echo("안녕하세요");
        t.upsert(e(1, EntryKind::User("안녕하세요".into())));
        assert_eq!(users(&mut t), vec!["안녕하세요".to_string()]);
        assert_eq!(t.items()[0].seq(), 1, "the durable one wins the spot");
    }

    /// **The 일/작업 case.** Their first message rides inside `ZNewJob::message` and may never
    /// come back as a `chat_user` at all — then the echo is the only copy there will ever be.
    #[test]
    fn an_echo_the_server_never_confirms_stays_on_screen() {
        let mut t = Timeline::new();
        t.echo("이걸 해 주세요");
        t.upsert(e(1, EntryKind::WorkStart("작업".into())));
        t.upsert(e(2, EntryKind::Agent("네".into())));
        assert_eq!(users(&mut t), vec!["이걸 해 주세요".to_string()]);
    }

    /// Sending the same words twice leaves two echoes, and each server copy retires exactly one —
    /// the oldest, so the pair never ends up in the wrong order.
    #[test]
    fn the_same_words_sent_twice_retire_one_echo_each() {
        let mut t = Timeline::new();
        t.echo("네");
        t.echo("네");
        t.upsert(e(1, EntryKind::User("네".into())));
        assert_eq!(users(&mut t).len(), 2, "one echo left plus the durable one");
        t.upsert(e(2, EntryKind::User("네".into())));
        assert_eq!(users(&mut t).len(), 2, "both are durable now");
        assert!(t.items().iter().all(|i| i.seq() > 0), "no echo is left: {:?}", t.items());
    }

    /// An echo stands where it was typed, not at the top — its seq is negative, and order must
    /// come from the anchor rather than from comparing seqs.
    #[test]
    fn an_echo_stands_where_it_was_typed() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("첫 질문".into())));
        t.upsert(e(2, EntryKind::Agent("첫 답".into())));
        t.echo("다음 질문");
        assert_eq!(t.items().last().map(|i| i.seq()), Some(-1), "{:?}", t.items());
        t.upsert(e(3, EntryKind::Agent("두 번째 답".into())));
        let seqs: Vec<i64> = t.items().iter().map(|i| i.seq()).collect();
        assert_eq!(seqs, vec![1, 2, -1, 3], "the echo must keep its place: {seqs:?}");
    }

    /// Echoes draw from the same negative counter as `say`, so nothing can collide with a server
    /// seq — fold state and the row cache both key on it.
    #[test]
    fn an_echo_never_collides_with_a_server_seq() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("안녕".into())));
        t.say("앱이 한 말");
        t.echo("내가 친 말");
        let seqs: Vec<i64> = t.items().iter().map(|i| i.seq()).collect();
        let unique: std::collections::HashSet<i64> = seqs.iter().copied().collect();
        assert_eq!(seqs.len(), unique.len(), "seq values collide: {seqs:?}");
        assert_eq!(seqs.iter().filter(|s| **s < 0).count(), 2, "{seqs:?}");
    }

    /// The app's line and the user's echo are different things on screen — one is a notice, the
    /// other is the message. Drawing an echo as a System line would look like the app said it.
    #[test]
    fn an_echo_is_drawn_as_the_user_and_what_the_app_says_is_not() {
        let mut t = Timeline::new();
        t.say("앱이 한 말");
        t.echo("내가 친 말");
        let kinds: Vec<&Item> = t.items().iter().collect();
        assert!(matches!(kinds[0], Item::System { .. }), "{kinds:?}");
        assert!(matches!(kinds[1], Item::User { .. }), "{kinds:?}");
    }

    /// `/clear` empties **only the screen**. It does not delete the server's history.
    #[test]
    fn clearing_empties_the_screen() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("안녕".into())));
        t.say("무언가");
        t.clear();
        assert!(t.items().is_empty(), "{:?}", t.items());
    }

    fn step_at(seq: i64, name: &str, note: &str) -> Step {
        Step {
            seq,
            name: name.into(),
            note: note.into(),
            failed: false,
            detail: "인자\n{}".into(),
            diff: None,
        }
    }

    /// An updated event comes again with the same seq. Appending would make two cards.
    #[test]
    fn re_receiving_the_same_seq_replaces_it_instead_of_appending() {
        let mut t = Timeline::new();
        t.upsert(e(10, EntryKind::WorkStart(String::new())));
        t.upsert(e(10, EntryKind::WorkStart("스크롤 계산 위치를 찾는 중".into())));

        let items = t.items();
        assert_eq!(items.len(), 1, "the same seq must stay a single entry");
        match &items[0] {
            Item::Work { title, .. } => assert_eq!(title, "스크롤 계산 위치를 찾는 중"),
            other => panic!("it must be a work card: {other:?}"),
        }
    }

    /// **Reasoning that arrives before any work card must show too.**
    ///
    /// attacca creates a `work_summary` when a run starts, but there are turns where the first reasoning delta
    /// arrives before it, and turns with no `work_summary` at all (short answers that use no tools).
    /// Dropping that reasoning would look to the person like **"I sent a prompt and nothing happened"** —
    /// not folded, just nothing on screen.
    #[test]
    fn thinking_that_arrives_before_any_work_card_still_shows() {
        let mut t = Timeline::default();
        t.upsert(e(1, EntryKind::Thinking("무엇부터 볼까".into())));

        let items = t.items().to_vec();
        assert_eq!(items.len(), 1, "exactly one card must appear: {items:?}");
        match &items[0] {
            Item::Work { parts, .. } => {
                assert_eq!(parts, &vec![Part::Think("무엇부터 볼까".into())])
            }
            other => panic!("it must be a work card: {other:?}"),
        }
    }

    /// Tool calls are the same. **Dropped, what was done disappears entirely.**
    #[test]
    fn a_tool_call_before_any_work_card_still_shows() {
        let mut t = Timeline::default();
        t.upsert(e(
            1,
            EntryKind::Tool {
                name: "zyris__arch__search__grep".into(),
                summary: "fn main".into(),
                failed: false,
                detail: String::new(),
                diff: None,
            },
        ));
        match &t.items()[0] {
            Item::Work { parts, .. } => assert_eq!(parts.len(), 1, "{parts:?}"),
            other => panic!("it must be a work card: {other:?}"),
        }
    }

    /// **An implicit card's fold key must not collide with its first tool's.** If it collided,
    /// expanding the card would also expand the first tool's detail under the same key — in tool-only
    /// turns with no reasoning, pressing the card opened the first tool's args and result (it actually looked that way).
    #[test]
    fn an_implicit_cards_fold_key_never_collides_with_its_first_tool() {
        let mut t = Timeline::new();
        t.upsert(e(
            2,
            EntryKind::Tool {
                name: "zyris__arch__terminal__exec".into(),
                summary: "커밋".into(),
                failed: false,
                detail: "인자\n{}\n\n출력\nok".into(),
                diff: None,
            },
        ));
        let Item::Work { seq, parts, .. } = &t.items()[0] else {
            panic!("it must be an implicit card");
        };
        let Part::Step(first) = &parts[0] else {
            panic!("the first part must be a tool");
        };
        assert_ne!(*seq, first.seq, "the card fold key collides with the first tool");
    }

    /// **The streaming answer stands where the turn started.** Appended at the end, it would be pushed
    /// below the tool rows that came after, looking out of order (it actually did).
    #[test]
    fn the_streaming_answer_sits_where_the_turn_started() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("질문".into())));
        t.push_delta(ZDeltaKind::Assistant, "전부 통과");
        t.upsert(e(
            2,
            EntryKind::Tool {
                name: "zyris__arch__terminal__exec".into(),
                summary: "커밋".into(),
                failed: false,
                detail: "인자\n{}\n\n출력\nok".into(),
                diff: None,
            },
        ));
        let items = t.items().to_vec();
        let agent_at =
            items.iter().position(|i| matches!(i, Item::Agent { text, .. } if text == "전부 통과"));
        let work_at = items.iter().position(|i| matches!(i, Item::Work { .. }));
        assert!(agent_at.is_some() && work_at.is_some(), "{items:?}");
        assert!(
            agent_at.unwrap() < work_at.unwrap(),
            "the text was pushed below the tool: {items:?}"
        );
    }

    /// **A late `work_summary` takes over the card it belongs to.** Events are reordered by seq,
    /// so it should merge into one titled card — split in two, the same run would look like
    /// two blocks on screen.
    #[test]
    fn a_late_work_summary_takes_over_the_card_it_belongs_to() {
        let mut t = Timeline::default();
        t.upsert(e(2, EntryKind::Thinking("먼저 구조를 보자".into())));
        t.upsert(e(1, EntryKind::WorkStart("리팩터링".into())));

        let items = t.items().to_vec();
        assert_eq!(items.len(), 1, "the card was split in two: {items:?}");
        match &items[0] {
            Item::Work { title, parts, .. } => {
                assert_eq!(title, "리팩터링");
                assert_eq!(parts, &vec![Part::Think("먼저 구조를 보자".into())]);
            }
            other => panic!("it must be a work card: {other:?}"),
        }
    }

    /// The server defines the card's boundary — everything before the next work_summary belongs to this card.
    #[test]
    fn a_work_card_owns_the_steps_until_the_next_work_summary() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("첫 런".into())));
        t.upsert(e(2, EntryKind::Thinking("먼저 구조를 보자".into())));
        t.upsert(e(
            3,
            EntryKind::Tool {
                name: "grep".into(),
                summary: "viewport".into(),
                failed: false,
                detail: "인자\n{}".into(),
                diff: None,
            },
        ));
        t.upsert(e(4, EntryKind::WorkStart("둘째 런".into())));
        t.upsert(e(
            5,
            EntryKind::Tool {
                name: "read".into(),
                summary: "rows.rs".into(),
                failed: false,
                detail: "인자\n{}".into(),
                diff: None,
            },
        ));

        let items = t.items();
        assert_eq!(items.len(), 2);
        match (&items[0], &items[1]) {
            (Item::Work { parts: first, .. }, Item::Work { parts: second, .. }) => {
                assert_eq!(
                    first,
                    &vec![
                        Part::Think("먼저 구조를 보자".into()),
                        Part::Step(step_at(3, "grep", "viewport")),
                    ],
                    "첫 카드는 생각 다음 도구"
                );
                assert_eq!(second.len(), 1, "the second card holds one read");
            }
            other => panic!("there must be two work cards: {other:?}"),
        }
    }

    /// **Tools stand where they were used.** Piling all the reasoning up and stacking the tools below
    /// would lose what was thought while doing what.
    #[test]
    fn thinking_and_tools_stay_in_the_order_they_happened() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("런".into())));
        t.upsert(e(2, EntryKind::Thinking("먼저 어디를 볼까".into())));
        t.upsert(e(
            3,
            EntryKind::Tool {
                name: "grep".into(),
                summary: "rows".into(),
                failed: false,
                detail: "인자\n{}".into(),
                diff: None,
            },
        ));
        t.upsert(e(4, EntryKind::Thinking("찾았다. 이제 고치자".into())));
        t.upsert(e(
            5,
            EntryKind::Tool {
                name: "edit".into(),
                summary: "rows.rs".into(),
                failed: false,
                detail: "인자\n{}".into(),
                diff: None,
            },
        ));

        let Item::Work { parts, .. } = &t.items()[0] else { panic!("it must be a work card") };
        assert_eq!(
            parts,
            &vec![
                Part::Think("먼저 어디를 볼까".into()),
                Part::Step(step_at(3, "grep", "rows")),
                Part::Think("찾았다. 이제 고치자".into()),
                Part::Step(step_at(5, "edit", "rows.rs")),
            ]
        );
    }

    /// Reasoning that continues with no tool in between is one block. Nothing happened between, so there's no reason to split.
    #[test]
    fn back_to_back_thinking_merges_into_one_block() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("런".into())));
        t.upsert(e(2, EntryKind::Thinking("첫 생각".into())));
        t.upsert(e(3, EntryKind::Thinking("이어지는 생각".into())));

        let Item::Work { parts, .. } = &t.items()[0] else { panic!("it must be a work card") };
        assert_eq!(parts, &vec![Part::Think("첫 생각\n이어지는 생각".into())]);
    }

    /// Reasoning deltas flowing in after a tool must attach **below that tool**.
    #[test]
    fn reasoning_deltas_after_a_tool_land_below_that_tool() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("런".into())));
        t.upsert(e(2, EntryKind::Thinking("먼저 보자".into())));
        t.upsert(e(
            3,
            EntryKind::Tool {
                name: "grep".into(),
                summary: "rows".into(),
                failed: false,
                detail: "인자\n{}".into(),
                diff: None,
            },
        ));
        t.push_delta(ZDeltaKind::Reasoning, "결과를 읽어 보니");

        let Item::Work { parts, .. } = &t.items()[0] else { panic!("it must be a work card") };
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert_eq!(parts[2], Part::Think("결과를 읽어 보니".into()));
    }

    /// Answer deltas must show on screen until the durable event arrives.
    #[test]
    fn assistant_deltas_show_before_the_durable_event_arrives() {
        let mut t = Timeline::new();
        t.push_delta(ZDeltaKind::Assistant, "답변이 ");
        t.push_delta(ZDeltaKind::Assistant, "흘러온다");

        match t.items().last().unwrap() {
            Item::Agent { text, .. } => assert_eq!(text, "답변이 흘러온다"),
            other => panic!("it must be an answer: {other:?}"),
        }
    }

    /// When the durable event arrives, the delta buffer must not remain as a duplicate.
    #[test]
    fn the_durable_agent_event_supersedes_the_delta_buffer() {
        let mut t = Timeline::new();
        t.push_delta(ZDeltaKind::Assistant, "답변이 흘러온다");
        t.upsert(e(7, EntryKind::Agent("답변이 흘러온다".into())));

        let agents = t.items().iter().filter(|i| matches!(i, Item::Agent { .. })).count();
        assert_eq!(agents, 1, "a delta and its durable event must not become two entries");
    }

    /// Reasoning deltas go into the open work card.
    #[test]
    fn reasoning_deltas_go_into_the_open_work_card() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart(String::new())));
        t.push_delta(ZDeltaKind::Reasoning, "무엇부터 볼까");

        match &t.items()[0] {
            Item::Work { parts, .. } => {
                assert_eq!(parts, &vec![Part::Think("무엇부터 볼까".into())])
            }
            other => panic!("it must be a work card: {other:?}"),
        }
    }

    /// If nothing changed, the items are not rebuilt.
    ///
    /// Rebuilding every frame clones every event's strings each time. The longer the conversation,
    /// the more the cost grows linearly and the screen stutters — this actually caused lag once.
    #[test]
    fn items_are_not_rebuilt_when_nothing_changed() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("안녕".into())));

        t.items();
        t.items();
        t.items();
        assert_eq!(t.rebuilds(), 1, "unchanged, it must be built only once");

        t.push_delta(ZDeltaKind::Assistant, "답");
        t.items();
        assert_eq!(t.rebuilds(), 2, "a delta must rebuild it");

        t.upsert(e(2, EntryKind::Agent("답".into())));
        t.items();
        assert_eq!(t.rebuilds(), 3, "an event must rebuild it");
    }

    /// When a new run starts, the previous run's reasoning deltas must not carry over.
    #[test]
    fn a_new_work_run_starts_with_empty_reasoning() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("첫 런".into())));
        t.push_delta(ZDeltaKind::Reasoning, "첫 런의 생각");
        t.upsert(e(2, EntryKind::WorkStart("둘째 런".into())));

        match &t.items()[1] {
            Item::Work { parts, .. } => assert!(parts.is_empty(), "{parts:?}"),
            other => panic!("it must be a work card: {other:?}"),
        }
    }

    fn tool_at(seq: i64, name: &str, note: &str) -> Entry {
        e(
            seq,
            EntryKind::Tool {
                name: name.into(),
                summary: note.into(),
                failed: false,
                // Use the same shape for the detail to match `step_at`'s expected values.
                detail: "인자\n{}".into(),
                diff: None,
            },
        )
    }

    /// **Answers that arrive mid-run stand inside the card** — if messages between tools were pushed
    /// out of the card, "what was said while doing what" would scatter. The card must not fragment either.
    #[test]
    fn agent_text_during_a_run_goes_into_the_card_in_order() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("커밋".into())));
        t.upsert(e(2, EntryKind::Agent("이제 커밋합니다".into())));
        t.upsert(tool_at(3, "exec", "git commit"));
        t.upsert(e(4, EntryKind::Agent("커밋하고 푸시하는 중".into())));
        t.upsert(tool_at(5, "exec", "git push"));

        let items = t.items().to_vec();
        assert_eq!(items.len(), 1, "the card must not be split: {items:?}");
        match &items[0] {
            Item::Work { parts, .. } => assert_eq!(
                parts,
                &vec![
                    Part::Text("이제 커밋합니다".into()),
                    Part::Step(step_at(3, "exec", "git commit")),
                    Part::Text("커밋하고 푸시하는 중".into()),
                    Part::Step(step_at(5, "exec", "git push")),
                ],
                "말한 순서 그대로 카드 안에 있어야 한다"
            ),
            other => panic!("it must be a work card: {other:?}"),
        }
    }

    /// **Streaming text stands before the tool that follows it** — inside the card, above the tool row.
    #[test]
    fn live_text_during_a_run_lands_before_the_following_tool() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("커밋".into())));
        t.push_delta(ZDeltaKind::Assistant, "전부 통과. 이제 커밋합니다");
        t.upsert(tool_at(2, "exec", "git commit"));
        let Item::Work { parts, .. } = &t.items()[0] else {
            panic!("it must be a work card");
        };
        assert_eq!(
            parts,
            &vec![
                Part::Text("전부 통과. 이제 커밋합니다".into()),
                Part::Step(step_at(2, "exec", "git commit")),
            ],
            "텍스트가 도구 아래로 밀렸다"
        );
    }

    /// **Streaming text split by a tool stays two segments** — merged into one,
    /// the two messages would look like a single block.
    #[test]
    fn live_text_split_by_a_tool_stays_two_segments() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("커밋".into())));
        t.push_delta(ZDeltaKind::Assistant, "이제 커밋합니다");
        t.upsert(tool_at(2, "exec", "git commit"));
        t.push_delta(ZDeltaKind::Assistant, "커밋하고 푸시하는 중");
        let Item::Work { parts, .. } = &t.items()[0] else {
            panic!("it must be a work card");
        };
        assert_eq!(
            parts,
            &vec![
                Part::Text("이제 커밋합니다".into()),
                Part::Step(step_at(2, "exec", "git commit")),
                Part::Text("커밋하고 푸시하는 중".into()),
            ],
            "도구 사이 두 토막이 합쳐졌다: {parts:?}"
        );
    }

    /// **When the user interrupts, the text after it is a standalone answer outside the card.**
    #[test]
    fn text_after_a_user_interrupt_stays_standalone() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("런".into())));
        t.upsert(tool_at(2, "grep", "rows"));
        t.upsert(e(3, EntryKind::User("잠깐".into())));
        t.push_delta(ZDeltaKind::Assistant, "네 알겠습니다");

        let items = t.items().to_vec();
        let work = items.iter().filter(|i| matches!(i, Item::Work { .. })).count();
        assert_eq!(work, 1, "no new card may appear after the interruption: {items:?}");
        let user_at = items.iter().position(|i| matches!(i, Item::User { .. })).unwrap();
        let agent_at = items
            .iter()
            .position(|i| matches!(i, Item::Agent { text, .. } if text == "네 알겠습니다"))
            .expect("no standalone answer");
        assert!(user_at < agent_at, "the answer must come after the user message: {items:?}");
    }

    /// **Snippets don't duplicate across rebuilds** — placement starts over from scratch each time,
    /// so even if a snippet remains, it enters the card only once.
    #[test]
    fn live_segments_are_not_duplicated_across_rebuilds() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("런".into())));
        t.push_delta(ZDeltaKind::Assistant, "생각");
        let _ = t.items();
        t.upsert(tool_at(2, "grep", "rows"));
        let Item::Work { parts, .. } = &t.items()[0] else {
            panic!("it must be a work card");
        };
        assert_eq!(
            parts,
            &vec![Part::Text("생각".into()), Part::Step(step_at(2, "grep", "rows")),],
            "토막이 두 번 들어갔다: {parts:?}"
        );
    }
}
