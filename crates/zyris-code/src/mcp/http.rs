//! Talks to one remote MCP server over HTTP.
//!
//! **The same three calls as stdio** — `initialize` · `tools/list` · `tools/call` — only the pipe
//! is different. So this file is a transport and nothing else; what a tool is and how its name is
//! washed stays in `client.rs`.
//!
//! The shape is MCP's **streamable HTTP**: every request is a POST of one JSON-RPC message, and the
//! answer comes back either as `application/json` or as a `text/event-stream` holding one. Both are
//! read here, because servers pick freely between them and a client that only knows one of them
//! looks broken against half the servers in the world.
//!
//! **The reply is read whole, not streamed.** A server closes the event stream once it has answered
//! the POST, so waiting for the end costs nothing and saves carrying a streaming body around. The
//! one thing it rules out is server-initiated messages on a long-lived channel, which nothing here
//! asks for.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

/// The session header the server hands out on `initialize`. **Every later request must carry it**
/// or the server answers 404 and the tool list comes back empty for no visible reason.
const SESSION: &str = "mcp-session-id";

pub struct HttpClient {
    http: reqwest::Client,
    url: String,
    headers: HashMap<String, String>,
    session: Option<String>,
    next_id: u64,
}

impl HttpClient {
    /// Opens the connection and completes the handshake. A failure here means the server is
    /// treated as though it did not exist, the same as a stdio server that will not spawn.
    pub async fn connect(
        url: &str,
        headers: &HashMap<String, String>,
        protocol: &str,
        client_name: &str,
        client_version: &str,
    ) -> Result<HttpClient> {
        let http = reqwest::Client::builder()
            // **A server that never answers must not hold a tool call open forever.** attacca cuts
            // a node call at 60s, so anything past that is a worse error than saying so here.
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("could not build the HTTP client")?;
        let mut client = HttpClient {
            http,
            url: url.to_string(),
            headers: headers.clone(),
            session: None,
            next_id: 0,
        };
        client
            .request(
                "initialize",
                json!({
                    "protocolVersion": protocol,
                    "capabilities": {},
                    "clientInfo": {"name": client_name, "version": client_version},
                }),
            )
            .await?;
        client.notify("notifications/initialized", json!({})).await?;
        Ok(client)
    }

    /// One JSON-RPC round trip.
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let response = self.post(&body).await?;

        // **The session id only comes back once**, on initialize. Later replies do not repeat it.
        if self.session.is_none() {
            if let Some(value) = response.headers().get(SESSION).and_then(|v| v.to_str().ok()) {
                self.session = Some(value.to_string());
            }
        }
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("{method}: the server answered {status}: {}", first_line(&text));
        }
        let Some(message) = reply_to(&text, id) else {
            bail!("{method}: the server sent no answer to this request");
        };
        if let Some(e) = message.get("error") {
            let why = e.get("message").and_then(Value::as_str).unwrap_or("unknown error");
            bail!("{method}: {why}");
        }
        Ok(message.get("result").cloned().unwrap_or(Value::Null))
    }

    /// A message with no id, so there is nothing to wait for. **The body is still drained** — an
    /// unread response can hold the connection out of the pool.
    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let body = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let response = self.post(&body).await?;
        let status = response.status();
        let _ = response.text().await;
        if !status.is_success() && status.as_u16() != 202 {
            bail!("{method}: the server answered {status}");
        }
        Ok(())
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response> {
        // **Both content types are declared acceptable.** A server that streams its answers refuses
        // outright when only `application/json` is offered.
        let mut request = self
            .http
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(session) = &self.session {
            request = request.header(SESSION, session);
        }
        request.json(body).send().await.with_context(|| format!("could not reach {}", self.url))
    }
}

/// Only the first line, for an error message. A server's failure body can be a whole HTML page.
fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > 200 {
        line.chars().take(200).collect::<String>() + "…"
    } else {
        line.to_string()
    }
}

