//! 플러그인 — 도구(MCP 서버)와 스킬을 한 묶음으로 더한다.
//!
//! 새 실행 형식을 만들지 않는다. 플러그인이 하는 일은 **이미 있는 두 자리를 가리키는
//! 것**뿐이다: `mcp`는 작업 10의 설정 모양 그대로이고, `skills/`는 `tools::skill`이 읽는
//! 그 모양 그대로다. 그래서 플러그인을 지원하는 데 새 실행 경로가 없다.
//!
//! ```text
//! plugins/
//!   깃허브/
//!     plugin.json     { "name": "깃허브", "mcp": { "gh": { "command": "npx", … } } }
//!     skills/
//!       리뷰/SKILL.md
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::mcp::bridge::ServerSpec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    pub name: String,
    pub description: String,
    /// 이 플러그인이 얹는 MCP 서버들.
    pub mcp: Vec<ServerSpec>,
    /// 이 플러그인의 `skills/` 디렉터리. 없으면 `None`.
    pub skills: Option<PathBuf>,
    /// 어느 디렉터리에서 왔는가. **받아 온 것과 손으로 둔 것을 이걸로 가른다** —
    /// 이름만으로는 알 수 없고, 지울 수 있는 것은 받아 온 쪽뿐이다.
    pub root: PathBuf,
}

impl Plugin {
    /// `/plugin`으로 받아 온 것인가.
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

/// 파일에 적히는 모양. `ServerSpec`의 `slug`는 키에서 오므로 여기 없다.
#[derive(Debug, Deserialize)]
struct ServerSpecFile {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

/// 사람이 준 글에서 clone할 곳과 그것이 놓일 디렉터리 이름을 뽑는다.
///
/// **`git`으로 가져온다.** 새 의존이 없고, 코딩 도구를 쓰는 자리에 git이 없을 리 없고,
/// `/plugin update`가 `git pull` 한 줄로 따라온다. 받침이 필요한 것은 아카이브를 풀고
/// 갱신을 직접 짜는 쪽이다.
///
/// 받는 모양:
///
/// - `owner/repo` — 깃허브로 친다. 제일 많이 칠 모양이다
/// - `https://github.com/owner/repo`(`.git`·끝의 `/` 있어도 됨)
/// - `git@github.com:owner/repo.git`
/// - 그 밖의 `scheme://…` — 깃허브가 아니어도 clone할 수 있으면 받는다
/// - `/…`·`~/…`·`./…` — 로컬 리포. **플러그인을 만들면서 시험할 때 이 길이 필요하다**
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
        // `owner/repo` 지름길. 조각이 정확히 둘이어야 한다 — 경로를 준 것과 갈라야 한다.
        let parts: Vec<&str> = text.split('/').collect();
        if parts.len() != 2 || parts.iter().any(|p| p.is_empty()) {
            return None;
        }
        format!("https://github.com/{text}.git")
    };
    // 이름은 마지막 조각이다. `git@host:owner/repo`도 `/`로 갈리므로 같이 걸린다.
    let last = url.rsplit(['/', ':']).next()?.trim_end_matches(".git");
    let name = sanitize(last);
    (!name.is_empty()).then_some((url, name))
}

/// 디렉터리 이름 하나로 쓸 수 있게 씻는다.
///
/// **경로 조각 하나여야 한다.** `..`나 `/`가 남으면 플러그인 디렉터리 밖에 쓰게 된다.
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

/// 플러그인을 찾을 자리 둘. 뒤가 이긴다 — 프로젝트가 홈보다 구체적이다.
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

/// 받아 온 플러그인이 사는 곳. **홈 쪽 하나다** — 프로젝트에 남의 코드를 받아 두면
/// 그 리포의 커밋에 섞인다.
pub fn install_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".config/zyris-code/plugins"),
        None => std::env::temp_dir().join("zyris-code/plugins"),
    }
}

/// `git`을 한 번 돌린다. 실패하면 git이 한 말을 그대로 옮긴다 — 우리가 고쳐 쓰면
/// "그런 리포가 없다"가 "설치에 실패했습니다"로 뭉개진다.
async fn git(args: &[&str], at: Option<&Path>) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args);
    if let Some(at) = at {
        cmd.current_dir(at);
    }
    let out = cmd.output().await.map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "git이 없습니다. 플러그인은 git으로 받아 옵니다.".into(),
        _ => format!("git을 돌리지 못했습니다: {e}"),
    })?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    let why = String::from_utf8_lossy(&out.stderr);
    Err(why.lines().last().unwrap_or("git이 실패했습니다").trim().to_string())
}

/// 받아서 자리에 놓는다. 이미 있으면 받지 않는다 — 덮어쓰면 손댄 것이 조용히 사라진다.
///
/// **`plugin.json`이 없으면 되돌린다.** 아무 리포나 받아 두면 다음 실행 때 조용히 무시되고,
/// 사람은 설치가 된 줄 안다.
pub async fn install(text: &str) -> Result<Plugin, String> {
    install_into(&install_dir(), text).await
}

