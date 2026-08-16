//! What a tool call looks like on screen — **the one place that knows tools by name.**
//!
//! `event.rs` moves an event into an entry and `rows.rs` draws lines; neither should have to know
//! that `terminal.exec`'s interesting argument is `command` while `search.grep`'s is `pattern`.
//! That knowledge lives here, and it produces exactly two things:
//!
//! * [`action`] — the words beside the tool name. **What was run, not the first argument.**
//!   It used to be "the first string in the arguments JSON, clipped to 56", which made `write`'s
//!   `content` come out whole and turned one tool row into a file.
//! * [`detail`] — what to show when the row is opened, **hardened into a drawable shape right
//!   here.** Holding raw JSON would weigh the timeline down, and it only ever reaches the screen
//!   as text anyway.
//!
//! **A tool this file has never heard of must still work.** Results come back around the server as
//! untyped JSON, so anything can arrive; every reader below is defensive and falls through to
//! [`Detail::Json`], which is the plain dump the app has always shown.

use serde_json::Value;

use crate::tools::diff::Diff;

/// How far along one tool call is.
///
/// **Read from `result`/`error` presence, not from the turn's running flag.** attacca writes a
/// `tool_call` event when the call starts (`result: null`) and updates it in place when it returns.
/// Guessing "the last tool of a running turn is the live one" gets parallel calls wrong — every
/// call but the last would be painted as finished while it was still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Pending,
    Ok,
    Failed,
}

/// One `search.grep` match, kept for drawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub line: u32,
    pub text: String,
}

/// What an opened tool row shows. **Built when the event arrives**, never from live JSON.
///
/// `Eq` matters: `rows::Cache` compares whole `Item`s to decide what to redraw.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Detail {
    /// Nothing to expand — pressing the row does nothing, so it isn't made clickable.
    #[default]
    None,
    /// A file change. Only the diff is drawn; the same content as JSON would double the height.
    Diff(Diff),
    /// A shell run. `exit` is absent for the PTY reads that share this shape.
    Exec { exit: Option<i64>, timed_out: bool, out: String, err: String },
    /// Content matches, with the files-scanned count that tells a narrow pattern from a wide one.
    Hits { scanned: u32, hits: Vec<Hit>, truncated: bool },
    /// A list of paths — `search.glob` and `file_io.list`.
    Paths { paths: Vec<String>, truncated: bool },
    /// One labelled block of text — a file that was read, a skill that was loaded.
    Body { label: String, text: String },
    /// The fallback: arguments and result, pretty-printed. **Every tool this file doesn't know.**
    Json { args: String, result: String },
}

/// How much of a body is kept. One tool has no reason to hold thousands of screen lines, and the
/// web UI keeps the original if the whole thing is wanted.
const BODY_LIMIT: usize = 4000;
/// How many hits/paths are kept. Beyond this the row stops being skimmable.
const ROW_LIMIT: usize = 60;
/// How wide the words beside a tool name may get.
const ACTION_LIMIT: usize = 72;

/// The tool's own name, without attacca's `zyris__{node}__{capability}__` prefix.
fn tail(name: &str) -> &str {
    name.rsplit("__").next().unwrap_or(name)
}

/// The capability segment, when the wire name carries one — `terminal` in
/// `zyris__arch__terminal__read`. Two capabilities share the tool name `read`, and they mean
/// entirely different things.
fn cap(name: &str) -> &str {
    let mut parts = name.rsplit("__");
    parts.next();
    parts.next().unwrap_or("")
}

