//! Plugins — tools, skills, commands, agents and hooks as one bundle.
//!
//! **The layout is not ours.** Plugins in the wild are written for Claude Code, and the ones people
//! actually want — superpowers, frontend-design, the official marketplace — all follow its shape.
//! Inventing a second layout would mean every plugin needed a port before it could be used here, so
//! this reads theirs:
//!
//! ```text
//! plugins/
//!   frontend-design/
//!     .claude-plugin/plugin.json   the manifest. `plugin.json` at the root is read too
//!     .mcp.json                    servers, same shape as everywhere else
//!     commands/*.md                slash commands — a prompt with frontmatter
//!     skills/<name>/SKILL.md       procedures loaded on demand
//!     agents/*.md                  agent definitions, read as skills (see `agent_dirs`)
//!     hooks/hooks.json             commands to run around a tool call
//! ```
//!
//! Every part is optional, and a plugin carrying only one of them is ordinary — most carry skills
//! and nothing else.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::mcp::bridge::ServerSpec;

const GIT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    pub name: String,
    pub description: String,
    /// The MCP servers this plugin adds.
    pub mcp: Vec<ServerSpec>,
    /// This plugin's `skills/` directory. `None` when absent.
    pub skills: Option<PathBuf>,
    /// Its `agents/` directory. **Read as skills**, because that is honestly what they are here:
    /// an agent file is a description and a prompt, and which agent answers a session is attacca's
    /// to decide (`ZNewSession.agent_id`), not a local file's. Loaded on demand, the prompt is
    /// exactly the procedure the plugin author wrote.
    pub agents: Option<PathBuf>,
    /// The slash commands it adds, already read (`commands/*.md`).
    pub commands: Vec<PluginCommand>,
    /// What to run around a tool call (`hooks/hooks.json`), if anything.
    pub hooks: Vec<crate::hooks::Hook>,
    /// Which directory it came from. **This is what tells fetched and hand-placed apart** — the
    /// name alone can't, and only the fetched side can be removed.
    pub root: PathBuf,
    /// What the manifest says about itself besides its name: version, who wrote it, where it lives.
    pub about: About,
}

/// What a manifest says about itself besides its name and its parts.
///
/// **Read for the screen, not for behaviour.** None of these changes what loads — they are what
/// answers "what is this, and whose is it?" in the plugin panel, which used to show a name and a
/// sentence and nothing else. Every field is optional because no plugin in the wild carries all of
/// them, and a missing one must not stop the plugin from being read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct About {
    pub version: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub keywords: Vec<String>,
}

impl Plugin {
    /// Whether it was fetched via `/plugin`.
    pub fn fetched(&self) -> bool {
        self.root.starts_with(install_dir())
    }
}

/// One slash command a plugin adds.
///
/// **A command is a prompt, not a program.** That is what it is in Claude Code, and it is the only
/// reading that makes sense here: this app has no way to run someone's arbitrary code on a
/// keystroke, and would not want one. Typing `/review` sends the body as the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommand {
    /// What is typed, without the slash. **Namespaced when it would collide** — see `commands_in`.
    pub name: String,
    /// The one-line description, from the frontmatter or the first heading.
    pub description: String,
    /// The prompt body, frontmatter stripped.
    pub prompt: String,
    /// Which plugin it came from, so `/help` can say.
    pub plugin: String,
}

