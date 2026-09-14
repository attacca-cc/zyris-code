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
//! What the user wrote in `~/.config/zyris-code/mcp.json` still starts by itself. A repository's
//! `./.mcp.json` is only a candidate: cloning a repository is not consent to run its programs.

use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::mcp::bridge::{merge_configs, ServerSpec};

/// Which discovered servers this machine has said yes to.
///
/// **Kept apart from `config.rs` on purpose.** That struct is what the `/config` form shows, one
/// fixed-width value cell per line; this is an open-ended list toggled by `/mcp on|off` and never
/// drawn there. Folding it in would have made the whole settings form carry a growable field for
/// something it does not show.
const VERSION: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Allowed {
    #[serde(default = "current_version")]
    version: u8,
    #[serde(default)]
    servers: Vec<String>,
    #[serde(default)]
    approvals: Vec<Approval>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Approval {
    project: String,
    kind: String,
    name: String,
    digest: String,
}

const fn current_version() -> u8 {
    VERSION
}

impl Default for Allowed {
    fn default() -> Self {
        Self { version: VERSION, servers: Vec::new(), approvals: Vec::new() }
    }
}

/// Where the answers are kept. Beside the settings and the credentials.
fn store() -> Option<PathBuf> {
    crate::conn::credential_dir().map(|dir| dir.join("mcp-enabled.json"))
}

impl Allowed {
    pub fn load() -> Allowed {
        let Some(at) = store() else { return Allowed::default() };
        let Ok(text) = std::fs::read_to_string(&at) else { return Allowed::default() };
        let mut allowed: Allowed = serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not read which MCP servers are allowed");
            Allowed::default()
        });
        if allowed.version != VERSION {
            tracing::warn!(
                version = allowed.version,
                "ignoring project approvals from an unknown format"
            );
            allowed.approvals.clear();
            allowed.version = VERSION;
        }
        allowed
    }

    /// **The app keeps running if this fails** — the answer is already in effect for this run.
    pub fn save(&self) {
        let Some(at) = store() else { return };
        if let Some(dir) = at.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(text) = serde_json::to_string(self) else { return };
        let temp = at.with_extension(format!("{}.tmp", std::process::id()));
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(text.as_bytes())?;
                file.sync_all()
            })
            .and_then(|()| std::fs::rename(&temp, &at));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&temp);
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

    pub fn allows_found(&self, cwd: &Path, found: &Found) -> bool {
        if !found.project {
            return self.allows(&found.spec.slug);
        }
        self.allows_exact(
            cwd,
            &format!("mcp:{}", found.source),
            &found.spec.slug,
            &server_digest(&found.spec),
        )
    }

    pub fn set_found(&mut self, cwd: &Path, found: &Found, on: bool) -> bool {
        if !found.project {
            return self.set(&found.spec.slug, on);
        }
        self.set_exact(
            cwd,
            &format!("mcp:{}", found.source),
            &found.spec.slug,
            server_digest(&found.spec),
            on,
        )
    }

    pub fn allows_plugin(&self, cwd: &Path, plugin: &crate::plugin::Plugin) -> bool {
        self.allows_exact(cwd, "plugin", &plugin.name, &crate::plugin::fingerprint(plugin))
    }

    pub fn set_plugin(&mut self, cwd: &Path, plugin: &crate::plugin::Plugin, on: bool) -> bool {
        self.set_exact(cwd, "plugin", &plugin.name, crate::plugin::fingerprint(plugin), on)
    }

    fn allows_exact(&self, cwd: &Path, kind: &str, name: &str, digest: &str) -> bool {
        let project = project_id(cwd);
        self.approvals
            .iter()
            .any(|a| a.project == project && a.kind == kind && a.name == name && a.digest == digest)
    }

    fn set_exact(&mut self, cwd: &Path, kind: &str, name: &str, digest: String, on: bool) -> bool {
        let project = project_id(cwd);
        let had = self.approvals.iter().any(|a| {
            a.project == project && a.kind == kind && a.name == name && a.digest == digest
        });
        self.approvals.retain(|a| !(a.project == project && a.kind == kind && a.name == name));
        if on {
            self.approvals.push(Approval {
                project,
                kind: kind.to_string(),
                name: name.to_string(),
                digest,
            });
        }
        had != on
    }
}