/// The words shown beside the tool name: **what it was run against.**
pub fn action(name: &str, args: Option<&Value>, result: Option<&Value>) -> String {
    let lang = crate::lang::current();
    let s = |k: &str| args.and_then(|a| a.get(k)).and_then(Value::as_str).filter(|v| !v.is_empty());
    let n = |k: &str| args.and_then(|a| a.get(k)).and_then(Value::as_i64);

    let text = match (cap(name), tail(name)) {
        // A shell line is the whole story. Whitespace is folded so a heredoc stays one row.
        (_, "exec") => s("command").map(one_line),
        // The changed file is authoritative from the result — the argument's path may be relative.
        (_, "edit" | "multi_edit" | "write") => result
            .and_then(|r| r.get("path"))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .or_else(|| s("path"))
            .map(str::to_string),
        (_, "glob") => Some(join(s("pattern"), s("path"))),
        (_, "grep") => Some(join(
            s("pattern").map(|p| format!("\"{p}\"")).as_deref(),
            s("glob").or_else(|| s("path")),
        )),
        // `file_io.read` takes a byte range; saying which range was asked for keeps repeated reads
        // of one file apart.
        ("file_io", "read" | "read_stream") => {
            Some(join(s("path"), n("offset").map(|o| lang.action_from_byte(o)).as_deref()))
        }
        ("file_io", _) => s("path").map(str::to_string),
        // For anything that keeps a PTY going, which PTY it is is everything.
        ("terminal", "open" | "open_stream") => {
            Some(s("shell").unwrap_or(lang.default_shell()).to_string())
        }
        ("terminal", _) => s("pty").map(str::to_string),
        (_, "load") => s("name").map(str::to_string),
        // `wait.start` takes either a shell line or an argv vector.
        (_, "start") => s("command").map(one_line).or_else(|| argv(args)),
        (_, "until") => s("command")
            .map(one_line)
            .or_else(|| s("job").map(str::to_string))
            .or_else(|| s("work").map(str::to_string)),
        (_, "logs" | "stop") => s("job").map(str::to_string),
        // The `work` capability: a goal when one is being made, an id when one is being driven.
        ("work", _) => s("goal").map(one_line).or_else(|| s("work").map(str::to_string)),
        // Unknown tools — server built-ins and MCP. Look through the usual names, then take the
        // first string. **`content` is deliberately last**: a whole file body is a poor label.
        _ => ["path", "pattern", "query", "name", "title", "url", "id", "content"]
            .into_iter()
            .find_map(s)
            .map(one_line)
            .or_else(|| {
                args.and_then(Value::as_object)?.values().find_map(Value::as_str).map(one_line)
            }),
    };
    clip(&text.unwrap_or_default(), ACTION_LIMIT)
}

/// `a · b`, dropping whichever side is missing so no separator is left dangling.
fn join(a: Option<&str>, b: Option<&str>) -> String {
    match (a.filter(|s| !s.is_empty()), b.filter(|s| !s.is_empty())) {
        (Some(a), Some(b)) => format!("{a} ∙ {b}"),
        (Some(a), None) => a.to_string(),
        (None, Some(b)) => b.to_string(),
        (None, None) => String::new(),
    }
}

/// `wait.start`'s vector form, shown the way it would be typed.
fn argv(args: Option<&Value>) -> Option<String> {
    let parts: Vec<&str> =
        args?.get("argv")?.as_array()?.iter().filter_map(Value::as_str).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Folds all whitespace to single spaces. **A leftover newline turns one tool row into several.**
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    s.chars().take(limit).chain(['…']).collect()
}

fn clip_body(s: String) -> String {
    if s.chars().count() <= BODY_LIMIT {
        return s;
    }
    let cut: String = s.chars().take(BODY_LIMIT).collect();
    format!("{cut}\n{}", crate::lang::current().detail_clipped())
}