/// Reads `commands/*.md` — a Markdown file with optional YAML frontmatter.
///
/// **Only `description` is taken from the frontmatter.** The rest of what Claude Code puts there
/// (`allowed-tools`, `model`, `argument-hint`) describes a harness this app does not have, and
/// pretending to honour it would be worse than plainly not.
fn commands_in(dir: &Path, plugin: &str) -> Vec<PluginCommand> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Some(name) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else {
            continue;
        };
        let (front, body) = split_frontmatter(&text);
        let description = front_field(front, "description")
            .or_else(|| {
                body.lines()
                    .find(|l| l.starts_with("# "))
                    .map(|l| l.trim_start_matches("# ").trim().to_string())
            })
            .unwrap_or_default();
        out.push(PluginCommand {
            name,
            description,
            prompt: body.trim().to_string(),
            plugin: plugin.to_string(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Splits `---` frontmatter off the front. Returns `(frontmatter, body)`; no frontmatter gives an
/// empty first half and the whole text as the body.
///
/// **A file that opens with `---` but never closes it is all body.** Treating the rest as
/// frontmatter would silently swallow the entire prompt.
fn split_frontmatter(text: &str) -> (&str, &str) {
    let rest = match text.strip_prefix("---\n") {
        Some(rest) => rest,
        None => return ("", text),
    };
    match rest.find("\n---") {
        Some(at) => {
            let body = &rest[at + 4..];
            (&rest[..at], body.strip_prefix('\n').unwrap_or(body))
        }
        None => ("", text),
    }
}

/// One `key: value` out of frontmatter. Quotes are stripped; anything fancier is not read, because
/// nothing here needs it.
fn front_field(front: &str, key: &str) -> Option<String> {
    front.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        (k.trim() == key).then(|| v.trim().trim_matches(['"', '\'']).to_string())
    })
}

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    author: Option<Author>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    /// **Read through the same parser the config files use** (`mcp::bridge::SpecFile`), so a
    /// plugin can point at a remote server exactly the way a config file does.
    #[serde(default, rename = "mcpServers", alias = "mcp")]
    mcp: HashMap<String, crate::mcp::bridge::SpecFile>,
}

/// `author` is written both ways in the wild — a bare name, or an object with one in it.
///
/// **Untagged, so neither shape is an error.** A manifest that failed to parse is dropped whole
/// (`discover_in`), so refusing the object shape would silently lose the plugin over a field that
/// nothing here acts on.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Author {
    Named(String),
    Object { name: Option<String> },
}

impl Author {
    fn name(self) -> Option<String> {
        match self {
            Author::Named(name) => (!name.trim().is_empty()).then_some(name),
            Author::Object { name } => name.filter(|n| !n.trim().is_empty()),
        }
    }
}

