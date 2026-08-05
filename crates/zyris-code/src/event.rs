//! Moves one `ZSessionEvent` into one screen entry.
//!
//! This only looks at **one** event. Grouping into work cards is `timeline.rs`'s job.
//!
//! `payload` is untyped JSON and `kind` is a string. The canonical copy lives in attacca's
//! `attacca-domain/src/session_event.rs`.

use serde_json::Value;
use zyris_attacca::ZSessionEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub seq: i64,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    User(String),
    Agent(String),
    /// Start of a work run. Empty content means the title isn't decided yet.
    WorkStart(String),
    Thinking(String),
    Tool {
        name: String,
        summary: String,
        failed: bool,
        /// What to show when expanded — what it received and what it returned. Empty means nothing to expand.
        detail: String,
        /// Carries arguments and result only for `todo_*` tools — the sidebar tasks come from here.
        /// Carrying every other tool would make the timeline needlessly heavy.
        todo: Option<(serde_json::Value, Option<serde_json::Value>)>,
        /// For tools that changed files, what changed and how. The screen draws it in green/red.
        diff: Option<crate::tools::diff::Diff>,
    },
    /// What the agent asked. Picking an answer sends it back as an ordinary message.
    ///
    /// A question that already has an answer isn't shown again — `answered` is the marker for that.
    Question {
        steps: Vec<crate::question::Step>,
        answered: bool,
    },
    Todo(String),
    Subagent(String),
    Error(String),
}

/// `None` when there is nothing to render.
///
/// `recall` and `chat_system` are model-only events and are deliberately dropped — drawing them
/// would show messages the user never wrote in the conversation window.
pub fn entry_from(event: &ZSessionEvent) -> Option<Entry> {
    let p = &event.payload;
    let kind = match event.kind.as_str() {
        "chat_user" => EntryKind::User(text(p, "content")),
        "chat_agent" => EntryKind::Agent(text(p, "content")),
        "work_summary" => EntryKind::WorkStart(text(p, "content")),
        "thinking" => EntryKind::Thinking(text(p, "content")),
        "error" => EntryKind::Error(text(p, "message")),
        "todo_change" => EntryKind::Todo(text(p, "to_status")),
        "subagent_update" => EntryKind::Subagent(text(p, "summary")),
        "tool_call" => {
            let name = text(p, "name");
            let failed = p.get("error").is_some_and(|e| !e.is_null());
            // Questions arrive as tool calls, but they must become a pickable screen, not a one-line summary.
            if name == "question" {
                if let Some(steps) = p.get("arguments").and_then(crate::question::parse) {
                    // If a result is attached, the answer is already in. Asking again would be wrong.
                    let answered = p.get("result").is_some_and(|r| !r.is_null()) || failed;
                    return Some(Entry {
                        seq: event.seq,
                        kind: EntryKind::Question { steps, answered },
                    });
                }
            }
            let todo = name.starts_with("todo_").then(|| {
                (
                    p.get("arguments").cloned().unwrap_or(Value::Null),
                    p.get("result").cloned().filter(|r| !r.is_null()),
                )
            });
            EntryKind::Tool {
                summary: tool_summary(p, &name),
                detail: tool_detail(p, &name),
                diff: diff_of(&name, p.get("result")),
                name,
                failed,
                todo,
            }
        }
        // Model-only. Never rendered.
        "recall" | "chat_system" => return None,
        // Things v1 doesn't handle, and future kinds that don't exist yet. Ignore without dying.
        _ => return None,
    };
    Some(Entry { seq: event.seq, kind })
}

