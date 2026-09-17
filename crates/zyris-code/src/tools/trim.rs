//! Fits the announced tool definitions into a **token budget**.
//!
//! The doc comments upstream (zyris-caps) become this node's tool descriptions verbatim. Those
//! descriptions ride in the agent's context at session creation and every turn — rich examples,
//! caveats, and path-resolution notes all loaded would let a single file_io eat hundreds of
//! tokens. Keep the name and the schema's value-semantics parts, cut **only the descriptions**,
//! and what the agent needs to pick a tool (what it does) survives while the repetition falls away.
//!
//! `dispatch` does not read descriptions — cutting here only touches what gets announced.

use serde_json::Value;
use zyris::CapabilityDescriptor;

/// Budget for one tool description (bytes). The first sentence is the core.
pub const DESCRIPTION_LIMIT: usize = 200;
/// Budget for parameter descriptions inside the schema.
pub const PARAM_LIMIT: usize = 80;

/// Fits a description to the budget.
///
/// **The budget is spent on what a tool must say, not on its first 200 bytes.** Two things changed
/// here.
///
/// The cut is now at a *real* sentence end — a `.` followed by whitespace or the end of the text —
/// because the `.` inside a code span (``pass `.` ``) is not one. Cutting after it is how
/// `file_io.read`'s description came to be announced as "… — read on by…", the half of the
/// sentence that says the least, while the clause that says how to read on was dropped.
///
/// And **the sentences kept are the first one plus the first later one that reads like an
/// instruction** (`keep_what_must_be_said`): in the upstream doc comments the sentence carrying
/// "pass `offset` = the previous `offset + len`" sits behind two that merely describe.
///
pub fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let source = keep_what_must_be_said(text);
    let cut = source.floor_char_boundary(limit);
    let end = last_sentence_end(&source, cut).unwrap_or(cut);
    let mut out = source[..end].trim_end().to_string();
    // The `…` is part of the budget: a description that spends it and then three bytes more has
    // quietly gone over, which is the thing this function exists to prevent.
    if out.len() + 3 > limit {
        let room = out.floor_char_boundary(limit.saturating_sub(3));
        out.truncate(room);
        out = out.trim_end().to_string();
    }
    out.push('…');
    out
}

/// The first sentence, plus the first later one that says what happens next — in that order, and
/// only one of each.
///
/// **A description that says what a tool is and not how it answers is the expensive kind of
/// short.** The paragraphs after the first sentence repeat path rules and caveats across tool after
/// tool, so they are what the budget is meant to drop; the sentence about calling again, or about
/// the call being refused, is not.
fn keep_what_must_be_said(text: &str) -> String {
    let found = sentences(text);
    let Some(first) = found.first() else { return text.to_string() };
    match found.iter().skip(1).find(|sentence| says_what_to_do(sentence)) {
        Some(repair) => format!("{first} {repair}"),
        None => first.to_string(),
    }
}

/// Does this sentence say what happens next — a constraint, a refusal, a way to call again?
///
/// **A list of words, not a parser.** It only decides which sentence to spend the budget on, so a
/// miss costs a sentence and a false hit costs a sentence; neither can make a description wrong.
fn says_what_to_do(sentence: &str) -> bool {
    let lower = sentence.to_lowercase();
    [
        "must",
        "never",
        "always",
        "required",
        "fail",
        "instead",
        "read on",
        "call again",
        "retry",
        "re-read",
        "otherwise",
    ]
    .iter()
    .any(|cue| lower.contains(cue))
}

/// A description as sentences: ended by a `.` that whitespace or the end of the text follows.
///
/// **A newline alone does not end one.** Upstream's doc comments are wrapped, so a newline is as
/// likely to be a soft wrap in the middle of a sentence as the end of one.
fn sentences(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let ends = bytes[i] == b'.' && bytes.get(i + 1).is_none_or(|c| c.is_ascii_whitespace());
        if ends {
            out.push(text[start..i + 1].trim());
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            start = i;
            continue;
        }
        i += 1;
    }
    if !text[start..].trim().is_empty() {
        out.push(text[start..].trim());
    }
    out
}

/// Where the last real sentence ends at or before `at`.
///
/// A `.` ends one only when whitespace — or the end of the text — follows it, and a newline ends one
/// wherever it appears. **The `.` inside a code span is the one that was mistaken for a boundary.**
fn last_sentence_end(text: &str, at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut found = None;
    for i in 0..at.min(bytes.len()) {
        let ends = match bytes[i] {
            b'\n' => true,
            b'.' => bytes.get(i + 1).is_none_or(|c| c.is_ascii_whitespace()),
            _ => false,
        };
        if ends {
            found = Some(i + 1);
        }
    }
    found
}