/// Pulls the place to clone and the directory name it lands in out of what the person typed.
///
/// **It's fetched with `git`.** No new dependency ‒ a place using coding tools can't lack git, and
/// `/plugin update` comes down to one `git pull` line. It's the archive path that needs scaffolding:
/// unpacking and writing the update logic yourself.
///
/// Accepted shapes:
///
/// - `owner/repo` ‒ treated as GitHub. The shape people type most
/// - `https://github.com/owner/repo` (`.git` or trailing `/` allowed)
/// - `git@github.com:owner/repo.git`
/// - any other `scheme://…` ‒ accepted if it can be cloned even when it isn't GitHub
/// - `/…`∙`~/…`∙`./…` ‒ a local repo. **This path is needed when testing while building a plugin**
pub fn source(text: &str) -> Option<(String, String)> {
    let text = text.trim().trim_end_matches('/');
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let url = if text.contains("://") || text.starts_with("git@") {
        text.to_string()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home().join(rest).to_string_lossy().into_owned()
    } else if is_local_path(text) {
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

/// Is this a local filesystem path rather than a remote or an `owner/repo`?
///
/// The Unix shapes (`/…`, `./…`, `../…`, `~/…`) were always accepted. Windows adds a
/// drive-letter absolute path (`C:\…`, `C:/…`) and backslash-relative shapes (`.\…`, `..\…`).
/// Without those, `/plugin add C:\path\to\plugin` was refused on Windows.
fn is_local_path(text: &str) -> bool {
    text.starts_with('/')
        || text.starts_with("./")
        || text.starts_with("../")
        || text.starts_with(".\\")
        || text.starts_with("..\\")
        || is_drive_absolute(text)
}

/// `C:\…` or `C:/…` — a Windows drive-letter absolute path.
fn is_drive_absolute(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
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

/// Where `~/…` points. **One definition for the whole app** (`conn::user_home`) — Windows has no
/// `$HOME`, and expanding to `/` there turned every `~/…` plugin path into a drive-relative `\…`.
fn home() -> PathBuf {
    crate::conn::user_home().unwrap_or_else(|| PathBuf::from("/"))
}

/// The two places to look for plugins. The latter wins — the project is more specific than home.
///
/// **The user-level one is `conn::app_dir`**, the same directory the credentials and settings use.
/// Joining `$HOME/.config/…` here instead meant this tier did not exist at all on Windows.
pub fn plugin_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = crate::conn::app_dir() {
        out.push(dir.join("plugins"));
    }
    out.push(cwd.join(".zyris-code/plugins"));
    out
}

pub fn discover(cwd: &Path) -> Vec<Plugin> {
    discover_in(&plugin_dirs(cwd))
}

/// Plugins whose contributions may be used in this process. User-level plugins were installed or
/// placed in this app's own directory; repository plugins need an exact approval first.
pub fn active(cwd: &Path) -> Vec<Plugin> {
    let dirs = plugin_dirs(cwd);
    let project_dir = cwd.join(".zyris-code/plugins");
    let user_dirs: Vec<PathBuf> = dirs.into_iter().filter(|dir| *dir != project_dir).collect();
    let mut found = discover_in(&user_dirs);
    let allowed = crate::mcp::discovery::Allowed::load();
    for plugin in active_project(cwd, &discover_in(&[project_dir]), &allowed) {
        match found.iter_mut().find(|p| p.name == plugin.name) {
            Some(slot) => *slot = plugin,
            None => found.push(plugin),
        }
    }
    // **A plugin can be switched off without being thrown away.** Until this, the only way to stop
    // a fetched plugin contributing was to delete its directory — which loses the `git` clone
    // `update` needs, and the edits somebody made in it.
    found.retain(|plugin| !allowed.plugin_off(&plugin.name));
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

pub fn active_project(
    cwd: &Path,
    plugins: &[Plugin],
    allowed: &crate::mcp::discovery::Allowed,
) -> Vec<Plugin> {
    plugins.iter().filter(|p| allowed.allows_plugin(cwd, p)).cloned().collect()
}

pub fn enabled(cwd: &Path, plugin: &Plugin, allowed: &crate::mcp::discovery::Allowed) -> bool {
    !plugin.root.starts_with(cwd.join(".zyris-code/plugins")) || allowed.allows_plugin(cwd, plugin)
}

/// The complete contribution is approved as one unit. Hashing its files is both smaller and safer
/// than trying to parse shell commands to guess which hook scripts they might reach.
pub fn fingerprint(plugin: &Plugin) -> String {
    fn visit(at: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(at) else { return };
        for entry in entries.flatten() {
            if entry.file_name() == ".git" {
                continue;
            }
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => visit(&path, files),
                Ok(_) => files.push(path),
                Err(_) => files.push(path),
            }
        }
    }

    let mut files = Vec::new();
    visit(&plugin.root, &mut files);
    files.sort();
    let mut hash = Sha256::new();
    for path in files {
        let relative = path.strip_prefix(&plugin.root).unwrap_or(&path);
        hash.update(relative.to_string_lossy().as_bytes());
        hash.update(b"\0");
        if let Ok(target) = std::fs::read_link(&path) {
            hash.update(b"link\0");
            hash.update(target.to_string_lossy().as_bytes());
        } else if let Ok(bytes) = std::fs::read(&path) {
            hash.update(&bytes);
        } else {
            hash.update(b"unreadable");
        }
        hash.update(b"\0");
    }
    format!("{:x}", hash.finalize())
}

/// Where fetched plugins live. **Only the home side** — fetching someone else's code into a project
/// would mix it into that repo's commits.
pub fn install_dir() -> PathBuf {
    // **The same entry `plugin_dirs` reads.** These two used to be computed apart, and only this
    // one had a fallback — so where the fallback fired, `/plugin add` reported success and the
    // next launch found nothing.
    match crate::conn::app_dir() {
        Some(dir) => dir.join("plugins"),
        None => std::env::temp_dir().join("zyris-code/plugins"),
    }
}

/// Runs `git` once. On failure it passes git's words through verbatim — if we rewrote them,
/// "no such repo" would get crushed into "installation failed".
async fn git(args: &[&str], at: Option<&Path>) -> Result<String, String> {
    git_with_timeout(args, at, GIT_TIMEOUT).await
}

async fn git_with_timeout(
    args: &[&str],
    at: Option<&Path>,
    within: Duration,
) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args);
    if let Some(at) = at {
        cmd.current_dir(at);
    }
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(within, cmd.output())
        .await
        .map_err(|_| format!("git timed out after {within:?}"))?
        .map_err(|e| match e.kind() {
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

    // **The manifest is wherever the plugin keeps it.** Looking only for a root `plugin.json` here
    // meant every real plugin was cloned, judged manifest-less, and deleted again — with the
    // manifest sitting in `.claude-plugin/` the whole time.
    if manifest_text(&at).is_none() {
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
    let named = manifest_text(at)
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
        .filter(|e| manifest_text(&e.path()).is_some())
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
        if manifest_text(&at).is_some() {
            return Ok(at);
        }
    }
    discover_in(std::slice::from_ref(&dir.to_path_buf()))
        .into_iter()
        .find(|p| p.name == name)
        .map(|p| p.root)
        .ok_or_else(|| crate::lang::current().plugin_not_found(name))
}

/// The manifest, wherever it is kept. **`.claude-plugin/plugin.json` first** — that is where every
/// plugin in the wild has it, and a root `plugin.json` beside it is usually a leftover.
fn manifest_text(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join(".claude-plugin/plugin.json"))
        .or_else(|_| std::fs::read_to_string(root.join("plugin.json")))
        .ok()
}

/// **If one breaks, the rest survive.** An unreadable plugin is skipped with a log entry left behind.
pub fn discover_in(dirs: &[PathBuf]) -> Vec<Plugin> {
    let mut found: Vec<Plugin> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let root = entry.path();
            // **Both manifest places.** `.claude-plugin/plugin.json` is where every plugin in the
            // wild keeps it; the bare `plugin.json` at the root is what this app asked for first
            // and still honours.
            let text = manifest_text(&root);
            let Some(text) = text else { continue };
            let manifest: Manifest = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("could not read the plugin config ({}): {e}", root.display());
                    continue;
                }
            };
            let slug = entry.file_name().to_string_lossy().to_string();
            let name = if manifest.name.is_empty() { slug } else { manifest.name };
            let skills = root.join("skills");
            let agents = root.join("agents");
            // Servers can be in the manifest or in a `.mcp.json` beside it. **The manifest wins**,
            // being the more specific of the two.
            let mut mcp: Vec<ServerSpec> = crate::mcp::bridge::merge_configs(
                std::fs::read_to_string(root.join(".mcp.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .into_iter()
                    .collect(),
            );
            for (slug, spec) in manifest.mcp {
                let Some(transport) = spec.into_transport() else { continue };
                let spec = ServerSpec { slug: slug.clone(), transport };
                match mcp.iter_mut().find(|s| s.slug == slug) {
                    Some(slot) => *slot = spec,
                    None => mcp.push(spec),
                }
            }
            let plugin = Plugin {
                mcp,
                skills: skills.is_dir().then_some(skills),
                agents: agents.is_dir().then_some(agents),
                commands: commands_in(&root.join("commands"), &name),
                hooks: crate::hooks::read(&root, &name),
                description: manifest.description,
                about: About {
                    version: manifest.version,
                    author: manifest.author.and_then(Author::name),
                    homepage: manifest.homepage,
                    repository: manifest.repository,
                    license: manifest.license,
                    keywords: manifest.keywords,
                },
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

/// All the skill directories the plugins add — **`agents/` among them.**
///
/// An agent file is a description and a prompt, which is what a skill is here. Which agent answers
/// a session is attacca's to decide (`ZNewSession.agent_id`) and no local file can change it, so
/// the honest thing to do with a plugin's agents is to let the model load them when they are
/// wanted rather than drop them on the floor.
pub fn skill_dirs(plugins: &[Plugin]) -> Vec<PathBuf> {
    plugins.iter().flat_map(|p| [p.skills.clone(), p.agents.clone()]).flatten().collect()
}

/// Every slash command the plugins add, with collisions namespaced.
///
/// **Two plugins may both offer `/review`.** Numbering them would be unreadable, so the loser is
/// prefixed with its plugin — `/frontend-design:review` — which is also how Claude Code writes it.
/// A built-in name always wins: `/help` must stay `/help`.
pub fn commands(plugins: &[Plugin]) -> Vec<PluginCommand> {
    let builtin = crate::command::builtin_names();
    let mut out: Vec<PluginCommand> = Vec::new();
    for plugin in plugins {
        for command in &plugin.commands {
            let taken = builtin.contains(&command.name.as_str())
                || out.iter().any(|c| c.name == command.name);
            let mut command = command.clone();
            if taken {
                command.name = format!("{}:{}", plugin.name, command.name);
            }
            if out.iter().any(|c| c.name == command.name) {
                continue;
            }
            out.push(command);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Everything the plugins want run around a tool call.
pub fn hooks(plugins: &[Plugin]) -> Vec<crate::hooks::Hook> {
    plugins.iter().flat_map(|p| p.hooks.iter().cloned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The shape people type most.** If `owner/repo` didn't work, they'd paste the address every time.
    #[test]
    fn a_bare_owner_slash_repo_means_github() {
        let (url, name) =
            source("attacca-cc/zyris").expect("the place to clone into must be reported");
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
        let (url, name) = source("/tmp/내플러그인").expect("a local path must be accepted too");
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
    fn origin_at(manifest_path: &str, body: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let manifest = d.path().join(manifest_path);
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(manifest, body).unwrap();
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

    fn origin(body: &str) -> tempfile::TempDir {
        origin_at("plugin.json", body)
    }

    const MANIFEST: &str = r#"{"name":"깃허브","description":"이슈를 본다",
        "mcpServers":{"gh":{"command":"npx","args":["-y","x"]}}}"#;

    /// Lays out a plugin the way the ones in the wild are actually laid out.
    fn real_plugin(root: &Path) {
        let put = |at: &str, body: &str| {
            let path = root.join(at);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        };
        put(".claude-plugin/plugin.json", r#"{"name":"superpowers","description":"d"}"#);
        put(".mcp.json", r#"{"mcpServers":{"remote":{"url":"https://x.test/mcp"}}}"#);
        put(
            "commands/brainstorm.md",
            "---\ndescription: Think it through\nallowed-tools: Bash\n---\n\nDo the thing.\n",
        );
        put("skills/one/SKILL.md", "# one");
        put("agents/reviewer.md", "---\nname: reviewer\n---\nReview it.");
        put(
            "hooks/hooks.json",
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"true"}]}]}}"#,
        );
    }

    /// **The layout is Claude Code's, because that is what plugins are written for.** Reading only
    /// a root `plugin.json` with `mcp` and `skills/` meant every real plugin came up empty — the
    /// manifest is not even in the place this app used to look.
    #[test]
    fn a_plugin_written_the_way_real_ones_are_is_read_whole() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("superpowers");
        std::fs::create_dir_all(&root).unwrap();
        real_plugin(&root);

        let found = discover_in(&[dir.path().to_path_buf()]);
        assert_eq!(found.len(), 1, "{found:?}");
        let p = &found[0];
        assert_eq!(p.name, "superpowers");
        assert!(p.skills.is_some(), "skills/ was missed");
        assert!(p.agents.is_some(), "agents/ was missed");
        assert_eq!(p.commands.len(), 1, "{:?}", p.commands);
        assert_eq!(p.commands[0].name, "brainstorm");
        assert_eq!(p.commands[0].description, "Think it through");
        assert_eq!(p.commands[0].prompt, "Do the thing.", "the frontmatter leaked into the prompt");
        assert_eq!(p.mcp.len(), 1, "a bare .mcp.json was missed: {:?}", p.mcp);
        assert_eq!(p.hooks.len(), 1, "{:?}", p.hooks);
    }

    /// **An agent file is a description and a prompt**, which is what a skill is here. Which agent
    /// answers a session is attacca's to decide, so the honest thing is to let the model load them.
    #[test]
    fn a_plugins_agents_are_reachable_as_skills() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        std::fs::create_dir_all(&root).unwrap();
        real_plugin(&root);
        let dirs = skill_dirs(&discover_in(&[dir.path().to_path_buf()]));
        assert!(dirs.iter().any(|d| d.ends_with("agents")), "{dirs:?}");
        assert!(dirs.iter().any(|d| d.ends_with("skills")), "{dirs:?}");
    }

    /// **A plugin may not take a built-in name.** `/help` has to stay `/help`; the plugin's own is
    /// reachable under its plugin instead.
    #[test]
    fn a_command_that_would_shadow_a_built_in_is_namespaced() {
        let plugin = |name: &str, command: &str| Plugin {
            name: name.into(),
            description: String::new(),
            mcp: vec![],
            skills: None,
            agents: None,
            commands: vec![PluginCommand {
                name: command.into(),
                description: String::new(),
                prompt: "p".into(),
                plugin: name.into(),
            }],
            hooks: vec![],
            about: About::default(),
            root: "/tmp".into(),
        };
        let got = commands(&[plugin("a", "help"), plugin("b", "review"), plugin("c", "review")]);
        let names: Vec<&str> = got.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"a:help"), "a built-in was shadowed: {names:?}");
        assert!(!names.contains(&"help"), "{names:?}");
        // The first one to claim a free name keeps it; the second is namespaced.
        assert!(names.contains(&"review"), "{names:?}");
        assert!(names.contains(&"c:review"), "{names:?}");
    }

    /// A file that opens with `---` and never closes it is all body. **Swallowing the rest as
    /// frontmatter would lose the whole prompt.**
    #[test]
    fn an_unclosed_frontmatter_is_not_taken_as_frontmatter() {
        assert_eq!(split_frontmatter("---\nname: x\nbody here"), ("", "---\nname: x\nbody here"));
        assert_eq!(split_frontmatter("no front"), ("", "no front"));
        assert_eq!(split_frontmatter("---\na: 1\n---\nbody"), ("a: 1", "body"));
    }

    #[tokio::test]
    async fn installing_puts_the_plugin_where_discovery_finds_it() {
        let into = scoped();
        let into = into.path();
        let from = origin(MANIFEST);

        let got =
            install_into(into, &from.path().to_string_lossy()).await.expect("it must be accepted");
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
            agents: None,
            commands: Vec::new(),
            hooks: Vec::new(),
            about: About::default(),
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
        assert!(discover_in(&[into.to_path_buf()]).is_empty(), "what was fetched is still there");
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
        // Asserted against the current language — a Korean literal here only passed because
        // another test had set the global language first.
        let name = into.read_dir().unwrap().next().unwrap().unwrap().file_name();
        assert_eq!(why, crate::lang::current().plugin_already_there(&name.to_string_lossy()));
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

    #[tokio::test]
    async fn bulk_update_finds_claude_plugin_manifest() {
        let into = scoped();
        let from = origin_at(".claude-plugin/plugin.json", MANIFEST);
        install_into(into.path(), &from.path().to_string_lossy()).await.unwrap();

        let done = update_in(into.path(), None).await;
        assert_eq!(done.len(), 1, "canonical plugin was omitted from bulk update: {done:?}");
        assert!(done[0].1.is_ok(), "canonical plugin was found but not updated: {done:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hung_git_returns_timeout() {
        let started = std::time::Instant::now();
        let why = git_with_timeout(
            &["-c", "alias.hang=!sleep 60", "hang"],
            None,
            std::time::Duration::from_millis(100),
        )
        .await
        .unwrap_err();

        assert!(why.contains("timed out"), "{why}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    /// **It must be removable by the name shown on screen.** That's what a person types.
    #[tokio::test]
    async fn removing_works_by_the_name_that_is_shown() {
        let into = scoped();
        let into = into.path();
        let from = origin(MANIFEST);
        install_into(into, &from.path().to_string_lossy()).await.unwrap();

        remove_from(into, "깃허브").expect("it must be removable by the name shown");
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
        assert!(victim.exists(), "a directory outside was deleted");
    }

    /// When it's unclear what to fetch, it must not pass silently.
    #[test]
    fn nonsense_is_not_a_source() {
        for bad in ["", "   ", "그냥 글자 여럿", "onlyone", "owner/repo/extra"] {
            assert_eq!(source(bad), None, "{bad:?} was taken as a place to clone into");
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
        assert_eq!(found[0].mcp[0].transport.summary(), "npx");
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

    #[test]
    fn project_plugin_is_disabled_until_approved() {
        let dir = tempfile::tempdir().unwrap();
        let plugins_dir = dir.path().join(".zyris-code/plugins");
        write_plugin(&plugins_dir, "local", r#"{"name":"local"}"#);
        let plugins = discover_in(&[plugins_dir]);
        let mut allowed = crate::mcp::discovery::Allowed::default();

        assert!(active_project(dir.path(), &plugins, &allowed).is_empty());
        assert!(allowed.set_plugin(dir.path(), &plugins[0], true));
        assert_eq!(active_project(dir.path(), &plugins, &allowed).len(), 1);

        std::fs::write(
            dir.path().join(".zyris-code/plugins/local/plugin.json"),
            r#"{"name":"local","description":"changed"}"#,
        )
        .unwrap();
        let changed = discover_in(&[dir.path().join(".zyris-code/plugins")]);
        assert!(active_project(dir.path(), &changed, &allowed).is_empty());
    }
}
