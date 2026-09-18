//! The **only** way an agent changes files.
//!
//! `file_io`'s `write`·`remove`·`mkdir` aren't granted (`tools::readonly`). If there were two ways to change a file,
//! the agent would pick whole-file overwrites, diffs would spread across the whole file, and the approval gate
//! would have to be hung in two places. With this as the only way, **every change passes one approval gate and one diff screen.**
//!
//! **There is no tool for deleting files at all.** Deleting is done by the human.
//!
//! Only the methods' doc comments go over the wire and become the description the agent reads. **So the comments ARE the contract.**
//!
//! If several sessions edit one repository at the same time, they can overwrite files without knowing. That fight is
//! stopped with `base_version` — send the version token from when you read with the edit, and if the file changed in between,
//! it fails instead of silently overwriting. The token comes from the `file_io.read` response's
//! `stat.modified_unix_ms:stat.size`, or from `code_edit.version`'s `version`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use similar::TextDiff;
use tokio::io::AsyncWriteExt;
use zyris::WireError;
use zyris_caps::resolve_under;

use crate::tools::diff::diff;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EditSpec {
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EditResult {
    pub path: String,
    pub added: u32,
    pub removed: u32,
    /// Unified diff of what changed.
    pub diff: String,
    /// The file's version token after this write ("mtime_ms:size"). Use it as the base_version of the next edit.
    pub version: String,
    /// The edits that matched with more than their exact text, as `edit N: line-trimmed` or
    /// `edit N: whitespace`. **Empty — and left out of the answer — when every edit matched
    /// exactly.** A non-empty list means the text that was sent is not byte-for-byte what is on
    /// disk, which is worth a fresh read before the next edit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relaxed: Vec<String>,
}

/// A file's version token. Pass it straight to the `base_version` argument.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileVersion {
    pub path: String,
    /// Modification time (ms). The same value as `stat.modified_unix_ms` in the `file_io.read` response.
    pub mtime_ms: u64,
    /// Byte count. The same value as `stat.size`.
    pub size: u64,
    /// SHA-256 of the content. Used when a stronger comparison than `version` is needed.
    pub sha256: String,
    /// The token to pass straight to `base_version` — of the form "mtime_ms:size".
    pub version: String,
}

#[zyris::capability(name = "code_edit", version = 2)]
pub trait CodeEdit {
    /// Change a file by replacing exact strings. Give one edit or several; they apply in order,
    /// and if any of them fails nothing is written at all — the answer says which edit failed.
    /// Read the file with `file_io.read` first: each `old_string` must appear exactly once unless
    /// its `replace_all` is set. Indentation and runs of spaces may differ from the file and the
    /// answer lists those edits under `relaxed`; a different word may not, and an edit that
    /// changes nothing is refused rather than answered as a success.
    ///
    /// **One edit is a list of one.** There was a second tool for that case, and all it ever did
    /// was ask every caller to choose between two ways of saying the same thing.
    ///
    /// `base_version`: the file's version token **as it was when you last read it** —
    /// either "`<stat.modified_unix_ms>:<stat.size>`" from the read response, or the
    /// `version`/`sha256:` token from `code_edit.version`. If the file changed on disk since
    /// you read it, this call FAILS instead of editing a file you haven't seen — re-read and
    /// retry with the new token. Pass null to skip the check (not recommended).
    ///
    /// `path` is relative to the working directory, or absolute when it starts with `/`.
    async fn edit(
        &self,
        path: String,
        edits: Vec<EditSpec>,
        base_version: Option<String>,
    ) -> zyris::Result<EditResult>;

    /// Write a whole file. For a NEW file, pass `base_version: null`.
    /// For an EXISTING file, `base_version` is REQUIRED: pass the version you read; if the
    /// file changed since, the write fails instead of silently overwriting someone else's
    /// (or the user's) changes. Missing parent directories are created.
    async fn write(
        &self,
        path: String,
        content: String,
        base_version: Option<String>,
    ) -> zyris::Result<EditResult>;

    /// Return the file's current version token(s). Call this when you want a strong
    /// `sha256:` token to pass as `base_version`; the `version` field is the same
    /// "mtime_ms:size" token you can read from `file_io.read`'s stat.
    async fn version(&self, path: String) -> zyris::Result<FileVersion>;
}

#[derive(Clone)]
pub struct LocalEdit {
    root: PathBuf,
    /// Where the pre-change content is kept. **Lives outside the user's repo** (`~/.cache`).
    undo: crate::undo::Undo,
}

/// Which of the three tools is running. **The two differ in exactly two rules, and both are about
/// proof**: a whole-file write has to show it has seen what it overwrites, and an edit that would
/// change nothing is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Edit,
    Write,
}

/// What a change produced: the new body, and the edits that needed forgiving (`Found`).
struct Changed {
    body: String,
    relaxed: Vec<String>,
}

impl LocalEdit {
    pub fn new(root: PathBuf) -> LocalEdit {
        let undo = crate::undo::Undo::for_dir(&root);
        LocalEdit { root, undo }
    }