fn text(payload: &Value, field: &str) -> String {
    payload.get(field).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// The **short** argument summary for a tool row. The name isn't included here — the screen paints
/// the two separately.
///
/// The full arguments aren't expanded. Since the detail can be seen by pressing to expand, what
/// belongs here is the one piece of "what it was done to". **It used to just take the first JSON value**, which made `write`'s `content` come out whole and one tool row become a whole file.
const SUMMARY_LIMIT: usize = 56;

fn tool_summary(payload: &Value, name: &str) -> String {
    let args = payload.get("arguments");
    let pick = |k: &str| args.and_then(|a| a.get(k)).and_then(Value::as_str);
    let tail = name.rsplit("__").next().unwrap_or(name);

    let chosen = match tail {
        "exec" => pick("command"),
        "glob" | "grep" => pick("pattern"),
        "load" => pick("name"),
        "open" | "open_stream" => pick("shell").or(Some(crate::lang::current().default_shell())),
        // For things that keep a PTY going, which shell it is is everything.
        "read" | "write" | "screen" | "resize" | "close" if pick("pty").is_some() => pick("pty"),
        "edit" | "multi_edit" | "write" | "version" | "stat" | "list" | "read_stream" => {
            pick("path")
        }
        // For unknown tools (server built-ins · MCP), look through the common names first, and fall back to the first string.
        _ => ["path", "name", "query", "title", "content", "url", "id"]
            .into_iter()
            .find_map(pick)
            .or_else(|| args.and_then(Value::as_object)?.values().find_map(Value::as_str)),
    };
    clip_summary(chosen.unwrap_or_default())
}

/// Make it one line and clip it when long. **A leftover newline turns one tool row into several lines.**
fn clip_summary(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= SUMMARY_LIMIT {
        return one;
    }
    one.chars().take(SUMMARY_LIMIT).chain(['…']).collect()
}

/// Pulls the diff out of a file-changing tool's result.
///
/// attacca sends tool names as `zyris__{slug}__{capability}__{tool}`, so only the tail is looked
/// at. If it can't be pulled, it's `None` and falls back to a JSON dump as now — **not dying is the
/// point.** The result shape is ours to define (`tools::edit::EditResult`), but it's JSON that came
/// back around the server, so anything can arrive.
fn diff_of(name: &str, result: Option<&Value>) -> Option<crate::tools::diff::Diff> {
    let tail = name.rsplit("__").next()?;
    if !matches!(tail, "edit" | "multi_edit" | "write") {
        return None;
    }
    let r = result?;
    let text = r.get("diff")?.as_str()?;
    let path = r.get("path").and_then(Value::as_str).unwrap_or_default();
    let added = r.get("added")?.as_u64()? as u32;
    let removed = r.get("removed")?.as_u64()? as u32;
    crate::tools::diff::Diff::parse(text, path, added, removed)
}

/// Unfolds a `terminal` result so a person can read it.
///
/// Without this, passing through `to_string_pretty` would make `"stdout": "   Compiling…\n"` into
/// **escaped newlines and the entire build log on one line.**
///
/// A different shape means `None` and it falls back to JSON as now — the result shape is set
/// upstream (`zyris_caps::ExecOutput`) and it's JSON that came back around the server, so anything
/// can arrive. Same place, same approach as `diff_of`.
fn exec_detail(name: &str, result: Option<&Value>) -> Option<String> {
    let tail = name.rsplit("__").next()?;
    if !matches!(tail, "exec" | "read" | "screen") {
        return None;
    }
    let r = result?;
    // Only exec has an exit code. read·screen are a single screen text.
    let out = r.get("stdout").or_else(|| r.get("data")).or_else(|| r.get("text"))?.as_str()?;
    let err = r.get("stderr").and_then(Value::as_str).unwrap_or_default();

    let mut s = String::new();
    if r.get("timed_out").and_then(Value::as_bool) == Some(true) {
        s.push_str(crate::lang::current().detail_timed_out());
        s.push('\n');
    } else if let Some(code) = r.get("exit_code").and_then(Value::as_i64).filter(|c| *c != 0) {
        s.push_str(&crate::lang::current().detail_exit_code(code));
        s.push('\n');
    }
    if !out.is_empty() {
        s.push_str(out);
        if !s.ends_with('\n') {
            s.push('\n');
        }
    }
    if !err.is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str("stderr\n");
        s.push_str(err);
    }
    // **If nothing is said, the tool looks broken.** Quiet commands that succeed are common.
    if s.is_empty() {
        s.push_str(crate::lang::current().detail_no_output());
    }
    Some(s)
}

/// What to show when expanded — what it received and what it returned.
///
/// **It's hardened into text right here.** Holding the raw JSON would weigh down the timeline, and
/// it only ever reaches the screen as text anyway. Very long results are clipped too — one tool has
/// no reason to take thousands of screen lines, and the web UI holds the original if you want it all.
const DETAIL_LIMIT: usize = 4000;

fn tool_detail(payload: &Value, name: &str) -> String {
    let lang = crate::lang::current();
    let mut out = String::new();
    if let Some(args) = payload.get("arguments").filter(|v| !v.is_null()) {
        out.push_str(lang.detail_args());
        out.push('\n');
        out.push_str(&flatten(args));
    }
    if let Some(err) = payload.get("error").filter(|v| !v.is_null()) {
        push_section(&mut out, lang.detail_error(), err);
    } else if let Some(res) = payload.get("result").filter(|v| !v.is_null()) {
        // What the shell spat out is prose, not JSON.
        match exec_detail(name, Some(res)) {
            Some(text) => push_section(&mut out, lang.detail_output(), &Value::String(text)),
            None => push_section(&mut out, lang.detail_result(), res),
        }
    }
    clip(out)
}