/// 받을 자리를 받는 판. **테스트가 이것을 쓴다** — 진짜 홈을 건드리지 않고,
/// 환경변수를 흔들지 않아도 된다.
pub async fn install_into(dir: &Path, text: &str) -> Result<Plugin, String> {
    let Some((url, name)) = source(text) else {
        return Err(format!(
            "`{text}`에서 받을 곳을 못 찾았습니다. `owner/repo`나 clone할 수 있는 주소를 주세요."
        ));
    };
    let at = dir.join(&name);
    if at.exists() {
        return Err(format!("`{name}`은 이미 있습니다. 갱신은 `/plugin update {name}`입니다."));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("플러그인 자리를 못 만들었습니다: {e}"))?;

    // 히스토리는 필요 없다. 얕게 받으면 큰 리포에서 몇 배 빠르다.
    git(&["clone", "--depth", "1", &url, &at.to_string_lossy()], None).await?;

    if !at.join("plugin.json").exists() {
        let _ = std::fs::remove_dir_all(&at);
        return Err(format!(
            "`{name}`에 plugin.json이 없어 플러그인이 아닙니다. 받은 것은 지웠습니다."
        ));
    }
    let wanted = manifest_name(&at, &name);
    discover_in(std::slice::from_ref(&dir.to_path_buf()))
        .into_iter()
        .find(|p| p.name == wanted)
        .ok_or_else(|| format!("`{name}`의 plugin.json을 읽지 못했습니다."))
}

/// 매니페스트가 말하는 이름. 없으면 디렉터리 이름이다 — `discover_in`과 같은 규칙이다.
fn manifest_name(at: &Path, slug: &str) -> String {
    let named = std::fs::read_to_string(at.join("plugin.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Manifest>(&t).ok())
        .map(|m| m.name)
        .filter(|n| !n.is_empty());
    named.unwrap_or_else(|| slug.to_string())
}

/// 받아 둔 것을 지운다. **받아 온 것만 지운다** — 손으로 만든 플러그인은 여기 없다.
pub fn remove(name: &str) -> Result<(), String> {
    remove_from(&install_dir(), name)
}

pub fn remove_from(dir: &Path, name: &str) -> Result<(), String> {
    let at = installed_path(dir, name)?;
    std::fs::remove_dir_all(&at).map_err(|e| format!("지우지 못했습니다: {e}"))
}

/// 하나, 또는 받아 둔 것 전부를 갱신한다. 실패한 것은 사유와 함께 돌려준다.
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

/// 받아 둔 플러그인 디렉터리 이름들.
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

/// 받아 둔 것 하나의 자리.
///
/// 디렉터리 이름으로도, 매니페스트가 말하는 이름으로도 찾는다 — 화면에 보이는 것은
/// 후자라 사람은 그것을 친다.
///
/// **이름을 씻어 쓴다.** `../`가 든 이름으로 남의 디렉터리를 지우게 두면 안 된다.
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
        .ok_or_else(|| format!("받아 둔 플러그인 중에 `{name}`이 없습니다."))
}

/// **하나가 망가져도 나머지는 산다.** 읽을 수 없는 플러그인은 로그만 남기고 넘어간다.
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
    // 이름 순서로 고정한다. 디렉터리 순서 그대로 두면 실행마다 announce가 달라진다.
    found.sort_by(|a, b| a.name.cmp(&b.name));
    for p in &mut found {
        p.mcp.sort_by(|a, b| a.slug.cmp(&b.slug));
    }
    found
}

/// 플러그인들이 얹는 MCP 서버 전부.
pub fn mcp_servers(plugins: &[Plugin]) -> Vec<ServerSpec> {
    plugins.iter().flat_map(|p| p.mcp.iter().cloned()).collect()
}

