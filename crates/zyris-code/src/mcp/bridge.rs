//! MCP 서버 하나를 이 노드의 캐퍼빌리티 하나로 바꾼다.
//!
//! **에이전트는 MCP라는 것을 모른다.** 도구 이름이 `zyris__{노드}__mcp_{이름}__{도구}`로
//! 보일 뿐이고, 부르면 우리가 stdio로 넘긴다.
//!
//! `#[zyris::capability]` 매크로를 쓸 수 없다 — 매크로는 컴파일 타임에 도구를 정하는데
//! 여기는 서버에 물어봐야 무엇이 있는지 안다. `ServeCapability`가 공개 트레이트라
//! **런타임에 만든 `CapabilityDescriptor`를 그대로 내주는 것이 합법이다.**

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

/// 설정 파일 하나에 적힌 서버 하나.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServerSpec {
    /// 설정에 적힌 이름. 캐퍼빌리티 이름이 `mcp_{여기}`가 된다.
    #[serde(skip)]
    pub slug: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default, rename = "mcpServers")]
    servers: HashMap<String, ServerSpec>,
}

pub struct McpCapability {
    /// 와이어에 나가는 캐퍼빌리티 이름. **이미 씻은 것이다.**
    ///
    /// 슬러그만 씻고 `mcp_`를 앞에 붙이면 안 된다 — 슬러그가 통째로 `_`로 줄어드는 이름
    /// (한글 등)에서 `mcp__`가 되어 **attacca가 쪼개는 바로 그 글자가 다시 생긴다.**
    /// 실제로 그렇게 나갔고 그 도구는 끝내 안 불렸다. 붙이고 나서 씻는다.
    name: String,
    tools: Vec<McpTool>,
    /// **한 번에 하나만 말한다.** stdio는 줄 하나에 답 하나라 두 호출이 겹치면 서로의
    /// 답을 가져간다. 여기서 줄 세우는 것이 가장 싸다.
    client: Mutex<McpClient>,
}

impl McpCapability {
    pub async fn start(spec: &ServerSpec) -> anyhow::Result<McpCapability> {
        let mut client = McpClient::spawn(&spec.command, &spec.args, &spec.env).await?;
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
                    // MCP는 결과 스키마를 주지 않는다. 느슨하게 열어 둔다.
                    response_schema: Some(json!({"type": "object"})),
                    item_schema: None,
                })
                .collect(),
        };
        // 설명을 예산에 맞춘다 — `tools::trim`과 같은 이유다.
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
            // **서버가 아는 이름으로 부른다.** 씻은 이름은 와이어에서만 쓴다.
            .call(&tool.raw, args)
            .await
            .map_err(|e| WireError::internal(e.to_string()))?;
        encode_response(&out)
    }
}

/// 설정을 읽을 자리 둘. **뒤가 이긴다** — 프로젝트가 홈보다 구체적이다.
pub fn config_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        out.push(PathBuf::from(home).join(".config/zyris-code/mcp.json"));
    }
    out.push(cwd.join(".mcp.json"));
    out
}

/// 적혀 있는 서버들. 읽을 수 없는 파일은 조용히 건너뛴다 — 설정이 없는 것이 정상이다.
pub fn load_config(cwd: &Path) -> Vec<ServerSpec> {
    let files: Vec<Value> = config_paths(cwd)
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|s| match serde_json::from_str::<Value>(&s) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("MCP 설정을 읽지 못했다: {e}");
                None
            }
        })
        .collect();
    merge_configs(files)
}

