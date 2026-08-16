//! Talks to one MCP server — a local child process over stdio, or a remote one over HTTP.
//!
//! **We don't pull in an SDK.** All we use is the `initialize`·`tools/list`·`tools/call` trio, and
//! over stdio the transport is line-delimited JSON-RPC. There's no reason to add another dependency
//! for that much. The HTTP half lives next door in `http.rs`; everything above the pipe — what a
//! tool is, how its name is washed, how a result is flattened — is shared and lives here.
//!
//! The server is a child process. **stderr goes to the log** — if it leaks to the terminal it lands in the middle of the TUI,
//! and ratatui's double buffer treats that spot as "unchanged" and won't redraw it either.

use std::collections::HashMap;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

/// The protocol version we declare we speak. Even if the server knows something newer, we settle on this.
const PROTOCOL: &str = "2024-11-05";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTool {
    /// **The washed name.** attacca splits tool names on `__`, so that sequence must not remain.
    pub name: String,
    /// The name the server actually knows. This is what we use when calling it.
    pub raw: String,
    pub description: String,
    pub input_schema: Value,
}

/// What this app calls itself in the MCP handshake.
const CLIENT: &str = "zyris-code";

/// One server, however it is reached.
///
/// **Only the pipe varies.** What a tool is, how its name is washed for the wire, and how a result
/// is flattened are the same either way, so they live out here and each transport only has to
/// answer one JSON-RPC request at a time.
///
/// **Both sides are boxed.** A child process handle and an HTTP client are wildly different sizes,
/// and every `McpClient` would otherwise be as big as the larger of them — one per server, held
/// for the life of the app.
pub enum McpClient {
    Stdio(Box<StdioClient>),
    Http(Box<crate::mcp::http::HttpClient>),
}

impl McpClient {
    /// Spawns a local server and completes the handshake.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<McpClient> {
        Ok(McpClient::Stdio(Box::new(StdioClient::spawn(command, args, env).await?)))
    }

    /// Reaches a remote server over HTTP and completes the handshake.
    pub async fn connect(url: &str, headers: &HashMap<String, String>) -> Result<McpClient> {
        let http = crate::mcp::http::HttpClient::connect(
            url,
            headers,
            PROTOCOL,
            CLIENT,
            env!("CARGO_PKG_VERSION"),
        )
        .await?;
        Ok(McpClient::Http(Box::new(http)))
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        match self {
            McpClient::Stdio(c) => c.request(method, params).await,
            McpClient::Http(c) => c.request(method, params).await,
        }
    }

    /// The tools this server offers. Names are washed; collisions get a number appended.
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

    /// Calls one tool. `name` is **the name the server knows** (`McpTool::raw`).
    pub async fn call(&mut self, name: &str, args: Value) -> Result<Value> {
        let result = self.request("tools/call", json!({"name": name, "arguments": args})).await?;
        Ok(json!({ "content": flatten_content(&result) }))
    }

    /// Shuts the server down. A remote one has nothing to shut down — dropping it is the whole of it.
    pub async fn shutdown(self) {
        if let McpClient::Stdio(c) = self {
            c.shutdown().await;
        }
    }
}

pub struct StdioClient {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

/// Builds the command that starts an MCP server.
///
/// **On Windows it goes through `cmd /C`.** Rust's `Command` resolves a bare name on `PATH` by
/// appending `.exe` only — it does not honour `PATHEXT`. The canonical MCP entry is
/// `"command": "npx"`, and on Windows npx is `npx.cmd`, so every npx-based server failed to spawn
/// with "program not found" and was reported in `/mcp` as a broken server. Batch launchers
/// (`npx`, `pnpm`, `yarn`) are the common case, so the shell has to resolve it.
fn spawner(command: &str, args: &[String]) -> tokio::process::Command {
    if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command).args(args);
        return c;
    }
    let mut c = tokio::process::Command::new(command);
    c.args(args);
    c
}

impl StdioClient {
    /// Spawns the server and completes the handshake. If this fails, we treat that server as nonexistent.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<StdioClient> {
        let mut child = spawner(command, args)
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
            StdioClient { child, stdin, stdout: BufReader::new(stdout).lines(), next_id: 0 };

        client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL,
                    "capabilities": {},
                    "clientInfo": {"name": CLIENT, "version": env!("CARGO_PKG_VERSION")},
                }),
            )
            .await?;
        // A notification, so there is no reply. Some servers won't hand out tools if this is skipped.
        client.notify("notifications/initialized", json!({})).await?;
        Ok(client)
    }

    pub(crate) async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await?;

        // **Keep reading until our id comes back.** The server may interleave notifications
        // between replies, and mistaking one for the reply shifts everything after it by one.
        loop {
            let Some(line) = self.stdout.next_line().await? else {
                bail!("MCP 서버가 답하기 전에 끊었습니다: {method}");
            };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                tracing::debug!("skipping a non-JSON line: {line}");
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

    /// Shuts the server down. There's `kill_on_drop`, but we also keep an explicit way to close it.
    pub(crate) async fn shutdown(mut self) {
        let _ = self.child.kill().await;
    }
}

