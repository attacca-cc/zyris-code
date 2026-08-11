//! Turns one MCP server into one capability of this node.
//!
//! **The agent doesn't know about MCP.** The tool name just looks like
//! `zyris__{node}__mcp_{name}__{tool}` — and when called, we hand it over via stdio.
//!
//! The `#[zyris::capability]` macro can't be used — the macro fixes the tools at compile time, but
//! here we have to ask the server to learn what exists. `ServeCapability` is a public trait, so
//! **handing out a `CapabilityDescriptor` built at runtime is legitimate.**

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use zyris::{
    encode_response, unknown_tool, CapabilityDescriptor, IncomingCall, Outgoing, Result,
    ServeCapability, ToolDescriptor, Transfer, WireError,
};

use crate::mcp::client::{sanitize, McpClient, McpTool};

/// One server written in one config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    /// The name written in the config. The capability name becomes `mcp_{here}`.
    pub slug: String,
    pub transport: Transport,
}

/// How the server is reached. **Read from whatever shape the file uses**, because every client
/// writes these files a little differently and a person pointing us at their existing config
/// should not have to rewrite it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// A child process speaking line-delimited JSON-RPC.
    Stdio { command: String, args: Vec<String>, env: HashMap<String, String> },
    /// A remote server over HTTP (`mcp::http`).
    Http { url: String, headers: HashMap<String, String> },
}

impl Transport {
    /// A one-line description for `/mcp`. **The command or the host, never the whole thing** —
    /// an args list runs off the screen and a URL can carry a token in its query.
    pub fn summary(&self) -> String {
        match self {
            Transport::Stdio { command, .. } => command.clone(),
            Transport::Http { url, .. } => {
                let host = url.split("://").nth(1).unwrap_or(url);
                host.split('/').next().unwrap_or(host).to_string()
            }
        }
    }
}

/// The file shape, as written by hand or by another client.
///
/// **`type` is a hint, not the decider.** Plenty of configs leave it out entirely, and the fields
/// that are present say it plainly enough: a `url` is remote, a `command` is a child process. A
/// `type` that says `http` or `sse` only settles the case where both are somehow there.
#[derive(Debug, Deserialize)]
pub struct SpecFile {
    #[serde(rename = "type")]
    kind: Option<String>,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: HashMap<String, String>,
}

impl SpecFile {
    pub fn into_transport(self) -> Option<Transport> {
        let remote = self.kind.as_deref().is_some_and(|k| k == "http" || k == "sse");
        match (self.url, self.command) {
            (Some(url), command) if remote || command.is_none() => {
                Some(Transport::Http { url, headers: self.headers })
            }
            (_, Some(command)) => {
                Some(Transport::Stdio { command, args: self.args, env: self.env })
            }
            // Neither a command nor a url. **Dropped rather than guessed at** — a half-written
            // entry that starts nothing is better than one that starts the wrong thing.
            _ => None,
        }
    }
}

/// A config file. **`servers` is read as well as `mcpServers`** — VS Code writes the former, and a
/// person pointing us at their file should not have to rename anything.
#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default, rename = "mcpServers")]
    servers: HashMap<String, SpecFile>,
    #[serde(default, rename = "servers")]
    vscode: HashMap<String, SpecFile>,
}

pub struct McpCapability {
    /// The capability name that goes on the wire. **Already sanitized.**
    ///
    /// Don't sanitize only the slug and then prepend `mcp_` — for a slug that collapses entirely to
    /// `_` (e.g. Korean), this becomes `mcp__`, **recreating exactly the character attacca splits on.**
    /// That actually happened on the wire, and that tool was never called. Sanitize after joining.
    name: String,
    tools: Vec<McpTool>,
    /// **Only one call speaks at a time.** stdio answers one line per call, so two overlapping
    /// calls would steal each other's answers. Queuing here is the cheapest fix.
    client: Mutex<McpClient>,
}

impl McpCapability {
    pub async fn start(spec: &ServerSpec) -> anyhow::Result<McpCapability> {
        let mut client = match &spec.transport {
            Transport::Stdio { command, args, env } => McpClient::spawn(command, args, env).await?,
            Transport::Http { url, headers } => McpClient::connect(url, headers).await?,
        };
        let tools = client.list_tools().await?;
        Ok(McpCapability {
            name: sanitize(&format!("mcp_{}", spec.slug)),
            tools,
            client: Mutex::new(client),
        })
    }
}

#[async_trait]
impl ServeCapability for McpCapability {
    fn descriptor(&self) -> CapabilityDescriptor {
        let mut descriptor = CapabilityDescriptor {
            name: self.name.clone(),
            version: 1,
            tools: self
                .tools
                .iter()
                .map(|t| ToolDescriptor {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    transfer: Transfer::Unary,
                    request_schema: t.input_schema.clone(),
                    // MCP doesn't give a result schema. Keep it loosely open.
                    response_schema: Some(json!({"type": "object"})),
                    item_schema: None,
                })
                .collect(),
        };
        // Trim the descriptions to fit the budget — same reason as `tools::trim`.
        crate::tools::trim::trim_descriptor(&mut descriptor);
        descriptor
    }