    /// The same, with the undo history's cache root given. **Tests use this** so they do not have
    /// to move `XDG_CACHE_HOME`, which is process-global and raced between tests.
    pub fn under(root: PathBuf, cache_root: &std::path::Path) -> LocalEdit {
        let undo = crate::undo::Undo::under(cache_root, &root);
        LocalEdit { root, undo }
    }

    /// The undo log. `/undo` uses it.
    pub fn undo(&self) -> crate::undo::Undo {
        self.undo.clone()
    }

    /// Read, change, write, and return a diff. All three tools meet here — so no matter which path,
    /// the result has the same shape.
    ///
    /// `base_version` is the version token from when this file was last read. If the file changed in between,
    /// it **fails without writing** — instead of silently overwriting, it forces a re-read.
    async fn apply<F>(
        &self,
        tool: Tool,
        path: &str,
        base_version: Option<&str>,
        change: F,
    ) -> zyris::Result<EditResult>
    where
        F: FnOnce(&str) -> Result<Changed, WireError>,
    {
        let full = resolve_under(&self.root, path);
        let existed = tokio::fs::try_exists(&full).await.unwrap_or(false);
        let old = tokio::fs::read_to_string(&full).await.unwrap_or_default();

        // **Concurrency check.** If the file changed after reading, don't silently overwrite.
        if let Some(base) = base_version {
            let now = current_version(&full);
            let ok = match (base.strip_prefix("sha256:"), now.as_ref()) {
                (Some(hex), _) => sha256_of(old.as_bytes()) == hex,
                (None, Ok(cur)) => cur == base,
                (None, Err(_)) => false,
            };
            if !ok {
                let now_s = match now {
                    Ok(v) => v,
                    Err(e) => e.to_string(),
                };
                return Err(WireError::invalid_params(format!(
                    "'{}' changed after it was read (base_version {base} != current {now_s}). \
                     Re-read the current content with file_io.read and retry with the new \
                     version token as base_version.",
                    clip(path)
                )));
            }
        }
        // Whole-file writes default to new files only — to overwrite an existing file you must
        // present proof via base_version that you've seen the file.
        if tool == Tool::Write && existed && base_version.is_none() {
            return Err(WireError::invalid_params(format!(
                "'{}' already exists ‒ pass base_version to overwrite it. Use the \
                 stat.modified_unix_ms:stat.size of the read response, or code_edit.version's \
                 version, as-is.",
                clip(path)
            )));
        }

        let Changed { body: new, relaxed } = change(&old)?;

        // **An edit that changes nothing is a mistake, not a success.** It is what a call looks
        // like when the text behind it is not the file: the lines it quotes are already the lines
        // it asks for. Answering "saved" would hide a stale view behind a green result.
        //
        // **`write` is not held to this.** Writing a file its own content is a legitimate no-op —
        // it is the shape a "create an empty file" call has — and a file that does not exist yet
        // arrives here with nothing read at all.
        if tool == Tool::Edit && new == old {
            return Err(WireError::invalid_params(format!(
                "'{}' changed nothing, so it was not written: the file already reads that way. \
                 Check the lines you quoted: they may already say what you meant, and the edit that \
                 matters is somewhere else.",
                clip(path)
            )));
        }

        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                WireError::internal(format!("couldn't create the parent directory: {e}"))
            })?;
        }
        // **Snapshot right before writing.** All three tools meet here, so there's a single spot.
        // A failure doesn't block the edit — if a missing safety net stopped work,
        // you'd end up with files that can't be fixed (see `undo::snapshot`'s comment).
        self.undo.snapshot(&full);
        atomic_write(&full, new.as_bytes())
            .await
            .map_err(|e| WireError::internal(format!("couldn't write: {e}")))?;
        // **The bytes on disk have to be the bytes this call meant to write.** Nothing else checks
        // it, and a mount that lies, a hook that rewrites the file, or another session landing in
        // the same moment all end the same way — a tool answer that says "saved" over a file that
        // does not say so.
        let on_disk = tokio::fs::read(&full).await.map_err(|e| {
            WireError::internal(format!("couldn't read '{}' back: {e}", clip(path)))
        })?;
        if on_disk != new.as_bytes() {
            return Err(WireError::internal(format!(
                "'{}' was written but does not hold what this call wrote: something else is \
                 changing the file. What was there before this call is in the undo history \
                 (`/undo`).",
                clip(path)
            )));
        }
        let shown = full.to_string_lossy().to_string();
        let d = diff(&old, &new, &shown);
        let version = current_version(&full).unwrap_or_else(|_| "?".into());
        Ok(EditResult {
            path: shown,
            added: d.added,
            removed: d.removed,
            diff: d.to_unified(),
            version,
            relaxed,
        })
    }
}

/// The file's current version token ("mtime_ms:size"). The same value as what the read response's `stat` yields.
fn current_version(full: &Path) -> std::io::Result<String> {
    let md = std::fs::metadata(full)?;
    Ok(format!("{}:{}", mtime_ms(&md), md.len()))
}

fn mtime_ms(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn sha256_of(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Write to a temp file and apply atomically via rename. A half-written file is never visible.
async fn atomic_write(full: &Path, content: &[u8]) -> std::io::Result<()> {
    let dir = full.parent().unwrap_or_else(|| Path::new("."));
    let name = full.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let unique = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    );
    let tmp = dir.join(format!(".{name}.zyris-tmp-{unique}"));
    let r = async {
        let mut f = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).await?;
        f.write_all(content).await?;
        f.sync_all().await?;
        drop(f);
        tokio::fs::rename(&tmp, full).await
    }
    .await;
    if r.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    r
}

