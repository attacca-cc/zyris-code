//! Finds files. **Only this node knows this computer's filesystem.**
//!
//! Without this the agent would have to call `rg` via `terminal.exec`, and that path leaks in four ways —
//! it asks for approval every time even for a read in manual mode, it fails on machines without `rg`, the
//! result is one big string that can't be folded, and it hits the 55-second deadline on large trees.
//!
//! **Read-only.** That's why `gate.rs` unconditionally passes it, alongside `file_io`·`skill`.
//!
//! Only the methods' doc comments go out on the wire as the description the agent reads. **So the comment is the contract.**

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::WireError;

/// Maximum number of files returned at once.
const GLOB_LIMIT: u32 = 200;
/// Maximum number of matches returned at once.
const GREP_LIMIT: u32 = 100;
/// Maximum matches taken from one file. One file must not eat the whole result.
const PER_FILE: usize = 20;
/// Maximum length of one match line. One line of a minified file would cover the screen.
const LINE_LIMIT: usize = 400;
/// The prefix examined to tell whether something is binary.
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

    /// Where to search. Relative paths are rooted at root; paths starting with `/` are used as-is —
    /// kept the same as capkit's path rules. **Not a jail** (what stops things is `Gate`).
    fn at(&self, path: Option<&str>) -> PathBuf {
        match path {
            Some(p) if p.starts_with('/') => PathBuf::from(p),
            Some(p) if !p.is_empty() => self.root.join(p),
            _ => self.root.clone(),
        }
    }

    /// The name put on a result. Inside root, it shrinks to a relative path.
    ///
    /// **Given as an absolute path**, the home directory name rides on every result eating context,
    /// and on screen all the prefixes look the same, making it hard to tell which file it is.
    fn show(&self, p: &Path) -> String {
        p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().into_owned()
    }
}

/// Filters the walk with a single glob.
///
/// **Not written by hand.** Hand-rolling the `**` rules is guaranteed to be wrong; what's here is
/// exactly the implementation ripgrep uses.
fn matcher(root: &Path, pattern: &str) -> Result<ignore::overrides::Override, WireError> {
    let mut b = ignore::overrides::OverrideBuilder::new(root);
    b.add(pattern).map_err(|e| WireError::invalid_params(format!("패턴이 잘못됐습니다: {e}")))?;
    b.build().map_err(|e| WireError::invalid_params(format!("패턴이 잘못됐습니다: {e}")))
}

/// Builds the walk.
///
/// Three things are twisted from the defaults:
///
/// - `hidden(false)` — `.github/`·`.cargo/` are real code. Skipped as hidden, they'd never be found.
/// - `require_git(false)` — **by default `.gitignore` is only honored inside a git repo.** If the
///   working directory isn't git, `target/` gets searched as-is, which on this machine is tens of seconds.
/// - `.git/` is removed by hand — with hidden files on, skipping it would read tens of thousands of entries.
fn walk(at: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(at)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != std::ffi::OsStr::new(".git"))
        .build()
}

/// A NUL in the prefix marks it as binary. Carried as-is, it would fill the screen and context with garbage.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(SNIFF).any(|b| *b == 0)
}

/// Overlong lines are clipped. Only at character boundaries — cutting by byte would mangle Korean.
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
            // `metadata()` returns `ignore::Error` and `modified()` returns `io::Error`, so they
            // can't be chained with `and_then`. Whichever fails, treat it as "very old" and push it back.
            let at = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            found.push((at, self.show(entry.path())));
        }
        // Most recently modified first. The file being worked on now must come to the top to be useful.
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
            // **Must be a sentence the agent can read and fix.**
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
                // If one file ate the whole result, the other files wouldn't be seen.
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
        // **Walking and reading are blocking.** Left where they are, they'd stall the runtime worker
        // on large trees, and meanwhile the screen and every other tool would freeze.
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

    /// Normalizes an expected `/`-separated path to this platform's separator — the search
    /// tool returns OS-native paths (`src/app.rs` on Unix, `src\app.rs` on Windows).
    fn p(s: &str) -> String {
        s.replace('/', &std::path::MAIN_SEPARATOR.to_string())
    }

    /// Makes a tree. **Must be real files** — a mock filesystem wouldn't catch mistakes in
    /// `.gitignore` interpretation and path normalization.
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
        assert!(out.iter().any(|x| x == &p("src/app.rs")), "{out:?}");
        assert!(!out.iter().any(|x| x.starts_with(p("target/").as_str())), "gitignore did not apply: {out:?}");
    }

    /// **Paths are relative.** Given an absolute path, the home directory name rides on every result.
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
        assert_eq!(f.hits[0].path, p("src/app.rs"));
        assert_eq!(f.hits[0].line, 2, "counting must start at 1");
        assert!(f.hits[0].text.contains("fn scroll"));
        assert!(!f.truncated);
    }

    /// Must be able to narrow the target with a glob. Without it, large trees hit the deadline.
    #[tokio::test]
    async fn grep_can_be_narrowed_by_a_glob() {
        let d = tree();
        let s = LocalSearch::new(d.path().to_path_buf());
        let f = s.grep("fn draw".into(), None, Some("**/rows.rs".into()), None).await.unwrap();
        assert_eq!(f.hits.len(), 1, "{:?}", f.hits);
        assert_eq!(f.hits[0].path, p("src/rows.rs"));
    }

    /// **If truncated, it must say so.** Otherwise the agent reads it as "absent".
    #[tokio::test]
    async fn a_truncated_search_says_so() {
        let d = tree();
        let s = LocalSearch::new(d.path().to_path_buf());
        let f = s.grep("fn ".into(), None, None, Some(1)).await.unwrap();
        assert_eq!(f.hits.len(), 1);
        assert!(f.truncated, "it was truncated but did not say so");
    }

    /// Carrying binaries as-is would fill the screen and context with garbage bytes.
    #[tokio::test]
    async fn binary_files_are_skipped() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.bin"), b"fn draw\x00\x01\x02fn draw").unwrap();
        let s = LocalSearch::new(d.path().to_path_buf());
        let f = s.grep("fn draw".into(), None, None, None).await.unwrap();
        assert!(f.hits.is_empty(), "{:?}", f.hits);
    }

    /// A bad regex must come back as **a sentence the agent can fix**.
    #[tokio::test]
    async fn a_bad_pattern_explains_itself() {
        let d = tree();
        let s = LocalSearch::new(d.path().to_path_buf());
        assert!(s.grep("fn (".into(), None, None, None).await.is_err());
    }

    /// This capability is read-only. A writing tool mixed in would undermine the case for opening the gate.
    #[test]
    fn it_announces_only_read_only_tools() {
        use zyris::ServeCapability;
        let cap = SearchServer(LocalSearch::new(PathBuf::from("/tmp"))).descriptor();
        let names: Vec<&str> = cap.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(cap.name, "search");
        assert_eq!(names, vec!["glob", "grep"], "{names:?}");
    }
}
