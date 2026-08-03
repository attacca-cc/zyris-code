//! 이 노드가 에이전트에게 내주는 것.
//!
//! **announce하는 순간 그 계정의 모든 세션의 에이전트가 이 노드를 본다.** 다른 창에서
//! 돌던 세션도 이 컴퓨터를 만질 수 있고, capkit의 경로 해석은 감옥이 아니라서
//! (`path.rs`: "the root is a default, not a jail") 절대경로는 작업 디렉터리를 벗어난다.
//! **그 벗어남을 잡는 것이 `Gate`다** — 작업 디렉터리 밖은 사람에게 묻는다.

pub mod bridge;
pub mod diff;
pub mod edit;
pub mod gate;
pub mod guard;
pub mod readonly;
pub mod search;
pub mod skill;
pub mod trim;
pub mod work;

use std::path::PathBuf;

use zyris::runtime::Runner;
use zyris_capkit::PtyTerminal;
use zyris_caps::TerminalServer;

use bridge::Bridge;
use edit::{CodeEditServer, LocalEdit};
use guard::Gate;
use readonly::ReadOnlyFileIo;

/// 도구가 상대경로를 푸는 기준 — 프로세스를 띄운 자리다.
///
/// **화면과 도구가 같은 것을 봐야 한다.** 사이드바가 A를 가리키는데 도구가 B에서 돌면
/// 승인 화면에 뜬 `src/app.rs`가 어느 파일인지 알 수 없다. 그래서 정의는 여기 하나다.
pub fn working_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// 이 노드가 내주는 것 전부를 Runner에 단다.
///
/// `cwd`는 프로세스의 작업 디렉터리다. 상대경로가 이것을 기준으로 풀리지만 **감옥은
/// 아니다** — 절대경로는 그대로 나간다. 막는 것은 `Gate`다.
///
/// **하나도 빠짐없이 `Gate`로 감싼다.** 하나라도 맨몸으로 나가면 그 캐퍼빌리티가
/// 승인을 거치지 않는 뒷문이 된다.
pub fn announce(
    runner: Runner,
    cwd: PathBuf,
    bridge: Bridge,
    api: tokio::sync::watch::Receiver<Option<std::sync::Arc<zyris_attacca::AttaccaApiClient>>>,
) -> Runner {
    let edit = LocalEdit::new(cwd.clone());
    // `/undo`가 편집 도구와 **같은 기록**을 봐야 한다. 각자 만들면 잠금이 따로 논다.
    bridge.set_undo(edit.undo());
    // **밖으로 나가는지 재는 기준.** 이것이 없으면 게이트가 아무것도 밖으로 안 본다.
    bridge.set_root(cwd.clone());

    // 스킬은 이 노드의 것과 플러그인의 것을 합친 것이다. 목록은 세션 preamble로 한 번
    // 가고 본문은 `skill.load`로 간다 — 다 실으면 매 턴 컨텍스트를 먹는다.
    let mut skill_dirs = vec![cwd.join(".zyris-code/skills")];
    if let Some(home) = std::env::var_os("HOME") {
        skill_dirs.insert(0, PathBuf::from(home).join(".config/zyris-code/skills"));
    }
    skill_dirs.extend(skill::plugin_skill_dirs(&cwd));
    let skills = skill::Skills::new(skill_dirs);

    // 세션에 실을 것 둘: **이 리포의 규약**(`CLAUDE.md`·`AGENTS.md`)과 스킬 목록.
    // 규약이 먼저다 — 무엇을 하든 지켜야 하는 것이고, 스킬은 해당할 때만 쓴다.
    let parts = [crate::instructions::preamble(&cwd), skill::preamble(&skills)];
    let joined: Vec<String> = parts.into_iter().flatten().collect();
    bridge.set_preamble((!joined.is_empty()).then(|| joined.join("\n\n")));

    runner
        .capability(Gate::new(skill::SkillServer(skills), bridge.clone()))
        .capability(Gate::new(ReadOnlyFileIo::new(cwd.clone()), bridge.clone()))
        // 검색도 읽기다. 다만 **밖으로 나가는 경로는 읽기여도 묻는다.**
        .capability(Gate::new(search::SearchServer(search::LocalSearch::new(cwd)), bridge.clone()))
        .capability(Gate::new(TerminalServer(PtyTerminal::default()), bridge.clone()))
        .capability(Gate::new(CodeEditServer(edit), bridge.clone()))
        // **이것만 이 컴퓨터 밖을 향한다.** 큰 일을 attacca에 넘겨 서브에이전트가 돌리게
        // 한다(`work.rs`). 손잡이는 붙은 뒤에 오므로 `watch`로 받아 둔다.
        .capability(Gate::new(work::WorkServer(work::Works::new(api)), bridge))
}

/// 적혀 있는 MCP 서버를 **배경에서** 띄워 캐퍼빌리티로 붙인다.
///
/// 앱이 뜨는 것을 막지 않는다 — `npx`로 받아 오는 서버는 몇 초씩 걸리는데 그동안 화면이
/// 안 뜨면 안 된다. `Capabilities::add`는 전체 교체 announce라 연결한 뒤에 늘어나도 된다.
///
/// **여기서 붙는 것도 `Gate`를 지난다.** 남의 코드가 이 컴퓨터에서 도는 자리라 오히려
/// 더 지나야 한다.
pub fn start_mcp(caps: zyris::Capabilities, cwd: PathBuf, bridge: Bridge) {
    tokio::spawn(async move {
        // 플러그인이 얹는 것 + 설정 파일에 적힌 것. **설정 파일이 이긴다** — 사람이
        // 직접 적은 쪽이 구체적이다.
        let mut specs = crate::plugin::mcp_servers(&crate::plugin::discover(&cwd));
        for spec in crate::mcp::bridge::load_config(&cwd) {
            match specs.iter_mut().find(|s| s.slug == spec.slug) {
                Some(slot) => *slot = spec,
                None => specs.push(spec),
            }
        }
        if specs.is_empty() {
            return;
        }
        let (started, failed) = crate::mcp::bridge::start_all(&specs).await;
        for (slug, why) in &failed {
            tracing::warn!("MCP 서버 '{slug}'를 띄우지 못했다: {why}");
            // **조용히 빠지면 사람은 도구가 있는 줄 알고 기다린다.** 상태 줄은 6초 뒤
            // 사라지므로 `/mcp`가 나중에도 볼 수 있게 따로 적어 둔다.
            bridge.note_mcp(slug, Err(why.clone()));
            bridge.frame(crate::app::Frame::Notice(format!(
                "MCP 서버 '{slug}'를 띄우지 못했습니다: {why}"
            )));
        }
        for cap in started {
            let d = zyris::ServeCapability::descriptor(&cap);
            let (name, tools) = (d.name, d.tools.len());
            if let Err(e) = caps.add(Gate::new(cap, bridge.clone())).await {
                tracing::warn!("{name}을 내주지 못했다: {e}");
                bridge.note_mcp(&name, Err(e.to_string()));
            } else {
                tracing::info!("{name}을 내준다");
                bridge.note_mcp(&name, Ok(tools));
            }
        }
    });
}