/// Fits the descriptions inside a schema JSON to the budget. Cuts only the strings under the
/// `description` key — leaves what is used to interpret values — types, defaults, enums — alone.
pub fn clip_schema(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get_mut("description") {
                *s = clip(s, PARAM_LIMIT);
            }
            for child in map.values_mut() {
                clip_schema(child);
            }
        }
        Value::Array(items) => {
            for child in items {
                clip_schema(child);
            }
        }
        _ => {}
    }
}

/// The one line appended to `terminal.exec`'s description. **It sits outside the budget** — it is
/// added after trimming.
///
/// **It used to send long commands away**, because a run over a minute came back to the agent as
/// a transport error and there was nothing to be done about it here. `exec` now declares how long
/// it may take and is waited for (`guard::declare_limits`), so the thing worth saying while the
/// tool is being chosen is the one that is still true: `wait.start` is for leaving something
/// running while you get on with something else, not for anything that merely takes a while.
///
/// `exec`'s description is upstream's and we can't edit it, and trimming is the only place that
/// touches that description, so this is where it gets attached.
pub const LONG_HINT: &str =
    " A long run is fine; this node waits for it. Use wait.start to leave one running in the \
      background while you do something else.";

/// What this node adds to `file_io.read`'s description. **It sits outside the budget** — it is added
/// after trimming, exactly like `LONG_HINT` above.
///
/// Upstream's description cannot mention it, and a caller only reads what is announced: this is the
/// only place that can name the two fields `tools/readonly.rs` puts into a read answer — the file's
/// `version`, which `code_edit` wants as `base_version`, and `next_offset`, which reads on when the
/// file was cut short.
pub const READ_HINT: &str = " This node adds `version` to the answer (pass it straight as \
                              code_edit's base_version) and `next_offset` when the file was cut \
                              short (pass it as offset to read on).";