/// 플러그인들이 얹는 스킬 디렉터리 전부.
pub fn skill_dirs(plugins: &[Plugin]) -> Vec<PathBuf> {
    plugins.iter().filter_map(|p| p.skills.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **제일 많이 칠 모양이다.** `owner/repo`가 안 되면 매번 주소를 붙여넣어야 한다.
    #[test]
    fn a_bare_owner_slash_repo_means_github() {
        let (url, name) = source("attacca-cc/zyris").expect("받을 곳이 나와야 한다");
        assert_eq!(url, "https://github.com/attacca-cc/zyris.git");
        assert_eq!(name, "zyris");
    }

    /// 주소를 그대로 붙여넣는 쪽이 더 흔하다. 꼬리가 어떻든 같은 이름이 나와야 한다.
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

    /// **로컬 리포도 받는다.** 플러그인을 만들면서 시험할 때 이 길이 필요하다.
    #[test]
    fn a_local_path_is_a_source_too() {
        let (url, name) = source("/tmp/내플러그인").expect("로컬 경로도 받아야 한다");
        assert_eq!(url, "/tmp/내플러그인");
        assert_eq!(name, "내플러그인");
    }

    /// 깃허브가 아니어도 clone할 수 있으면 받는다.
    #[test]
    fn another_host_is_taken_as_given() {
        let (url, name) = source("https://gitlab.com/someone/thing.git").unwrap();
        assert_eq!(url, "https://gitlab.com/someone/thing.git");
        assert_eq!(name, "thing");
    }

    /// **경로 조각 하나여야 한다.** `..`가 남으면 플러그인 디렉터리 밖에 쓰게 된다.
    #[test]
    fn a_name_can_never_climb_out_of_the_plugin_directory() {
        for bad in ["../../etc", "..", "a/../b", "  "] {
            let name = sanitize(bad);
            assert!(!name.contains(".."), "{bad} → {name}");
            assert!(!name.contains('/'), "{bad} → {name}");
        }
    }

    /// 받을 자리. **환경변수를 흔들지 않는다** — 인자로 받으므로 그럴 이유가 없다.
    fn scoped() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// 받아 올 원본. **진짜 git 리포여야 한다** — 모의 clone으로는 얕은 복제도
    /// `git pull`도 확인되지 않는다.
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

        // **진짜 판정은 이것이다** — 다음 실행 때 이 자리에서 읽힌다.
        let found = discover_in(&[into.to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "깃허브");
    }

    /// 받아 온 것과 손으로 둔 것을 가른다 — 지울 수 있는 것은 받아 온 쪽뿐이다.
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

    /// **아무 리포나 받아 두면 안 된다.** 조용히 무시되면 사람은 설치가 된 줄 안다.
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

    /// 덮어쓰면 손댄 것이 조용히 사라진다.
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

    /// 갱신은 새 커밋을 실제로 가져와야 한다.
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

    /// **화면에 보이는 이름으로 지울 수 있어야 한다.** 사람은 그것을 친다.
    #[tokio::test]
    async fn removing_works_by_the_name_that_is_shown() {
        let into = scoped();
        let into = into.path();
        let from = origin(MANIFEST);
        install_into(into, &from.path().to_string_lossy()).await.unwrap();

        remove_from(into, "깃허브").expect("보이는 이름으로 지워져야 한다");
        assert!(discover_in(&[into.to_path_buf()]).is_empty());
    }

    /// 없는 것을 지우라고 하면 그렇게 말한다. 조용히 성공하면 지운 줄 안다.
    #[test]
    fn removing_something_that_is_not_there_says_so() {
        let into = scoped();
        let into = into.path();
        assert!(remove_from(into, "없는것").is_err());
    }

    /// **남의 디렉터리를 지우게 두면 안 된다.**
    #[test]
    fn a_climbing_name_cannot_remove_anything_outside() {
        let into = scoped();
        let victim = into.path().join("건드리면안됨");
        std::fs::create_dir_all(&victim).unwrap();
        let _ = remove_from(&into.path().join("plugins"), "../건드리면안됨");
        assert!(victim.exists(), "밖의 디렉터리가 지워졌다");
    }

    /// 무엇을 받으라는 건지 모를 때 조용히 넘어가면 안 된다.
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

    /// 플러그인의 mcp 설정이 그대로 브리지로 간다.
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

    /// `skills/`가 있으면 그것도 딸려 온다 — 플러그인이 더하는 것은 도구만이 아니다.
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

    /// `skills/`가 없으면 아무것도 가리키지 않는다. 빈 경로를 들려 보내면 안 된다.
    #[test]
    fn a_plugin_without_skills_points_nowhere() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "git", r#"{"name":"깃"}"#);
        assert_eq!(discover_in(&[dir.path().to_path_buf()])[0].skills, None);
    }

    /// **하나가 망가져도 나머지는 산다.** 앱이 통째로 멈추면 안 된다.
    #[test]
    fn a_broken_plugin_does_not_hide_the_good_ones() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "broken", "{이건 JSON이 아니다");
        write_plugin(dir.path(), "good", r#"{"name":"멀쩡"}"#);
        let found = discover_in(&[dir.path().to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "멀쩡");
    }

    /// 이름이 없으면 디렉터리 이름을 쓴다 — 이름을 안 적었다고 사라지면 안 된다.
    #[test]
    fn a_plugin_without_a_name_uses_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "이름없음", "{}");
        assert_eq!(discover_in(&[dir.path().to_path_buf()])[0].name, "이름없음");
    }

    /// 플러그인 디렉터리가 아예 없는 것이 정상이다.
    #[test]
    fn no_plugins_at_all_is_fine() {
        assert!(discover_in(&[PathBuf::from("/이런건/없다")]).is_empty());
    }
}