/// Replaces one fragment.
///
/// **What was found, how many times, and — when it was found nowhere — the nearest text all go
/// into the error.** An agent has to be able to add more context and call again, and a bare
/// "failed" does not say what to fix. One failed edit drops the chance of that edit ever landing
/// from 90.5% to 57.2% (SWE-agent's agent-computer-interface ablations), which is why every
/// harness we compared against spends its most careful code here.
fn substitute(body: &str, spec: &EditSpec) -> Result<(String, Option<Found>), WireError> {
    if spec.old_string.is_empty() {
        return Err(WireError::invalid_params(
            "`old_string` is empty: there is nothing to look for. Use `write` to create a file, or \
             quote the text you mean to replace.",
        ));
    }
    match locate(body, &spec.old_string) {
        Match::One(at) => {
            let mut out = String::with_capacity(body.len() + spec.new_string.len());
            out.push_str(&body[..at.start]);
            out.push_str(&spec.new_string);
            out.push_str(&body[at.end..]);
            Ok((out, (at.how != Found::Exact).then_some(at.how)))
        }
        // Every occurrence, in order and in one pass — `replace_all` means exactly that.
        Match::Several(at) if spec.replace_all => {
            let mut out = String::with_capacity(body.len() + at.len() * spec.new_string.len());
            let mut done = 0;
            for one in &at {
                out.push_str(&body[done..one.start]);
                out.push_str(&spec.new_string);
                done = one.end;
            }
            out.push_str(&body[done..]);
            Ok((out, at.iter().map(|one| one.how).find(|how| *how != Found::Exact)))
        }
        Match::Several(at) => {
            let mut places: Vec<String> = at
                .iter()
                .take(PLACES_SHOWN)
                .map(|one| line_at(body, one.start).to_string())
                .collect();
            if at.len() > PLACES_SHOWN {
                places.push(format!("and {} more", at.len() - PLACES_SHOWN));
            }
            Err(WireError::invalid_params(format!(
                "'{}' matches {} places in the file{}: lines {}. Add more context to point at \
                 one spot, or turn replace_all on.",
                clip(&spec.old_string),
                at.len(),
                forgave(at[0].how),
                places.join(", ")
            )))
        }
        Match::None => Err(WireError::invalid_params(not_found(body, &spec.old_string))),
    }
}

/// How many places the error lists before it says "and N more".
const PLACES_SHOWN: usize = 5;
/// How similar the nearest text has to be before quoting it helps more than it misleads.
const CLOSEST_FLOOR: f32 = 0.5;
/// How many of the best first-line candidates get a full window comparison.
const CLOSEST_CANDIDATES: usize = 5;
/// The longest window compared against the missing text.
const CLOSEST_WINDOW: usize = 12;

/// How an `old_string` was found in the file.
///
/// **Only whitespace is forgiven.** Everything else has to be there as written: a matcher that
/// guesses at words edits the wrong line and says nothing, which is the failure measured for
/// anchors carrying too little information (arXiv 2609.11957: line-number-anchored matching
/// corrupts 99.1% of files under a one-line shift, while content-anchored matching fails cleanly).
/// Forgiving indentation is the cheap half of that trade — it is how text copied off a read and
/// re-indented stops costing a round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Found {
    /// Byte for byte — the only way this worked before.
    Exact,
    /// Every line matched once its ends were trimmed: indentation or trailing spaces differ.
    LineTrimmed,
    /// Every line matched with each run of whitespace collapsed to one space.
    Whitespace,
}

impl Found {
    fn label(self) -> &'static str {
        match self {
            Found::Exact => "exact",
            Found::LineTrimmed => "line-trimmed",
            Found::Whitespace => "whitespace",
        }
    }
}

/// The words an error uses when the match it is describing needed forgiving.
fn forgave(how: Found) -> &'static str {
    match how {
        Found::Exact => "",
        Found::LineTrimmed => " once indentation and trailing spaces are ignored",
        Found::Whitespace => " once runs of whitespace are ignored",
    }
}

/// Where a fragment sits in the body, and how it was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Located {
    start: usize,
    end: usize,
    how: Found,
}

/// What `locate` found.
#[derive(Debug)]
enum Match {
    One(Located),
    /// More than one — every occurrence, so the caller can either refuse or `replace_all` them.
    Several(Vec<Located>),
    None,
}

/// Finds `needle` in `body`: exactly first, then with the two whitespace relaxations.
///
/// **The relaxations only run when the exact search found nothing**, so a file holding the text as
/// written behaves exactly as it did before. Each tier still demands uniqueness: two
/// whitespace-variant matches are as ambiguous as two exact ones, and picking one of them is how an
/// edit lands in the wrong place without saying so.
fn locate(body: &str, needle: &str) -> Match {
    let exact = occurrences(body, needle);
    match exact.len() {
        0 => {}
        1 => return Match::One(exact[0]),
        _ => return Match::Several(exact),
    }
    for how in [Found::LineTrimmed, Found::Whitespace] {
        let found = relaxed_occurrences(body, needle, how);
        match found.len() {
            0 => continue,
            1 => return Match::One(found[0]),
            _ => return Match::Several(found),
        }
    }
    Match::None
}

