//! `ZSessionEvent` 하나를 화면 항목 하나로 옮긴다.
//!
//! 여기는 이벤트 **하나**만 본다. 작업 카드 그룹핑은 `timeline.rs`가 한다.
//!
//! `payload`는 타입이 없는 JSON이고 `kind`가 문자열이다. 정본은 attacca의
//! `attacca-domain/src/session_event.rs`다.

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
    /// 작업 런의 시작. 내용이 비어 있으면 제목이 아직 안 정해진 것이다.
    WorkStart(String),
    Thinking(String),
    Tool {
        name: String,
        summary: String,
        failed: bool,
        /// 펼쳤을 때 보여 줄 것 — 무엇을 받고 무엇을 돌려줬는가. 비면 펼칠 것이 없다.
        detail: String,
        /// `todo_*` 도구일 때만 인자와 결과를 들고 온다 — 사이드바의 태스크가 여기서 온다.
        /// 다른 도구까지 실어 나르면 타임라인이 쓸데없이 무거워진다.
        todo: Option<(serde_json::Value, Option<serde_json::Value>)>,
        /// 파일을 바꾼 도구면 무엇이 어떻게 바뀌었는가. 화면이 초록/빨강으로 그린다.
        diff: Option<crate::tools::diff::Diff>,
    },
    /// 에이전트가 물어본 것. 답을 고르면 평범한 메시지로 돌려보낸다.
    ///
    /// 이미 답이 달린 질문은 다시 띄우지 않는다 — `answered`가 그 표시다.
    Question {
        steps: Vec<crate::question::Step>,
        answered: bool,
    },
    Todo(String),
    Subagent(String),
    Error(String),
}

/// 렌더할 것이 없으면 `None`.
///
/// `recall`과 `chat_system`은 모델만 보는 이벤트라 의도적으로 버린다 — 그리면
/// 사용자가 쓴 적 없는 메시지가 대화창에 나타난다.
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
            // 질문은 도구 호출로 오지만 한 줄 요약이 아니라 고를 수 있는 화면이 돼야 한다.
            if name == "question" {
                if let Some(steps) = p.get("arguments").and_then(crate::question::parse) {
                    // 결과가 붙었으면 이미 답이 간 것이다. 다시 물으면 안 된다.
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
        // 모델만 보는 것. 절대 렌더하지 않는다.
        "recall" | "chat_system" => return None,
        // v1에서 다루지 않는 것, 그리고 아직 없는 미래의 종류. 죽지 않고 무시한다.
        _ => return None,
    };
    Some(Entry { seq: event.seq, kind })
}

