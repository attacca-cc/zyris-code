//! Finds MCP servers that other coding clients already have set up.
//!
//! Writing a server down twice is the sort of chore that quietly decides which tool someone uses.
//! Most people already have Claude Code, Cursor or VS Code pointed at the servers they care about,
//! and those files are plain JSON in known places — so this reads them.
//!
//! **Nothing found here is started on its own.** A discovered entry is a *suggestion*: it names a
//! program somebody else's client was told to run, and running it because it happened to be on
//! disk is not a decision this app gets to make. `/mcp on <name>` is how it gets turned on, and the
//! answer is kept in this app's own settings (`config.rs`).
//!
//! What zyris-code was told directly — `~/.config/zyris-code/mcp.json` and `./.mcp.json` — is a
//! different thing and still starts by itself. Those files are ours; somebody wrote them *for*
//! this app.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::mcp::bridge::{merge_configs, ServerSpec};

/// Which discovered servers this machine has said yes to.
///
/// **Kept apart from `config.rs` on purpose.** That struct is what the `/config` form shows, one
/// fixed-width value cell per line; this is an open-ended list toggled by `/mcp on|off` and never
/// drawn there. Folding it in would have made the whole settings form carry a growable field for
/// something it does not show.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Allowed {
    #[serde(default)]
    servers: Vec<String>,
}

/// Where the answers are kept. Beside the settings and the credentials.
fn store() -> Option<PathBuf> {
    crate::conn::credential_dir().map(|dir| dir.join("mcp-enabled.json"))
}

impl Allowed {
    pub fn load() -> Allowed {
        let Some(at) = store() else { return Allowed::default() };
        let Ok(text) = std::fs::read_to_string(&at) else { return Allowed::default() };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not read which MCP servers are allowed");
            Allowed::default()
        })
    }

    /// **The app keeps running if this fails** — the answer is already in effect for this run.
    pub fn save(&self) {
        let Some(at) = store() else { return };
        if let Some(dir) = at.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(text) = serde_json::to_string(self) else { return };
        if let Err(e) = std::fs::write(&at, text) {
            tracing::warn!(error = %e, "could not save which MCP servers are allowed");
        }
    }

    pub fn allows(&self, slug: &str) -> bool {
        self.servers.iter().any(|s| s == slug)
    }

    /// Turns one on or off. Answers whether anything changed, so the caller can tell "done" from
    /// "it already was" — a command that says nothing new reads as not having worked.
    pub fn set(&mut self, slug: &str, on: bool) -> bool {
        let had = self.allows(slug);
        if on && !had {
            self.servers.push(slug.to_string());
        } else if !on && had {
            self.servers.retain(|s| s != slug);
        }
        had != on
    }
}

/// A server somebody else's client knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub spec: ServerSpec,
    /// Which client it was read from, for `/mcp` to say. **Where a server came from is the whole
    /// basis for deciding whether to trust it.**
    pub source: String,
}

/// Where to look, and what to call what is found there.
///
/// Home-level files come first and project-level after, matching how those clients read them
/// themselves. **A file that is not there is not an error** — almost nobody has all of these.
fn sources(cwd: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    if let Some(home) = crate::conn::user_home() {
        out.push(("Claude Code".into(), home.join(".claude.json")));
        out.push(("Claude Code".into(), home.join(".claude/settings.json")));
        out.push(("Cursor".into(), home.join(".cursor/mcp.json")));
        out.push(("Gemini CLI".into(), home.join(".gemini/settings.json")));
        out.push(("Windsurf".into(), home.join(".codeium/windsurf/mcp_config.json")));
    }
    out.push(("Claude Code".into(), cwd.join(".claude/settings.json")));
    out.push(("Claude Code".into(), cwd.join(".claude/settings.local.json")));
    out.push(("Cursor".into(), cwd.join(".cursor/mcp.json")));
    out.push(("VS Code".into(), cwd.join(".vscode/mcp.json")));
    out
}

