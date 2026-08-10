//! What this node hands out to agents.
//!
//! **The moment we announce, every agent in every session of that account sees this node.** Sessions
//! running in other windows can touch this computer too, and capkit's path resolution isn't a jail,
//! so (`path.rs`: "the root is a default, not a jail") absolute paths escape the working directory.
//! **Catching that escape is `Gate`'s job** — anything outside the working directory follows
//! the `/config` directory-access setting (deny by default; `allow` runs it without asking).

pub mod bridge;
pub mod diff;
pub mod edit;
pub mod gate;
pub mod guard;
pub mod jobs;
pub mod readonly;
pub mod rules;
pub mod search;
pub mod skill;
pub mod trim;
pub mod wait;
pub mod work;

use std::path::PathBuf;

use zyris::runtime::Runner;
use zyris_capkit::PtyTerminal;
use zyris_caps::TerminalServer;

use bridge::Bridge;
use edit::{CodeEditServer, LocalEdit};
use guard::Gate;
use readonly::ReadOnlyFileIo;
use rules::Rules;

/// The base from which tools resolve relative paths — where the process was launched.
///
/// **The screen and the tools must see the same thing.** If the strip above the input points at A
/// while the tool runs in B, you can't tell which file the `src/app.rs` on a tool line
/// means. So there's one definition here.
pub fn working_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Attaches everything this node hands out to the Runner.
///
/// `cwd` is the process's working directory. Relative paths resolve against it, but **it is not a
/// jail** — absolute paths go straight through. What stops them is `Gate`.
///
/// **Every single one is wrapped in `Gate`.** If even one goes out bare, that capability
/// becomes a back door around the fence.
pub fn announce(
    runner: Runner,
    cwd: PathBuf,
    bridge: Bridge,
    api: tokio::sync::watch::Receiver<Option<std::sync::Arc<zyris_attacca::AttaccaApiClient>>>,
) -> Runner {
    let edit = LocalEdit::new(cwd.clone());
    // `/undo` must see the **same history** as the edit tool. Built separately, their locks would drift apart.
    bridge.set_undo(edit.undo());
    // **The reference for what counts as going outside.** Without this, the gate wouldn't see anything as outside.
    bridge.set_root(cwd.clone());

    // The background-job registry. **The tool and the screen must see the same one** so that
    // `/jobs` sees what the tool started and the exit path can kill it.
    let jobs = jobs::Jobs::new(cwd.clone());
    bridge.set_jobs(jobs.clone());

    // Skills are this node's plus the plugins'. The list goes once in the session preamble and
    // bodies go through `skill.load` — loading everything would eat context every turn.
    let mut skill_dirs = vec![cwd.join(".zyris-code/skills")];
    // The user tier is `conn::app_dir` — see `Skills::discover`, which must stay in step.
    if let Some(dir) = crate::conn::app_dir() {
        skill_dirs.insert(0, dir.join("skills"));
    }
    skill_dirs.extend(skill::plugin_skill_dirs(&cwd));
    let skills = skill::Skills::new(skill_dirs);

    // Two things to put in the session: **this repo's conventions** (`CLAUDE.md`·`AGENTS.md`) and the skill list.
    // Conventions come first — they must be followed no matter what, while skills are only used when relevant.
    let parts = [crate::instructions::preamble(&cwd), skill::preamble(&skills)];
    let joined: Vec<String> = parts.into_iter().flatten().collect();
    bridge.set_preamble((!joined.is_empty()).then(|| joined.join("\n\n")));

    runner
        .capability(Gate::new(skill::SkillServer(skills), bridge.clone()))
        .capability(Gate::new(rules::RulesCapServer(Rules::new(cwd.clone())), bridge.clone()))
        .capability(Gate::new(ReadOnlyFileIo::new(cwd.clone()), bridge.clone()))
        // Search is reading too. But **paths going outside are asked about even when read-only.**
        .capability(Gate::new(search::SearchServer(search::LocalSearch::new(cwd)), bridge.clone()))
        .capability(Gate::new(TerminalServer(PtyTerminal::default()), bridge.clone()))
        .capability(Gate::new(CodeEditServer(edit), bridge.clone()))
        // **This is where the long-running work goes.** `exec` gets cut at the wire deadline and
        // the process dies with it, whereas `until` answers **with success** even when it hasn't
        // finished, so the agent doesn't read it as a failure. It lives here rather than upstream
        // because one of its branches waits on a work.
        .capability(Gate::new(
            wait::WaitServer(wait::Waits::new(jobs, api.clone(), bridge.clone())),
            bridge.clone(),
        ))
        // **Only this one reaches outside this computer.** Big jobs are handed to attacca so a
        // subagent runs them (`work.rs`). The handle arrives after we attach, so we receive it via `watch`.
        .capability(Gate::new(work::WorkServer(work::Works::new(api)), bridge))
}

/// Spawns the configured MCP servers **in the background** and attaches them as capabilities.
///
/// It must not block the app from coming up — servers fetched via `npx` take seconds, and the
/// screen must not wait. `Capabilities::add` is a full replacement announce, so growing later is fine.
///
/// **What attaches here passes through `Gate` too.** This is where someone else's code runs on
/// this computer, so it should pass even more.
pub fn start_mcp(caps: zyris::Capabilities, cwd: PathBuf, bridge: Bridge) {
    tokio::spawn(async move {
        // What plugins add + what the config file says. **The config file wins** — what a person
        // wrote directly is more specific.
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
            tracing::warn!("could not start MCP server '{slug}': {why}");
            // **If it fails silently, a person waits thinking the tool exists.** The status line
            // disappears after 6 seconds, so we note it separately so `/mcp` can still show it later.
            bridge.note_mcp(slug, Err(why.clone()));
            bridge.frame(crate::app::Frame::Notice(format!(
                "MCP 서버 '{slug}'를 띄우지 못했습니다: {why}"
            )));
        }
        for cap in started {
            let d = zyris::ServeCapability::descriptor(&cap);
            let (name, tools) = (d.name, d.tools.len());
            if let Err(e) = caps.add(Gate::new(cap, bridge.clone())).await {
                tracing::warn!("could not announce {name}: {e}");
                bridge.note_mcp(&name, Err(e.to_string()));
            } else {
                tracing::info!("announcing {name}");
                bridge.note_mcp(&name, Ok(tools));
            }
        }
    });
}