/// Finds the answer to `id` in a body that is **either** one JSON message or an event stream.
///
/// **Anything that is not our id is skipped, not mistaken for the answer.** A server may put
/// notifications and log messages on the same stream, and taking the first message as the reply
/// shifts every answer after it by one — the same rule the stdio side follows.
pub fn reply_to(body: &str, id: u64) -> Option<Value> {
    let matches = |v: &Value| v.get("id").and_then(Value::as_u64) == Some(id);
    if let Ok(one) = serde_json::from_str::<Value>(body.trim()) {
        return matches(&one).then_some(one);
    }
    // An event stream: `data:` lines, one per message, blank line between events. A single message
    // may be split over several `data:` lines, which are joined with newlines.
    let mut chunk = String::new();
    let mut out = None;
    for line in body.lines().chain(std::iter::once("")) {
        if let Some(rest) = line.strip_prefix("data:") {
            if !chunk.is_empty() {
                chunk.push('\n');
            }
            chunk.push_str(rest.trim_start());
            continue;
        }
        if line.trim().is_empty() && !chunk.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(&chunk) {
                if matches(&v) {
                    out = Some(v);
                }
            }
            chunk.clear();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_json_answer_is_read() {
        let body = r#"{"jsonrpc":"2.0","id":7,"result":{"tools":[]}}"#;
        assert_eq!(reply_to(body, 7).unwrap()["result"]["tools"], json!([]));
    }

    /// **An answer that arrives as an event stream is the same answer.** Servers choose freely
    /// between the two, and a client that reads only one of them is broken against half of them.
    #[test]
    fn an_answer_on_an_event_stream_is_read_too() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"ok\":true}}\n\n";
        assert_eq!(reply_to(body, 3).unwrap()["result"]["ok"], json!(true));
    }

    /// **Everything that is not our id is skipped.** Taking the first message as the reply shifts
    /// every answer after it by one.
    #[test]
    fn a_notification_sharing_the_stream_is_not_mistaken_for_the_reply() {
        let body = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{}}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"mine\":true}}\n\n",
        );
        assert_eq!(reply_to(body, 1).unwrap()["result"]["mine"], json!(true));
        assert!(reply_to(body, 2).is_none(), "an id nobody answered must not resolve");
    }

    /// One message split across several `data:` lines is joined, per the SSE rules.
    #[test]
    fn a_message_split_over_several_data_lines_is_joined() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":9,\ndata: \"result\":{\"n\":1}}\n\n";
        assert_eq!(reply_to(body, 9).unwrap()["result"]["n"], json!(1));
    }

    /// A body that says nothing readable resolves to nothing rather than panicking — what comes
    /// back over the wire can be anything at all, an HTML error page included.
    #[test]
    fn an_unreadable_body_answers_nothing() {
        assert!(reply_to("<html>gateway timeout</html>", 1).is_none());
        assert!(reply_to("", 1).is_none());
    }

    /// A stand-in MCP server: **speaks real HTTP on a real socket**, so the handshake, the session
    /// header and the two content types are exercised the way a server would exercise them. Reading
    /// the body would need a length parser; every request here has one, so the headers are enough.
    ///
    /// Answers `initialize` with a session id, `tools/list` as an event stream and `tools/call` as
    /// plain JSON — deliberately mixed, because servers mix them.
    async fn stand_in() -> (String, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut sessions: Vec<String> = Vec::new();
            // **Four, not three.** `connect` sends two — `initialize` and the
            // `notifications/initialized` that follows it — and a server that stops listening
            // after three refuses the last call for no reason a reader would guess.
            for _ in 0..4 {
                let Ok((mut socket, _)) = listener.accept().await else { break };
                let mut buf = vec![0u8; 8192];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                sessions.push(
                    request
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("mcp-session-id:"))
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                );
                let (kind, body) = if request.contains("\"initialize\"") {
                    ("application/json", r#"{"jsonrpc":"2.0","id":1,"result":{}}"#.to_string())
                } else if request.contains("tools/list") {
                    (
                        "text/event-stream",
                        "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"get-page\",\"description\":\"d\",\"inputSchema\":{}}]}}\n\n".to_string(),
                    )
                } else {
                    (
                        "application/json",
                        r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"hi"}]}}"#.to_string(),
                    )
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nmcp-session-id: s-1\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.flush().await;
            }
            sessions
        });
        (url, task)
    }

    /// **The whole round trip over a real socket.** The unit tests above prove the body reader; this
    /// proves the request that produced the body — the headers, the session id, and both content
    /// types coming back from the same server.
    #[tokio::test]
    async fn it_talks_to_a_real_http_server() {
        let (url, server) = stand_in().await;
        let mut client =
            HttpClient::connect(&url, &HashMap::new(), "2024-11-05", "zyris-code", "0")
                .await
                .expect("the handshake failed");

        let listed = client.request("tools/list", json!({})).await.unwrap();
        assert_eq!(listed["tools"][0]["name"], json!("get-page"), "{listed}");

        let called = client.request("tools/call", json!({"name": "get-page"})).await.unwrap();
        assert_eq!(called["content"][0]["text"], json!("hi"), "{called}");

        // **The session id is echoed from the second request on.** Without it a real server
        // answers 404 and the tool list comes back empty for no visible reason.
        let seen = server.await.unwrap();
        assert_eq!(seen[0], "", "nothing to echo on the first request");
        assert!(
            seen[1..].iter().all(|s| s.contains("s-1")),
            "the session id was not sent back: {seen:?}"
        );
    }

    #[test]
    fn an_error_page_is_cut_down_to_one_line() {
        let long = "x".repeat(500);
        assert!(first_line(&long).chars().count() <= 201);
        assert_eq!(first_line("\n\n  boom  \nmore"), "boom");
    }
}
