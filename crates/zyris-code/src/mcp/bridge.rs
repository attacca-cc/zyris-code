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
    /// A stdio server, from what the three form fields hold.
    ///
    /// **Args are split on whitespace**, and the field says so: the schema for what a person types
    /// here cannot carry quoting, and silently inventing shell rules for one line of a form would
    /// be worse than telling them to keep a path without spaces.
    pub fn stdio(command: &str, args: &str, env: &str) -> Result<Transport, String> {
        Ok(Transport::Stdio {
            command: command.trim().to_string(),
            args: args.split_whitespace().map(str::to_string).collect(),
            env: parse_env(env)?,
        })
    }

    /// A remote server, from the two fields that describe one.
    pub fn http(url: &str, headers: &str) -> Result<Transport, String> {
        Ok(Transport::Http { url: url.trim().to_string(), headers: parse_env(headers)? })
    }

    /// How it is written into a config file — the shape every client reads.
    pub fn as_entry(&self) -> Value {
        let mut obj = serde_json::Map::new();
        match self {
            Transport::Stdio { command, args, env } => {
                obj.insert("command".into(), Value::String(command.clone()));
                if !args.is_empty() {
                    obj.insert("args".into(), json!(args));
                }
                if !env.is_empty() {
                    obj.insert("env".into(), json!(env));
                }
            }
            Transport::Http { url, headers } => {
                // `type` is written even though a `url` alone says it, because some clients refuse
                // an entry without one and there is no cost to saying it.
                obj.insert("type".into(), Value::String("http".into()));
                obj.insert("url".into(), Value::String(url.clone()));
                if !headers.is_empty() {
                    obj.insert("headers".into(), json!(headers));
                }
            }
        }
        Value::Object(obj)
    }

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

    /// The whole invocation, for a manager row's detail block.
    ///
    /// **Args are shown and a URL's query is not.** Args are part of what the server will run and
    /// somebody deciding whether to trust it needs them; a query string is where a token lives,
    /// which is the same reason `summary` keeps only the host.
    pub fn detail(&self) -> String {
        match self {
            Transport::Stdio { command, args, .. } => {
                if args.is_empty() {
                    command.clone()
                } else {
                    format!("{command} {}", args.join(" "))
                }
            }
            Transport::Http { url, .. } => url.split('?').next().unwrap_or(url).to_string(),
        }
    }

    /// The **names** of what it is handed — env vars for a child process, headers for a remote
    /// one.
    ///
    /// **Names only. The values are where the secrets are**, and a panel is a thing people
    /// screenshot and paste into issues. That a token is being passed is the fact worth showing;
    /// which token it is is not.
    pub fn handed_names(&self) -> Vec<String> {
        let map = match self {
            Transport::Stdio { env, .. } => env,
            Transport::Http { headers, .. } => headers,
        };
        let mut names: Vec<String> = map.keys().cloned().collect();
        names.sort();
        names
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
        Self::start_with_timeout(spec, super::REQUEST_TIMEOUT).await
    }

    async fn start_with_timeout(
        spec: &ServerSpec,
        request_timeout: std::time::Duration,
    ) -> anyhow::Result<McpCapability> {
        let mut client = match &spec.transport {
            Transport::Stdio { command, args, env } => {
                McpClient::spawn_with_timeout(command, args, env, request_timeout).await?
            }
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
                    // **Nothing is declared, because nothing here knows.** MCP gives a server no way to
                    // say how long one of its tools takes, and answering on its behalf would either cut a
                    // slow one short or leave a caller waiting on a dead one. Saying nothing asks for the
                    // caller's own default, which is what these tools have always had.
                    call_limit: None,
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

/// Takes one server out of the file it is written in, leaving everything else in that file alone.
///
/// **Read, edited, written back — not parsed into a struct and re-serialized.** These files are
/// shared with other clients, and rebuilding one from this app's idea of its shape would drop every
/// key this app does not know about: `inputs`, `disabled`, a whole per-project block. The same
/// reason `merge_configs` reads three different wrapper names instead of one.
///
/// Both wrapper names and the bare top-level shape are tried, because a plugin's `.mcp.json` puts
/// its servers at the top level with no wrapper at all.
pub fn remove_server(path: &Path, slug: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut value: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut removed = false;
    for key in ["mcpServers", "servers"] {
        if let Some(map) = value.get_mut(key).and_then(Value::as_object_mut) {
            removed |= map.remove(slug).is_some();
        }
    }
    if !removed {
        if let Some(map) = value.as_object_mut() {
            removed = map.remove(slug).is_some();
        }
    }
    if !removed {
        return Err(format!("`{slug}` is not in {}", path.display()));
    }
    write_json(path, &value)
}

/// Writes a config file the way everything else here writes one: **a temp file, then a rename**, so
/// a crash halfway leaves the old file rather than half of a new one.
fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temp, format!("{text}\n")).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, path).map_err(|e| e.to_string())
}