fn project_id(cwd: &Path) -> String {
    std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf()).to_string_lossy().into_owned()
}

fn server_digest(spec: &ServerSpec) -> String {
    let mut hash = Sha256::new();
    hash.update(spec.slug.as_bytes());
    match &spec.transport {
        crate::mcp::bridge::Transport::Stdio { command, args, env } => {
            hash.update(b"stdio\0");
            hash.update(command.as_bytes());
            for arg in args {
                hash.update(b"\0arg\0");
                hash.update(arg.as_bytes());
            }
            let mut env: Vec<_> = env.iter().collect();
            env.sort_by_key(|(key, _)| *key);
            for (key, value) in env {
                hash.update(b"\0env\0");
                hash.update(key.as_bytes());
                hash.update(b"\0");
                hash.update(value.as_bytes());
            }
        }
        crate::mcp::bridge::Transport::Http { url, headers } => {
            hash.update(b"http\0");
            hash.update(url.as_bytes());
            let mut headers: Vec<_> = headers.iter().collect();
            headers.sort_by_key(|(key, _)| *key);
            for (key, value) in headers {
                hash.update(b"\0header\0");
                hash.update(key.as_bytes());
                hash.update(b"\0");
                hash.update(value.as_bytes());
            }
        }
    }
    format!("{:x}", hash.finalize())
}

/// A server somebody else's client knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub spec: ServerSpec,
    /// Which client it was read from, for `/mcp` to say. **Where a server came from is the whole
    /// basis for deciding whether to trust it.**
    pub source: String,
    pub project: bool,
}

/// Where to look, and what to call what is found there.
///
/// Home-level files come first and project-level after, matching how those clients read them
/// themselves. **A file that is not there is not an error** — almost nobody has all of these.
fn sources(home: Option<PathBuf>, cwd: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut out: Vec<(String, PathBuf, bool)> = Vec::new();
    if let Some(home) = home {
        out.push(("Claude Code".into(), home.join(".claude.json"), false));
        out.push(("Claude Code".into(), home.join(".claude/settings.json"), false));
        out.push(("Cursor".into(), home.join(".cursor/mcp.json"), false));
        out.push(("Gemini CLI".into(), home.join(".gemini/settings.json"), false));
        out.push(("Windsurf".into(), home.join(".codeium/windsurf/mcp_config.json"), false));
    }
    out.push(("Claude Code".into(), cwd.join(".claude/settings.json"), true));
    out.push(("Claude Code".into(), cwd.join(".claude/settings.local.json"), true));
    out.push(("Cursor".into(), cwd.join(".cursor/mcp.json"), true));
    out.push(("VS Code".into(), cwd.join(".vscode/mcp.json"), true));
    out
}

/// Everything that needs an explicit MCP approval: the repository and other clients.
///
/// **The repository wins duplicate names.** It is the definition nearest this working directory,
/// but remains off until approved.
pub fn found(cwd: &Path) -> Vec<Found> {
    let user: Vec<String> =
        crate::mcp::bridge::load_user_config().into_iter().map(|spec| spec.slug).collect();
    found_in(crate::conn::user_home(), cwd)
        .into_iter()
        .filter(|found| found.project || !user.contains(&found.spec.slug))
        .collect()
}