/// What an opened row shows.
///
/// The order matters: an error outranks everything (what failed is what should be read), a diff
/// outranks the tool's own shape, and everything unrecognized lands in [`Detail::Json`].
pub fn detail(
    name: &str,
    args: Option<&Value>,
    result: Option<&Value>,
    error: Option<&Value>,
) -> Detail {
    let lang = crate::lang::current();
    let args = args.filter(|v| !v.is_null());
    let result = result.filter(|v| !v.is_null());

    if let Some(e) = error.filter(|v| !v.is_null()) {
        return Detail::Json { args: pretty(args), result: flatten(e) };
    }
    // A call that hasn't returned yet still shows what it was asked to do.
    let Some(res) = result else {
        let args = pretty(args);
        return match args.is_empty() {
            true => Detail::None,
            false => Detail::Json { args, result: String::new() },
        };
    };
    if let Some(d) = diff_of(name, res) {
        return Detail::Diff(d);
    }
    match (cap(name), tail(name)) {
        (_, "exec") => exec_of(res).unwrap_or_else(|| json(args, res)),
        ("terminal", "read" | "screen") => screen_of(res).unwrap_or_else(|| json(args, res)),
        (_, "grep") => hits_of(res).unwrap_or_else(|| json(args, res)),
        (_, "glob") => paths_of(res).unwrap_or_else(|| json(args, res)),
        ("file_io", "list") => entries_of(res).unwrap_or_else(|| json(args, res)),
        ("file_io", "read") => read_of(res).unwrap_or_else(|| json(args, res)),
        (_, "load") => body_of(res, lang.detail_output()).unwrap_or_else(|| json(args, res)),
        _ => json(args, res),
    }
}

fn json(args: Option<&Value>, res: &Value) -> Detail {
    Detail::Json { args: pretty(args), result: flatten(res) }
}

/// **An empty object is nothing to show.** Left in, a no-argument tool draws an "Args / {}" block
/// and the row looks pressable while holding nothing.
fn pretty(v: Option<&Value>) -> String {
    match v.map(flatten) {
        Some(s) if s.trim() == "{}" || s.trim() == "[]" || s.trim().is_empty() => String::new(),
        Some(s) => s,
        None => String::new(),
    }
}

/// A string as-is, otherwise JSON pretty-printed.
fn flatten(v: &Value) -> String {
    clip_body(match v.as_str() {
        Some(s) => s.to_string(),
        None => serde_json::to_string_pretty(v).unwrap_or_default(),
    })
}

/// `zyris_caps::ExecOutput`.
fn exec_of(r: &Value) -> Option<Detail> {
    let out = r.get("stdout")?.as_str()?.to_string();
    let err = r.get("stderr").and_then(Value::as_str).unwrap_or_default().to_string();
    Some(Detail::Exec {
        exit: r.get("exit_code").and_then(Value::as_i64),
        timed_out: r.get("timed_out").and_then(Value::as_bool).unwrap_or(false),
        out: clip_body(out),
        err: clip_body(err),
    })
}

/// `zyris_caps::PtyRead` / `PtyScreen` — one screenful of a live terminal.
fn screen_of(r: &Value) -> Option<Detail> {
    let text = match r.get("content").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => {
            let lines = r.get("lines")?.as_array()?;
            lines.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")
        }
    };
    Some(Detail::Exec {
        exit: r.get("exited").and_then(Value::as_i64),
        timed_out: false,
        out: clip_body(text),
        err: String::new(),
    })
}

/// `tools::search::Found`.
fn hits_of(r: &Value) -> Option<Detail> {
    let raw = r.get("hits")?.as_array()?;
    let hits = raw
        .iter()
        .take(ROW_LIMIT)
        .filter_map(|h| {
            Some(Hit {
                path: h.get("path")?.as_str()?.to_string(),
                line: h.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
                text: one_line(h.get("text").and_then(Value::as_str).unwrap_or_default()),
            })
        })
        .collect();
    Some(Detail::Hits {
        scanned: r.get("scanned").and_then(Value::as_u64).unwrap_or(0) as u32,
        hits,
        truncated: r.get("truncated").and_then(Value::as_bool).unwrap_or(false)
            || raw.len() > ROW_LIMIT,
    })
}

/// `search.glob` — a bare array of paths.
fn paths_of(r: &Value) -> Option<Detail> {
    let raw = r.as_array()?;
    Some(Detail::Paths {
        paths: raw.iter().take(ROW_LIMIT).filter_map(Value::as_str).map(str::to_string).collect(),
        truncated: raw.len() > ROW_LIMIT,
    })
}

