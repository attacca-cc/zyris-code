//! Skills placed on this machine — procedures the agent reads and follows when needed.
//!
//! **The point is to keep the list separate from the bodies.** Names and descriptions go once into the session preamble;
//! bodies go only when `skill.load` is called. Loading them all would make procedures we never use
//! eat context every turn.
//!
//! It only reads, so the approval gate lets it through (`gate::decide`) — my files on my computer,
//! and that is what they are there for.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::WireError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillInfo {
    /// The name to pass to `skill.load`.
    pub name: String,
    /// One line on when to use it. This is all there is to choose from, so read it as the
    /// whole contract.
    pub description: String,
}

#[zyris::capability(name = "skill", version = 1)]
pub trait Skill {
    /// Every skill available on this machine, with its name and description.
    async fn list(&self) -> zyris::Result<Vec<SkillInfo>>;

    /// The body of one skill. Read it as an instruction to follow that procedure.
    async fn load(&self, name: String) -> zyris::Result<String>;
}

/// Skills found ahead of time. **Scan once and remember** — no reason to scan the disk on every call.
#[derive(Debug, Clone, Default)]
pub struct Skills {
    found: Vec<(SkillInfo, PathBuf)>,
}

impl Skills {
    /// Finds `<slug>/SKILL.md` in the given directories. Missing directories are simply skipped.
    ///
    /// Later directories win — the project is more specific than home.
    pub fn new(dirs: Vec<PathBuf>) -> Skills {
        let mut found: Vec<(SkillInfo, PathBuf)> = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let file = entry.path().join("SKILL.md");
                let Ok(body) = std::fs::read_to_string(&file) else { continue };
                let slug = entry.file_name().to_string_lossy().to_string();
                let info = front_matter(&body, &slug);
                match found.iter_mut().find(|(i, _)| i.name == info.name) {
                    Some(slot) => *slot = (info, file),
                    None => found.push((info, file)),
                }
            }
        }
        found.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        Skills { found }
    }

    /// The two default locations. Later wins.
    ///
    /// **The user tier is `conn::app_dir`** — the same directory the credentials and settings use,
    /// and the same one `tools::announce` assembles. Building `$HOME/.config/…` by hand left this
    /// tier missing entirely on Windows.
    pub fn discover(cwd: &std::path::Path) -> Skills {
        let mut dirs = Vec::new();
        if let Some(dir) = crate::conn::app_dir() {
            dirs.push(dir.join("skills"));
        }
        dirs.push(cwd.join(".zyris-code/skills"));
        Skills::new(dirs)
    }

    pub fn list(&self) -> Vec<SkillInfo> {
        self.found.iter().map(|(i, _)| i.clone()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.found.is_empty()
    }

    /// The body. For an unknown name, **say what is there** — "none" alone does not say what
    /// to call next.
    pub fn load(&self, name: &str) -> Result<String, WireError> {
        match self.found.iter().find(|(i, _)| i.name == name) {
            Some((_, path)) => std::fs::read_to_string(path)
                .map(|body| strip_front_matter(&body).to_string())
                .map_err(|e| WireError::internal(format!("스킬을 읽지 못했습니다: {e}"))),
            None => Err(WireError::invalid_params(format!(
                "'{name}' 스킬이 없습니다. 있는 것: {}",
                self.found.iter().map(|(i, _)| i.name.as_str()).collect::<Vec<_>>().join(", ")
            ))),
        }
    }
}

#[async_trait::async_trait]
impl Skill for Skills {
    async fn list(&self) -> zyris::Result<Vec<SkillInfo>> {
        Ok(Skills::list(self))
    }

    async fn load(&self, name: String) -> zyris::Result<String> {
        Skills::load(self, &name)
    }
}

/// The skill list to load when creating a session.
///
/// **Only names and descriptions.** Loading bodies would make procedures we never use eat context every turn.
/// Bodies go when `skill.load` is called.
///
/// `None` when there are no skills — a list with nothing in it would only take up space.
pub fn preamble(skills: &Skills) -> Option<String> {
    let list = skills.list();
    if list.is_empty() {
        return None;
    }
    let mut out = String::from(
        "이 노드에는 아래 스킬이 있습니다. 해당하는 일이면 이름이 '__skill__load'로 끝나는 \
         도구로 본문을 읽고 그 절차를 따르세요.\n\n",
    );
    for s in list {
        out.push_str(&format!("- {} — {}\n", s.name, s.description));
    }
    Some(out)
}

/// Reads `name` and `description` from the front matter wrapped in `---`.
///
/// Falls back to the directory name — **a skill must not vanish just because no front matter was written.**
fn front_matter(body: &str, slug: &str) -> SkillInfo {
    let mut info = SkillInfo { name: slug.to_string(), description: String::new() };
    for line in head_lines(body) {
        if let Some(v) = line.strip_prefix("name:") {
            info.name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("description:") {
            info.description = v.trim().to_string();
        }
    }
    if info.name.is_empty() {
        info.name = slug.to_string();
    }
    info
}

fn head_lines(body: &str) -> Vec<&str> {
    let mut lines = body.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Vec::new();
    }
    lines.take_while(|l| l.trim() != "---").collect()
}

