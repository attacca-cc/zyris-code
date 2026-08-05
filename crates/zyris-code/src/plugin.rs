//! Plugins — add tools (MCP servers) and skills as one bundle.
//!
//! No new execution format is created. A plugin only **points at two places that already exist**:
//! `mcp` is exactly the config shape from task 10, and `skills/` is exactly the shape `tools::skill`
//! reads. So supporting plugins adds no new execution path.
//!
//! ```text
//! plugins/
//!   github/
//!     plugin.json     { "name": "github", "mcp": { "gh": { "command": "npx", … } } }
//!     skills/
//!       review/SKILL.md
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::mcp::bridge::ServerSpec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    pub name: String,
    pub description: String,
    /// The MCP servers this plugin adds.
    pub mcp: Vec<ServerSpec>,
    /// This plugin's `skills/` directory. `None` when absent.
    pub skills: Option<PathBuf>,
    /// Which directory it came from. **This is what tells fetched and hand-placed apart** — the
    /// name alone can't, and only the fetched side can be removed.
    pub root: PathBuf,
}

impl Plugin {
    /// Whether it was fetched via `/plugin`.
    pub fn fetched(&self) -> bool {
        self.root.starts_with(install_dir())
    }
}

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "mcpServers", alias = "mcp")]
    mcp: HashMap<String, ServerSpecFile>,
}

/// The shape written in the file. `ServerSpec`'s `slug` comes from the key, so it's not here.
#[derive(Debug, Deserialize)]
struct ServerSpecFile {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

/// Pulls the place to clone and the directory name it lands in out of what the person typed.
///
/// **It's fetched with `git`.** No new dependency — a place using coding tools can't lack git, and
/// `/plugin update` comes down to one `git pull` line. It's the archive path that needs scaffolding:
/// unpacking and writing the update logic yourself.
///
/// Accepted shapes:
///
/// - `owner/repo` — treated as GitHub. The shape people type most
/// - `https://github.com/owner/repo` (`.git` or trailing `/` allowed)
/// - `git@github.com:owner/repo.git`
/// - any other `scheme://…` — accepted if it can be cloned even when it isn't GitHub
/// - `/…`·`~/…`·`./…` — a local repo. **This path is needed when testing while building a plugin**
pub fn source(text: &str) -> Option<(String, String)> {
    let text = text.trim().trim_end_matches('/');
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let url = if text.contains("://") || text.starts_with("git@") {
        text.to_string()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home().join(rest).to_string_lossy().into_owned()
    } else if text.starts_with('/') || text.starts_with("./") || text.starts_with("../") {
        text.to_string()
    } else {
        // The `owner/repo` shortcut. There must be exactly two pieces — to tell it apart from a given path.
        let parts: Vec<&str> = text.split('/').collect();
        if parts.len() != 2 || parts.iter().any(|p| p.is_empty()) {
            return None;
        }
        format!("https://github.com/{text}.git")
    };
    // The name is the last piece. `git@host:owner/repo` splits on `/` too, so it's caught the same way.
    let last = url.rsplit(['/', ':']).next()?.trim_end_matches(".git");
    let name = sanitize(last);
    (!name.is_empty()).then_some((url, name))
}

/// Washes a name until it can serve as one directory name.
///
/// **It must be a single path piece.** If `..` or `/` remains, it would write outside the plugin directory.
fn sanitize(name: &str) -> String {
    let kept: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' })
        .collect();
    kept.replace("..", "-").trim_matches(['-', '.']).to_string()
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// The two places to look for plugins. The latter wins — the project is more specific than home.
pub fn plugin_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        out.push(PathBuf::from(home).join(".config/zyris-code/plugins"));
    }
    out.push(cwd.join(".zyris-code/plugins"));
    out
}

pub fn discover(cwd: &Path) -> Vec<Plugin> {
    discover_in(&plugin_dirs(cwd))
}

