//! The session's todo list, rebuilt from the todo tool calls.
//!
//! **`todo_change` carries no words.** Its payload is `{todo_item_id, from_status, to_status}` and
//! nothing else, so drawing it can only ever put bare status words on screen — which is why
//! `event.rs` throws it away. The tool calls are the ones that say what a task *is*: `todo_add`
//! and `todo_update_status` both answer with the whole item (`id`, `content`, `status`) and
//! `todo_list` answers with every one of them. Folding those in `seq` order rebuilds the list
//! exactly, with no server change needed.
//!
//! **Changes are kept keyed by `seq`, never appended.** attacca updates a durable event in place
//! when the call returns, so the same `seq` arrives twice — once with `result: null`, once with the
//! item. Appending would leave the pending copy standing beside the real one forever. This is the
//! same invariant `Timeline` holds, for the same reason.

use std::collections::BTreeMap;

use serde_json::Value;
use zyris_attacca::ZSessionEvent;

/// Where a task stands. The server's three, under this app's names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    Doing,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Todo {
    /// The server's item id. A task still being added has a provisional one — see `change_from`.
    pub id: String,
    /// What the task says. **Only this is drawn** — a todo has no separate description, and the
    /// list is there to be skimmed.
    pub title: String,
    pub status: Status,
}

/// What one tool call did to the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The whole list, as `todo_list` answered it.
    All(Vec<Todo>),
    /// One item added or changed.
    One(Todo),
    /// One item removed.
    Gone(String),
}

/// Every change this session has made, keyed by the `seq` of the event that made it.
#[derive(Debug, Clone, Default)]
pub struct Todos {
    changes: BTreeMap<i64, Change>,
    items: Vec<Todo>,
}

impl Todos {
    pub fn new() -> Todos {
        Todos::default()
    }

    /// Records what one event did. **An upsert** — the same `seq` replaces, never adds.
    pub fn note(&mut self, seq: i64, change: Change) {
        self.changes.insert(seq, change);
        self.items = self.fold();
    }

    /// The list as it stands, in the order the tasks were added.
    ///
    /// **Folded when a change lands, not when it is read.** The activity line and the widget both
    /// read this on every frame, and a frame's cost must not follow how much has happened in the
    /// session — the rule the transcript cache exists for.
    pub fn items(&self) -> &[Todo] {
        &self.items
    }

    /// `(done, total)` — what the activity line counts.
    pub fn counts(&self) -> (usize, usize) {
        (self.items.iter().filter(|t| t.status == Status::Done).count(), self.items.len())
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn fold(&self) -> Vec<Todo> {
        let mut out: Vec<Todo> = Vec::new();
        for change in self.changes.values() {
            match change {
                Change::All(all) => out = all.clone(),
                Change::One(todo) => match out.iter_mut().find(|o| o.id == todo.id) {
                    Some(old) => *old = todo.clone(),
                    None => out.push(todo.clone()),
                },
                Change::Gone(id) => out.retain(|o| &o.id != id),
            }
        }
        out
    }
}

/// What this event did to the todo list, if anything.
///
/// **A call that is still running counts.** `todo_add` says what it is adding in its arguments, so
/// the row can go up the moment the agent asks for it instead of after the round trip. Its id is
/// provisional (`seq:N`) and never reaches the screen — when the answer lands it replaces this
/// change at the same `seq`, so the real item takes its place rather than doubling it.
pub fn change_from(event: &ZSessionEvent) -> Option<Change> {
    if event.kind != "tool_call" {
        return None;
    }
    let p = &event.payload;
    // **A call that failed changed nothing.** Showing what it asked for would put a task on the
    // list that the server never accepted.
    if p.get("error").is_some_and(|e| !e.is_null()) {
        return None;
    }
    let name = p.get("name").and_then(Value::as_str)?;
    let result = p.get("result").filter(|v| !v.is_null());
    let args = p.get("arguments");
    match name {
        "todo_list" => {
            Some(Change::All(result?.as_array()?.iter().filter_map(todo_from).collect()))
        }
        "todo_add" | "todo_update_status" => match result.and_then(todo_from) {
            Some(todo) => Some(Change::One(todo)),
            // Only `todo_add` knows the words before it is answered — `todo_update_status` is given
            // an id and a status, neither of which says what the task is.
            None if name == "todo_add" => {
                let title = args?.get("content").and_then(Value::as_str)?;
                Some(Change::One(Todo {
                    id: format!("seq:{}", event.seq),
                    title: title.to_string(),
                    status: Status::Pending,
                }))
            }
            None => None,
        },
        "todo_remove" => {
            let removed = result.and_then(|r| r.get("removed")).and_then(Value::as_str);
            let asked = args.and_then(|a| a.get("item_id")).and_then(Value::as_str);
            Some(Change::Gone(removed.or(asked)?.to_string()))
        }
        _ => None,
    }
}

fn todo_from(v: &Value) -> Option<Todo> {
    let id = v.get("id").and_then(Value::as_str)?;
    let title = v.get("content").and_then(Value::as_str)?;
    let status = match v.get("status").and_then(Value::as_str) {
        Some("completed") => Status::Done,
        Some("in_progress") => Status::Doing,
        // An unknown status is not a reason to lose the task. attacca may add one.
        _ => Status::Pending,
    };
    Some(Todo { id: id.to_string(), title: title.to_string(), status })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: i64, payload: Value) -> ZSessionEvent {
        ZSessionEvent { seq, cursor: seq, kind: "tool_call".into(), payload, created_at: None }
    }

    fn item(id: &str, content: &str, status: &str) -> Value {
        json!({
            "id": id,
            "todo_list_id": "list-1",
            "content": content,
            "status": status,
            "position": 0,
            "created_at": "2026-08-11T00:00:00Z",
            "updated_at": "2026-08-11T00:00:00Z",
        })
    }