/// Just the body. Front matter is metadata for humans, not a procedure for the model to follow.
fn strip_front_matter(body: &str) -> &str {
    if body.lines().next().map(str::trim) != Some("---") {
        return body;
    }
    let mut rest = body;
    // Skip the opening `---` line, then find the closing `---`.
    if let Some(at) = rest.find('\n') {
        rest = &rest[at + 1..];
    }
    match rest.find("\n---") {
        Some(at) => rest[at + 4..].trim_start_matches('\n'),
        None => rest,
    }
}

/// Skill directories contributed by plugins. The definition lives in `plugin`; here we only borrow the name.
pub fn plugin_skill_dirs(cwd: &std::path::Path) -> Vec<PathBuf> {
    crate::plugin::skill_dirs(&crate::plugin::discover(cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(dir: &std::path::Path, slug: &str, body: &str) {
        let path = dir.join(slug);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("SKILL.md"), body).unwrap();
    }

    /// The front matter's name and description show up in the list.
    #[test]
    fn a_skill_is_found_by_its_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            "search",
            "---\nname: 검색\ndescription: 코드에서 무언가를 찾는다\n---\n\n본문\n",
        );
        let skills = Skills::new(vec![dir.path().to_path_buf()]);
        assert_eq!(skills.list()[0].name, "검색");
        assert_eq!(skills.list()[0].description, "코드에서 무언가를 찾는다");
        assert!(skills.load("검색").unwrap().contains("본문"));
    }

    /// Only the body goes. Front matter is metadata for humans, not a procedure to follow.
    #[test]
    fn loading_a_skill_leaves_the_frontmatter_behind() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "s", "---\nname: 검색\ndescription: 설명\n---\n\n진짜 절차\n");
        let body = Skills::new(vec![dir.path().to_path_buf()]).load("검색").unwrap();
        assert!(body.starts_with("진짜 절차"), "{body:?}");
        assert!(!body.contains("description:"), "{body:?}");
    }

    /// **A skill must not vanish just because no front matter was written.** It is called by its directory name.
    #[test]
    fn a_skill_without_frontmatter_is_still_found() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "고치기", "그냥 본문입니다\n");
        let skills = Skills::new(vec![dir.path().to_path_buf()]);
        assert_eq!(skills.list()[0].name, "고치기");
        assert_eq!(skills.load("고치기").unwrap(), "그냥 본문입니다\n");
    }

    /// The preamble carries only names and descriptions. Loading bodies would eat context every turn.
    #[test]
    fn the_preamble_lists_skills_without_their_bodies() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            "s",
            "---\nname: 검색\ndescription: 찾는다\n---\n\n아주 긴 본문이 여기 있다\n",
        );
        let p = preamble(&Skills::new(vec![dir.path().to_path_buf()])).unwrap();
        assert!(p.contains("검색"));
        assert!(p.contains("찾는다"));
        assert!(!p.contains("아주 긴 본문"), "the body must not be carried:\n{p}");
    }

    /// No skills means no preamble. An empty list would only take up space.
    #[test]
    fn no_skills_means_no_preamble() {
        assert_eq!(preamble(&Skills::new(vec![])), None);
    }

    /// When an unknown name is requested, **say what is there** so the caller knows what to call next.
    #[test]
    fn asking_for_a_missing_skill_says_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "s", "---\nname: 검색\ndescription: 찾는다\n---\n본문\n");
        let e = Skills::new(vec![dir.path().to_path_buf()]).load("없는것").unwrap_err();
        assert!(e.message.contains("검색"), "{}", e.message);
    }

    /// Later directories win — the project is more specific than home.
    #[test]
    fn a_project_skill_overrides_the_one_from_home() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_skill(home.path(), "s", "---\nname: 검색\ndescription: 홈\n---\n홈 본문\n");
        write_skill(project.path(), "s", "---\nname: 검색\ndescription: 프로젝트\n---\n새 본문\n");
        let skills = Skills::new(vec![home.path().to_path_buf(), project.path().to_path_buf()]);
        assert_eq!(skills.list().len(), 1, "{:?}", skills.list());
        assert_eq!(skills.list()[0].description, "프로젝트");
        assert!(skills.load("검색").unwrap().contains("새 본문"));
    }

    /// Having no skill directories at all is normal. Dying on that would make the app unusable.
    #[test]
    fn a_missing_directory_is_not_an_error() {
        assert!(Skills::new(vec![PathBuf::from("/이런건/없다")]).is_empty());
    }
}