/// `zyris_caps::DirEntry` — a directory listing. Directories get a trailing `/` so the two kinds
/// are told apart at a glance.
fn entries_of(r: &Value) -> Option<Detail> {
    let raw = r.as_array()?;
    let paths = raw
        .iter()
        .take(ROW_LIMIT)
        .filter_map(|e| {
            let name = e.get("name")?.as_str()?;
            let dir = e.get("is_dir").and_then(Value::as_bool).unwrap_or(false);
            Some(if dir { format!("{name}/") } else { name.to_string() })
        })
        .collect();
    Some(Detail::Paths { paths, truncated: raw.len() > ROW_LIMIT })
}

/// `zyris_caps::FileRead`.
fn read_of(r: &Value) -> Option<Detail> {
    let text = r.get("content")?.as_str()?.to_string();
    let label = r
        .get("stat")
        .and_then(|s| s.get("path"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(Detail::Body { label, text: clip_body(text) })
}

/// A result that is one lump of text — `skill.load` and friends.
fn body_of(r: &Value, label: &str) -> Option<Detail> {
    let text = r
        .as_str()
        .map(str::to_string)
        .or_else(|| Some(r.get("content")?.as_str()?.to_string()))
        .or_else(|| Some(r.get("text")?.as_str()?.to_string()))?;
    Some(Detail::Body { label: label.to_string(), text: clip_body(text) })
}

/// Pulls the diff out of a file-changing tool's result.
///
/// If it can't be pulled it's `None` and the row falls back to a JSON dump — **not dying is the
/// point.** The result shape is ours (`tools::edit::EditResult`), but it came back around the
/// server as JSON, so anything can arrive.
fn diff_of(name: &str, r: &Value) -> Option<Diff> {
    if !matches!(tail(name), "edit" | "multi_edit" | "write") {
        return None;
    }
    Diff::parse(
        r.get("diff")?.as_str()?,
        r.get("path").and_then(Value::as_str).unwrap_or_default(),
        r.get("added")?.as_u64()? as u32,
        r.get("removed")?.as_u64()? as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wire(capability: &str, tool: &str) -> String {
        format!("zyris__arch-zyris-code__{capability}__{tool}")
    }

    #[test]
    fn an_exec_row_says_the_command_not_the_first_argument() {
        let args = json!({"command": "cargo test -p zyris-code", "timeout_ms": 50_000});
        assert_eq!(
            action(&wire("terminal", "exec"), Some(&args), None),
            "cargo test -p zyris-code"
        );
    }

    /// The old summary took the first string in the arguments, so a whole file body became the label.
    #[test]
    fn a_write_row_says_the_file_not_its_contents() {
        let args = json!({"content": "fn main() {}\n".repeat(100), "path": "src/main.rs"});
        let res = json!({"path": "src/main.rs", "diff": "", "added": 1, "removed": 0});
        assert_eq!(action(&wire("code_edit", "write"), Some(&args), Some(&res)), "src/main.rs");
    }

    #[test]
    fn a_grep_row_shows_the_pattern_and_where_it_looked() {
        let args = json!({"pattern": "fn row_line", "glob": "**/*.rs"});
        assert_eq!(action(&wire("search", "grep"), Some(&args), None), "\"fn row_line\" ∙ **/*.rs");
    }

    /// `file_io.read` and `terminal.read` share a tool name and mean different things.
    #[test]
    fn the_two_reads_are_told_apart_by_capability() {
        let file = json!({"path": "src/rows.rs"});
        let pty = json!({"pty": "pty-7"});
        assert_eq!(action(&wire("file_io", "read"), Some(&file), None), "src/rows.rs");
        assert_eq!(action(&wire("terminal", "read"), Some(&pty), None), "pty-7");
    }

    /// A missing half must not leave `a · ` behind.
    #[test]
    fn a_missing_piece_leaves_no_separator_behind() {
        let args = json!({"pattern": "**/*.rs"});
        assert_eq!(action(&wire("search", "glob"), Some(&args), None), "**/*.rs");
    }

    #[test]
    fn a_multiline_command_stays_one_row() {
        let args = json!({"command": "cat <<EOF\nhello\nEOF"});
        let out = action(&wire("terminal", "exec"), Some(&args), None);
        assert!(!out.contains('\n'), "a newline would split the row: {out:?}");
    }

    #[test]
    fn an_exec_result_becomes_an_exec_detail() {
        let res = json!({"exit_code": 0, "stdout": "ok\n", "stderr": "", "timed_out": false});
        match detail(&wire("terminal", "exec"), None, Some(&res), None) {
            Detail::Exec { exit, out, .. } => {
                assert_eq!(exit, Some(0));
                assert_eq!(out, "ok\n");
            }
            other => panic!("expected an exec detail, got {other:?}"),
        }
    }

    #[test]
    fn a_grep_result_becomes_hits() {
        let res = json!({
            "hits": [{"path": "src/rows.rs", "line": 88, "text": "fn row_line() {"}],
            "truncated": false,
            "scanned": 42,
        });
        match detail(&wire("search", "grep"), None, Some(&res), None) {
            Detail::Hits { scanned, hits, truncated } => {
                assert_eq!((scanned, truncated), (42, false));
                assert_eq!(hits[0].line, 88);
                assert_eq!(hits[0].path, "src/rows.rs");
            }
            other => panic!("expected hits, got {other:?}"),
        }
    }

    #[test]
    fn a_directory_listing_marks_its_directories() {
        let res = json!([{"name": "src", "is_dir": true}, {"name": "Cargo.toml", "is_dir": false}]);
        match detail(&wire("file_io", "list"), None, Some(&res), None) {
            Detail::Paths { paths, .. } => assert_eq!(paths, vec!["src/", "Cargo.toml"]),
            other => panic!("expected paths, got {other:?}"),
        }
    }

    /// The whole point of the fallback: a tool this file has never heard of still opens.
    #[test]
    fn an_unknown_tool_falls_back_to_the_json_dump() {
        let args = json!({"q": "weather"});
        let res = json!({"answer": "rain"});
        match detail("web_search", Some(&args), Some(&res), None) {
            Detail::Json { args, result } => {
                assert!(args.contains("weather"));
                assert!(result.contains("rain"));
            }
            other => panic!("expected a json dump, got {other:?}"),
        }
    }

    /// A shape the reader doesn't recognize must not swallow the result.
    #[test]
    fn a_result_of_the_wrong_shape_falls_back_instead_of_vanishing() {
        let res = json!({"unexpected": true});
        match detail(&wire("terminal", "exec"), None, Some(&res), None) {
            Detail::Json { result, .. } => assert!(result.contains("unexpected")),
            other => panic!("expected a json dump, got {other:?}"),
        }
    }

    /// An error is what should be read, whatever the tool is.
    #[test]
    fn an_error_outranks_the_tools_own_shape() {
        let res = json!({"exit_code": 0, "stdout": "", "stderr": "", "timed_out": false});
        let err = json!("permission denied");
        match detail(&wire("terminal", "exec"), None, Some(&res), Some(&err)) {
            Detail::Json { result, .. } => assert_eq!(result, "permission denied"),
            other => panic!("expected the error, got {other:?}"),
        }
    }

    /// A call still in flight has no result, but what it was asked to do is worth reading.
    #[test]
    fn a_pending_call_still_shows_what_it_was_asked_to_do() {
        let args = json!({"command": "cargo build"});
        match detail(&wire("terminal", "exec"), Some(&args), None, None) {
            Detail::Json { args, result } => {
                assert!(args.contains("cargo build"));
                assert!(result.is_empty());
            }
            other => panic!("expected the args alone, got {other:?}"),
        }
    }

    #[test]
    fn a_body_longer_than_the_limit_is_clipped() {
        let res = json!({"exit_code": 0, "stdout": "x".repeat(BODY_LIMIT * 2), "stderr": "", "timed_out": false});
        match detail(&wire("terminal", "exec"), None, Some(&res), None) {
            Detail::Exec { out, .. } => {
                assert!(out.chars().count() < BODY_LIMIT * 2);
                assert!(
                    out.ends_with(crate::lang::Lang::En.detail_clipped())
                        || out.ends_with(crate::lang::Lang::Ko.detail_clipped())
                );
            }
            other => panic!("expected an exec detail, got {other:?}"),
        }
    }
}
