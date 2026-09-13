//! The `report_result` call, in a form a person can read.
//!
//! **A job's result is the one thing the window cannot infer.** Everything else about a long run —
//! what it touched, what failed, how long it took — is a row in the conversation, and the rows
//! scroll away. The report is the sentence the agent wrote to say what came of it, and until now
//! it only ever appeared as one line on the status bar for a few seconds.
//!
//! So it becomes a card, in the same place the question card goes: the turn is over and this is
//! what is left to say. `Esc` or `Enter` puts it away.
//!
//! This module is pure — it only reads a session event.

use serde_json::Value;

/// One report, as the agent wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The event it came in on. Two reports in one thread are two different cards.
    pub seq: i64,
    /// Whether the work succeeded. The card's colour and one word come from this.
    pub ok: bool,
    /// The agent's own sentence, whole. Never cut — see `widgets::report`.
    pub summary: String,
}

/// The report a session event carries, if it carries one.
///
/// **By the tool's name, not by the capability's.** `report_result` is attacca's own tool, so it
/// arrives unprefixed — but a node's tools are `zyris__node__cap__tool`, and matching on the tail
/// keeps this from depending on which of those two shapes a deployment sends. (Same rule, same
/// reason, as `plan::submitted_from`.)
pub fn of(event: &zyris_attacca::ZSessionEvent) -> Option<Report> {
    if event.kind != "tool_call" {
        return None;
    }
    let payload = &event.payload;
    let name = payload.get("name").and_then(Value::as_str).unwrap_or_default();
    if name.rsplit("__").next() != Some("report_result") {
        return None;
    }
    let args = payload.get("arguments");
    let summary = args
        .and_then(|a| a.get("summary"))
        .and_then(Value::as_str)
        // A deployment that puts the sentence in the result instead of the arguments is still
        // saying something worth showing.
        .or_else(|| payload.get("result").and_then(Value::as_str))
        .unwrap_or_default()
        .trim()
        .to_string();
    // **A report with no words is not a report.** An argument that failed to arrive would put an
    // empty card over the input with nothing to read and no reason to be there.
    if summary.is_empty() {
        return None;
    }
    let ok = match args.and_then(|a| a.get("status")).and_then(Value::as_str) {
        Some(status) => status == "success",
        // A deployment that says nothing about status: what is left is whether the call itself
        // came back in error.
        None => !payload.get("error").is_some_and(|e| !e.is_null()),
    };
    Some(Report { seq: event.seq, ok, summary })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: i64, payload: Value) -> zyris_attacca::ZSessionEvent {
        zyris_attacca::ZSessionEvent {
            seq,
            cursor: seq,
            kind: "tool_call".into(),
            payload,
            created_at: None,
        }
    }

    fn call(args: Value, result: Value) -> Value {
        json!({ "name": "report_result", "arguments": args, "result": result, "error": null })
    }

    /// The ordinary shape: `{status, summary}`.
    #[test]
    fn a_report_is_read_out_of_the_call_that_made_it() {
        let r = of(&ev(
            7,
            call(json!({"status": "success", "summary": "빌드가 통과했습니다."}), json!("ok")),
        ))
        .expect("a report");
        assert_eq!(r.seq, 7);
        assert!(r.ok);
        assert_eq!(r.summary, "빌드가 통과했습니다.");
    }

    /// **A failure is a report too** — the one a person most needs to see.
    #[test]
    fn a_failed_report_comes_through_as_a_failure() {
        let r = of(&ev(
            1,
            call(json!({"status": "failure", "summary": "테스트가 깨졌습니다."}), json!(null)),
        ))
        .expect("a report");
        assert!(!r.ok);
    }

    /// Status missing: the call's own error is what is left to go on.
    #[test]
    fn a_report_without_a_status_falls_back_to_the_call_error() {
        let ok =
            of(&ev(1, call(json!({"summary": "끝났습니다."}), json!("done")))).expect("a report");
        assert!(ok.ok);
        let mut failed = call(json!({"summary": "끝났습니다."}), json!(null));
        failed["error"] = json!("boom");
        assert!(!of(&ev(1, failed)).expect("a report").ok);
    }

    /// **A prefixed name is the same tool.** A node's tools arrive as `zyris__node__cap__tool`, and
    /// which shape a deployment sends is not something to depend on.
    #[test]
    fn a_prefixed_tool_name_is_recognised() {
        let prefixed = json!({
            "name": "zyris__arch__planning__report_result",
            "arguments": {"status": "success", "summary": "끝났습니다."},
            "result": null,
        });
        assert!(of(&ev(1, prefixed)).is_some());
    }

    /// A report with no words is not a card — an empty box over the input is worse than no box.
    #[test]
    fn a_report_with_nothing_in_it_is_not_offered() {
        for args in [
            json!({}),
            json!({"status": "success"}),
            json!({"status": "success", "summary": "   "}),
        ] {
            assert!(of(&ev(1, call(args, json!(null)))).is_none(), "an empty card was offered");
        }
    }

    /// Anything else on the timeline is not a report.
    #[test]
    fn only_a_report_result_call_is_a_report() {
        assert!(of(&ev(1, json!({"name": "question", "arguments": {}}))).is_none());
        let mut other = ev(1, call(json!({"summary": "x"}), json!(null)));
        other.kind = "chat_agent".into();
        assert!(of(&other).is_none());
    }

    /// The sentence goes into the result instead of the arguments on some deployments.
    #[test]
    fn a_sentence_in_the_result_is_still_a_sentence() {
        let r = of(&ev(3, call(json!({"status": "success"}), json!("결과는 여기 있습니다."))))
            .expect("a report");
        assert_eq!(r.summary, "결과는 여기 있습니다.");
    }
}
