//! A plan handed back for approval, and how it sits on screen.
//!
//! **attacca's plan mode parks a turn on the user.** The agent investigates, calls `submit_plan`
//! with the whole plan as markdown, and waits. That call is an ordinary tool call on the timeline —
//! deliberately so, on attacca's side: the event carrying the plan *is* the record, and everything
//! that offers an approval reads it from there. So this side reads it from there too.
//!
//! **A decision is an ordinary message.** There is no approve call in the protocol and there does
//! not need to be one: attacca notices that the user's next message follows an open `submit_plan`
//! and tells the agent that message is the decision. Approval is therefore just sending, which is
//! why this panel needs no buttons — Enter on an empty draft approves, and anything typed is what
//! to change.
//!
//! Pure. What is on screen and how tall it is are decided here; the widget only draws it.

use serde_json::Value;

/// A plan as it arrived, before the screen has an opinion about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    pub seq: i64,
    pub markdown: String,
    /// The user has already decided this one, so there is nothing to offer.
    pub decided: bool,
}

/// The plan the screen is showing, and how much of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub seq: i64,
    pub markdown: String,
    /// Whether the whole plan is showing rather than its opening lines.
    ///
    /// **Folded to begin with.** A plan runs to pages and the input has to stay reachable; what is
    /// wanted at the moment it lands is "there is a plan, roughly this", and the rest on request.
    pub open: bool,
}

/// How many lines the folded panel shows of the plan itself.
const PEEK: usize = 3;

impl Plan {
    pub fn new(submitted: &Submitted) -> Plan {
        Plan { seq: submitted.seq, markdown: submitted.markdown.clone(), open: false }
    }

    /// The plan's own lines, as text. Blank lines are dropped so the peek is three lines of plan
    /// rather than three lines of spacing.
    pub fn body(&self) -> Vec<&str> {
        self.markdown.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect()
    }

    /// How many lines of plan the panel shows, given the room it may take.
    ///
    /// **Never all of it, however much room there is.** The panel is a summons, not a reader: the
    /// whole plan is on the timeline above as the tool call it arrived in, which scrolls.
    pub fn shown(&self, room: usize) -> usize {
        let body = self.body().len();
        match self.open {
            true => body.min(room),
            false => body.min(PEEK).min(room),
        }
    }

    /// How many lines are not being shown.
    pub fn hidden(&self, room: usize) -> usize {
        self.body().len().saturating_sub(self.shown(room))
    }
}

/// Whether a `submit_plan` result says the user's decision arrived.
///
/// **The same trap as `question`, and the same rule.** The wait hands back `status: "timeout"` as
/// an ordinary success when nobody replied in time, and the plan is still open — the decision is
/// expected as the user's next message. Reading "there is a result" as "it was decided" would hide
/// the one panel that can decide it, and a plan submitted while nobody was looking would be
/// unapprovable for good.
///
/// The two ways of being wrong are not alike: reopening a plan already dealt with costs one Esc,
/// while closing one that was not costs the approval.
pub fn decided(result: Option<&Value>, failed: bool) -> bool {
    // A call that errored will not consume a decision, so there is nothing to offer.
    if failed {
        return true;
    }
    let Some(result) = result else { return false };
    match result.get("status").and_then(Value::as_str) {
        Some(status) => status == "answered",
        // A deployment that says nothing about status: fall back to whether a decision came with it.
        None => result.get("decision").is_some_and(|d| !d.is_null()),
    }
}