    async fn dispatch(&self, call: IncomingCall) -> Result<Outgoing> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name == call.tool)
            .ok_or_else(|| unknown_tool(&self.name, &call.tool))?;
        let args = call.params.to_json().unwrap_or_else(|_| json!({}));
        let out = self
            .client
            .lock()
            .await
            // **Call by the name the server knows.** The sanitized name is only used on the wire.
            .call(&tool.raw, args)
            .await
            .map_err(|e| WireError::internal(e.to_string()))?;
        encode_response(&out)
    }
}

/// Two places to read config from. **The later one wins** — the project is more specific than home.
pub fn config_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    // The user tier is `conn::app_dir` — the same directory as the credentials and settings.
    // Joining `$HOME/.config/…` here meant Windows never read a user-level `mcp.json` at all.
    if let Some(dir) = crate::conn::app_dir() {
        out.push(dir.join("mcp.json"));
    }
    out.push(cwd.join(".mcp.json"));
    out
}

/// The servers that are written down. Unreadable files are silently skipped — no config is normal.
pub fn load_config(cwd: &Path) -> Vec<ServerSpec> {
    let files: Vec<Value> = config_paths(cwd)
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|s| match serde_json::from_str::<Value>(&s) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("could not read the MCP config: {e}");
                None
            }
        })
        .collect();
    merge_configs(files)
}