fn text(payload: &Value, field: &str) -> String {
    payload.get(field).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// 도구 줄에 붙는 **짧은** 인자 요약. 이름은 여기 안 넣는다 — 화면이 둘을 따로 칠한다.
///
/// 인자 전체를 펼치지 않는다. 상세는 눌러서 펴면 볼 수 있으므로, 여기 있어야 하는 것은
/// "무엇에 대고 한 일인가" 한 조각이다. **예전에는 JSON의 첫 값을 그냥 썼는데**, 그러면
/// `write`의 `content`가 통째로 나와 도구 줄이 파일 하나가 됐다.
const SUMMARY_LIMIT: usize = 56;

fn tool_summary(payload: &Value, name: &str) -> String {
    let args = payload.get("arguments");
    let pick = |k: &str| args.and_then(|a| a.get(k)).and_then(Value::as_str);
    let tail = name.rsplit("__").next().unwrap_or(name);

    let chosen = match tail {
        "exec" => pick("command"),
        "glob" | "grep" => pick("pattern"),
        "load" => pick("name"),
        "open" | "open_stream" => pick("shell").or(Some("기본 셸")),
        // PTY를 이어 쓰는 것들은 어느 셸인지가 전부다.
        "read" | "write" | "screen" | "resize" | "close" if pick("pty").is_some() => pick("pty"),
        "edit" | "multi_edit" | "write" | "version" | "stat" | "list" | "read_stream" => pick("path"),
        // 모르는 도구(서버 빌트인·MCP)는 흔한 이름부터 찾아보고, 없으면 첫 문자열이다.
        _ => ["path", "name", "query", "title", "content", "url", "id"]
            .into_iter()
            .find_map(pick)
            .or_else(|| args.and_then(Value::as_object)?.values().find_map(Value::as_str)),
    };
    clip_summary(chosen.unwrap_or_default())
}

/// 한 줄로 만들고 길면 자른다. **줄바꿈이 남으면 도구 줄 하나가 여러 줄이 된다.**
fn clip_summary(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= SUMMARY_LIMIT {
        return one;
    }
    one.chars().take(SUMMARY_LIMIT).chain(['…']).collect()
}

/// 파일을 바꾼 도구의 결과에서 diff를 꺼낸다.
///
/// 도구 이름은 attacca가 `zyris__{슬러그}__{캐퍼빌리티}__{도구}`로 만들어 보내므로 뒤쪽만
/// 본다. 못 꺼내면 `None`이고, 그러면 지금처럼 JSON 덤프로 떨어진다 — **죽지 않는 것이
/// 요점이다.** 결과 모양은 우리가 정하지만(`tools::edit::EditResult`) 서버를 한 바퀴
/// 돌아온 JSON이라 무엇이든 올 수 있다.
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

/// `terminal`의 결과를 사람이 읽을 수 있게 편다.
///
/// 안 하면 `to_string_pretty`를 지나면서 `"stdout": "   Compiling…\n"`이 되어
/// **줄바꿈이 이스케이프로 남고 빌드 로그 전체가 한 줄이 된다.**
///
/// 모양이 다르면 `None`이고 그러면 지금처럼 JSON으로 떨어진다 — 결과 모양은 상류가
/// 정하고(`zyris_caps::ExecOutput`) 서버를 한 바퀴 돌아온 JSON이라 무엇이든 올 수 있다.
/// `diff_of`와 같은 자리, 같은 방식이다.
fn exec_detail(name: &str, result: Option<&Value>) -> Option<String> {
    let tail = name.rsplit("__").next()?;
    if !matches!(tail, "exec" | "read" | "screen") {
        return None;
    }
    let r = result?;
    // exec만 종료 코드를 가진다. read·screen은 화면 텍스트 하나다.
    let out = r.get("stdout").or_else(|| r.get("data")).or_else(|| r.get("text"))?.as_str()?;
    let err = r.get("stderr").and_then(Value::as_str).unwrap_or_default();

    let mut s = String::new();
    if r.get("timed_out").and_then(Value::as_bool) == Some(true) {
        s.push_str("시간이 다 됐습니다\n");
    } else if let Some(code) = r.get("exit_code").and_then(Value::as_i64).filter(|c| *c != 0) {
        s.push_str(&format!("종료 코드 {code}\n"));
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
    // **아무 말도 안 하면 도구가 고장 난 줄 안다.** 성공했는데 조용한 명령이 흔하다.
    if s.is_empty() {
        s.push_str("(출력 없음)");
    }
    Some(s)
}

/// 펼쳤을 때 보여 줄 것 — 무엇을 받고 무엇을 돌려줬는가.
///
/// **여기서 글자로 굳혀 둔다.** 원본 JSON을 그대로 들고 있으면 타임라인이 무거워지는데
/// 어차피 화면에는 글자로만 나간다. 그리고 아주 긴 결과는 잘라 둔다 — 도구 하나가
/// 화면 몇천 줄을 차지할 이유가 없고, 다 보고 싶으면 웹 UI에 원본이 있다.
const DETAIL_LIMIT: usize = 4000;

fn tool_detail(payload: &Value, name: &str) -> String {
    let mut out = String::new();
    if let Some(args) = payload.get("arguments").filter(|v| !v.is_null()) {
        out.push_str("인자\n");
        out.push_str(&flatten(args));
    }
    if let Some(err) = payload.get("error").filter(|v| !v.is_null()) {
        push_section(&mut out, "오류", err);
    } else if let Some(res) = payload.get("result").filter(|v| !v.is_null()) {
        // 셸이 뱉은 것은 JSON이 아니라 글이다.
        match exec_detail(name, Some(res)) {
            Some(text) => push_section(&mut out, "출력", &Value::String(text)),
            None => push_section(&mut out, "결과", res),
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

/// 문자열이면 그대로, 아니면 보기 좋게 편 JSON.
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
    format!("{cut}\n… (잘렸습니다)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: i64, kind: &str, payload: serde_json::Value) -> ZSessionEvent {
        ZSessionEvent { seq, cursor: seq, kind: kind.into(), payload, created_at: None }
    }

    /// 와이어 이름은 `zyris__{노드}__{캐퍼빌리티}__{도구}`다.
    fn wire(tool: &str) -> String {
        format!("zyris__arch__terminal__{tool}")
    }

    fn exec_json(code: i32, out: &str, err: &str, timed_out: bool) -> Value {
        json!({"exit_code": code, "stdout": out, "stderr": err, "timed_out": timed_out})
    }

    /// **요약에 이름을 넣지 않는다.** 화면이 이름과 요약을 따로 칠하므로 여기서 섞으면
    /// 도로 갈라내야 한다.
    #[test]
    fn the_summary_is_only_the_argument() {
        let got =
            tool_summary(&json!({"arguments": {"command": "cargo build -j2"}}), &wire("exec"));
        assert_eq!(got, "cargo build -j2");
    }

    /// **JSON의 첫 값을 그냥 쓰면 안 된다.** `write`는 `content`가 파일 전체라
    /// 그것이 도구 줄로 나오면 한 줄이 파일 하나가 된다.
    #[test]
    fn writing_a_file_is_summarised_by_its_path_not_its_content() {
        let got = tool_summary(
            &json!({"arguments": {"content": "아주 긴 파일 내용\n두 번째 줄\n", "path": "src/app.rs"}}),
            "zyris__arch__code_edit__write",
        );
        assert_eq!(got, "src/app.rs");
    }

    /// 줄바꿈이 남으면 도구 줄 하나가 여러 줄이 된다.
    #[test]
    fn a_summary_is_always_one_line() {
        let got =
            tool_summary(&json!({"arguments": {"command": "echo 하나\necho 둘"}}), &wire("exec"));
        assert!(!got.contains('\n'), "{got:?}");
    }

    /// 길면 자른다. 도구 줄은 훑는 것이지 읽는 것이 아니다.
    #[test]
    fn a_long_summary_is_clipped() {
        let got = tool_summary(&json!({"arguments": {"command": "x".repeat(400)}}), &wire("exec"));
        assert!(got.chars().count() <= SUMMARY_LIMIT + 1, "{}칸", got.chars().count());
        assert!(got.ends_with('…'), "잘렸다는 표시가 없다");
    }

    /// 모르는 도구도 뭐라도 말해야 한다 — 서버 빌트인과 MCP가 여기로 온다.
    #[test]
    fn an_unknown_tool_still_gets_a_summary() {
        assert_eq!(
            tool_summary(&json!({"arguments": {"query": "ratatui"}}), "web_search"),
            "ratatui"
        );
        assert_eq!(tool_summary(&json!({"arguments": {"무엇": "값"}}), "mystery"), "값");
        assert_eq!(tool_summary(&json!({"arguments": {}}), "mystery"), "");
    }

    /// **진짜 `ExecOutput`을 직렬화해 넣는다.** 모양이 어긋나면 여기서 잡힌다 —
    /// `diff_of`가 `EditResult`로 하는 것과 같은 이음매 테스트다.
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

    /// 실패한 명령은 종료 코드가 보여야 한다. 0과 3을 구별 못 하면 로그를 다시 읽어야 한다.
    #[test]
    fn a_failed_command_shows_its_exit_code() {
        let got = exec_detail(
            &wire("exec"),
            Some(&exec_json(3, "", "error[E0308]: mismatched\n", false)),
        )
        .unwrap();
        assert!(got.contains("종료 코드 3"), "{got:?}");
        assert!(got.contains("E0308"), "{got:?}");
    }

    /// 시간이 다 된 것과 아무것도 안 나온 것은 다른 일이다.
    #[test]
    fn a_timed_out_command_says_so() {
        let got = exec_detail(&wire("exec"), Some(&exec_json(-1, "", "", true))).unwrap();
        assert!(got.contains("시간이 다 됐습니다"), "{got:?}");
    }

    /// 아무 말도 안 하면 도구가 고장 난 줄 안다.
    #[test]
    fn a_silent_command_still_says_something() {
        let got = exec_detail(&wire("exec"), Some(&exec_json(0, "", "", false))).unwrap();
        assert!(!got.trim().is_empty(), "빈 결과가 빈 화면이 됐다");
    }

    /// 모양이 다르면 `None`으로 떨어져 지금처럼 JSON이 나온다. **죽지 않는 것이 요점이다.**
    #[test]
    fn something_that_is_not_an_exec_result_falls_back() {
        assert!(exec_detail(&wire("exec"), Some(&json!({"nope": 1}))).is_none());
        assert!(exec_detail("zyris__arch__code_edit__edit", Some(&exec_json(0, "a", "", false)))
            .is_none());
        assert!(exec_detail(&wire("exec"), None).is_none());
    }

    /// PTY 화면도 글이지 JSON이 아니다.
    #[test]
    fn a_pty_screen_is_shown_as_text_too() {
        let got = exec_detail(&wire("screen"), Some(&json!({"data": "$ ls\na.rs  b.rs\n"})))
            .expect("screen 결과를 못 알아봤다");
        assert!(got.contains("a.rs  b.rs"), "{got:?}");
        assert!(!got.contains("\\n"), "{got:?}");
    }

    /// **화면에 실제로 닿는 길이어야 한다.** `exec_detail`만 맞고 `tool_detail`이
    /// 안 부르면 사용자가 보는 것은 그대로 JSON이다.
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

    /// 모델만 보는 이벤트다. 그리면 사용자가 쓴 적 없는 메시지가 대화창에 나타난다.
    #[test]
    fn recall_and_chat_system_are_never_rendered() {
        let recall = ev(2, "recall", json!({"kind": "recall", "content": "지난주에 …"}));
        let system =
            ev(3, "chat_system", json!({"kind": "chat_system", "content": "하위 에이전트 완료"}));
        assert_eq!(entry_from(&recall), None);
        assert_eq!(entry_from(&system), None);
    }

    /// 실패한 도구는 눈에 띄어야 한다 — 조용히 성공처럼 보이면 안 된다.
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

    /// work_summary는 런 시작에 빈 내용으로 만들어지고 나중에 제목이 채워진다.
    #[test]
    fn an_empty_work_summary_still_opens_a_card() {
        let e = ev(5, "work_summary", json!({"kind": "work_summary", "content": ""}));
        assert_eq!(entry_from(&e).unwrap().kind, EntryKind::WorkStart(String::new()));
    }

    /// 질문은 고를 수 있는 화면이 돼야 한다. 한 줄 요약으로 흘려보내면 답할 길이 없다.
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

    /// 결과가 달린 질문은 이미 답이 간 것이라 다시 띄우면 안 된다.
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

    /// 모양이 어긋난 질문은 평범한 도구 호출로 그린다 — 죽지 않는다.
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

    /// 도구 결과에 실린 diff를 화면 쪽이 다시 읽어야 한다. 못 읽으면 diff가 안 보인다.
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

    /// **도구가 실제로 내보내는 모양 그대로** 왕복해야 한다.
    ///
    /// `EditResult`의 필드 이름을 하나 바꾸거나 `to_unified`/`parse`가 어긋나면 diff가
    /// 조용히 사라지고 JSON 덤프로 떨어진다 — 화면 테스트는 손으로 만든 `Diff`를 쓰므로
    /// 그 어긋남을 못 본다. 이 테스트가 그 이음매를 잡는다.
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

    /// 모양이 아니면 죽지 않고 지금처럼 JSON 덤프로 떨어진다.
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

    /// 모르는 종류가 앱을 죽이면 안 된다. attacca가 새 이벤트를 추가해도 살아 있어야 한다.
    #[test]
    fn an_unknown_kind_is_ignored_rather_than_panicking() {
        let e = ev(6, "some_future_kind", json!({"kind": "some_future_kind"}));
        assert_eq!(entry_from(&e), None);
    }
}
