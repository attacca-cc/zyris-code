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
    /// One reasoning block. **`title` is the server's, not ours** — `agent_runtime.rs`'s
    /// `spawn_thought_title` labels each block with a small model and updates the event in place,
    /// so a card that opened untitled fills in by itself (the `seq`-keyed upsert is what makes that
    /// land in the same row). `None` until that lands, and forever if the side model is off.
    Thinking {
        title: Option<String>,
        text: String,
    },
    Tool {
        name: String,
        /// What was run, against what. Built by `tool_view::action`.
        action: String,
        state: crate::tool_view::ToolState,
        /// What to show when expanded, already hardened into a drawable shape.
        detail: crate::tool_view::Detail,
    },
    /// What the agent asked. Picking an answer sends it back as an ordinary message.
    ///
    /// A question that already has an answer isn't shown again — `answered` is the marker for that.
    Question {
        steps: Vec<crate::question::Step>,
        answered: bool,
    },
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
        "thinking" => EntryKind::Thinking {
            title: p
                .get("title")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string),
            text: text(p, "content"),
        },
        "error" => EntryKind::Error(text(p, "message")),
        // `todo_change` carries `{todo_item_id, from_status, to_status}` and **no text**, so there
        // is nothing to put on screen but a bare status word. Dropped until the server sends the
        // todo's own words (2026-08-10).
        "todo_change" => return None,
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
            let args = p.get("arguments");
            let result = p.get("result").filter(|v| !v.is_null());
            EntryKind::Tool {
                action: crate::tool_view::action(&name, args, result),
                detail: crate::tool_view::detail(&name, args, result, p.get("error")),
                // **Pending is `no result and no error`, not "the turn is running".** attacca
                // writes the event when the call starts and updates it in place when it returns.
                state: match (failed, result.is_some()) {
                    (true, _) => crate::tool_view::ToolState::Failed,
                    (false, true) => crate::tool_view::ToolState::Ok,
                    (false, false) => crate::tool_view::ToolState::Pending,
                },
                name,
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

    /// **The readable path must be the one that actually reaches the screen.** If only
    /// `tool_view::detail` is right and `entry_from` doesn't call it, the person still sees raw JSON.
    ///
    /// A real `ExecOutput` is serialized in, so a drift in the upstream shape is caught here — the
    /// same seam that caught capkit v3 adding `stdout_truncated`.
    #[test]
    fn a_real_exec_result_reaches_the_tool_row_as_readable_lines() {
        let out = zyris_caps::ExecOutput {
            exit_code: 0,
            stdout: "   Compiling zyris-code\n    Finished dev\n".into(),
            stderr: String::new(),
            timed_out: false,
            stdout_truncated: false,
            stderr_truncated: false,
        };
        let e = ev(
            7,
            "tool_call",
            json!({
                "name": wire("exec"),
                "arguments": {"command": "cargo build"},
                "result": serde_json::to_value(&out).unwrap(),
            }),
        );
        let EntryKind::Tool { detail, action, state, .. } = entry_from(&e).unwrap().kind else {
            panic!("not a tool entry");
        };
        assert_eq!(action, "cargo build");
        assert_eq!(state, crate::tool_view::ToolState::Ok);
        match detail {
            crate::tool_view::Detail::Exec { exit, out, .. } => {
                assert_eq!(exit, Some(0));
                assert!(out.contains("   Compiling zyris-code\n    Finished dev"), "{out:?}");
            }
            other => panic!("expected an exec detail, got {other:?}"),
        }
    }

    /// **A call with no result yet is pending, not a success.** attacca writes the event when the
    /// call starts and updates it in place when it returns; painting that green would say a build
    /// finished the moment it began.
    #[test]
    fn a_call_that_has_not_returned_is_pending() {
        let e = ev(
            6,
            "tool_call",
            json!({"name": wire("exec"), "arguments": {"command": "cargo build"}, "result": null}),
        );
        let EntryKind::Tool { state, .. } = entry_from(&e).unwrap().kind else {
            panic!("not a tool entry");
        };
        assert_eq!(state, crate::tool_view::ToolState::Pending);
    }

    /// The server labels each reasoning block with a small model and updates the event in place.
    /// **Dropping that title is what made the client clip a mid-sentence heading of its own.**
    #[test]
    fn a_thinking_event_keeps_the_title_the_server_gave_it() {
        let e =
            ev(3, "thinking", json!({"content": "먼저 …", "title": "현재 파일 상태를 읽는 중"}));
        assert_eq!(
            entry_from(&e).unwrap().kind,
            EntryKind::Thinking {
                title: Some("현재 파일 상태를 읽는 중".into()),
                text: "먼저 …".into(),
            }
        );
    }

    /// The title lands later (or never, if the side model is off). Until then there is none —
    /// the screen falls back on its own, and the `seq` upsert swaps it in when it arrives.
    #[test]
    fn a_thinking_event_without_a_title_yet_carries_none() {
        let e = ev(3, "thinking", json!({"content": "먼저 …", "title": null}));
        let EntryKind::Thinking { title, .. } = entry_from(&e).unwrap().kind else {
            panic!("not a thinking entry");
        };
        assert_eq!(title, None);
        let blank = ev(4, "thinking", json!({"content": "먼저 …", "title": "   "}));
        let EntryKind::Thinking { title, .. } = entry_from(&blank).unwrap().kind else {
            panic!("not a thinking entry");
        };
        assert_eq!(title, None, "a blank title is no title");
    }

    /// `todo_change` carries `{todo_item_id, from_status, to_status}` and no text at all, so all it
    /// could ever draw is a bare status word. It is dropped until the server sends the words.
    #[test]
    fn a_todo_change_draws_nothing() {
        let e = ev(5, "todo_change", json!({"todo_item_id": "x", "to_status": "in_progress"}));
        assert_eq!(entry_from(&e), None);
    }

    #[test]
    fn a_user_message_becomes_a_user_entry() {
        let e = ev(1, "chat_user", json!({"kind": "chat_user", "content": "안녕"}));
        let entry = entry_from(&e).expect("a user message is rendered");
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
            EntryKind::Tool { name, state, .. } => {
                assert_eq!(name, "web_search");
                assert_eq!(state, crate::tool_view::ToolState::Failed);
            }
            other => panic!("it must be a tool entry: {other:?}"),
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
                assert!(!answered, "there is no answer yet");
            }
            other => panic!("it must be a question: {other:?}"),
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
            other => panic!("it must be a question: {other:?}"),
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
            EntryKind::Tool { detail: crate::tool_view::Detail::Diff(d), .. } => {
                assert_eq!((d.added, d.removed), (1, 1));
                assert_eq!(d.lines.len(), 2);
            }
            other => panic!("a diff must be attached: {other:?}"),
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
            EntryKind::Tool { detail: crate::tool_view::Detail::Diff(back), .. } => {
                assert_eq!(back, d)
            }
            other => panic!("a diff must be attached: {other:?}"),
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
        assert!(matches!(
            entry_from(&e).unwrap().kind,
            EntryKind::Tool { detail: crate::tool_view::Detail::Json { .. }, .. }
        ));
    }

    /// An unknown kind must not kill the app. It must survive even when attacca adds new events.
    #[test]
    fn an_unknown_kind_is_ignored_rather_than_panicking() {
        let e = ev(6, "some_future_kind", json!({"kind": "some_future_kind"}));
        assert_eq!(entry_from(&e), None);
    }
}