/// Overwrites from the first one onward. On equal names **the later one wins.**
pub fn merge_configs(files: Vec<Value>) -> Vec<ServerSpec> {
    let mut merged: HashMap<String, ServerSpec> = HashMap::new();
    for file in files {
        let Ok(parsed) = serde_json::from_value::<ConfigFile>(file.clone()) else { continue };
        let mut entries: HashMap<String, SpecFile> =
            parsed.servers.into_iter().chain(parsed.vscode).collect();
        // **A plugin's `.mcp.json` puts the servers at the top level**, with no wrapper key at all
        // — that is what the official example plugin ships. Falling back only when neither wrapper
        // was found keeps this from reading arbitrary JSON as a server list.
        if entries.is_empty() {
            if let Ok(bare) = serde_json::from_value::<HashMap<String, SpecFile>>(file) {
                entries = bare;
            }
        }
        for (slug, spec) in entries {
            let Some(transport) = spec.into_transport() else {
                tracing::warn!("MCP server '{slug}' says neither a command nor a url");
                continue;
            };
            merged.insert(slug.clone(), ServerSpec { slug, transport });
        }
    }
    // Fix the order by name. Emitting in raw HashMap order would make the announce differ per run.
    let mut out: Vec<ServerSpec> = merged.into_values().collect();
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

/// Starts every server that is written down. **If one fails to start, the rest still start.**
///
/// Only the successful ones are returned; failures are reported as (name, reason) — if one fell
/// out silently, a person would wait thinking the tool exists.
pub async fn start_all(specs: &[ServerSpec]) -> (Vec<McpCapability>, Vec<(String, String)>) {
    let mut started: Vec<McpCapability> = Vec::new();
    let mut failed = Vec::new();
    for spec in specs {
        match McpCapability::start(spec).await {
            Ok(cap) => started.push(cap),
            Err(e) => failed.push((spec.slug.clone(), e.to_string())),
        }
    }
    // **Names can collide after sanitizing.** Two names with no alphanumerics both become `mcp_` —
    // if emitted collided, the later one is silently buried.
    let names = unique_names(started.iter().map(|c| c.name.clone()).collect());
    for (cap, name) in started.iter_mut().zip(names) {
        cap.name = name;
    }
    (started, failed)
}

/// Non-colliding capability names. **Even after numbering, no `__` may remain.**
///
/// Appending `_2` straight onto `mcp_` gives `mcp__2` and undoes the point of sanitizing. So
/// collisions are checked against the **re-sanitized** result of joining.
fn unique_names(names: Vec<String>) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    names
        .into_iter()
        .map(|name| {
            let mut candidate = sanitize(&name);
            let mut n = 1u32;
            while !seen.insert(candidate.clone()) {
                n += 1;
                candidate = sanitize(&format!("{name}_{n}"));
            }
            candidate
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> McpTool {
        McpTool {
            name: sanitize(name),
            raw: name.to_string(),
            description: "이슈를 만든다".into(),
            input_schema: json!({"type": "object", "properties": {"title": {"type": "string"}}}),
        }
    }

    async fn cap_of(slug: &str, tools: Vec<McpTool>) -> McpCapability {
        // These tests only look at the descriptor, so the server is stood up with `cat` — no talking.
        let client = McpClient::spawn("cat", &[], &HashMap::new()).await;
        McpCapability {
            name: sanitize(&format!("mcp_{slug}")),
            tools,
            client: Mutex::new(client.expect("cat exists everywhere")),
        }
    }

    /// **MCP's inputSchema must become the request_schema as-is.**
    /// Otherwise the agent can't construct arguments.
    #[tokio::test]
    async fn the_descriptor_carries_each_tools_schema() {
        let cap = cap_of("github", vec![tool("create-issue")]).await;
        let d = cap.descriptor();
        assert_eq!(d.name, "mcp_github");
        assert_eq!(d.tools[0].name, "create_issue", "the name must be sanitised");
        assert_eq!(d.tools[0].request_schema["properties"]["title"]["type"], json!("string"));
    }

    /// **The wire name must split into exactly four.** That is the real test.
    ///
    /// Sanitizing only the slug isn't enough. A name with no alphanumerics collapses entirely to
    /// `mcp_`, and the joining `__` then makes `mcp___echo` — that actually went out on the wire,
    /// and that tool was never called. **A place we got wrong twice.**
    #[tokio::test]
    async fn the_wire_name_still_splits_into_four() {
        for slug in ["my__server", "연습", "--", "깃 허브", "github"] {
            let cap = cap_of(slug, vec![tool("create-issue")]).await;
            let d = cap.descriptor();
            let wire = format!("zyris__arch__{}__{}", d.name, d.tools[0].name);
            assert_eq!(wire.split("__").count(), 4, "{slug} → {wire}");
        }
    }

    /// Even after numbering, no `__` may remain — `mcp_` + `_2` = `mcp__2`.
    #[test]
    fn numbering_a_collision_does_not_bring_the_double_underscore_back() {
        let out = unique_names(vec!["mcp_".into(), "mcp_".into(), "mcp_".into()]);
        assert_eq!(out, vec!["mcp", "mcp_2", "mcp_3"]);
        for n in &out {
            assert_eq!(format!("zyris__arch__{n}__x").split("__").count(), 4, "{n}");
        }
    }

    /// Names that collide after sanitizing must be split apart — collided, the later one is buried.
    #[tokio::test]
    async fn two_servers_that_wash_to_the_same_name_are_split() {
        let specs = vec![
            ServerSpec {
                slug: "연습".into(),
                transport: Transport::Stdio {
                    command: "cat".into(),
                    args: vec![],
                    env: HashMap::new(),
                },
            },
            ServerSpec {
                slug: "실습".into(),
                transport: Transport::Stdio {
                    command: "cat".into(),
                    args: vec![],
                    env: HashMap::new(),
                },
            },
        ];
        let (started, _) = start_all(&specs).await;
        let names: Vec<String> = started.iter().map(|c| c.descriptor().name).collect();
        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1], "colliding names went out unchanged: {names:?}");
        for n in &names {
            assert!(!n.contains("__"), "{n}");
        }
    }

    /// The working directory's config beats the home config.
    #[test]
    fn the_project_config_wins() {
        let merged = merge_configs(vec![
            json!({"mcpServers": {"a": {"command": "홈"}}}),
            json!({"mcpServers": {"a": {"command": "프로젝트"}}}),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].transport.summary(), "프로젝트");
        assert_eq!(merged[0].slug, "a");
    }

    /// A server only one file knows about survives as-is — an overwrite, not a replacement.
    #[test]
    fn servers_only_one_file_knows_about_survive() {
        let merged = merge_configs(vec![
            json!({"mcpServers": {"a": {"command": "가"}}}),
            json!({"mcpServers": {"b": {"command": "나"}}}),
        ]);
        assert_eq!(merged.iter().map(|s| s.slug.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
    }

    /// **If one fails to start, the rest still start.** The app must not come to a full stop.
    #[tokio::test]
    async fn a_server_that_fails_to_start_does_not_stop_the_others() {
        let specs = vec![
            ServerSpec {
                slug: "없는놈".into(),
                transport: Transport::Stdio {
                    command: "이런건-없다".into(),
                    args: vec![],
                    env: HashMap::new(),
                },
            },
            ServerSpec {
                slug: "좋은놈".into(),
                transport: Transport::Stdio {
                    command: "cat".into(),
                    args: vec![],
                    env: HashMap::new(),
                },
            },
        ];
        let (started, failed) = start_all(&specs).await;
        assert_eq!(started.len(), 1, "the one that works must come up");
        assert_eq!(failed.len(), 1, "what failed must be reported");
        assert_eq!(failed[0].0, "없는놈");
    }

    /// Having no config is normal. Dying then would make the app unusable.
    #[test]
    fn no_config_at_all_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_config(dir.path()).is_empty());
    }

    /// Even a broken JSON must not stop the app.
    #[test]
    fn a_broken_config_is_skipped_rather_than_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".mcp.json"), "{이건 JSON이 아니다").unwrap();
        assert!(load_config(dir.path()).is_empty());
    }
}
