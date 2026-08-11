//! Reads the plugins actually installed on this machine.
//!
//! **A fixture only proves the reader agrees with the fixture.** Plugin layout is somebody else's
//! format, written down nowhere this app controls, and the way to find out whether it is being read
//! correctly is to read the real ones — which is how the two things this caught were found: the
//! manifest sitting in `.claude-plugin/` rather than at the root, and a plugin's `.mcp.json`
//! putting its servers at the top level with no wrapper key.
//!
//! **It skips when there is no checkout**, so it is quiet on a machine that has none — including
//! CI. That makes it a probe rather than a guarantee, which is the honest thing for a test whose
//! subject is not in this repository.

use std::path::PathBuf;

fn marketplace() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").ok()?);
    let at = home.join(".claude/plugins/marketplaces/claude-plugins-official/plugins");
    at.is_dir().then_some(at)
}

#[test]
fn the_plugins_installed_on_this_machine_are_read_whole() {
    let Some(market) = marketplace() else {
        eprintln!("no plugin checkout on this machine — nothing to read");
        return;
    };
    let found = zyris_code::plugin::discover_in(std::slice::from_ref(&market));
    assert!(!found.is_empty(), "nothing was read from {}", market.display());

    let with = |f: fn(&zyris_code::plugin::Plugin) -> bool| found.iter().filter(|p| f(p)).count();
    let commands = with(|p| !p.commands.is_empty());
    let skills = with(|p| p.skills.is_some());
    let hooks = with(|p| !p.hooks.is_empty());
    let mcp = with(|p| !p.mcp.is_empty());
    eprintln!(
        "{} plugins — commands {commands} · skills {skills} · hooks {hooks} · mcp {mcp}",
        found.len()
    );

    // **Every part has to come back from something real.** Each of these was zero at some point
    // while the fixture tests were green.
    assert!(commands > 0, "no plugin's commands were read");
    assert!(skills > 0, "no plugin's skills were read");
    assert!(mcp > 0, "no plugin's MCP servers were read");
    assert!(hooks > 0, "no plugin's hooks were read");

    // A command must carry something to send. One that reads as empty would be a slash command
    // that does nothing when pressed.
    let any = found.iter().flat_map(|p| &p.commands).next().expect("no commands at all");
    assert!(!any.prompt.is_empty(), "a command with no prompt: {any:?}");
}
