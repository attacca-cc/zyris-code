//! 파일을 찾는다. **이 컴퓨터의 파일시스템을 아는 것은 이 노드뿐이다.**
//!
//! 이게 없으면 에이전트는 `terminal.exec`로 `rg`를 부르는 수밖에 없고, 그 길은 넷이 샌다 —
//! 수동 모드에서 읽기인데도 매번 승인을 묻고, `rg`가 없는 머신에서는 실패하고, 결과가
//! 통짜 문자열이라 접을 수 없고, 큰 트리에서 55초 마감에 걸린다.
//!
//! **읽기만 한다.** 그래서 `gate.rs`가 `file_io`·`skill`과 나란히 무조건 통과시킨다.
//!
//! 메서드의 doc 주석만 와이어로 나가 에이전트가 읽는 설명이 된다. **그래서 주석이 곧 규약이다.**

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::WireError;

/// 한 번에 돌려주는 최대 파일 수.
const GLOB_LIMIT: u32 = 200;
/// 한 번에 돌려주는 최대 매치 수.
const GREP_LIMIT: u32 = 100;
/// 한 파일에서 가져오는 최대 매치 수. 한 파일이 결과를 통째로 먹으면 안 된다.
const PER_FILE: usize = 20;
/// 매치 한 줄의 최대 길이. 미니파이된 파일 한 줄이 화면을 덮는다.
const LINE_LIMIT: usize = 400;
/// 바이너리인지 보는 앞부분.
const SNIFF: usize = 8192;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Hit {
    pub path: String,
    /// 1-based.
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Found {
    pub hits: Vec<Hit>,
    /// True when the limit cut the results short. Read the rest before concluding
    /// something is absent.
    pub truncated: bool,
    /// How many files were searched. Tells you whether the pattern was too narrow or too wide.
    pub scanned: u32,
}

#[zyris::capability(name = "search", version = 1)]
pub trait Search {
    /// Find files by name pattern, like `**/*.rs` or `src/**/mod.rs`.
    /// Most recently modified first, so whatever is being worked on comes at the top.
    ///
    /// `path` is where to look: relative to the working directory, or the whole working
    /// directory when omitted. Anything ignored by .gitignore is left out.
    async fn glob(
        &self,
        pattern: String,
        path: Option<String>,
        limit: Option<u32>,
    ) -> zyris::Result<Vec<String>>;

    /// Search file contents with a regular expression. To search by file name, use glob.
    ///
    /// `glob` narrows which files are read (for example "**/*.rs"). Too many matches are cut
    /// short with `truncated` set — narrow the pattern or pass a glob and call again.
    async fn grep(
        &self,
        pattern: String,
        path: Option<String>,
        glob: Option<String>,
        limit: Option<u32>,
    ) -> zyris::Result<Found>;
}

pub struct LocalSearch {
    root: PathBuf,
}

impl LocalSearch {
    pub fn new(root: PathBuf) -> LocalSearch {
        LocalSearch { root }
    }

    /// 뒤질 자리. 상대경로는 root 기준, `/`로 시작하면 그대로다 — capkit의 경로 규칙과
    /// 같게 둔다. **감옥이 아니다**(막는 것은 `Gate`다).
    fn at(&self, path: Option<&str>) -> PathBuf {
        match path {
            Some(p) if p.starts_with('/') => PathBuf::from(p),
            Some(p) if !p.is_empty() => self.root.join(p),
            _ => self.root.clone(),
        }
    }

    /// 결과에 실을 이름. root 안쪽이면 상대경로로 줄인다.
    ///
    /// **절대경로를 그대로 주면** 홈 디렉터리 이름이 매 결과에 실려 컨텍스트를 먹고,
    /// 화면에서도 앞부분이 다 똑같아 어느 파일인지 보기 어렵다.
    fn show(&self, p: &Path) -> String {
        p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().into_owned()
    }
}

/// glob 하나로 순회를 거른다.
///
/// **직접 짜지 않는다.** `**` 규칙을 손으로 짜면 반드시 틀리고, 여기 것은 ripgrep이
/// 쓰는 바로 그 구현이다.
fn matcher(root: &Path, pattern: &str) -> Result<ignore::overrides::Override, WireError> {
    let mut b = ignore::overrides::OverrideBuilder::new(root);
    b.add(pattern).map_err(|e| WireError::invalid_params(format!("패턴이 잘못됐습니다: {e}")))?;
    b.build().map_err(|e| WireError::invalid_params(format!("패턴이 잘못됐습니다: {e}")))
}

/// 순회를 만든다.
///
/// 세 가지를 기본값에서 비튼다:
///
/// - `hidden(false)` — `.github/`·`.cargo/`는 진짜 코드다. 숨김이라고 빼면 안 찾힌다.
/// - `require_git(false)` — **기본값은 git 저장소 안에서만 `.gitignore`를 본다.** 작업
///   디렉터리가 git이 아니면 `target/`을 그대로 뒤지는데, 이 머신에서 그건 수십 초다.
/// - `.git/`은 손으로 뺀다 — 숨김을 켰으니 안 빼면 `.git/objects` 수만 개를 읽는다.
fn walk(at: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(at)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != std::ffi::OsStr::new(".git"))
        .build()
}