/// Where fetched plugins live. **Only the home side** — fetching someone else's code into a project
/// would mix it into that repo's commits.
pub fn install_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".config/zyris-code/plugins"),
        None => std::env::temp_dir().join("zyris-code/plugins"),
    }
}

/// Runs `git` once. On failure it passes git's words through verbatim — if we rewrote them,
/// "no such repo" would get crushed into "installation failed".
async fn git(args: &[&str], at: Option<&Path>) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args);
    if let Some(at) = at {
        cmd.current_dir(at);
    }
    let out = cmd.output().await.map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => crate::lang::current().plugin_no_git().to_string(),
        _ => crate::lang::current().plugin_git_error(&e.to_string()),
    })?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    let why = String::from_utf8_lossy(&out.stderr);
    Err(why
        .lines()
        .last()
        .unwrap_or_else(|| crate::lang::current().plugin_git_failed())
        .trim()
        .to_string())
}

/// Fetches and puts it in place. If it's already there, it isn't fetched — overwriting would silently erase local edits.
///
/// **Without `plugin.json`, it's rolled back.** Fetching any random repo would silently ignore it
/// next run while the person believes it's installed.
pub async fn install(text: &str) -> Result<Plugin, String> {
    install_into(&install_dir(), text).await
}

/// The variant that takes the destination. **Tests use this** — the real home isn't touched and no
/// environment variables need to be shaken.
pub async fn install_into(dir: &Path, text: &str) -> Result<Plugin, String> {
    let Some((url, name)) = source(text) else {
        return Err(crate::lang::current().plugin_source_unclear(text));
    };
    let at = dir.join(&name);
    if at.exists() {
        return Err(crate::lang::current().plugin_already_there(&name));
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| crate::lang::current().plugin_dir_error(&e.to_string()))?;

    // History isn't needed. A shallow clone is several times faster on big repos.
    git(&["clone", "--depth", "1", &url, &at.to_string_lossy()], None).await?;

    if !at.join("plugin.json").exists() {
        let _ = std::fs::remove_dir_all(&at);
        return Err(crate::lang::current().plugin_no_manifest(&name));
    }
    let wanted = manifest_name(&at, &name);
    discover_in(std::slice::from_ref(&dir.to_path_buf()))
        .into_iter()
        .find(|p| p.name == wanted)
        .ok_or_else(|| crate::lang::current().plugin_manifest_unreadable(&name))
}

