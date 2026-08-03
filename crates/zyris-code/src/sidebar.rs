//! 오른쪽 사이드바에 실을 것 — 사용량과 태스크.
//!
//! 사용량은 `session_usage`가 준다. **태스크는 그렇게 간단하지 않다.** `todo_change`
//! 이벤트에는 `todo_item_id`와 상태만 있고 **본문이 없으며**, `attacca_api`에는 할 일
//! 목록을 읽는 도구가 없다. 그래서 에이전트가 부르는 `todo_*` 도구 호출에서 긁어 모은다:
//!
//! - `todo_list`의 **결과**가 가장 정확하다 — 그 시점의 전체 목록이다.
//! - `todo_add`의 **인자**에 새 항목의 본문이 있다.
//! - `todo_update_status`의 인자로 상태만 바뀐다.
//!
//! 그래서 여기 보이는 것은 "에이전트가 도구로 말한 것"이지 서버의 정본이 아니다.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    Running,
    Done,
}

impl TaskState {
    pub fn mark(self) -> &'static str {
        match self {
            TaskState::Pending => "○",
            TaskState::Running => "◐",
            TaskState::Done => "●",
        }
    }

    fn from_str(s: &str) -> TaskState {
        match s {
            "in_progress" | "running" => TaskState::Running,
            "done" | "completed" => TaskState::Done,
            _ => TaskState::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: Option<String>,
    pub text: String,
    pub state: TaskState,
}

/// 세션의 사용량. 값이 없으면 아직 턴을 돌지 않은 것이다.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub model: Option<String>,
    pub context_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub credits_used: Option<String>,
}

/// 사이드바가 들고 있는 것.
#[derive(Debug, Clone, Default)]
pub struct Sidebar {
    pub usage: Usage,
    pub tasks: Vec<Task>,
}

impl Sidebar {
    pub fn new() -> Self {
        Self::default()
    }

    /// 세션을 갈아탈 때 비운다 — 앞 세션의 숫자가 남으면 거짓말이 된다.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// `todo_*` 도구 호출 하나를 반영한다.
    pub fn apply_tool(&mut self, name: &str, arguments: &Value, result: Option<&Value>) {
        match name {
            // 전체 목록이 오면 그것이 정본이다.
            "todo_list" => {
                if let Some(items) = result.and_then(as_items) {
                    self.tasks = items;
                }
            }
            "todo_add" => {
                // 결과에 전체 목록이 실려 오면 그쪽이 낫다.
                if let Some(items) = result.and_then(as_items) {
                    self.tasks = items;
                    return;
                }
                if let Some(text) = arguments.get("content").and_then(Value::as_str) {
                    self.tasks.push(Task {
                        id: result
                            .and_then(|r| r.get("id"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        text: text.to_string(),
                        state: TaskState::Pending,
                    });
                }
            }
            "todo_update_status" => {
                if let Some(items) = result.and_then(as_items) {
                    self.tasks = items;
                    return;
                }
                let id = arguments.get("id").and_then(Value::as_str);
                let status = arguments.get("status").and_then(Value::as_str);
                if let (Some(id), Some(status)) = (id, status) {
                    if let Some(t) = self.tasks.iter_mut().find(|t| t.id.as_deref() == Some(id)) {
                        t.state = TaskState::from_str(status);
                    }
                }
            }
            "todo_remove" => {
                if let Some(items) = result.and_then(as_items) {
                    self.tasks = items;
                    return;
                }
                if let Some(id) = arguments.get("id").and_then(Value::as_str) {
                    self.tasks.retain(|t| t.id.as_deref() != Some(id));
                }
            }
            _ => {}
        }
    }

    /// 아직 안 끝난 것부터 몇 개.
    pub fn visible_tasks(&self, limit: usize) -> Vec<&Task> {
        let mut v: Vec<&Task> = self.tasks.iter().filter(|t| t.state != TaskState::Done).collect();
        if v.len() < limit {
            v.extend(self.tasks.iter().filter(|t| t.state == TaskState::Done));
        }
        v.into_iter().take(limit).collect()
    }
}

/// 도구 결과에서 항목 목록을 읽는다. 배열이거나 `{items: [...]}`이거나 둘 다 받는다.
fn as_items(v: &Value) -> Option<Vec<Task>> {
    let arr = v.as_array().or_else(|| v.get("items")?.as_array())?;
    let tasks: Vec<Task> = arr
        .iter()
        .filter_map(|it| {
            let text = it.get("content").and_then(Value::as_str)?;
            Some(Task {
                id: it.get("id").and_then(Value::as_str).map(str::to_string),
                text: text.to_string(),
                state: it
                    .get("status")
                    .and_then(Value::as_str)
                    .map(TaskState::from_str)
                    .unwrap_or(TaskState::Pending),
            })
        })
        .collect();
    (!tasks.is_empty()).then_some(tasks)
}

/// 이 모델이 한 번에 담는 컨텍스트. **모르면 `None`이고, 그러면 최대를 안 보여준다.**
///
/// 서버는 이 값을 주지 않는다 — `ZUsage`에는 지금 쓴 양만 있다. 그래서 모델 이름으로
/// 짐작하는데, **짐작은 조용히 낡는다.** 새 모델이 나오면 여기 없어서 최대가 사라질 뿐
/// 틀린 수를 보여주지는 않도록 했다. 아는 값이 틀렸으면 `ZYRIS_CODE_CONTEXT_MAX`로 덮는다.
pub fn context_limit(model: Option<&str>) -> Option<i64> {
    if let Some(n) = std::env::var("ZYRIS_CODE_CONTEXT_MAX").ok().and_then(|v| v.parse().ok()) {
        return Some(n);
    }
    let m = model?.to_ascii_lowercase();
    // 이름에 창 크기가 박혀 있으면 그게 가장 정확하다.
    if m.contains("1m") {
        return Some(1_000_000);
    }
    match () {
        _ if m.contains("claude") || m.contains("opus") || m.contains("sonnet") => Some(200_000),
        _ if m.contains("haiku") => Some(200_000),
        _ if m.contains("gemini") => Some(1_000_000),
        _ if m.contains("gpt-4o") => Some(128_000),
        _ => None,
    }
}

/// 큰 수를 짧게. 사이드바는 좁다.
///
/// 딱 떨어지면 소수점을 떼어 `200k`로 적는다 — `200.0k`는 정밀해 보이지만 읽기만 나쁘다.
pub fn compact(n: i64) -> String {
    let short = match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1_000.0),
        n => return n.to_string(),
    };
    short.replace(".0", "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_todo_list_result_replaces_everything() {
        let mut s = Sidebar::new();
        s.tasks.push(Task { id: None, text: "옛것".into(), state: TaskState::Pending });
        s.apply_tool(
            "todo_list",
            &json!({}),
            Some(&json!([
                {"id": "a", "content": "첫째", "status": "done"},
                {"id": "b", "content": "둘째", "status": "in_progress"}
            ])),
        );
        assert_eq!(s.tasks.len(), 2);
        assert_eq!(s.tasks[0].state, TaskState::Done);
        assert_eq!(s.tasks[1].state, TaskState::Running);
    }