/// Reads `KEY=value` pairs out of what was typed into an env or header field.
///
/// **A token with no `=` is refused rather than dropped.** A server that starts without the variable
/// it was meant to be given fails later and somewhere else — in a child process, in somebody else's
/// logs — which is the kind of failure this form exists to avoid.
pub fn parse_env(text: &str) -> Result<HashMap<String, String>, String> {
    let mut out = HashMap::new();
    for token in text.split_whitespace() {
        match token.split_once('=') {
            Some((key, value)) if !key.is_empty() => {
                out.insert(key.to_string(), value.to_string());
            }
            _ => return Err(token.to_string()),
        }
    }
    Ok(out)
}

/// Which of our two config files a server is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    /// `~/.config/zyris-code/mcp.json` — for every project on this machine.
    User,
    /// `./.mcp.json` — travels with the repository.
    Project,
}

impl Where {
    /// The file itself. `None` for the user tier when there is no config directory at all.
    pub fn path(self, cwd: &Path) -> Option<PathBuf> {
        match self {
            Where::User => crate::conn::app_dir().map(|dir| dir.join("mcp.json")),
            Where::Project => Some(cwd.join(".mcp.json")),
        }
    }
}

/// Writes one server into a config file, leaving everything else in that file alone.
///
/// **The same read-edit-write as `remove_server`**, for the same reason: these files are shared with
/// other clients, and rebuilding one from this app's idea of its shape would drop every key it does
/// not know about. A file that is not there yet is created with the `mcpServers` wrapper.
///
/// An entry of the same name is replaced — the form refuses a collision before it gets here, so a
/// replacement only happens when something outside this app has changed in between.
pub fn put_server(path: &Path, slug: &str, transport: &Transport) -> Result<(), String> {
    let mut value: Value = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if !value.is_object() {
        return Err(format!("{}: not a JSON object", path.display()));
    }
    // **`servers` is kept if that is what the file already uses** — VS Code writes that name, and a
    // file carrying both wrappers would have one of them read and the other ignored.
    let wrapper =
        if value.get("servers").is_some_and(Value::is_object) { "servers" } else { "mcpServers" };
    let obj = value.as_object_mut().expect("checked just above");
    let map = obj.entry(wrapper).or_insert_with(|| json!({}));
    let Some(map) = map.as_object_mut() else {
        return Err(format!("{}: `{wrapper}` is not an object", path.display()));
    };
    map.insert(slug.to_string(), transport.as_entry());
    write_json(path, &value)
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
    load_paths(&config_paths(cwd))
}

/// What the user wrote for this app. Presence here is already an explicit trust decision.
pub fn load_user_config() -> Vec<ServerSpec> {
    let paths: Vec<PathBuf> =
        crate::conn::app_dir().into_iter().map(|dir| dir.join("mcp.json")).collect();
    load_paths(&paths)
}

/// What arrived with the repository. These specs are candidates until explicitly approved.
pub fn load_project_config(cwd: &Path) -> Vec<ServerSpec> {
    load_paths(&[cwd.join(".mcp.json")])
}

fn load_paths(paths: &[PathBuf]) -> Vec<ServerSpec> {
    let files: Vec<Value> = paths
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
    start_all_with_timeout(specs, super::REQUEST_TIMEOUT).await
}

