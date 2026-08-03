//! 이 컴퓨터에 놓인 스킬 — 에이전트가 필요할 때 읽어 따라가는 절차서.
//!
//! **목록과 본문을 갈라 놓는 것이 요점이다.** 이름과 설명은 세션 preamble로 한 번 가고,
//! 본문은 `skill.load`를 부를 때만 간다. 다 실어 보내면 쓰지도 않을 절차서가 매 턴
//! 컨텍스트를 먹는다.
//!
//! 읽기만 하므로 승인 게이트가 통과시킨다(`gate::decide`) — 내 컴퓨터의 내 파일이고,
//! 그러라고 둔 것이다.

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

/// 찾아 둔 스킬들. **한 번 훑고 기억한다** — 매 호출마다 디스크를 훑을 이유가 없다.
#[derive(Debug, Clone, Default)]
pub struct Skills {
    found: Vec<(SkillInfo, PathBuf)>,
}

impl Skills {
    /// 준 디렉터리들에서 `<슬러그>/SKILL.md`를 찾는다. 없는 디렉터리는 그냥 넘어간다.
    ///
    /// 뒤 디렉터리가 이긴다 — 프로젝트가 홈보다 구체적이다.
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

    /// 기본 자리 둘. 뒤가 이긴다.
    pub fn discover(cwd: &std::path::Path) -> Skills {
        let mut dirs = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join(".config/zyris-code/skills"));
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

    /// 본문. 없는 이름이면 **무엇이 있는지 알려 준다** — "없다"만으로는 다음에 무엇을
    /// 부를지 알 수 없다.
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

/// 세션을 만들 때 실을 스킬 목록.
///
/// **이름과 설명만이다.** 본문까지 실으면 쓰지도 않을 절차서가 매 턴 컨텍스트를 먹는다.
/// 본문은 `skill.load`를 부를 때 간다.
///
/// 스킬이 하나도 없으면 `None`이다 — 빈 목록을 실어 봐야 자리만 먹는다.
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

/// `---`로 둘러싼 머리말에서 `name`과 `description`을 읽는다.
///
/// 없으면 디렉터리 이름을 쓴다 — **머리말을 안 썼다고 스킬이 사라지면 안 된다.**
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

/// 본문만. 머리말은 사람이 쓰는 메타데이터지 모델이 따라갈 절차가 아니다.
fn strip_front_matter(body: &str) -> &str {
    if body.lines().next().map(str::trim) != Some("---") {
        return body;
    }
    let mut rest = body;
    // 첫 `---` 줄을 건너뛴 뒤 닫는 `---`를 찾는다.
    if let Some(at) = rest.find('\n') {
        rest = &rest[at + 1..];
    }
    match rest.find("\n---") {
        Some(at) => rest[at + 4..].trim_start_matches('\n'),
        None => rest,
    }
}

/// 플러그인들이 얹는 스킬 디렉터리. 정의는 `plugin`에 있고 여기서는 이름만 빌린다.
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

    /// 머리말의 name과 description이 목록에 뜬다.
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

    /// 본문만 간다. 머리말은 사람이 쓰는 메타데이터지 따라갈 절차가 아니다.
    #[test]
    fn loading_a_skill_leaves_the_frontmatter_behind() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "s", "---\nname: 검색\ndescription: 설명\n---\n\n진짜 절차\n");
        let body = Skills::new(vec![dir.path().to_path_buf()]).load("검색").unwrap();
        assert!(body.starts_with("진짜 절차"), "{body:?}");
        assert!(!body.contains("description:"), "{body:?}");
    }

    /// **머리말을 안 썼다고 스킬이 사라지면 안 된다.** 디렉터리 이름으로 부른다.
    #[test]
    fn a_skill_without_frontmatter_is_still_found() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "고치기", "그냥 본문입니다\n");
        let skills = Skills::new(vec![dir.path().to_path_buf()]);
        assert_eq!(skills.list()[0].name, "고치기");
        assert_eq!(skills.load("고치기").unwrap(), "그냥 본문입니다\n");
    }

    /// preamble에는 이름과 설명만 싣는다. 본문까지 실으면 매 턴 컨텍스트를 먹는다.
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
        assert!(!p.contains("아주 긴 본문"), "본문이 실리면 안 된다:\n{p}");
    }

    /// 스킬이 하나도 없으면 preamble을 붙이지 않는다. 빈 목록은 자리만 먹는다.
    #[test]
    fn no_skills_means_no_preamble() {
        assert_eq!(preamble(&Skills::new(vec![])), None);
    }

    /// 없는 이름을 불렀을 때 **무엇이 있는지** 말해야 다음에 무엇을 부를지 안다.
    #[test]
    fn asking_for_a_missing_skill_says_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(dir.path(), "s", "---\nname: 검색\ndescription: 찾는다\n---\n본문\n");
        let e = Skills::new(vec![dir.path().to_path_buf()]).load("없는것").unwrap_err();
        assert!(e.message.contains("검색"), "{}", e.message);
    }

    /// 뒤 디렉터리가 이긴다 — 프로젝트가 홈보다 구체적이다.
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

    /// 스킬 디렉터리가 아예 없는 것이 정상이다. 그때 죽으면 앱을 못 쓴다.
    #[test]
    fn a_missing_directory_is_not_an_error() {
        assert!(Skills::new(vec![PathBuf::from("/이런건/없다")]).is_empty());
    }
}