fn push_section(out: &mut String, head: &str, v: &Value) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(head);
    out.push('\n');
    out.push_str(&flatten(v));
}

/// A string as-is, otherwise JSON pretty-printed.
fn flatten(v: &Value) -> String {
    match v.as_str() {
        Some(s) => s.to_string(),
        None => serde_json::to_string_pretty(v).unwrap_or_default(),
    }
}

fn clip(s: String) -> String {
    if s.chars().count() <= DETAIL_LIMIT {
        return s;
    }
    let cut: String = s.chars().take(DETAIL_LIMIT).collect();
    format!("{cut}\n{}", crate::lang::current().detail_clipped())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: i64, kind: &str, payload: serde_json::Value) -> ZSessionEvent {
        ZSessionEvent { seq, cursor: seq, kind: kind.into(), payload, created_at: None }
    }

    /// The wire name is `zyris__{node}__{capability}__{tool}`.
    fn wire(tool: &str) -> String {
        format!("zyris__arch__terminal__{tool}")
    }

    fn exec_json(code: i32, out: &str, err: &str, timed_out: bool) -> Value {
        json!({"exit_code": code, "stdout": out, "stderr": err, "timed_out": timed_out})
    }

    /// **The name isn't put in the summary.** The screen paints name and summary separately, so
    /// mixing them here would mean splitting them apart again.
    #[test]
    fn the_summary_is_only_the_argument() {
        let got =
            tool_summary(&json!({"arguments": {"command": "cargo build -j2"}}), &wire("exec"));
        assert_eq!(got, "cargo build -j2");
    }

    /// **Don't just take the first JSON value.** For `write`, `content` is the whole file, so if it
    /// became the tool row, one line would be a whole file.
    #[test]
    fn writing_a_file_is_summarised_by_its_path_not_its_content() {
        let got = tool_summary(
            &json!({"arguments": {"content": "아주 긴 파일 내용\n두 번째 줄\n", "path": "src/app.rs"}}),
            "zyris__arch__code_edit__write",
        );
        assert_eq!(got, "src/app.rs");
    }

    /// A leftover newline turns one tool row into several lines.
    #[test]
    fn a_summary_is_always_one_line() {
        let got =
            tool_summary(&json!({"arguments": {"command": "echo 하나\necho 둘"}}), &wire("exec"));
        assert!(!got.contains('\n'), "{got:?}");
    }

    /// Long ones are clipped. A tool row is for skimming, not reading.
    #[test]
    fn a_long_summary_is_clipped() {
        let got = tool_summary(&json!({"arguments": {"command": "x".repeat(400)}}), &wire("exec"));
        assert!(got.chars().count() <= SUMMARY_LIMIT + 1, "{}칸", got.chars().count());
        assert!(got.ends_with('…'), "잘렸다는 표시가 없다");
    }

    /// Unknown tools must still say something — server built-ins and MCP come here.
    #[test]
    fn an_unknown_tool_still_gets_a_summary() {
        assert_eq!(
            tool_summary(&json!({"arguments": {"query": "ratatui"}}), "web_search"),
            "ratatui"
        );
        assert_eq!(tool_summary(&json!({"arguments": {"무엇": "값"}}), "mystery"), "값");
        assert_eq!(tool_summary(&json!({"arguments": {}}), "mystery"), "");
    }

    /// **A real `ExecOutput` is serialized in.** If the shape drifts, it's caught here — the same
    /// seam test `diff_of` does with `EditResult`.
    #[test]
    fn a_real_exec_result_becomes_readable_lines() {
        let out = zyris_caps::ExecOutput {
            exit_code: 0,
            stdout: "   Compiling zyris-code\n    Finished dev\n".into(),
            stderr: String::new(),
            timed_out: false,
        };
        let got = exec_detail(&wire("exec"), Some(&serde_json::to_value(&out).unwrap()))
            .expect("exec 결과를 못 알아봤다");
        assert!(got.contains("   Compiling zyris-code\n    Finished dev"), "{got:?}");
        assert!(!got.contains("\\n"), "줄바꿈이 이스케이프로 남았다: {got:?}");
        assert!(!got.contains("exit_code"), "JSON 키가 그대로 보인다: {got:?}");
    }

    /// A failed command must show its exit code. If 0 and 3 can't be told apart, the log has to be read again.
    #[test]
    fn a_failed_command_shows_its_exit_code() {
        crate::lang::set(crate::lang::Lang::Ko);
        let got = exec_detail(
            &wire("exec"),
            Some(&exec_json(3, "", "error[E0308]: mismatched\n", false)),
        )
        .unwrap();
        assert!(got.contains("종료 코드 3"), "{got:?}");
        assert!(got.contains("E0308"), "{got:?}");
    }

    /// Timing out and producing nothing are different things.
    #[test]
    fn a_timed_out_command_says_so() {
        crate::lang::set(crate::lang::Lang::Ko);
        let got = exec_detail(&wire("exec"), Some(&exec_json(-1, "", "", true))).unwrap();
        assert!(got.contains("시간이 다 됐습니다"), "{got:?}");
    }

    /// If nothing is said, the tool looks broken.
    #[test]
    fn a_silent_command_still_says_something() {
        let got = exec_detail(&wire("exec"), Some(&exec_json(0, "", "", false))).unwrap();
        assert!(!got.trim().is_empty(), "빈 결과가 빈 화면이 됐다");
    }

    /// A different shape falls to `None` and JSON comes out as now. **Not dying is the point.**
    #[test]
    fn something_that_is_not_an_exec_result_falls_back() {
        assert!(exec_detail(&wire("exec"), Some(&json!({"nope": 1}))).is_none());
        assert!(exec_detail("zyris__arch__code_edit__edit", Some(&exec_json(0, "a", "", false)))
            .is_none());
        assert!(exec_detail(&wire("exec"), None).is_none());
    }

    /// A PTY screen is prose too, not JSON.
    #[test]
    fn a_pty_screen_is_shown_as_text_too() {
        let got = exec_detail(&wire("screen"), Some(&json!({"data": "$ ls\na.rs  b.rs\n"})))
            .expect("screen 결과를 못 알아봤다");
        assert!(got.contains("a.rs  b.rs"), "{got:?}");
        assert!(!got.contains("\\n"), "{got:?}");
    }

    /// **It must be the path that actually reaches the screen.** If only `exec_detail` is right and
    /// `tool_detail` doesn't call it, the user still sees raw JSON.
    #[test]
    fn the_readable_output_actually_reaches_the_tool_row() {
        let e = ev(
            7,
            "tool_call",
            json!({
                "name": wire("exec"),
                "arguments": {"command": "cargo build"},
                "result": exec_json(0, "   Compiling zyris-code\n", "", false),
            }),
        );
        let EntryKind::Tool { detail, .. } = entry_from(&e).unwrap().kind else {
            panic!("도구 항목이 아니다");
        };
        assert!(detail.contains("   Compiling zyris-code"), "{detail:?}");
        assert!(!detail.contains("\\n"), "{detail:?}");
    }

    #[test]
    fn a_user_message_becomes_a_user_entry() {
        let e = ev(1, "chat_user", json!({"kind": "chat_user", "content": "안녕"}));
        let entry = entry_from(&e).expect("사용자 메시지는 렌더한다");
        assert_eq!(entry.seq, 1);
        assert_eq!(entry.kind, EntryKind::User("안녕".into()));
    }

    /// A model-only event. Drawing it would show messages the user never wrote in the conversation window.
    #[test]
    fn recall_and_chat_system_are_never_rendered() {
        let recall = ev(2, "recall", json!({"kind": "recall", "content": "지난주에 …"}));
        let system =
            ev(3, "chat_system", json!({"kind": "chat_system", "content": "하위 에이전트 완료"}));
        assert_eq!(entry_from(&recall), None);
        assert_eq!(entry_from(&system), None);
    }

    /// A failed tool must stand out — it must not quietly look like a success.
    #[test]
    fn a_failed_tool_call_is_marked_failed() {
        let e = ev(
            4,
            "tool_call",
            json!({
                "kind": "tool_call", "call_id": "c1", "name": "web_search",
                "arguments": {"query": "ratatui"}, "result": null, "error": "timeout"
            }),
        );
        let entry = entry_from(&e).unwrap();
        match entry.kind {
            EntryKind::Tool { name, failed, .. } => {
                assert_eq!(name, "web_search");
                assert!(failed);
            }
            other => panic!("도구 항목이어야 한다: {other:?}"),
        }
    }

    /// work_summary is created with empty content at run start, and the title is filled in later.
    #[test]
    fn an_empty_work_summary_still_opens_a_card() {
        let e = ev(5, "work_summary", json!({"kind": "work_summary", "content": ""}));
        assert_eq!(entry_from(&e).unwrap().kind, EntryKind::WorkStart(String::new()));
    }

    /// A question must become a pickable screen. Let through as a one-line summary, there'd be no way to answer.
    #[test]
    fn a_question_tool_call_becomes_a_question_entry() {
        let e = ev(
            7,
            "tool_call",
            json!({
                "kind": "tool_call", "call_id": "c2", "name": "question",
                "arguments": {"questions": [
                    {"question": "어느 쪽?", "options": [{"label": "A"}, {"label": "B"}]}
                ]},
                "result": null, "error": null
            }),
        );
        match entry_from(&e).unwrap().kind {
            EntryKind::Question { steps, answered } => {
                assert_eq!(steps.len(), 1);
                assert_eq!(steps[0].options.len(), 2);
                assert!(!answered, "아직 답이 없다");
            }
            other => panic!("질문이어야 한다: {other:?}"),
        }
    }

    /// A question with a result attached is already answered, so it must not be shown again.
    #[test]
    fn an_answered_question_is_marked_answered() {
        let e = ev(
            8,
            "tool_call",
            json!({
                "kind": "tool_call", "call_id": "c3", "name": "question",
                "arguments": {"questions": [{"question": "어느 쪽?"}]},
                "result": {"status": "answered"}, "error": null
            }),
        );
        match entry_from(&e).unwrap().kind {
            EntryKind::Question { answered, .. } => assert!(answered),
            other => panic!("질문이어야 한다: {other:?}"),
        }
    }

    /// A malformed question is drawn as an ordinary tool call — it doesn't die.
    #[test]
    fn a_malformed_question_falls_back_to_a_plain_tool_row() {
        let e = ev(
            9,
            "tool_call",
            json!({
                "kind": "tool_call", "name": "question", "arguments": {}, "result": null, "error": null
            }),
        );
        assert!(matches!(entry_from(&e).unwrap().kind, EntryKind::Tool { .. }));
    }

    /// The screen side must re-read the diff carried in a tool result. If it can't, the diff won't show.
    #[test]
    fn a_code_edit_result_carries_its_diff() {
        let e = ev(
            10,
            "tool_call",
            json!({
                "kind": "tool_call", "name": "zyris__arch__code_edit__edit",
                "arguments": {"path": "src/app.rs"},
                "result": {"path": "src/app.rs", "added": 1, "removed": 1, "diff": "-옛\n+새\n"},
                "error": null
            }),
        );
        match entry_from(&e).unwrap().kind {
            EntryKind::Tool { diff: Some(d), .. } => {
                assert_eq!((d.added, d.removed), (1, 1));
                assert_eq!(d.lines.len(), 2);
            }
            other => panic!("diff가 붙어야 한다: {other:?}"),
        }
    }

    /// **It must round-trip in the exact shape the tool actually emits.**
    ///
    /// Change one field name of `EditResult`, or let `to_unified`/`parse` drift, and the diff
    /// silently vanishes into a JSON dump — the screen tests use a hand-made `Diff`, so they can't
    /// see that drift. This test catches that seam.
    #[test]
    fn a_real_edit_result_round_trips_into_a_drawable_diff() {
        let d = crate::tools::diff::diff("a\n옛 줄\nc\n", "a\n새 줄\nc\n", "src/app.rs");
        let result = serde_json::to_value(crate::tools::edit::EditResult {
            path: d.path.clone(),
            added: d.added,
            removed: d.removed,
            diff: d.to_unified(),
            version: "0:0".into(),
        })
        .unwrap();
        let e = ev(
            12,
            "tool_call",
            json!({
                "kind": "tool_call", "name": "zyris__arch__code_edit__edit",
                "arguments": {"path": "src/app.rs"}, "result": result, "error": null
            }),
        );
        match entry_from(&e).unwrap().kind {
            EntryKind::Tool { diff: Some(back), .. } => assert_eq!(back, d),
            other => panic!("diff가 붙어야 한다: {other:?}"),
        }
    }

    /// Not the shape? It doesn't die — it falls back to a JSON dump as now.
    #[test]
    fn a_result_without_a_diff_is_still_a_plain_tool_row() {
        let e = ev(
            11,
            "tool_call",
            json!({
                "kind": "tool_call", "name": "web_search",
                "arguments": {"query": "x"}, "result": {"hits": 3}, "error": null
            }),
        );
        assert!(matches!(entry_from(&e).unwrap().kind, EntryKind::Tool { diff: None, .. }));
    }

    /// An unknown kind must not kill the app. It must survive even when attacca adds new events.
    #[test]
    fn an_unknown_kind_is_ignored_rather_than_panicking() {
        let e = ev(6, "some_future_kind", json!({"kind": "some_future_kind"}));
        assert_eq!(entry_from(&e), None);
    }
}