/// Everything the other clients know about, minus what this app was told directly.
///
/// **Ours win and are not listed twice.** A name that appears in both is already going to start;
/// offering to turn it on again would read as two different servers.
pub fn found(cwd: &Path) -> Vec<Found> {
    let ours: Vec<String> =
        crate::mcp::bridge::load_config(cwd).into_iter().map(|s| s.slug).collect();
    let mut out: Vec<Found> = Vec::new();
    for (source, path) in sources(cwd) {
        for spec in read(&path) {
            if ours.contains(&spec.slug) {
                continue;
            }
            // The same server in two clients is one server. **The first sighting wins**, so the
            // home-level file — the one a person is most likely to recognise — names it.
            if out.iter().any(|f| f.spec.slug == spec.slug) {
                continue;
            }
            out.push(Found { spec, source: source.clone() });
        }
    }
    out.sort_by(|a, b| a.spec.slug.cmp(&b.spec.slug));
    out
}

/// One file's servers. Unreadable or absent is normal and says nothing.
///
/// `~/.claude.json` also keeps a per-project block, so those are read too — that is where Claude
/// Code puts a server added with `claude mcp add` inside a project.
fn read(path: &Path) -> Vec<ServerSpec> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        tracing::debug!("could not read {} as JSON", path.display());
        return Vec::new();
    };
    let mut files = vec![value.clone()];
    if let Some(projects) = value.get("projects").and_then(Value::as_object) {
        files.extend(projects.values().cloned());
    }
    merge_configs(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::bridge::Transport;

    fn write(at: &Path, body: &str) {
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, body).unwrap();
    }

    #[test]
    fn a_servers_written_in_another_clients_project_file_is_found() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join(".cursor/mcp.json"),
            r#"{"mcpServers":{"playwright":{"command":"npx","args":["-y","@playwright/mcp"]}}}"#,
        );
        let got = found(dir.path());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].spec.slug, "playwright");
        assert_eq!(got[0].source, "Cursor");
    }

    /// **A remote server is read as remote**, whether or not the file bothered to say `type`.
    #[test]
    fn a_url_is_enough_to_mean_a_remote_server() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join(".vscode/mcp.json"),
            r#"{"servers":{"docs":{"url":"https://example.test/mcp","headers":{"x":"1"}}}}"#,
        );
        let got = found(dir.path());
        assert_eq!(got.len(), 1, "{got:?}");
        match &got[0].spec.transport {
            Transport::Http { url, headers } => {
                assert_eq!(url, "https://example.test/mcp");
                assert_eq!(headers.get("x").map(String::as_str), Some("1"));
            }
            other => panic!("it must be remote: {other:?}"),
        }
    }

    /// **What this app was told directly is not offered as a discovery.** It already starts, and
    /// listing it again reads as a second server of the same name.
    #[test]
    fn a_server_this_app_already_runs_is_not_offered_again() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".mcp.json"), r#"{"mcpServers":{"mine":{"command":"a"}}}"#);
        write(&dir.path().join(".cursor/mcp.json"), r#"{"mcpServers":{"mine":{"command":"b"}}}"#);
        assert!(found(dir.path()).is_empty(), "{:?}", found(dir.path()));
    }

    /// The same server set up in two clients is one server, named by the first sighting.
    #[test]
    fn one_server_seen_twice_is_listed_once() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".cursor/mcp.json"), r#"{"mcpServers":{"a":{"command":"x"}}}"#);
        write(&dir.path().join(".vscode/mcp.json"), r#"{"servers":{"a":{"command":"x"}}}"#);
        let got = found(dir.path());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].source, "Cursor");
    }

    /// A file that is not there, or is not JSON at all, says nothing. **Most people have none of
    /// these** — treating an absent file as a problem would put a warning on every launch.
    #[test]
    fn a_missing_or_broken_file_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(found(dir.path()).is_empty());
        write(&dir.path().join(".vscode/mcp.json"), "not json at all");
        assert!(found(dir.path()).is_empty());
    }

    /// Claude Code files a project's servers under `projects.<path>`, so that block is read too.
    #[test]
    fn claude_codes_per_project_block_is_read() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join(".claude/settings.json"),
            r#"{"projects":{"/somewhere":{"mcpServers":{"deep":{"command":"d"}}}}}"#,
        );
        let got = found(dir.path());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].spec.slug, "deep");
    }
}