/// Every place `needle` starts. An empty needle is not a thing to find — `substitute` refuses it
/// before this runs.
fn occurrences(body: &str, needle: &str) -> Vec<Located> {
    if needle.is_empty() {
        return Vec::new();
    }
    body.match_indices(needle)
        .map(|(start, _)| Located { start, end: start + needle.len(), how: Found::Exact })
        .collect()
}

/// One line of `text`, with the byte range it occupies **including its line ending**, so a
/// replacement can be spliced over whole lines.
struct Line<'a> {
    at: usize,
    end: usize,
    text: &'a str,
}

fn lines_of(text: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut at = 0;
    for piece in text.split_inclusive('\n') {
        let end = at + piece.len();
        out.push(Line { at, end, text: piece });
        at = end;
    }
    out
}

/// Occurrences of `needle` where whole lines are compared the way `how` asks for.
///
/// **Whole lines only, and the span covers them completely**: what a caller quotes is lines of a
/// file, and replacing anything less than whole lines is what leaves half-an-indentation behind.
fn relaxed_occurrences(body: &str, needle: &str, how: Found) -> Vec<Located> {
    let want = core_lines(needle, how);
    if want.is_empty() {
        return Vec::new();
    }
    let have = lines_of(body);
    if have.len() < want.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for start in 0..=(have.len() - want.len()) {
        let window = &have[start..start + want.len()];
        let same =
            window.iter().zip(want.iter()).all(|(line, want)| normalize(line.text, how) == *want);
        if same {
            out.push(Located { start: window[0].at, end: window[window.len() - 1].end, how });
        }
    }
    out
}