/// 앞의 것부터 덮어쓴다. 같은 이름이면 **뒤엣것이 이긴다.**
pub fn merge_configs(files: Vec<Value>) -> Vec<ServerSpec> {
    let mut merged: HashMap<String, ServerSpec> = HashMap::new();
    for file in files {
        let Ok(parsed) = serde_json::from_value::<ConfigFile>(file) else { continue };
        for (slug, mut spec) in parsed.servers {
            spec.slug = slug.clone();
            merged.insert(slug, spec);
        }
    }
    // 이름 순서로 고정한다. HashMap 순서 그대로 내보내면 announce가 실행마다 달라진다.
    let mut out: Vec<ServerSpec> = merged.into_values().collect();
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

/// 적힌 서버를 모두 띄운다. **하나가 안 떠도 나머지는 뜬다.**
///
/// 되는 것만 돌려주고 안 된 것은 (이름, 까닭)으로 알린다 — 조용히 빠지면 사람은 도구가
/// 있는 줄 알고 기다린다.
pub async fn start_all(specs: &[ServerSpec]) -> (Vec<McpCapability>, Vec<(String, String)>) {
    let mut started: Vec<McpCapability> = Vec::new();
    let mut failed = Vec::new();
    for spec in specs {
        match McpCapability::start(spec).await {
            Ok(cap) => started.push(cap),
            Err(e) => failed.push((spec.slug.clone(), e.to_string())),
        }
    }
    // **씻고 나서 이름이 겹칠 수 있다.** 영숫자가 없는 이름 둘은 둘 다 `mcp_`가 된다 —
    // 겹친 채로 내보내면 뒤엣것이 조용히 묻힌다.
    let names = unique_names(started.iter().map(|c| c.name.clone()).collect());
    for (cap, name) in started.iter_mut().zip(names) {
        cap.name = name;
    }
    (started, failed)
}

/// 겹치지 않는 캐퍼빌리티 이름들. **번호를 붙인 뒤에도 `__`가 남으면 안 된다.**
///
/// `mcp_`에 `_2`를 그대로 이으면 `mcp__2`가 되어 씻은 보람이 사라진다. 그래서 붙이고
/// **다시 씻은 것**을 기준으로 겹침을 본다.
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
        // descriptor만 보는 테스트라 서버는 `cat`으로 세워 둔다 — 말은 걸지 않는다.
        let client = McpClient::spawn("cat", &[], &HashMap::new()).await;
        McpCapability {
            name: sanitize(&format!("mcp_{slug}")),
            tools,
            client: Mutex::new(client.expect("cat은 어디에나 있다")),
        }
    }

    /// **MCP의 inputSchema가 그대로 request_schema가 되어야 한다.**
    /// 안 되면 에이전트가 인자를 만들 수 없다.
    #[tokio::test]
    async fn the_descriptor_carries_each_tools_schema() {
        let cap = cap_of("github", vec![tool("create-issue")]).await;
        let d = cap.descriptor();
        assert_eq!(d.name, "mcp_github");
        assert_eq!(d.tools[0].name, "create_issue", "이름이 씻겨야 한다");
        assert_eq!(d.tools[0].request_schema["properties"]["title"]["type"], json!("string"));
    }

    /// **와이어 이름이 정확히 넷으로 쪼개져야 한다.** 이것이 진짜 판정이다.
    ///
    /// 슬러그만 씻는 것으로는 모자라다. 영숫자가 하나도 없는 이름은 통째로 줄어들어
    /// `mcp_`가 되고, 거기에 이어 붙이는 `__`가 만나 `mcp___echo`가 된다 — 실제로 그렇게
    /// 나갔고 그 도구는 끝내 안 불렸다. **두 번 틀린 자리다.**
    #[tokio::test]
    async fn the_wire_name_still_splits_into_four() {
        for slug in ["my__server", "연습", "--", "깃 허브", "github"] {
            let cap = cap_of(slug, vec![tool("create-issue")]).await;
            let d = cap.descriptor();
            let wire = format!("zyris__arch__{}__{}", d.name, d.tools[0].name);
            assert_eq!(wire.split("__").count(), 4, "{slug} → {wire}");
        }
    }

    /// 번호를 붙인 뒤에도 `__`가 남으면 안 된다 — `mcp_` + `_2` = `mcp__2`.
    #[test]
    fn numbering_a_collision_does_not_bring_the_double_underscore_back() {
        let out = unique_names(vec!["mcp_".into(), "mcp_".into(), "mcp_".into()]);
        assert_eq!(out, vec!["mcp", "mcp_2", "mcp_3"]);
        for n in &out {
            assert_eq!(format!("zyris__arch__{n}__x").split("__").count(), 4, "{n}");
        }
    }

    /// 씻고 나서 겹친 이름은 갈라놔야 한다 — 겹치면 뒤엣것이 조용히 묻힌다.
    #[tokio::test]
    async fn two_servers_that_wash_to_the_same_name_are_split() {
        let specs = vec![
            ServerSpec {
                slug: "연습".into(),
                command: "cat".into(),
                args: vec![],
                env: HashMap::new(),
            },
            ServerSpec {
                slug: "실습".into(),
                command: "cat".into(),
                args: vec![],
                env: HashMap::new(),
            },
        ];
        let (started, _) = start_all(&specs).await;
        let names: Vec<String> = started.iter().map(|c| c.descriptor().name).collect();
        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1], "겹친 이름이 그대로 나갔다: {names:?}");
        for n in &names {
            assert!(!n.contains("__"), "{n}");
        }
    }

    /// 작업 디렉터리의 설정이 홈의 설정을 이긴다.
    #[test]
    fn the_project_config_wins() {
        let merged = merge_configs(vec![
            json!({"mcpServers": {"a": {"command": "홈"}}}),
            json!({"mcpServers": {"a": {"command": "프로젝트"}}}),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].command, "프로젝트");
        assert_eq!(merged[0].slug, "a");
    }

    /// 한쪽에만 있는 서버는 그대로 남는다 — 덮어쓰기지 갈아치우기가 아니다.
    #[test]
    fn servers_only_one_file_knows_about_survive() {
        let merged = merge_configs(vec![
            json!({"mcpServers": {"a": {"command": "가"}}}),
            json!({"mcpServers": {"b": {"command": "나"}}}),
        ]);
        assert_eq!(merged.iter().map(|s| s.slug.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
    }

    /// **하나가 안 떠도 나머지는 뜬다.** 앱이 통째로 멈추면 안 된다.
    #[tokio::test]
    async fn a_server_that_fails_to_start_does_not_stop_the_others() {
        let specs = vec![
            ServerSpec {
                slug: "없는놈".into(),
                command: "이런건-없다".into(),
                args: vec![],
                env: HashMap::new(),
            },
            ServerSpec {
                slug: "좋은놈".into(),
                command: "cat".into(),
                args: vec![],
                env: HashMap::new(),
            },
        ];
        let (started, failed) = start_all(&specs).await;
        assert_eq!(started.len(), 1, "되는 것은 떠야 한다");
        assert_eq!(failed.len(), 1, "안 된 것은 알려야 한다");
        assert_eq!(failed[0].0, "없는놈");
    }

    /// 설정이 없는 것이 정상이다. 그때 죽으면 앱을 못 쓴다.
    #[test]
    fn no_config_at_all_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_config(dir.path()).is_empty());
    }

    /// 망가진 JSON도 앱을 멈추면 안 된다.
    #[test]
    fn a_broken_config_is_skipped_rather_than_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".mcp.json"), "{이건 JSON이 아니다").unwrap();
        assert!(load_config(dir.path()).is_empty());
    }
}