/// 앞부분에 NUL이 있으면 바이너리로 본다. 그대로 실으면 화면과 컨텍스트가 쓰레기로 찬다.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(SNIFF).any(|b| *b == 0)
}

/// 너무 긴 줄은 자른다. 문자 경계에서만 자른다 — 바이트로 자르면 한글이 깨진다.
fn clip_line(s: &str) -> String {
    if s.chars().count() <= LINE_LIMIT {
        return s.to_string();
    }
    let mut out: String = s.chars().take(LINE_LIMIT).collect();
    out.push('…');
    out
}

impl LocalSearch {
    fn glob_now(
        &self,
        pattern: &str,
        path: Option<&str>,
        limit: u32,
    ) -> Result<Vec<String>, WireError> {
        let at = self.at(path);
        let only = matcher(&at, pattern)?;
        let mut found: Vec<(std::time::SystemTime, String)> = Vec::new();
        for entry in walk(&at).flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if !only.matched(entry.path(), false).is_whitelist() {
                continue;
            }
            // `metadata()`는 `ignore::Error`, `modified()`는 `io::Error`라 `and_then`으로
            // 잇지 못한다. 어느 쪽이 실패하든 "아주 오래된 것"으로 두고 뒤로 민다.
            let at = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            found.push((at, self.show(entry.path())));
        }
        // 최근에 고친 것부터. 지금 만지고 있는 파일이 위에 와야 쓸모가 있다.
        found.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        found.truncate(limit as usize);
        Ok(found.into_iter().map(|(_, p)| p).collect())
    }

    fn grep_now(
        &self,
        pattern: &str,
        path: Option<&str>,
        glob: Option<&str>,
        limit: u32,
    ) -> Result<Found, WireError> {
        let re = regex::Regex::new(pattern)
            // **에이전트가 읽고 고칠 수 있는 문장이어야 한다.**
            .map_err(|e| WireError::invalid_params(format!("정규식이 잘못됐습니다: {e}")))?;
        let at = self.at(path);
        let only = glob.map(|g| matcher(&at, g)).transpose()?;

        let mut hits: Vec<Hit> = Vec::new();
        let mut scanned = 0u32;
        let mut truncated = false;
        for entry in walk(&at).flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if let Some(only) = &only {
                if !only.matched(entry.path(), false).is_whitelist() {
                    continue;
                }
            }
            let Ok(bytes) = std::fs::read(entry.path()) else { continue };
            if looks_binary(&bytes) {
                continue;
            }
            let Ok(text) = String::from_utf8(bytes) else { continue };
            scanned += 1;

            let name = self.show(entry.path());
            let mut here = 0usize;
            for (i, line) in text.lines().enumerate() {
                if !re.is_match(line) {
                    continue;
                }
                if hits.len() >= limit as usize {
                    truncated = true;
                    break;
                }
                hits.push(Hit { path: name.clone(), line: i as u32 + 1, text: clip_line(line) });
                here += 1;
                // 한 파일이 결과를 통째로 먹으면 나머지 파일이 안 보인다.
                if here >= PER_FILE {
                    truncated = true;
                    break;
                }
            }
            if hits.len() >= limit as usize {
                truncated = true;
                break;
            }
        }
        Ok(Found { hits, truncated, scanned })
    }
}

#[async_trait::async_trait]
impl Search for LocalSearch {
    async fn glob(
        &self,
        pattern: String,
        path: Option<String>,
        limit: Option<u32>,
    ) -> zyris::Result<Vec<String>> {
        // **순회와 읽기는 블로킹이다.** 안 옮기면 큰 트리에서 런타임 워커를 막고,
        // 그동안 화면도 다른 도구도 멈춘다.
        let me = LocalSearch { root: self.root.clone() };
        let limit = limit.unwrap_or(GLOB_LIMIT).max(1);
        tokio::task::spawn_blocking(move || me.glob_now(&pattern, path.as_deref(), limit))
            .await
            .map_err(|e| WireError::internal(format!("검색이 끝나지 못했습니다: {e}")))?
    }