async fn start_all_with_timeout(
    specs: &[ServerSpec],
    request_timeout: std::time::Duration,
) -> (Vec<McpCapability>, Vec<(String, String)>) {
    let mut started: Vec<McpCapability> = Vec::new();
    let mut failed = Vec::new();
    for spec in specs {
        match McpCapability::start_with_timeout(spec, request_timeout).await {
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

    /// **A token with no `=` is refused, not dropped.** A server that starts without the variable it
    /// was given fails later and somewhere else.
    #[test]
    fn env_pairs_are_read_and_a_bad_one_is_refused() {
        let got = parse_env("A=1 B=two").expect("both are pairs");
        assert_eq!(got.get("A").map(String::as_str), Some("1"));
        assert_eq!(got.get("B").map(String::as_str), Some("two"));
        assert_eq!(parse_env("NOEQUALS"), Err("NOEQUALS".to_string()));
        assert_eq!(parse_env("=value"), Err("=value".to_string()));
        assert!(parse_env("").expect("nothing to read").is_empty());
    }

    /// **The file is edited, not rebuilt.** What the form writes has to read back as the server it
    /// was, and everything else in that file has to survive — other servers, and keys this app has
    /// never heard of.
    #[test]
    fn a_new_server_is_written_into_the_file_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let stdio = Transport::stdio("npx", "-y @playwright/mcp", "TOKEN=x").unwrap();
        put_server(&path, "playwright", &stdio).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"mcpServers\""), "{text}");
        let found = load_paths(std::slice::from_ref(&path));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].slug, "playwright");
        match &found[0].transport {
            Transport::Stdio { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(args, &vec!["-y".to_string(), "@playwright/mcp".to_string()]);
                assert_eq!(env.get("TOKEN").map(String::as_str), Some("x"));
            }
            other => panic!("it came back as {other:?}"),
        }

        let keep = dir.path().join("kept.json");
        std::fs::write(&keep, r#"{"mcpServers":{"a":{"command":"x"}},"other":1}"#).unwrap();
        put_server(&keep, "b", &stdio).unwrap();
        let text = std::fs::read_to_string(&keep).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["other"], json!(1), "a key we do not know was dropped: {text}");
        assert!(value["mcpServers"]["a"]["command"] == json!("x"), "{text}");
        assert!(value["mcpServers"]["b"].is_object(), "{text}");
    }

    /// A remote server is written as `type: http` with its address — the shape other clients read.
    #[test]
    fn a_remote_server_is_written_the_way_other_clients_read_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let remote = Transport::http("https://x.test/mcp", "K=v").unwrap();
        put_server(&path, "docs", &remote).unwrap();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["mcpServers"]["docs"]["type"], json!("http"));
        assert_eq!(value["mcpServers"]["docs"]["url"], json!("https://x.test/mcp"));
        assert_eq!(value["mcpServers"]["docs"]["headers"]["K"], json!("v"));
    }

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

    /// A stdio echo command usable as a stand-in MCP server, or `None` when there is none.
    /// The bridge tests stand the server up with `cat`, which Windows does not ship, and no
    /// Python (the other option) is not guaranteed either.
    fn echo_server() -> Option<&'static str> {
        if cfg!(windows) {
            None
        } else {
            Some("cat")
        }
    }

    /// **MCP's inputSchema must become the request_schema as-is.**
    /// Otherwise the agent can't construct arguments.
    #[tokio::test]
    async fn the_descriptor_carries_each_tools_schema() {
        if echo_server().is_none() {
            return;
        }
        let cap = cap_of("github", vec![tool("create-issue")]).await;
        let d = cap.descriptor();
        assert_eq!(d.name, "mcp_github");
        assert_eq!(d.tools[0].name, "create_issue", "the name must be sanitised");
        assert_eq!(d.tools[0].request_schema["properties"]["title"]["type"], json!("string"));
    }

    /// **The wire name must split into exactly three.** That is the real test.
    ///
    /// Sanitizing only the slug isn't enough. A name with no alphanumerics collapses entirely to
    /// `mcp_`, and the joining `__` then makes `mcp___echo` — that actually went out on the wire,
    /// and that tool was never called. **A place we got wrong twice.**
    #[tokio::test]
    async fn the_wire_name_still_splits_into_three() {
        if echo_server().is_none() {
            return;
        }
        for slug in ["my__server", "연습", "--", "깃 허브", "github"] {
            let cap = cap_of(slug, vec![tool("create-issue")]).await;
            let d = cap.descriptor();
            let wire = format!("zyris__{}_v{}__{}", d.name, d.version, d.tools[0].name);
            assert_eq!(wire.split("__").count(), 3, "{slug} → {wire}");
        }
    }

    /// Even after numbering, no `__` may remain — `mcp_` + `_2` = `mcp__2`.
    #[test]
    fn numbering_a_collision_does_not_bring_the_double_underscore_back() {
        let out = unique_names(vec!["mcp_".into(), "mcp_".into(), "mcp_".into()]);
        assert_eq!(out, vec!["mcp", "mcp_2", "mcp_3"]);
        for n in &out {
            assert_eq!(format!("zyris__{n}_v1__x").split("__").count(), 3, "{n}");
        }
    }

    /// Names that collide after sanitizing must be split apart — collided, the later one is buried.
    #[tokio::test]
    async fn two_servers_that_wash_to_the_same_name_are_split() {
        if echo_server().is_none() {
            return;
        }
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
        if echo_server().is_none() {
            return;
        }
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

    #[cfg(unix)]
    #[tokio::test]
    async fn hung_stdio_server_does_not_block_the_next_server() {
        let specs = vec![
            ServerSpec {
                slug: "hung".into(),
                transport: Transport::Stdio {
                    command: "sh".into(),
                    args: vec!["-c".into(), "sleep 60".into()],
                    env: HashMap::new(),
                },
            },
            ServerSpec {
                slug: "next".into(),
                transport: Transport::Stdio {
                    command: "cat".into(),
                    args: vec![],
                    env: HashMap::new(),
                },
            },
        ];

        let (started, failed) =
            start_all_with_timeout(&specs, std::time::Duration::from_millis(100)).await;
        assert_eq!(started.len(), 1, "the server after the hung one did not start");
        assert_eq!(failed.len(), 1, "the hung server was not reported");
        assert_eq!(failed[0].0, "hung");
        assert!(failed[0].1.contains("timed out"), "{}", failed[0].1);
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