/// **A `__` must not remain, and neither end may be `_`.**
///
/// attacca builds node tool names as `zyris__{node}__{capability}__{tool}` and re-reads them by
/// splitting on `__`. So both things are a problem:
///
/// - A `__` **inside** the name splits it at that spot.
/// - A name that **ends or starts** with `_` meets the joining `__` and becomes `___`, which
///   splits too. `mcp_` + `__` + `echo` = `mcp___echo`. **This actually happened live, and that
///   tool was never called.**
///
/// If nothing remains, it's `unnamed` — an empty name can't be called. Collisions are counted by `dedup`.
pub fn sanitize(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    // Also folds `_`s that were consecutive to begin with.
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

/// After washing, appends a number to colliding names. Left collided, the later one can never be called.
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

/// From the result's `content` array, keeps **only what the model can see**.
///
/// Sending images or resources whole just means the model can't see them and they only eat the
/// token budget — so we note only what was there.
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

    /// attacca splits tool names on `__`. If one gets in, routing breaks.
    #[test]
    fn a_name_never_keeps_a_double_underscore() {
        assert_eq!(sanitize("a__b"), "a_b");
        assert_eq!(sanitize("a___b"), "a_b");
        assert_eq!(sanitize("get-issue"), "get_issue");
        assert!(!sanitize("a--__--b").contains("__"));
    }

    /// **Trailing and leading `_`s must go too.** Meeting a joining `__` makes `___`, which splits the same way.
    /// `mcp_` + `__` + `echo` = `mcp___echo` — this actually happened live.
    #[test]
    fn a_name_never_ends_or_starts_with_an_underscore() {
        for raw in ["연습", "mcp_", "_x_", "--", "파일 읽기", "  "] {
            let name = sanitize(raw);
            assert!(!name.starts_with('_'), "{raw} → {name}");
            assert!(!name.ends_with('_'), "{raw} → {name}");
            assert!(!name.is_empty(), "{raw} → an empty name cannot be called");
            // Actually join them and split again. This is the real test.
            let wire = format!("zyris__arch__cap__{name}");
            assert_eq!(wire.split("__").count(), 4, "{raw} → {wire}");
        }
    }

    /// If washed names collide, append a number. Left collided, the later one can't be called.
    #[test]
    fn colliding_names_get_a_number() {
        let names = dedup(vec!["a-b".into(), "a_b".into()]);
        assert_eq!(names, vec!["a-b".to_string(), "a_b".to_string()]);
        let names = dedup(vec!["a_b".into(), "a_b".into(), "a_b".into()]);
        assert_eq!(names, vec!["a_b".to_string(), "a_b_2".to_string(), "a_b_3".to_string()]);
    }

    /// Blocks the model can't see only take up space. We note only what was there.
    #[test]
    fn a_result_keeps_the_text_and_names_the_rest() {
        let r = json!({"content": [
            {"type": "text", "text": "첫 줄"},
            {"type": "image", "data": "AAAA"},
        ]});
        let out = flatten_content(&r);
        assert!(out.contains("첫 줄"));
        assert!(!out.contains("AAAA"), "raw image bytes were carried through: {out}");
        assert!(out.contains("image"), "it must say what is missing: {out}");
    }

    /// Is `python3` actually runnable? The fake server is a Python script, and Python is not
    /// guaranteed to be installed — on some Windows boxes `python3.exe` is only the Microsoft
    /// Store stub, which reports an error instead of running anything.
    fn python3_available() -> bool {
        std::process::Command::new("python3")
            .arg("-c")
            .arg("print(1)")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// **Spawns a real process.** A mock wouldn't catch stdio framing mistakes.
    #[tokio::test]
    async fn it_talks_to_a_real_stdio_server() {
        if !python3_available() {
            return;
        }
        let (_dir, script) = fake_server();
        let mut c = McpClient::spawn("python3", &[script], &HashMap::new())
            .await
            .expect("the fake server must come up");

        let tools = c.list_tools().await.unwrap();
        assert_eq!(tools.len(), 2, "{tools:?}");
        assert_eq!(tools[0].name, "echo");
        // The pre-wash name must be kept as-is so it can be called.
        assert_eq!(tools[1].raw, "get-issue");
        assert_eq!(tools[1].name, "get_issue");

        let r = c.call("echo", json!({"say": "안녕"})).await.unwrap();
        assert!(r.to_string().contains("안녕"), "{r}");
        c.shutdown().await;
    }

    /// If the server returns an error, its words must come through unchanged — swallowing them hides the cause.
    #[tokio::test]
    async fn a_server_error_comes_back_as_an_error() {
        if !python3_available() {
            return;
        }
        let (_dir, script) = fake_server();
        let mut c = McpClient::spawn("python3", &[script], &HashMap::new()).await.unwrap();
        let e = c.call("boom", json!({})).await.unwrap_err();
        assert!(e.to_string().contains("터졌다"), "{e}");
        c.shutdown().await;
    }

    /// Mistaking a notification wedged between replies for the reply shifts everything after it by one.
    #[tokio::test]
    async fn a_notification_between_replies_does_not_shift_anything() {
        if !python3_available() {
            return;
        }
        let (_dir, script) = fake_server();
        let mut c = McpClient::spawn("python3", &[script], &HashMap::new()).await.unwrap();
        // The fake server sends one notification first for every tools/call.
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