    async fn grep(
        &self,
        pattern: String,
        path: Option<String>,
        glob: Option<String>,
        limit: Option<u32>,
    ) -> zyris::Result<Found> {
        let me = LocalSearch { root: self.root.clone() };
        let limit = limit.unwrap_or(GREP_LIMIT).max(1);
        tokio::task::spawn_blocking(move || {
            me.grep_now(&pattern, path.as_deref(), glob.as_deref(), limit)
        })
        .await
        .map_err(|e| WireError::internal(format!("검색이 끝나지 못했습니다: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 트리를 하나 만든다. **진짜 파일이어야 한다** — 모의 파일시스템으로는 `.gitignore`
    /// 해석과 경로 정규화 실수가 안 잡힌다.
    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let w = |p: &str, s: &str| {
            let at = d.path().join(p);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, s).unwrap();
        };
        w(".gitignore", "target/\n");
        w("src/app.rs", "fn draw() {}\nfn scroll() {}\n");
        w("src/rows.rs", "fn draw() {}\n");
        w("target/junk.rs", "fn draw() {}\n");
        d
    }

    #[tokio::test]
    async fn glob_finds_by_name_and_skips_ignored_paths() {
        let d = tree();
        let s = LocalSearch::new(d.path().to_path_buf());
        let out = s.glob("**/*.rs".into(), None, None).await.unwrap();
        assert!(out.iter().any(|p| p == "src/app.rs"), "{out:?}");
        assert!(!out.iter().any(|p| p.starts_with("target/")), "gitignore가 안 걸렸다: {out:?}");
    }

    /// **경로는 상대경로다.** 절대경로를 주면 홈 디렉터리 이름이 매 결과에 실린다.
    #[tokio::test]
    async fn paths_come_back_relative_to_the_working_dir() {
        let d = tree();
        let s = LocalSearch::new(d.path().to_path_buf());
        let out = s.glob("**/*.rs".into(), None, None).await.unwrap();
        assert!(!out.is_empty());
        assert!(out.iter().all(|p| !p.starts_with('/')), "{out:?}");
    }

    #[tokio::test]
    async fn grep_gives_the_line_number_and_the_text() {
        let d = tree();
        let s = LocalSearch::new(d.path().to_path_buf());
        let f = s.grep("fn scroll".into(), None, None, None).await.unwrap();
        assert_eq!(f.hits.len(), 1, "{:?}", f.hits);
        assert_eq!(f.hits[0].path, "src/app.rs");
        assert_eq!(f.hits[0].line, 2, "1부터 세야 한다");
        assert!(f.hits[0].text.contains("fn scroll"));
        assert!(!f.truncated);
    }

    /// glob으로 대상을 좁힐 수 있어야 한다. 못 좁히면 큰 트리에서 마감에 걸린다.
    #[tokio::test]
    async fn grep_can_be_narrowed_by_a_glob() {
        let d = tree();
        let s = LocalSearch::new(d.path().to_path_buf());
        let f = s.grep("fn draw".into(), None, Some("**/rows.rs".into()), None).await.unwrap();
        assert_eq!(f.hits.len(), 1, "{:?}", f.hits);
        assert_eq!(f.hits[0].path, "src/rows.rs");
    }

    /// **잘렸으면 잘렸다고 말해야 한다.** 모르면 에이전트가 "없다"로 읽는다.
    #[tokio::test]
    async fn a_truncated_search_says_so() {
        let d = tree();
        let s = LocalSearch::new(d.path().to_path_buf());
        let f = s.grep("fn ".into(), None, None, Some(1)).await.unwrap();
        assert_eq!(f.hits.len(), 1);
        assert!(f.truncated, "잘렸는데 말하지 않았다");
    }

    /// 바이너리를 그대로 실으면 화면과 컨텍스트가 쓰레기 바이트로 찬다.
    #[tokio::test]
    async fn binary_files_are_skipped() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.bin"), b"fn draw\x00\x01\x02fn draw").unwrap();
        let s = LocalSearch::new(d.path().to_path_buf());
        let f = s.grep("fn draw".into(), None, None, None).await.unwrap();
        assert!(f.hits.is_empty(), "{:?}", f.hits);
    }

    /// 잘못된 정규식은 **에이전트가 고칠 수 있는 문장**으로 돌아와야 한다.
    #[tokio::test]
    async fn a_bad_pattern_explains_itself() {
        let d = tree();
        let s = LocalSearch::new(d.path().to_path_buf());
        assert!(s.grep("fn (".into(), None, None, None).await.is_err());
    }

    /// 이 캐퍼빌리티는 읽기만 한다. 쓰는 도구가 섞이면 게이트를 여는 근거가 무너진다.
    #[test]
    fn it_announces_only_read_only_tools() {
        use zyris::ServeCapability;
        let cap = SearchServer(LocalSearch::new(PathBuf::from("/tmp"))).descriptor();
        let names: Vec<&str> = cap.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(cap.name, "search");
        assert_eq!(names, vec!["glob", "grep"], "{names:?}");
    }
}