    fn added(seq: i64, id: &str, content: &str) -> ZSessionEvent {
        ev(
            seq,
            json!({"name": "todo_add", "arguments": {"content": content}, "result": item(id, content, "pending")}),
        )
    }

    /// A real `todo_add` answer, straight off attacca's `TodoItem`.
    #[test]
    fn an_added_task_arrives_with_its_words() {
        let change = change_from(&added(1, "t1", "picker 테스트 고치기"));
        assert_eq!(
            change,
            Some(Change::One(Todo {
                id: "t1".into(),
                title: "picker 테스트 고치기".into(),
                status: Status::Pending,
            }))
        );
    }

    /// **The row goes up before the round trip finishes.** `todo_add` says what it is adding in
    /// its arguments, and waiting for the answer would leave the list a step behind the agent.
    #[test]
    fn a_task_still_being_added_already_says_what_it_is() {
        let e = ev(7, json!({"name": "todo_add", "arguments": {"content": "빌드 돌리기"}}));
        let Some(Change::One(todo)) = change_from(&e) else { panic!("{:?}", change_from(&e)) };
        assert_eq!(todo.title, "빌드 돌리기");
        assert_eq!(todo.status, Status::Pending);
    }

    /// **The same `seq` replaces, never adds.** attacca writes the event when the call starts and
    /// updates it in place when it returns, so both arrive — appending would leave the provisional
    /// copy standing beside the real one forever.
    #[test]
    fn the_answer_to_an_add_replaces_the_row_it_put_up() {
        let mut todos = Todos::new();
        let pending = ev(7, json!({"name": "todo_add", "arguments": {"content": "빌드 돌리기"}}));
        todos.note(7, change_from(&pending).unwrap());
        todos.note(7, change_from(&added(7, "t1", "빌드 돌리기")).unwrap());
        assert_eq!(todos.items().len(), 1, "{:?}", todos.items());
        assert_eq!(todos.items()[0].id, "t1");
    }

    #[test]
    fn a_status_update_moves_the_task_it_names() {
        let mut todos = Todos::new();
        todos.note(1, change_from(&added(1, "t1", "하나")).unwrap());
        todos.note(2, change_from(&added(2, "t2", "둘")).unwrap());
        let done = ev(
            3,
            json!({"name": "todo_update_status", "arguments": {"item_id": "t1", "status": "completed"}, "result": item("t1", "하나", "completed")}),
        );
        todos.note(3, change_from(&done).unwrap());
        assert_eq!(todos.counts(), (1, 2));
        assert_eq!(todos.items()[0].status, Status::Done);
        assert_eq!(todos.items()[1].status, Status::Pending, "the other task must not move");
    }

    /// The order tasks were added in is the order they are read in. A status change must not
    /// shuffle the list under the reader's eyes.
    #[test]
    fn finishing_a_task_leaves_it_where_it_was() {
        let mut todos = Todos::new();
        for (seq, id, title) in [(1, "t1", "하나"), (2, "t2", "둘"), (3, "t3", "셋")] {
            todos.note(seq, change_from(&added(seq, id, title)).unwrap());
        }
        let done =
            ev(4, json!({"name": "todo_update_status", "result": item("t1", "하나", "completed")}));
        todos.note(4, change_from(&done).unwrap());
        let ids: Vec<&str> = todos.items().iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["t1", "t2", "t3"]);
    }

    /// `todo_list` answers with everything, so it stands in for whatever came before — the one
    /// change that can put the list back in step after an event went missing.
    #[test]
    fn listing_replaces_what_was_known() {
        let mut todos = Todos::new();
        todos.note(1, change_from(&added(1, "t1", "옛것")).unwrap());
        let listed = ev(
            2,
            json!({"name": "todo_list", "arguments": {}, "result": [item("t9", "새것", "in_progress")]}),
        );
        todos.note(2, change_from(&listed).unwrap());
        assert_eq!(todos.items().len(), 1);
        assert_eq!(todos.items()[0].title, "새것");
        assert_eq!(todos.items()[0].status, Status::Doing);
    }

    #[test]
    fn a_removed_task_leaves_the_list() {
        let mut todos = Todos::new();
        todos.note(1, change_from(&added(1, "t1", "하나")).unwrap());
        let gone = ev(
            2,
            json!({"name": "todo_remove", "arguments": {"item_id": "t1"}, "result": {"removed": "t1"}}),
        );
        todos.note(2, change_from(&gone).unwrap());
        assert!(todos.is_empty(), "{:?}", todos.items());
    }

    /// **A call that failed changed nothing.** Putting up what it asked for would show a task the
    /// server never accepted.
    #[test]
    fn a_failed_call_changes_nothing() {
        let e = ev(
            1,
            json!({"name": "todo_add", "arguments": {"content": "안 될 것"}, "error": "todo list is full"}),
        );
        assert_eq!(change_from(&e), None);
    }

    /// Everything else on the wire is not a todo. **An unknown tool must not die here** — the
    /// result comes back through the server and can hold anything.
    #[test]
    fn other_tools_and_other_events_say_nothing() {
        let tool =
            ev(1, json!({"name": "zyris__arch__terminal__exec", "arguments": {"command": "ls"}}));
        assert_eq!(change_from(&tool), None);
        let thinking = ZSessionEvent {
            seq: 2,
            cursor: 2,
            kind: "thinking".into(),
            payload: json!({"content": "todo_add"}),
            created_at: None,
        };
        assert_eq!(change_from(&thinking), None);
        let empty = ev(3, json!({}));
        assert_eq!(change_from(&empty), None);
    }
}