/// The name the manifest states. Without one, the directory name — same rule as `discover_in`.
fn manifest_name(at: &Path, slug: &str) -> String {
    let named = std::fs::read_to_string(at.join("plugin.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Manifest>(&t).ok())
        .map(|m| m.name)
        .filter(|n| !n.is_empty());
    named.unwrap_or_else(|| slug.to_string())
}

/// Removes something fetched. **Only fetched things are removed** — hand-made plugins aren't here.
pub fn remove(name: &str) -> Result<(), String> {
    remove_from(&install_dir(), name)
}

pub fn remove_from(dir: &Path, name: &str) -> Result<(), String> {
    let at = installed_path(dir, name)?;
    std::fs::remove_dir_all(&at)
        .map_err(|e| crate::lang::current().plugin_remove_error(&e.to_string()))
}

/// Updates one, or everything fetched. Failures are returned along with their reasons.
pub async fn update(name: Option<&str>) -> Vec<(String, Result<String, String>)> {
    update_in(&install_dir(), name).await
}

pub async fn update_in(dir: &Path, name: Option<&str>) -> Vec<(String, Result<String, String>)> {
    let names = match name {
        Some(n) => vec![n.to_string()],
        None => installed_in(dir),
    };
    let mut out = Vec::new();
    for name in names {
        let done = match installed_path(dir, &name) {
            Ok(at) => git(&["pull", "--ff-only"], Some(&at)).await,
            Err(e) => Err(e),
        };
        out.push((name, done));
    }
    out
}

/// The directory names of fetched plugins.
pub fn installed() -> Vec<String> {
    installed_in(&install_dir())
}

pub fn installed_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join("plugin.json").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// The location of one fetched thing.
///
/// Found by directory name or by the name the manifest states — what's shown on screen is the
/// latter, so that's what a person types.
///
/// **The name is washed before use.** A name carrying `../` must not be allowed to delete someone else's directory.
fn installed_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let slug = sanitize(name);
    if !slug.is_empty() {
        let at = dir.join(&slug);
        if at.join("plugin.json").exists() {
            return Ok(at);
        }
    }
    discover_in(std::slice::from_ref(&dir.to_path_buf()))
        .into_iter()
        .find(|p| p.name == name)
        .map(|p| p.root)
        .ok_or_else(|| crate::lang::current().plugin_not_found(name))
}

/// **If one breaks, the rest survive.** An unreadable plugin is skipped with a log entry left behind.
pub fn discover_in(dirs: &[PathBuf]) -> Vec<Plugin> {
    let mut found: Vec<Plugin> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let root = entry.path();
            let Ok(text) = std::fs::read_to_string(root.join("plugin.json")) else { continue };
            let manifest: Manifest = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("플러그인 설정을 읽지 못했다({}): {e}", root.display());
                    continue;
                }
            };
            let slug = entry.file_name().to_string_lossy().to_string();
            let name = if manifest.name.is_empty() { slug } else { manifest.name };
            let skills = root.join("skills");
            let plugin = Plugin {
                mcp: manifest
                    .mcp
                    .into_iter()
                    .map(|(slug, s)| ServerSpec {
                        slug,
                        command: s.command,
                        args: s.args,
                        env: s.env,
                    })
                    .collect(),
                skills: skills.is_dir().then_some(skills),
                description: manifest.description,
                root: root.clone(),
                name,
            };
            match found.iter_mut().find(|p| p.name == plugin.name) {
                Some(slot) => *slot = plugin,
                None => found.push(plugin),
            }
        }
    }
    // Fixed to name order. Left in directory order, the announce would change from run to run.
    found.sort_by(|a, b| a.name.cmp(&b.name));
    for p in &mut found {
        p.mcp.sort_by(|a, b| a.slug.cmp(&b.slug));
    }
    found
}

/// All the MCP servers the plugins add.
pub fn mcp_servers(plugins: &[Plugin]) -> Vec<ServerSpec> {
    plugins.iter().flat_map(|p| p.mcp.iter().cloned()).collect()
}

