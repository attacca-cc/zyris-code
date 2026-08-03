//! 로컬 MCP 서버 하나와 stdio로 말한다.
//!
//! **SDK를 당기지 않는다.** 우리가 쓰는 것은 `initialize`·`tools/list`·`tools/call` 셋뿐이고
//! 전송은 줄 단위 JSON-RPC다. 그만한 것에 의존을 하나 더 얹을 이유가 없다.
//!
//! 서버는 자식 프로세스다. **stderr는 로그로 보낸다** — 터미널로 새면 TUI 한가운데 찍히고,
//! 그 자리를 ratatui의 이중 버퍼가 "안 바뀌었다"고 여겨 다시 그리지도 않는다.

use std::collections::HashMap;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

/// 우리가 말한다고 밝히는 프로토콜 판. 서버가 더 새 것을 알아도 이걸로 맞춰 준다.
const PROTOCOL: &str = "2024-11-05";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTool {
    /// **씻어 낸 이름.** attacca가 도구 이름을 `__`로 쪼개므로 그 글자가 남으면 안 된다.
    pub name: String,
    /// 서버가 실제로 아는 이름. 부를 때 이것을 쓴다.
    pub raw: String,
    pub description: String,
    pub input_schema: Value,
}

pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl McpClient {
    /// 서버를 띄우고 악수를 마친다. 여기서 실패하면 그 서버는 없는 셈 친다.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<McpClient> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("MCP 서버를 띄우지 못했습니다: {command}"))?;

        let stdin = child.stdin.take().context("stdin이 없습니다")?;
        let stdout = child.stdout.take().context("stdout이 없습니다")?;
        if let Some(stderr) = child.stderr.take() {
            let name = command.to_string();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(server = %name, "{line}");
                }
            });
        }

        let mut client =
            McpClient { child, stdin, stdout: BufReader::new(stdout).lines(), next_id: 0 };

        client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL,
                    "capabilities": {},
                    "clientInfo": {"name": "zyris-code", "version": env!("CARGO_PKG_VERSION")},
                }),
            )
            .await?;
        // 알림이라 답이 없다. 이걸 빠뜨리면 어떤 서버는 도구를 안 내준다.
        client.notify("notifications/initialized", json!({})).await?;
        Ok(client)
    }

    /// 이 서버가 내주는 도구들. 이름은 씻고, 겹치면 번호를 붙인다.
    pub async fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        let result = self.request("tools/list", json!({})).await?;
        let listed = result.get("tools").and_then(Value::as_array).cloned().unwrap_or_default();
        let raw: Vec<String> = listed
            .iter()
            .map(|t| t.get("name").and_then(Value::as_str).unwrap_or_default().to_string())
            .collect();
        let names = dedup(raw.iter().map(|n| sanitize(n)).collect());
        Ok(listed
            .into_iter()
            .zip(raw)
            .zip(names)
            .map(|((t, raw), name)| McpTool {
                name,
                raw,
                description: t
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input_schema: t.get("inputSchema").cloned().unwrap_or_else(|| json!({})),
            })
            .collect())
    }

    /// 도구 하나를 부른다. `name`은 **서버가 아는 이름**(`McpTool::raw`)이다.
    pub async fn call(&mut self, name: &str, args: Value) -> Result<Value> {
        let result = self.request("tools/call", json!({"name": name, "arguments": args})).await?;
        Ok(json!({ "content": flatten_content(&result) }))
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await?;

        // **내 id가 올 때까지 읽는다.** 서버는 답 사이사이에 알림을 끼워 보낼 수 있고,
        // 그것을 답으로 착각하면 그 뒤가 전부 한 칸씩 밀린다.
        loop {
            let Some(line) = self.stdout.next_line().await? else {
                bail!("MCP 서버가 답하기 전에 끊었습니다: {method}");
            };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                tracing::debug!("JSON이 아닌 줄을 넘긴다: {line}");
                continue;
            };
            if msg.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(e) = msg.get("error") {
                let message = e.get("message").and_then(Value::as_str).unwrap_or("알 수 없는 오류");
                bail!("{method}: {message}");
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": params})).await
    }

    async fn write(&mut self, message: Value) -> Result<()> {
        let mut line = serde_json::to_string(&message)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// 서버를 끝낸다. `kill_on_drop`이 있지만 명시적으로 닫는 길도 둔다.
    pub async fn shutdown(mut self) {
        let _ = self.child.kill().await;
    }
}

/// **`__`가 남으면 안 되고, 양 끝이 `_`여도 안 된다.**
///
/// attacca는 노드 도구 이름을 `zyris__{노드}__{캐퍼빌리티}__{도구}`로 만들고 그것을 `__`로
/// 쪼개 되읽는다. 그래서 두 가지가 다 문제다:
///
/// - 이름 **안에** `__`가 있으면 그 자리에서 갈라진다.
/// - 이름이 `_`로 **끝나거나 시작하면** 이어 붙이는 `__`와 만나 `___`이 되어 역시 갈라진다.
///   `mcp_` + `__` + `echo` = `mcp___echo`다. **라이브에서 실제로 이렇게 나갔고 그 도구는
///   끝내 안 불렸다.**
///
/// 남는 것이 없으면 `unnamed`다 — 빈 이름은 부를 수가 없다. 겹치는 것은 `dedup`이 센다.
pub fn sanitize(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    // 원래부터 이어져 있던 `_`도 접는다.
    let mut folded = String::new();
    for ch in out.chars() {
        if !(ch == '_' && folded.ends_with('_')) {
            folded.push(ch);
        }
    }
    let trimmed = folded.trim_matches('_');
    if trimmed.is_empty() {
        "unnamed".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 씻고 나서 겹친 이름에 번호를 붙인다. 겹친 채로 두면 뒤엣것을 영영 못 부른다.
pub fn dedup(names: Vec<String>) -> Vec<String> {
    let mut seen: HashMap<String, u32> = HashMap::new();
    names
        .into_iter()
        .map(|name| {
            let n = seen.entry(name.clone()).or_insert(1);
            *n += 1;
            if *n == 2 {
                name
            } else {
                format!("{name}_{}", *n - 1)
            }
        })
        .collect()
}

/// 결과의 `content` 배열에서 **모델이 볼 수 있는 것만** 남긴다.
///
/// 이미지나 리소스를 통째로 실어 보내 봐야 모델이 못 보고 토큰 예산만 먹는다 —
/// 무엇이 있었는지만 적는다.
fn flatten_content(result: &Value) -> String {
    let Some(blocks) = result.get("content").and_then(Value::as_array) else {
        return result.to_string();
    };
    let mut out = String::new();
    for block in blocks {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
        if !out.is_empty() {
            out.push('\n');
        }
        match kind {
            "text" => out.push_str(block.get("text").and_then(Value::as_str).unwrap_or_default()),
            other => out.push_str(&format!("[{other}는 글자로 옮길 수 없어 생략했습니다]")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// attacca가 도구 이름을 `__`로 쪼갠다. 들어가면 라우팅이 깨진다.
    #[test]
    fn a_name_never_keeps_a_double_underscore() {
        assert_eq!(sanitize("a__b"), "a_b");
        assert_eq!(sanitize("a___b"), "a_b");
        assert_eq!(sanitize("get-issue"), "get_issue");
        assert!(!sanitize("a--__--b").contains("__"));
    }

    /// **양 끝의 `_`도 없애야 한다.** 이어 붙이는 `__`와 만나 `___`이 되면 똑같이 갈라진다.
    /// `mcp_` + `__` + `echo` = `mcp___echo` — 라이브에서 실제로 이렇게 나갔다.
    #[test]
    fn a_name_never_ends_or_starts_with_an_underscore() {
        for raw in ["연습", "mcp_", "_x_", "--", "파일 읽기", "  "] {
            let name = sanitize(raw);
            assert!(!name.starts_with('_'), "{raw} → {name}");
            assert!(!name.ends_with('_'), "{raw} → {name}");
            assert!(!name.is_empty(), "{raw} → 빈 이름은 부를 수가 없다");
            // 실제로 이어 붙여 보고 쪼개 본다. 이게 진짜 판정이다.
            let wire = format!("zyris__arch__cap__{name}");
            assert_eq!(wire.split("__").count(), 4, "{raw} → {wire}");
        }
    }

    /// 씻은 뒤 이름이 겹치면 뒤에 숫자를 붙인다. 겹친 채로 두면 뒤엣것을 못 부른다.
    #[test]
    fn colliding_names_get_a_number() {
        let names = dedup(vec!["a-b".into(), "a_b".into()]);
        assert_eq!(names, vec!["a-b".to_string(), "a_b".to_string()]);
        let names = dedup(vec!["a_b".into(), "a_b".into(), "a_b".into()]);
        assert_eq!(names, vec!["a_b".to_string(), "a_b_2".to_string(), "a_b_3".to_string()]);
    }

    /// 모델이 못 보는 블록은 자리만 먹는다. 무엇이 있었는지만 적는다.
    #[test]
    fn a_result_keeps_the_text_and_names_the_rest() {
        let r = json!({"content": [
            {"type": "text", "text": "첫 줄"},
            {"type": "image", "data": "AAAA"},
        ]});
        let out = flatten_content(&r);
        assert!(out.contains("첫 줄"));
        assert!(!out.contains("AAAA"), "이미지 바이트가 그대로 실렸다: {out}");
        assert!(out.contains("image"), "무엇이 빠졌는지는 말해야 한다: {out}");
    }

    /// **진짜 프로세스를 띄워 본다.** 모의 객체로는 stdio 프레이밍 실수가 안 잡힌다.
    #[tokio::test]
    async fn it_talks_to_a_real_stdio_server() {
        let (_dir, script) = fake_server();
        let mut c = McpClient::spawn("python3", &[script], &HashMap::new())
            .await
            .expect("가짜 서버가 떠야 한다");

        let tools = c.list_tools().await.unwrap();
        assert_eq!(tools.len(), 2, "{tools:?}");
        assert_eq!(tools[0].name, "echo");
        // 씻기 전 이름은 그대로 들고 있어야 부를 수 있다.
        assert_eq!(tools[1].raw, "get-issue");
        assert_eq!(tools[1].name, "get_issue");

        let r = c.call("echo", json!({"say": "안녕"})).await.unwrap();
        assert!(r.to_string().contains("안녕"), "{r}");
        c.shutdown().await;
    }

    /// 서버가 오류를 돌려주면 그 말이 그대로 올라와야 한다 — 삼키면 원인을 알 수 없다.
    #[tokio::test]
    async fn a_server_error_comes_back_as_an_error() {
        let (_dir, script) = fake_server();
        let mut c = McpClient::spawn("python3", &[script], &HashMap::new()).await.unwrap();
        let e = c.call("boom", json!({})).await.unwrap_err();
        assert!(e.to_string().contains("터졌다"), "{e}");
        c.shutdown().await;
    }

    /// 답 사이에 낀 알림을 답으로 착각하면 그 뒤가 전부 한 칸씩 밀린다.
    #[tokio::test]
    async fn a_notification_between_replies_does_not_shift_anything() {
        let (_dir, script) = fake_server();
        let mut c = McpClient::spawn("python3", &[script], &HashMap::new()).await.unwrap();
        // 가짜 서버는 tools/call마다 알림을 하나 먼저 보낸다.
        for _ in 0..3 {
            let r = c.call("echo", json!({"say": "여전히"})).await.unwrap();
            assert!(r.to_string().contains("여전히"), "{r}");
        }
        c.shutdown().await;
    }

    const FAKE: &str = r#"
import json, sys

def send(o):
    sys.stdout.write(json.dumps(o) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    if not line.strip():
        continue
    r = json.loads(line)
    m = r.get("method")
    if m == "initialize":
        send({"jsonrpc": "2.0", "id": r["id"], "result": {"protocolVersion": "2024-11-05",
              "capabilities": {}, "serverInfo": {"name": "fake", "version": "0"}}})
    elif m == "tools/list":
        send({"jsonrpc": "2.0", "id": r["id"], "result": {"tools": [
              {"name": "echo", "description": "받은 말을 돌려준다",
               "inputSchema": {"type": "object", "properties": {"say": {"type": "string"}}}},
              {"name": "get-issue", "description": "이슈를 읽는다",
               "inputSchema": {"type": "object"}}]}})
    elif m == "tools/call":
        # 답 앞에 알림을 하나 끼워 보낸다. 클라이언트가 id를 보고 골라야 한다.
        send({"jsonrpc": "2.0", "method": "notifications/message",
              "params": {"level": "info", "data": "가는 중"}})
        if r["params"]["name"] == "boom":
            send({"jsonrpc": "2.0", "id": r["id"],
                  "error": {"code": -32000, "message": "터졌다"}})
        else:
            say = r["params"]["arguments"].get("say", "")
            send({"jsonrpc": "2.0", "id": r["id"],
                  "result": {"content": [{"type": "text", "text": say}]}})
"#;

    fn fake_server() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake_mcp.py");
        std::fs::write(&path, FAKE).unwrap();
        let shown = path.to_string_lossy().to_string();
        (dir, shown)
    }
}