    /// 결과에 목록이 없으면 인자의 본문으로라도 채운다.
    #[test]
    fn todo_add_falls_back_to_its_argument() {
        let mut s = Sidebar::new();
        s.apply_tool("todo_add", &json!({"content": "새 할 일"}), None);
        assert_eq!(s.tasks.len(), 1);
        assert_eq!(s.tasks[0].text, "새 할 일");
        assert_eq!(s.tasks[0].state, TaskState::Pending);
    }

    #[test]
    fn updating_status_moves_the_right_task() {
        let mut s = Sidebar::new();
        s.apply_tool("todo_add", &json!({"content": "일"}), Some(&json!({"id": "x"})));
        s.apply_tool("todo_update_status", &json!({"id": "x", "status": "done"}), None);
        assert_eq!(s.tasks[0].state, TaskState::Done);
    }

    /// 끝난 것보다 안 끝난 것이 먼저 보여야 한다 — 지금 할 일이 위다.
    #[test]
    fn unfinished_tasks_come_first() {
        let mut s = Sidebar::new();
        s.apply_tool(
            "todo_list",
            &json!({}),
            Some(&json!([
                {"id": "a", "content": "끝난 것", "status": "done"},
                {"id": "b", "content": "남은 것", "status": "pending"}
            ])),
        );
        let v = s.visible_tasks(1);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].text, "남은 것");
    }

    /// 세션을 갈아타면 비워야 한다. 앞 세션 숫자가 남으면 거짓말이 된다.
    #[test]
    fn clearing_drops_everything() {
        let mut s = Sidebar::new();
        s.usage.total_tokens = Some(100);
        s.apply_tool("todo_add", &json!({"content": "일"}), None);
        s.clear();
        assert!(s.tasks.is_empty());
        assert_eq!(s.usage.total_tokens, None);
    }

    #[test]
    fn big_numbers_get_short() {
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1_500), "1.5k");
        assert_eq!(compact(2_400_000), "2.4M");
        assert_eq!(compact(200_000), "200k", "딱 떨어지면 소수점을 뗀다");
    }

    /// 모르는 도구는 무시한다.
    #[test]
    fn an_unrelated_tool_changes_nothing() {
        let mut s = Sidebar::new();
        s.apply_tool("web_search", &json!({"query": "x"}), None);
        assert!(s.tasks.is_empty());
    }
}