/// The plan a session event carries, if it carries one.
///
/// **By the tool's name, not by the capability's.** `submit_plan` is attacca's own tool, so it
/// arrives unprefixed — but a node's tools are `zyris__node__cap__tool`, and matching on the tail
/// keeps this from depending on which of those two shapes a deployment sends.
pub fn submitted_from(event: &zyris_attacca::ZSessionEvent) -> Option<Submitted> {
    if event.kind != "tool_call" {
        return None;
    }
    let payload = &event.payload;
    let name = payload.get("name").and_then(Value::as_str).unwrap_or_default();
    if name.rsplit("__").next() != Some("submit_plan") {
        return None;
    }
    let markdown = payload
        .get("arguments")
        .and_then(|a| a.get("markdown"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // **A plan with no words is not a plan.** An argument that failed to arrive would put an empty
    // panel over the input with nothing to read and no reason to be there.
    if markdown.trim().is_empty() {
        return None;
    }
    let failed = payload.get("error").is_some_and(|e| !e.is_null());
    let decided = decided(payload.get("result").filter(|v| !v.is_null()), failed);
    Some(Submitted { seq: event.seq, markdown, decided })
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

    fn submitted(result: Value) -> Value {
        json!({"name": "submit_plan", "arguments": {"markdown": "# Plan\n\nstep one"}, "result": result})
    }

    /// **A call that came back is not a plan that was decided.** The wait hands back a timeout as
    /// an ordinary success, and the plan is still open — the decision arrives as the next message.
    /// Reading a result as a decision hides the only panel that can approve it.
    #[test]
    fn a_wait_that_ran_out_leaves_the_plan_open() {
        let waiting =
            submitted_from(&ev(1, submitted(json!({"submitted": true, "status": "timeout"}))));
        assert_eq!(waiting.map(|p| p.decided), Some(false));

        let settled =
            submitted_from(&ev(1, submitted(json!({"submitted": true, "status": "answered"}))));
        assert_eq!(settled.map(|p| p.decided), Some(true));

        // Still running: no result at all, and certainly not decided.
        let running = submitted_from(&ev(
            1,
            json!({
                "name": "submit_plan", "arguments": {"markdown": "# Plan"}, "result": null,
            }),
        ));
        assert_eq!(running.map(|p| p.decided), Some(false));

        // A deployment that says nothing about status falls back to the decision being there.
        assert!(decided(Some(&json!({"decision": "go ahead"})), false));
        assert!(!decided(Some(&json!({"submitted": true})), false));
        // And a call that errored consumes nothing, so there is nothing to offer.
        assert!(decided(None, true));
    }

    /// The plan comes off the tool's own argument, whichever shape the name arrives in.
    #[test]
    fn the_plan_is_read_out_of_the_call_that_submitted_it() {
        let plain = submitted_from(&ev(7, submitted(json!(null)))).expect("a plan");
        assert_eq!(plain.seq, 7);
        assert!(plain.markdown.starts_with("# Plan"));

        let prefixed = submitted_from(&ev(
            7,
            json!({"name": "zyris__arch__planning__submit_plan", "arguments": {"markdown": "x"}}),
        ));
        assert!(prefixed.is_some(), "a prefixed tool name was not recognised");

        // Anything else on the timeline is not a plan.
        assert!(submitted_from(&ev(7, json!({"name": "question", "arguments": {}}))).is_none());
        let mut other = ev(7, submitted(json!(null)));
        other.kind = "chat_agent".into();
        assert!(submitted_from(&other).is_none());
    }

    /// **A plan with no words is not a plan.** An argument that failed to arrive would put an empty
    /// panel over the input with nothing to read and no reason to be there.
    #[test]
    fn a_plan_with_nothing_in_it_is_not_offered() {
        assert!(submitted_from(&ev(1, json!({"name": "submit_plan", "arguments": {}}))).is_none());
        let blank = json!({"name": "submit_plan", "arguments": {"markdown": "   \n\n"}});
        assert!(submitted_from(&ev(1, blank)).is_none());
    }

    /// **Folded it is a summons; opened it is as much as there is room for.** Never all of it
    /// regardless — the whole plan is on the timeline above, in the tool call it arrived in, and
    /// that scrolls where this cannot.
    #[test]
    fn a_folded_plan_shows_its_opening_lines_and_says_how_many_are_left() {
        let long = (1..=20).map(|n| format!("step {n}")).collect::<Vec<_>>().join("\n\n");
        let mut plan = Plan::new(&Submitted { seq: 1, markdown: long, decided: false });

        assert_eq!(plan.shown(40), PEEK, "a folded plan showed more than a peek");
        assert_eq!(plan.hidden(40), 17);

        plan.open = true;
        assert_eq!(plan.shown(40), 20, "an opened plan held something back for no reason");
        assert_eq!(plan.hidden(40), 0);

        // And it never takes more room than it was given, open or not.
        assert_eq!(plan.shown(5), 5);
        assert_eq!(plan.hidden(5), 15);
    }

    /// Blank lines are spacing, not plan. Three lines of peek must be three lines of words.
    #[test]
    fn the_peek_is_three_lines_of_plan_not_three_lines_of_spacing() {
        let spaced = "# Plan\n\n\n\n- one\n\n\n- two\n";
        let plan = Plan::new(&Submitted { seq: 1, markdown: spaced.into(), decided: false });
        assert_eq!(plan.body(), ["# Plan", "- one", "- two"]);
        assert_eq!(plan.shown(40), 3);
    }
}