/// The same, over a home directory that is given rather than looked up.
///
/// **Because a test that reads the real one is not a test.** Every assertion below counts what was
/// found, and each of them passed here while failing on any machine whose owner has an MCP server
/// of their own: a Windows check came back with six of them red, all reporting one extra server
/// that belonged to the person running them (2026-08-17). Whether a test passes must not depend on
/// whose machine it is.
pub fn found_in(home: Option<PathBuf>, cwd: &Path) -> Vec<Found> {
    let mut out: Vec<Found> = crate::mcp::bridge::load_project_config(cwd)
        .into_iter()
        .map(|spec| Found { spec, source: ".mcp.json".to_string(), project: true })
        .collect();
    for (source, path, project) in sources(home, cwd) {
        for spec in read(&path) {
            // The same server in two clients is one server. **The first sighting wins**, so the
            // home-level file — the one a person is most likely to recognise — names it.
            if out.iter().any(|f| f.spec.slug == spec.slug) {
                continue;
            }
            out.push(Found { spec, source: source.clone(), project });
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

    /// Discovery over an empty home and whatever the test wrote into its project directory.
    ///
    /// **Never the real home.** See `found_in`: run against the home of somebody who uses Claude
    /// Code or Cursor, these counts include that person's servers.
    fn found_here(cwd: &Path) -> Vec<Found> {
        let home = tempfile::tempdir().expect("somewhere empty to use as a home");
        found_in(Some(home.path().to_path_buf()), cwd)
    }

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
        let got = found_here(dir.path());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].spec.slug, "playwright");
        assert_eq!(got[0].source, "Cursor");
        assert!(got[0].project);
    }

    /// **A remote server is read as remote**, whether or not the file bothered to say `type`.
    #[test]
    fn a_url_is_enough_to_mean_a_remote_server() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join(".vscode/mcp.json"),
            r#"{"servers":{"docs":{"url":"https://example.test/mcp","headers":{"x":"1"}}}}"#,
        );
        let got = found_here(dir.path());
        assert_eq!(got.len(), 1, "{got:?}");
        match &got[0].spec.transport {
            Transport::Http { url, headers } => {
                assert_eq!(url, "https://example.test/mcp");
                assert_eq!(headers.get("x").map(String::as_str), Some("1"));
            }
            other => panic!("it must be remote: {other:?}"),
        }
    }

    /// The repository definition is the candidate shown to the user and shadows another client's
    /// entry of the same name. It no longer starts merely because it exists.
    #[test]
    fn a_project_server_shadows_the_same_discovered_name() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".mcp.json"), r#"{"mcpServers":{"mine":{"command":"a"}}}"#);
        write(&dir.path().join(".cursor/mcp.json"), r#"{"mcpServers":{"mine":{"command":"b"}}}"#);
        let found = found_here(dir.path());
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].project);
        assert_eq!(found[0].spec.transport.summary(), "a");
    }

    /// The same server set up in two clients is one server, named by the first sighting.
    #[test]
    fn one_server_seen_twice_is_listed_once() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".cursor/mcp.json"), r#"{"mcpServers":{"a":{"command":"x"}}}"#);
        write(&dir.path().join(".vscode/mcp.json"), r#"{"servers":{"a":{"command":"x"}}}"#);
        let got = found_here(dir.path());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].source, "Cursor");
    }

    /// A file that is not there, or is not JSON at all, says nothing. **Most people have none of
    /// these** — treating an absent file as a problem would put a warning on every launch.
    #[test]
    fn a_missing_or_broken_file_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(found_here(dir.path()).is_empty());
        write(&dir.path().join(".vscode/mcp.json"), "not json at all");
        assert!(found_here(dir.path()).is_empty());
    }

    /// **The home it is given is the home it reads.** Isolation that always looked at nothing
    /// would leave the home half of `sources` untested and passing.
    #[test]
    fn a_server_written_in_the_home_is_found_there() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".cursor/mcp.json"),
            r#"{"mcpServers":{"athome":{"command":"x"}}}"#,
        );
        let got = found_in(Some(home.path().to_path_buf()), cwd.path());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].spec.slug, "athome");
        assert!(!got[0].project);
    }

    /// Claude Code files a project's servers under `projects.<path>`, so that block is read too.
    #[test]
    fn claude_codes_per_project_block_is_read() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join(".claude/settings.json"),
            r#"{"projects":{"/somewhere":{"mcpServers":{"deep":{"command":"d"}}}}}"#,
        );
        let got = found_here(dir.path());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].spec.slug, "deep");
    }

    #[test]
    fn project_mcp_is_disabled_until_approved() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"project-server":{"command":"first"}}}"#,
        );
        let found = found_here(dir.path());
        let server = found.iter().find(|f| f.spec.slug == "project-server").unwrap();
        let mut allowed = Allowed::default();

        assert!(server.project);
        assert!(!allowed.allows_found(dir.path(), server));
        let legacy: Allowed = serde_json::from_str(r#"{"servers":["project-server"]}"#).unwrap();
        assert!(!legacy.allows_found(dir.path(), server));
        assert!(allowed.set_found(dir.path(), server, true));
        assert!(allowed.allows_found(dir.path(), server));

        write(
            &dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"project-server":{"command":"changed"}}}"#,
        );
        let changed = found_here(dir.path());
        let changed = changed.iter().find(|f| f.spec.slug == "project-server").unwrap();
        assert!(!allowed.allows_found(dir.path(), changed));
    }
}