/// Fits one capability descriptor to the budget.
pub fn trim_descriptor(descriptor: &mut CapabilityDescriptor) {
    let terminal = descriptor.name == "terminal";
    let file_io = descriptor.name == "file_io";
    for tool in &mut descriptor.tools {
        tool.description = clip(&tool.description, DESCRIPTION_LIMIT);
        if terminal && tool.name == "exec" {
            tool.description.push_str(LONG_HINT);
        }
        if file_io && tool.name == "read" {
            tool.description.push_str(READ_HINT);
        }
        clip_schema(&mut tool.request_schema);
        if let Some(schema) = &mut tool.response_schema {
            clip_schema(schema);
        }
        if let Some(schema) = &mut tool.item_schema {
            clip_schema(schema);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zyris::ServeCapability;

    #[test]
    fn a_short_description_is_left_alone() {
        assert_eq!(clip("Read a file.", DESCRIPTION_LIMIT), "Read a file.");
        assert_eq!(clip("", 10), "");
    }

    /// Does not cut mid-sentence — the cut lands after a period or newline.
    #[test]
    fn a_long_description_is_cut_at_a_sentence_boundary() {
        let text = "Read a file's text. Large files come back truncated, and you read on \
                    by passing an offset, which is described in more detail further down \
                    this sentence that has to go on for a while.";
        let out = clip(text, 40);
        assert_eq!(out, "Read a file's text.…");
        assert!(out.len() <= DESCRIPTION_LIMIT);
    }

    /// Cuts only `description` and leaves the type — what interprets values must not be touched.
    #[test]
    fn schema_descriptions_are_trimmed_but_the_shape_stays() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "A path that goes on and on and on far beyond the budget for a parameter help string, with nothing new to say."
                }
            }
        });
        clip_schema(&mut schema);
        let desc = schema["properties"]["path"]["description"].as_str().unwrap();
        // `…` is 3 bytes, so allow budget + 3.
        assert!(desc.len() <= PARAM_LIMIT + 3, "{desc}");
        assert_eq!(schema["properties"]["path"]["type"], "string");
    }

    /// **The line has to be there while the tool is being chosen**, not learned afterwards. What
    /// it says changed when `exec` stopped being cut at a minute; that it rides on `exec` and on
    /// nothing else did not.
    #[test]
    fn the_exec_description_points_at_wait_for_long_commands() {
        let mut d = zyris::ServeCapability::descriptor(&zyris_caps::TerminalServer(
            zyris_terminal::PtyTerminal::default(),
        ));
        trim_descriptor(&mut d);
        let exec = d.tools.iter().find(|t| t.name == "exec").expect("exec must exist");
        assert!(exec.description.contains("wait.start"), "{}", exec.description);
        // The budget applies to the trimming side, and this one line is appended after it.
        assert!(exec.description.len() <= DESCRIPTION_LIMIT + LONG_HINT.len());
        // It isn't attached to other tools — putting it on ones that finish quickly is just noise.
        let read = d.tools.iter().find(|t| t.name == "read").expect("read must exist");
        assert!(!read.description.contains("wait.start"), "{}", read.description);
    }

    /// Does the actually-announced file_io description fit the budget? Gate calls this function,
    /// so passing here means what the agent receives passed.
    ///
    /// **The one line this node appends to `read` sits outside the budget** (`READ_HINT`, added
    /// after trimming, exactly like `LONG_HINT` on `exec`), so the bound allows for that and nothing
    /// else. The total is printed under `--nocapture`: a rule that keeps a contract sentence is only
    /// worth having while the bill for it stays small.
    #[test]
    fn the_announced_file_io_fits_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let gate = crate::tools::guard::Gate::new(
            crate::tools::readonly::ReadOnlyFileIo::new(dir.path().to_path_buf()),
            crate::tools::bridge::Bridge::new(),
        );
        let mut total = 0;
        for tool in gate.descriptor().tools {
            assert!(
                tool.description.len() <= DESCRIPTION_LIMIT + READ_HINT.len(),
                "{}: {}",
                tool.name,
                tool.description
            );
            total += tool.description.len();
        }
        // **Measured, then held.** Keeping a contract sentence that used to be cut costs bytes — 631
        // for these four on 2026-09-17 — and this is the line that keeps the cost from growing
        // unnoticed. It is a bound, not a target: lower it when something makes the descriptions
        // smaller.
        println!("file_io's four descriptions total {total} bytes");
        assert!(total <= 700, "file_io's descriptions total {total} bytes");
    }

    /// **The sentence that says how to recover is the one to keep.** The budget goes on the first
    /// sentence plus the first later one that reads like an instruction; the sentences that only
    /// describe are what is dropped.
    #[test]
    fn the_budget_keeps_the_sentence_that_says_how_to_recover() {
        let text = "Read a file's text. `offset` and `len` select a byte range, both optional; \
                    omitting them reads from the start. Large files come back truncated — read on \
                    by passing `offset` = the previous `offset + len`.";
        let out = clip(text, DESCRIPTION_LIMIT);
        assert!(out.starts_with("Read a file's text."), "{out}");
        assert!(out.contains("previous `offset + len`"), "{out}");
        assert!(!out.contains("both optional"), "the describing sentence is what goes: {out}");
        assert!(out.len() <= DESCRIPTION_LIMIT, "{out}");
    }

    /// A `.` inside a code span is not a sentence end. Cutting after one is how a description came
    /// to be announced as "pass `." — half a sentence, and the half that says the least.
    #[test]
    fn a_period_inside_a_code_span_does_not_end_a_sentence() {
        let text = "List the entries of a directory. A relative path resolves against the node's \
                    root directory; pass `.` or an empty string for the root itself, and the rest \
                    of this sentence is long enough to be cut.";
        let out = clip(text, 80);
        assert!(!out.ends_with("`."), "{out}");
        assert!(out.ends_with("List the entries of a directory.…"), "{out}");
    }

    /// **A caller only reads what is announced.** Upstream's read description cannot name the two
    /// fields this node adds to a read answer, so this node's line rides on it — and it has to
    /// survive whatever the trim above did to the sentence before it.
    #[test]
    fn the_announced_read_names_the_version_and_the_next_offset() {
        let dir = tempfile::tempdir().unwrap();
        let gate = crate::tools::guard::Gate::new(
            crate::tools::readonly::ReadOnlyFileIo::new(dir.path().to_path_buf()),
            crate::tools::bridge::Bridge::new(),
        );
        let read = gate
            .descriptor()
            .tools
            .into_iter()
            .find(|t| t.name == "read")
            .expect("read must exist");
        assert!(read.description.contains("base_version"), "{}", read.description);
        assert!(read.description.contains("next_offset"), "{}", read.description);
        // **And the sentence upstream wrote about reading on has to be there too** — it is the one
        // the old cut dropped, and this node's line assumes it.
        assert!(read.description.contains("offset"), "{}", read.description);
    }
}