/// The needle's lines as a relaxation compares them, with whitespace-only lines at either end
/// dropped: text quoted off a screen often brings a blank line along, and the blank line is not
/// what the caller is asking to replace.
fn core_lines(needle: &str, how: Found) -> Vec<String> {
    let mut lines: Vec<String> = lines_of(needle).iter().map(|l| normalize(l.text, how)).collect();
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// One line, as a relaxation compares it. `Exact` is not relaxed at all.
fn normalize(line: &str, how: Found) -> String {
    match how {
        Found::Exact => line.to_string(),
        Found::LineTrimmed => line.trim().to_string(),
        Found::Whitespace => line.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

/// The 1-based line a byte offset sits on.
fn line_at(body: &str, at: usize) -> usize {
    body[..at].bytes().filter(|b| *b == b'\n').count() + 1
}

/// What to say when the text is nowhere in the file — **with the nearest text alongside it**.
///
/// A model told only "not found" reads the file again and tries the same thing; the lines below
/// are the ones it probably meant, so it can see the indentation or the stale line it is working
/// from without spending the round trip. aider answers a failed match the same way.
fn not_found(body: &str, needle: &str) -> String {
    let mut out = format!(
        "'{}' wasn't found in the file, even once indentation and runs of whitespace were ignored. \
         Read the current content with file_io.read and copy the text from there.",
        clip(needle)
    );
    if let Some(close) = closest(body, needle) {
        out.push_str(&close.shown(body));
    }
    out
}

/// The place in `body` that most resembles `needle`, when anything resembles it enough to show.
struct Closest {
    /// How similar, 0–100.
    percent: u32,
    /// 1-based line the window starts at.
    line: usize,
    /// Line indexes the window covers.
    from: usize,
    to: usize,
}

impl Closest {
    /// The window plus two lines either side, with line numbers in front.
    ///
    /// **The numbers are here on purpose**, unlike in `file_io.read`'s answer: the whole point of
    /// this text is to say *where* to look, and a model that copies a line number by mistake gets a
    /// clean failure rather than a wrong edit.
    fn shown(&self, body: &str) -> String {
        const CONTEXT: usize = 2;
        let have = lines_of(body);
        let from = self.from.saturating_sub(CONTEXT);
        let to = (self.to + CONTEXT).min(have.len());
        let mut out =
            format!("\nClosest match ({}% similar) at line {}:\n", self.percent, self.line);
        for (i, line) in have[from..to].iter().enumerate() {
            let inside = (from + i) >= self.from && (from + i) < self.to;
            let marker = if inside { '>' } else { ' ' };
            let text = line.text.trim_end_matches('\n');
            out.push_str(&format!("{marker} {:>4} | {text}\n", line_at(body, line.at)));
        }
        out.push_str(
            "Check those lines against what you sent (whitespace included); if they are the ones \
             you meant, copy them from a fresh read.",
        );
        out
    }
}

/// The nearest window of `body` to `needle`, or `None` when nothing is close enough to be worth
/// showing — quoting a bad guess as "the closest match" would send a model to the wrong lines.
fn closest(body: &str, needle: &str) -> Option<Closest> {
    let want = core_lines(needle, Found::LineTrimmed);
    let first = want.first()?;
    let have = lines_of(body);
    let window = want.len().min(CLOSEST_WINDOW);
    if have.len() < window {
        return None;
    }
    let want_text = want[..window].join("\n");
    // A cheap screen first: character similarity between the needle's first line and one file line.
    // Only the handful of best lines pay for a full comparison of their window.
    let mut ranked: Vec<(f32, usize)> = have
        .iter()
        .enumerate()
        .map(|(i, line)| (TextDiff::from_chars(line.text.trim(), first).ratio(), i))
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    ranked.truncate(CLOSEST_CANDIDATES);
    let mut best: Option<(f32, usize)> = None;
    for (_, at) in ranked {
        if at + window > have.len() {
            continue;
        }
        let text = have[at..at + window]
            .iter()
            .map(|line| line.text.trim_end_matches('\n'))
            .collect::<Vec<_>>()
            .join("\n");
        let ratio = TextDiff::from_chars(&text, &want_text).ratio();
        if best.is_none_or(|(was, _)| ratio > was) {
            best = Some((ratio, at));
        }
    }
    let (ratio, at) = best?;
    // Comparing characters rather than lines is what makes a one-line needle score at all: two
    // lines that differ by a word share no line, but plenty of characters.
    (ratio >= CLOSEST_FLOOR).then(|| Closest {
        percent: (ratio * 100.0).round() as u32,
        line: line_at(body, have[at].at),
        from: at,
        to: at + window,
    })
}

/// Only as much as fits an error message. Loading a long fragment whole would let the message cover the screen.
fn clip(s: &str) -> String {
    let head: String = s.chars().take(40).collect();
    if head.chars().count() < s.chars().count() {
        format!("{head}…")
    } else {
        head
    }
}

#[async_trait::async_trait]
impl CodeEdit for LocalEdit {
    async fn edit(
        &self,
        path: String,
        edits: Vec<EditSpec>,
        base_version: Option<String>,
    ) -> zyris::Result<EditResult> {
        // Apply everything in memory, then write once. If it wrote halfway and failed, nobody would
        // know what state the file is in — neither the agent nor the human.
        self.apply(Tool::Edit, &path, base_version.as_deref(), move |body| {
            let mut out = body.to_string();
            let mut relaxed = Vec::new();
            for (i, spec) in edits.iter().enumerate() {
                match substitute(&out, spec) {
                    Ok((next, how)) => {
                        out = next;
                        if let Some(how) = how {
                            relaxed.push(format!("edit {}: {}", i + 1, how.label()));
                        }
                    }
                    // **Which edit failed, and that none of them landed.** With only the fragment's
                    // own message an agent cannot tell whether the edits before it were written,
                    // and the safe-looking move — send everything again — repeats work that would
                    // have succeeded.
                    Err(e) => {
                        return Err(WireError::invalid_params(if edits.len() == 1 {
                            format!("nothing was written: {}", e.message)
                        } else {
                            format!(
                                "edit {} of {} failed, so nothing was written: {}",
                                i + 1,
                                edits.len(),
                                e.message
                            )
                        }))
                    }
                }
            }
            Ok(Changed { body: out, relaxed })
        })
        .await
    }

    async fn write(
        &self,
        path: String,
        content: String,
        base_version: Option<String>,
    ) -> zyris::Result<EditResult> {
        // A trailing newline isn't added automatically — adding one would put the whole file in the diff.
        self.apply(Tool::Write, &path, base_version.as_deref(), move |_| {
            Ok(Changed { body: content, relaxed: Vec::new() })
        })
        .await
    }

    async fn version(&self, path: String) -> zyris::Result<FileVersion> {
        let full = resolve_under(&self.root, &path);
        let content = tokio::fs::read(&full).await.map_err(|e| {
            WireError::invalid_params(format!("couldn't read '{}': {e}", clip(&path)))
        })?;
        let md = tokio::fs::metadata(&full).await.map_err(|e| {
            WireError::invalid_params(format!("couldn't stat '{}': {e}", clip(&path)))
        })?;
        let mtime_ms = mtime_ms(&md);
        let size = md.len();
        let sha256 = sha256_of(&content);
        Ok(FileVersion { path, mtime_ms, size, sha256, version: format!("{mtime_ms}:{size}") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(body: &str) -> (tempfile::TempDir, LocalEdit, String) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), body).unwrap();
        let edit = LocalEdit::new(dir.path().to_path_buf());
        (dir, edit, "a.txt".to_string())
    }

    /// The same with the undo history pointed at its own directory.
    ///
    /// **No environment variable.** These tests used to move `XDG_CACHE_HOME`, which is
    /// process-global — each one pointed it at its own temp directory and overwrote whatever
    /// another test had just set, so they passed alone and failed at random in the suite.
    fn scratch_with_undo(body: &str) -> (tempfile::TempDir, tempfile::TempDir, LocalEdit, String) {
        let cache = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), body).unwrap();
        let edit = LocalEdit::under(dir.path().to_path_buf(), cache.path());
        (cache, dir, edit, "a.txt".to_string())
    }

    /// **An edit must be undoable.** In a directory without git, this is the only safety net.
    #[tokio::test]
    async fn an_edit_leaves_something_to_undo() {
        let (_cache, dir, edit, path) = scratch_with_undo("before\n");
        let undo = edit.undo();
        assert!(undo.is_empty(), "nothing has been changed yet");

        edit.edit(
            path,
            vec![EditSpec {
                old_string: "before".into(),
                new_string: "after".into(),
                replace_all: false,
            }],
            None,
        )
        .await
        .unwrap();
        assert!(!undo.is_empty(), "it was changed but there is nothing to undo");

        undo.revert_last().unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "before\n");
    }

    /// Creating a new file must be undoable too — reverting makes the file disappear.
    #[tokio::test]
    async fn creating_a_file_can_be_undone_too() {
        let (_cache, dir, edit, _) = scratch_with_undo("아무거나\n");
        let undo = edit.undo();
        edit.write("새로.txt".into(), "내용\n".into(), None).await.unwrap();
        assert!(dir.path().join("새로.txt").exists());

        undo.revert_last().unwrap();
        assert!(!dir.path().join("새로.txt").exists(), "the created file is still there");
    }

    #[tokio::test]
    async fn editing_replaces_the_one_match() {
        let (_d, edit, p) = scratch("하나\n둘\n셋\n");
        let r = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "둘".into(),
                    new_string: "TWO".into(),
                    replace_all: false,
                }],
                None,
            )
            .await
            .unwrap();
        assert_eq!((r.added, r.removed), (1, 1));
        assert!(r.diff.contains("+TWO"), "{}", r.diff);
    }

    /// Replacing only one of two occurrences without a word would quietly change the wrong place.
    #[tokio::test]
    async fn two_matches_fail_and_say_how_many() {
        let (_d, edit, p) = scratch("x\nx\n");
        let e = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "x".into(),
                    new_string: "y".into(),
                    replace_all: false,
                }],
                None,
            )
            .await
            .unwrap_err();
        assert!(e.message.contains('2'), "it must say how many times it appeared: {}", e.message);
    }

    /// When it's not found, it must also say what to do.
    #[tokio::test]
    async fn no_match_fails_and_says_what_to_do() {
        let (_d, edit, p) = scratch("x\n");
        let e = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "없다".into(),
                    new_string: "y".into(),
                    replace_all: false,
                }],
                None,
            )
            .await
            .unwrap_err();
        assert!(e.message.contains("read"), "it must say to read again: {}", e.message);
    }

    #[tokio::test]
    async fn replace_all_takes_every_match() {
        let (_d, edit, p) = scratch("x\nx\n");
        let r = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "x".into(),
                    new_string: "y".into(),
                    replace_all: true,
                }],
                None,
            )
            .await
            .unwrap();
        assert_eq!(r.added, 2);
    }

    /// If it wrote halfway and failed, nobody would know what state the file is in.
    #[tokio::test]
    async fn a_failing_edit_leaves_the_file_alone() {
        let (dir, edit, p) = scratch("하나\n둘\n");
        let edits = vec![
            EditSpec { old_string: "하나".into(), new_string: "ONE".into(), replace_all: false },
            EditSpec { old_string: "없다".into(), new_string: "X".into(), replace_all: false },
        ];
        assert!(edit.edit(p, edits, None).await.is_err());
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "하나\n둘\n");
    }

    #[tokio::test]
    async fn a_whole_edit_applies_in_order() {
        let (dir, edit, p) = scratch("하나\n둘\n");
        let edits = vec![
            EditSpec { old_string: "하나".into(), new_string: "ONE".into(), replace_all: false },
            EditSpec { old_string: "둘".into(), new_string: "TWO".into(), replace_all: false },
        ];
        edit.edit(p, edits, None).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "ONE\nTWO\n");
    }

    /// If a trailing newline were added automatically, the whole file would land in the diff.
    #[tokio::test]
    async fn writing_does_not_add_a_trailing_newline() {
        let (dir, edit, _) = scratch("");
        edit.write("b.txt".into(), "한 줄".into(), None).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("b.txt")).unwrap(), "한 줄");
    }

    /// It must be possible to create a new file in a missing directory.
    #[tokio::test]
    async fn writing_creates_missing_parents() {
        let (dir, edit, _) = scratch("");
        edit.write("깊은/곳/c.txt".into(), "내용".into(), None).await.unwrap();
        assert!(dir.path().join("깊은/곳/c.txt").exists());
    }

    /// If the file changed after reading, it must fail instead of silently overwriting.
    #[tokio::test]
    async fn editing_with_a_stale_base_version_fails_and_leaves_the_file() {
        let (dir, edit, p) = scratch("before\n");
        // The version token the agent read
        let read_at = current_version(&dir.path().join("a.txt")).unwrap();
        // Pretend another session changed it in between
        std::fs::write(dir.path().join("a.txt"), "before\nother-session\n").unwrap();
        let e = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "before".into(),
                    new_string: "after".into(),
                    replace_all: false,
                }],
                Some(read_at.clone()),
            )
            .await
            .unwrap_err();
        // **Asserted against the current language, not a Korean literal.** This used to read
        // `contains("바뀌었")` and only passed because another test had set the global language to
        // Korean first — the process default is English.
        assert_eq!(e.message, stale_message(&dir, &read_at));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "before\nother-session\n"
        );
    }

    /// If base_version matches the current disk, the edit goes through.
    #[tokio::test]
    async fn editing_with_a_matching_base_version_succeeds() {
        let (dir, edit, p) = scratch("before\n");
        let v = current_version(&dir.path().join("a.txt")).unwrap();
        edit.edit(
            p,
            vec![EditSpec {
                old_string: "before".into(),
                new_string: "after".into(),
                replace_all: false,
            }],
            Some(v),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "after\n");
    }

    /// The same check works with a sha256 token — a content change fails it.
    #[tokio::test]
    async fn sha256_base_version_checks_content() {
        let (dir, edit, p) = scratch("before\n");
        let good = format!("sha256:{}", sha256_of(b"before\n"));
        std::fs::write(dir.path().join("a.txt"), "before\nother\n").unwrap();
        let e = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "before".into(),
                    new_string: "after".into(),
                    replace_all: false,
                }],
                Some(good.clone()),
            )
            .await
            .unwrap_err();
        assert_eq!(e.message, stale_message(&dir, &good));
    }

    /// The stale-version message. **One sentence, in English, whatever the screen's language is**
    /// — this is a tool's answer, and the reader is the agent. It used to be built from
    /// `lang::current()` and only passed because another test had set the global language to
    /// Korean first; the process default is English.
    fn stale_message(dir: &tempfile::TempDir, base: &str) -> String {
        let now = current_version(&dir.path().join("a.txt")).unwrap();
        format!(
            "'a.txt' changed after it was read (base_version {base} != current {now}). \
             Re-read the current content with file_io.read and retry with the new version \
             token as base_version."
        )
    }

    /// Overwriting an existing file without base_version must be refused — it's the prime source of silent overwrites.
    #[tokio::test]
    async fn writing_over_an_existing_file_requires_base_version() {
        let (dir, edit, p) = scratch("keep\n");
        let e = edit.write(p.clone(), "clobber\n".into(), None).await.unwrap_err();
        assert!(e.message.contains("base_version"), "{}", e.message);
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "keep\n");

        let v = current_version(&dir.path().join("a.txt")).unwrap();
        edit.write(p, "clobber\n".into(), Some(v)).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "clobber\n");
    }

    /// The token version() returns must match the disk — it can be used as base_version as is.
    #[tokio::test]
    async fn version_matches_the_file_on_disk() {
        let (dir, edit, p) = scratch("abc\n");
        let fv = edit.version(p).await.unwrap();
        assert_eq!(fv.version, current_version(&dir.path().join("a.txt")).unwrap());
        assert_eq!(fv.size, 4);
        assert_eq!(fv.sha256, sha256_of(b"abc\n"));
    }

    /// The edit result carries the new version token — usable as the next edit's base_version.
    #[tokio::test]
    async fn edit_reports_the_new_version() {
        let (dir, edit, p) = scratch("x\n");
        let r = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "x".into(),
                    new_string: "y".into(),
                    replace_all: false,
                }],
                None,
            )
            .await
            .unwrap();
        assert_eq!(r.version, current_version(&dir.path().join("a.txt")).unwrap());
    }

    /// **Indentation that does not match is forgiven, and the answer says it was.**
    ///
    /// This is the failure the other harnesses spend their most careful code on: text copied off a
    /// read comes back re-indented, the exact match finds nothing, and a round trip goes by. What is
    /// forgiven is whitespace only — a different word is still a failure.
    #[tokio::test]
    async fn indentation_that_does_not_match_is_forgiven_and_reported() {
        let (dir, edit, p) = scratch("fn a() {\n    let x = 1;\n}\n");
        let r = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "fn a() {\n  let x = 1;\n}\n".into(),
                    new_string: "fn a() {\n    let x = 2;\n}\n".into(),
                    replace_all: false,
                }],
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "fn a() {\n    let x = 2;\n}\n"
        );
        assert_eq!(r.relaxed, vec!["edit 1: line-trimmed".to_string()]);
    }

    /// Tabs and runs of spaces inside a line are the same kind of difference, forgiven the same way.
    #[tokio::test]
    async fn runs_of_whitespace_are_forgiven_too() {
        let (dir, edit, p) = scratch("let  x\t=\t1;\n");
        let r = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "let x = 1;\n".into(),
                    new_string: "let x = 2;\n".into(),
                    replace_all: false,
                }],
                None,
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "let x = 2;\n");
        assert_eq!(r.relaxed, vec!["edit 1: whitespace".to_string()]);
    }

    /// **A forgiven match that is ambiguous is still refused.** Two places differing only in
    /// indentation are two places, and choosing one of them is the silent wrong edit this tool
    /// exists to prevent.
    #[tokio::test]
    async fn a_forgiven_match_that_is_ambiguous_is_refused() {
        let (dir, edit, p) = scratch("fn a() {\n  one();\n}\nfn b() {\n  one();\n}\n");
        let e = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "    one();\n".into(),
                    new_string: "    two();\n".into(),
                    replace_all: false,
                }],
                None,
            )
            .await
            .unwrap_err();
        assert!(e.message.contains("2 places"), "{}", e.message);
        assert!(e.message.contains("lines 2, 5"), "{}", e.message);
        assert!(e.message.contains("indentation"), "{}", e.message);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "fn a() {\n  one();\n}\nfn b() {\n  one();\n}\n"
        );
    }

    /// Forgiving the whitespace must not turn `replace_all` into something else — every occurrence
    /// of the relaxed match is replaced, and the answer names the relaxation.
    #[tokio::test]
    async fn replace_all_covers_every_forgiven_match() {
        let (dir, edit, p) = scratch("  a();\n  b();\n");
        let r = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "    a();\n".into(),
                    new_string: "    A();\n".into(),
                    replace_all: true,
                }],
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "    A();\n  b();\n"
        );
        assert_eq!(r.relaxed, vec!["edit 1: line-trimmed".to_string()]);
    }

    /// **Where the nearest text is, when the text is nowhere.** A bare "not found" is what sends an
    /// agent back to re-read the file and try the same thing again.
    #[tokio::test]
    async fn a_missing_edit_points_at_the_closest_lines() {
        let (dir, edit, p) = scratch("fn main() {\n    let value = 1;\n    let other = 2;\n}\n");
        let e = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "    let value = 3;\n".into(),
                    new_string: "    let value = 4;\n".into(),
                    replace_all: false,
                }],
                None,
            )
            .await
            .unwrap_err();
        assert!(e.message.contains("wasn't found"), "{}", e.message);
        assert!(e.message.contains("Closest match"), "{}", e.message);
        assert!(e.message.contains("at line 2"), "{}", e.message);
        assert!(e.message.contains("let value = 1;"), "{}", e.message);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "fn main() {\n    let value = 1;\n    let other = 2;\n}\n"
        );
    }

    /// Nothing resembling the text at all: the error says so instead of quoting a bad guess.
    #[tokio::test]
    async fn a_missing_edit_with_nothing_like_it_quotes_nothing() {
        let (_dir, edit, p) = scratch("alpha\nbeta\n");
        let e = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "qqqqqqqqqqqqqqqqqqqq\n".into(),
                    new_string: "z\n".into(),
                    replace_all: false,
                }],
                None,
            )
            .await
            .unwrap_err();
        assert!(!e.message.contains("Closest match"), "{}", e.message);
    }

    /// **An edit that changes nothing is refused.** It is what a call looks like when the text
    /// behind it is not the file — the lines it quotes already say what it wants.
    #[tokio::test]
    async fn an_edit_that_changes_nothing_is_refused() {
        let (dir, edit, p) = scratch("same\n");
        let e = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: "same".into(),
                    new_string: "same".into(),
                    replace_all: false,
                }],
                None,
            )
            .await
            .unwrap_err();
        assert!(e.message.contains("changed nothing"), "{}", e.message);
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "same\n");
    }

    /// **A whole-file write of the same content is not refused** — the no-op rule is about edits,
    /// and a file that does not exist yet arrives here with nothing read at all.
    #[tokio::test]
    async fn writing_the_same_content_back_is_allowed() {
        let (dir, edit, p) = scratch("same\n");
        let v = current_version(&dir.path().join("a.txt")).unwrap();
        edit.write(p, "same\n".into(), Some(v)).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "same\n");
    }

    /// **Which edit failed, and that none of them landed.** Without the index an agent cannot tell
    /// whether the edits before it were written, and the safe-looking move — send everything again
    /// — repeats the work that already matched.
    #[tokio::test]
    async fn a_failed_edit_says_which_one_failed() {
        let (dir, edit, p) = scratch("하나\n둘\n");
        let edits = vec![
            EditSpec { old_string: "하나".into(), new_string: "ONE".into(), replace_all: false },
            EditSpec { old_string: "없다".into(), new_string: "X".into(), replace_all: false },
            EditSpec { old_string: "둘".into(), new_string: "TWO".into(), replace_all: false },
        ];
        let e = edit.edit(p, edits, None).await.unwrap_err();
        assert!(e.message.contains("edit 2 of 3"), "{}", e.message);
        assert!(e.message.contains("nothing was written"), "{}", e.message);
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "하나\n둘\n");
    }

    /// Every edit matched with something other than its exact text is named, so an agent can tell
    /// that what it is holding is not quite what is on disk.
    #[tokio::test]
    async fn every_forgiven_edit_is_named_in_the_answer() {
        let (dir, edit, p) = scratch("  a();\n  b();\n");
        let r = edit
            .edit(
                p,
                vec![
                    EditSpec {
                        old_string: "    a();\n".into(),
                        new_string: "    A();\n".into(),
                        replace_all: false,
                    },
                    EditSpec {
                        old_string: "    b();\n".into(),
                        new_string: "    B();\n".into(),
                        replace_all: false,
                    },
                ],
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "    A();\n    B();\n"
        );
        assert_eq!(
            r.relaxed,
            vec!["edit 1: line-trimmed".to_string(), "edit 2: line-trimmed".to_string()]
        );
    }

    /// An empty `old_string` is not a search. Saying so beats matching everywhere or nowhere.
    #[tokio::test]
    async fn an_empty_old_string_is_refused() {
        let (dir, edit, p) = scratch("x\n");
        let e = edit
            .edit(
                p,
                vec![EditSpec {
                    old_string: String::new(),
                    new_string: "y".into(),
                    replace_all: false,
                }],
                None,
            )
            .await
            .unwrap_err();
        assert!(e.message.contains("empty"), "{}", e.message);
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "x\n");
    }
}