/// All the skill directories the plugins add.
pub fn skill_dirs(plugins: &[Plugin]) -> Vec<PathBuf> {
    plugins.iter().filter_map(|p| p.skills.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The shape people type most.** If `owner/repo` didn't work, they'd paste the address every time.
    #[test]
    fn a_bare_owner_slash_repo_means_github() {
        let (url, name) = source("attacca-cc/zyris").expect("받을 곳이 나와야 한다");
        assert_eq!(url, "https://github.com/attacca-cc/zyris.git");
        assert_eq!(name, "zyris");
    }

    /// Pasting the address as-is is more common. Whatever the tail, the same name must come out.
    #[test]
    fn every_github_url_shape_gives_the_same_name() {
        for text in [
            "https://github.com/attacca-cc/zyris",
            "https://github.com/attacca-cc/zyris.git",
            "https://github.com/attacca-cc/zyris/",
            "git@github.com:attacca-cc/zyris.git",
        ] {
            let (_, name) = source(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(name, "zyris", "{text}");
        }
    }

    /// **Local repos are accepted too.** This path is needed when testing while building a plugin.
    #[test]
    fn a_local_path_is_a_source_too() {
        let (url, name) = source("/tmp/내플러그인").expect("로컬 경로도 받아야 한다");
        assert_eq!(url, "/tmp/내플러그인");
        assert_eq!(name, "내플러그인");
    }

    /// Even if it isn't GitHub, anything clonable is accepted.
    #[test]
    fn another_host_is_taken_as_given() {
        let (url, name) = source("https://gitlab.com/someone/thing.git").unwrap();
        assert_eq!(url, "https://gitlab.com/someone/thing.git");
        assert_eq!(name, "thing");
    }

    /// **It must be a single path piece.** If `..` remains, it would write outside the plugin directory.
    #[test]
    fn a_name_can_never_climb_out_of_the_plugin_directory() {
        for bad in ["../../etc", "..", "a/../b", "  "] {
            let name = sanitize(bad);
            assert!(!name.contains(".."), "{bad} → {name}");
            assert!(!name.contains('/'), "{bad} → {name}");
        }
    }

    /// The destination. **No environment variables are shaken** — it's taken as an argument, so there's no reason to.
    fn scoped() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// The origin to fetch from. **It must be a real git repo** — a mock clone wouldn't exercise
    /// shallow cloning or `git pull`.
    fn origin(body: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("plugin.json"), body).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(d.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .expect("git");
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["add", "-A"]);
        run(&["commit", "-qm", "first"]);
        d
    }

    const MANIFEST: &str = r#"{"name":"깃허브","description":"이슈를 본다",
        "mcpServers":{"gh":{"command":"npx","args":["-y","x"]}}}"#;

    #[tokio::test]
    async fn installing_puts_the_plugin_where_discovery_finds_it() {
        let into = scoped();
        let into = into.path();
        let from = origin(MANIFEST);

        let got = install_into(into, &from.path().to_string_lossy()).await.expect("받아져야 한다");
        assert_eq!(got.name, "깃허브");
        assert_eq!(got.mcp.len(), 1);

        // **This is the real verdict** — on the next run it's read from this place.
        let found = discover_in(&[into.to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "깃허브");
    }

    /// Tells fetched and hand-placed apart — only the fetched side can be removed.
    #[test]
    fn only_what_was_fetched_counts_as_fetched() {
        let inside = Plugin {
            name: "받은것".into(),
            description: String::new(),
            mcp: Vec::new(),
            skills: None,
            root: install_dir().join("받은것"),
        };
        let outside = Plugin { root: PathBuf::from("/tmp/직접둔것"), ..inside.clone() };
        assert!(inside.fetched());
        assert!(!outside.fetched());
    }

    /// **Don't fetch just any repo.** Silently ignored, the person would believe it's installed.
    #[tokio::test]
    async fn a_repository_without_a_manifest_is_rejected_and_cleaned_up() {
        let into = scoped();
        let into = into.path();
        let from = tempfile::tempdir().unwrap();
        std::fs::write(from.path().join("README.md"), "플러그인 아님").unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(from.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["add", "-A"]);
        run(&["commit", "-qm", "first"]);

        let why = install_into(into, &from.path().to_string_lossy()).await.unwrap_err();
        assert!(why.contains("plugin.json"), "{why}");
        assert!(discover_in(&[into.to_path_buf()]).is_empty(), "받은 것이 남아 있다");
    }

    /// Overwriting would silently erase local edits.
    #[tokio::test]
    async fn installing_the_same_thing_twice_refuses() {
        let into = scoped();
        let into = into.path();
        let from = origin(MANIFEST);
        let at = from.path().to_string_lossy().into_owned();

        install_into(into, &at).await.unwrap();
        let why = install_into(into, &at).await.unwrap_err();
        assert!(why.contains("이미 있습니다"), "{why}");
    }

    /// An update must actually pull the new commit.
    #[tokio::test]
    async fn updating_pulls_the_new_commit() {
        let into = scoped();
        let into = into.path();
        let from = origin(MANIFEST);
        install_into(into, &from.path().to_string_lossy()).await.unwrap();

        std::fs::write(
            from.path().join("plugin.json"),
            r#"{"name":"깃허브","description":"바뀐 설명"}"#,
        )
        .unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-qm", "second"]] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(from.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
        }

        let done = update_in(into, None).await;
        assert_eq!(done.len(), 1);
        assert!(done[0].1.is_ok(), "{:?}", done[0].1);
        assert_eq!(discover_in(&[into.to_path_buf()])[0].description, "바뀐 설명");
    }

    /// **It must be removable by the name shown on screen.** That's what a person types.
    #[tokio::test]
    async fn removing_works_by_the_name_that_is_shown() {
        let into = scoped();
        let into = into.path();
        let from = origin(MANIFEST);
        install_into(into, &from.path().to_string_lossy()).await.unwrap();

        remove_from(into, "깃허브").expect("보이는 이름으로 지워져야 한다");
        assert!(discover_in(&[into.to_path_buf()]).is_empty());
    }

    /// Asked to remove something absent, it says so. A silent success would read as removed.
    #[test]
    fn removing_something_that_is_not_there_says_so() {
        let into = scoped();
        let into = into.path();
        assert!(remove_from(into, "없는것").is_err());
    }

    /// **Must not allow deleting someone else's directory.**
    #[test]
    fn a_climbing_name_cannot_remove_anything_outside() {
        let into = scoped();
        let victim = into.path().join("건드리면안됨");
        std::fs::create_dir_all(&victim).unwrap();
        let _ = remove_from(&into.path().join("plugins"), "../건드리면안됨");
        assert!(victim.exists(), "밖의 디렉터리가 지워졌다");
    }

    /// When it's unclear what to fetch, it must not pass silently.
    #[test]
    fn nonsense_is_not_a_source() {
        for bad in ["", "   ", "그냥 글자 여럿", "onlyone", "owner/repo/extra"] {
            assert_eq!(source(bad), None, "{bad:?}가 받을 곳으로 잡혔다");
        }
    }

    fn write_plugin(dir: &Path, slug: &str, manifest: &str) {
        let root = dir.join(slug);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("plugin.json"), manifest).unwrap();
    }

    /// The plugin's mcp config goes to the bridge as-is.
    #[test]
    fn a_plugin_contributes_its_mcp_servers() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "git", r#"{"name":"깃","mcp":{"gh":{"command":"npx"}}}"#);
        let found = discover_in(&[dir.path().to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "깃");
        assert_eq!(found[0].mcp.len(), 1);
        assert_eq!(found[0].mcp[0].slug, "gh");
        assert_eq!(found[0].mcp[0].command, "npx");
    }

    /// If `skills/` exists, it comes along too — a plugin adds more than just tools.
    #[test]
    fn a_plugin_contributes_its_skills_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "git", r#"{"name":"깃"}"#);
        std::fs::create_dir_all(dir.path().join("git/skills/리뷰")).unwrap();
        std::fs::write(dir.path().join("git/skills/리뷰/SKILL.md"), "---\nname: 리뷰\n---\n본문")
            .unwrap();

        let found = discover_in(&[dir.path().to_path_buf()]);
        let skills = crate::tools::skill::Skills::new(skill_dirs(&found));
        assert_eq!(skills.list()[0].name, "리뷰");
    }

    /// Without `skills/` it points at nothing. An empty path must not be handed along.
    #[test]
    fn a_plugin_without_skills_points_nowhere() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "git", r#"{"name":"깃"}"#);
        assert_eq!(discover_in(&[dir.path().to_path_buf()])[0].skills, None);
    }

    /// **If one breaks, the rest survive.** The app must not stop entirely.
    #[test]
    fn a_broken_plugin_does_not_hide_the_good_ones() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "broken", "{이건 JSON이 아니다");
        write_plugin(dir.path(), "good", r#"{"name":"멀쩡"}"#);
        let found = discover_in(&[dir.path().to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "멀쩡");
    }

    /// Without a name, the directory name is used — it must not vanish just because no name was written.
    #[test]
    fn a_plugin_without_a_name_uses_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "이름없음", "{}");
        assert_eq!(discover_in(&[dir.path().to_path_buf()])[0].name, "이름없음");
    }

    /// Having no plugin directory at all is normal.
    #[test]
    fn no_plugins_at_all_is_fine() {
        assert!(discover_in(&[PathBuf::from("/이런건/없다")]).is_empty());
    }
}
