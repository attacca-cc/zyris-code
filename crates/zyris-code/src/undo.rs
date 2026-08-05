//! Reverts edits.
//!
//! `code_edit` changes the disk, but the app had no way to undo it. In a directory without git,
//! that was it. The approval gate guards "before running", so **an "after running" one is needed to match.**
//!
//! **Nothing is created inside the user's repo.** Putting it in `.zyris-code/undo/` creates a directory
//! that ends up in commits unless it's added to `.gitignore`. Don't dirty someone else's repo.
//!
//! ```text
//! ~/.cache/zyris-code/undo/home-ruma-zyris-code/
//!     log.jsonl          one revert per line, oldest first
//!     000001-app.rs.bak  the content exactly as before the change
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Maximum kept. Beyond that, the oldest are dropped.
const KEEP: usize = 200;
/// Maximum length of a directory name. Most filesystems cap at 255 bytes.
const NAME_LIMIT: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    /// Unix seconds. Only used when showing to a human.
    at: u64,
    /// Absolute path of the changed file.
    path: String,
    /// Name of the backup file. Its name within the directory.
    backup: String,
    /// **Whether the file existed originally.** If it didn't, reverting means deleting —
    /// reverting to an empty file would leave a file that never existed.
    existed: bool,
}

/// One touched file. One row even if edited many times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub path: PathBuf,
    /// How many times it was edited.
    pub edits: usize,
    /// Whether it created a file that didn't exist.
    pub created: bool,
    pub added: u32,
    pub removed: u32,
}

/// One working directory's revert history.
#[derive(Clone)]
pub struct Undo(Arc<Inner>);

struct Inner {
    dir: PathBuf,
    /// Lines up all file operations in a single queue. Edits arrive per tool call and those are
    /// different tasks — without it, two log lines would overwrite each other.
    lock: Mutex<()>,
}

impl Undo {
    /// Opens this working directory's history. The directory is created on first write.
    pub fn for_dir(cwd: &Path) -> Undo {
        Undo(Arc::new(Inner { dir: home().join("undo").join(slug(cwd)), lock: Mutex::new(()) }))
    }

    /// Called **right before** the write.
    ///
    /// **Failure doesn't block the edit.** Blocking work because the safety net failed would create
    /// unfixable files — the same call as `Preview` failing not blocking approval. Instead, that edit
    /// isn't recorded, and `/undo` reverts the one before it.
    pub fn snapshot(&self, path: &Path) {
        if let Err(e) = self.try_snapshot(path) {
            tracing::warn!(path = %path.display(), "되돌림 기록을 남기지 못했다: {e}");
        }
    }

    fn try_snapshot(&self, path: &Path) -> std::io::Result<()> {
        let _guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(&self.0.dir)?;

        let before = std::fs::read(path);
        let existed = before.is_ok();
        let mut log = self.read_log();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // Prefix a number so duplicate names don't overwrite.
        let backup = format!("{:06}-{}.bak", self.next_number(&log), name);
        std::fs::write(self.0.dir.join(&backup), before.unwrap_or_default())?;

        log.push(Record {
            at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
            path: path.to_string_lossy().into_owned(),
            backup,
            existed,
        });
        self.trim(&mut log);
        self.write_log(&log)
    }

    /// Reverts the last single edit. Returns the reverted file's path.
    pub fn revert_last(&self) -> Result<PathBuf, String> {
        let _guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut log = self.read_log();
        let Some(last) = log.pop() else {
            return Err(crate::lang::current().nothing_to_undo().to_string());
        };
        let path = PathBuf::from(&last.path);
        let backup = self.0.dir.join(&last.backup);

        let outcome = if last.existed {
            std::fs::read(&backup).and_then(|content| std::fs::write(&path, content))
        } else {
            // The edit had created a file that didn't exist. Reverting means deleting.
            // If a human already deleted it, that's also the desired state, so count it as success.
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        };
        // **This row is removed whether it succeeded or failed.** Kept, every `/undo` press
        // would hit the same one and never reach the edits before it.
        let _ = std::fs::remove_file(&backup);
        let _ = self.write_log(&log);
        outcome.map(|()| path).map_err(|e| crate::lang::current().undo_failed(&e.to_string()))
    }

    /// Whether there is anything to revert.
    pub fn is_empty(&self) -> bool {
        let _guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read_log().is_empty()
    }

    /// Files changed in this directory. **Most recently touched comes first.**
    ///
    /// Per file, the backup of the **oldest record** is "before touching", and it's compared against what's
    /// on disk now. What's wanted isn't each edit but how much changed overall —
    /// how many lines ultimately changed matters before the fact that one file was edited five times.
    ///
    /// The log survives restarts, so **it isn't just this run's.** It matches the range `/undo` walks back —
    /// if the two diverge, what can be reverted and what's shown fall out of sync.
    pub fn changed(&self) -> Vec<Changed> {
        let _guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        let log = self.read_log();

        // Walk from oldest, grabbing each file's first record; order is by last touched.
        let mut order: Vec<&str> = Vec::new();
        let mut first: std::collections::HashMap<&str, &Record> = std::collections::HashMap::new();
        let mut edits: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for record in &log {
            first.entry(&record.path).or_insert(record);
            *edits.entry(&record.path).or_default() += 1;
            order.retain(|p| *p != record.path);
            order.push(&record.path);
        }
        order.reverse();

        order
            .into_iter()
            .map(|path| {
                let record = first[path];
                let before =
                    std::fs::read_to_string(self.0.dir.join(&record.backup)).unwrap_or_default();
                // If it's gone now, it was deleted. Comparing against empty text shows all-removed — correct.
                let now = std::fs::read_to_string(path).unwrap_or_default();
                let d = crate::tools::diff::diff(&before, &now, path);
                Changed {
                    path: PathBuf::from(path),
                    edits: edits[path],
                    created: !record.existed,
                    added: d.added,
                    removed: d.removed,
                }
            })
            .collect()
    }

    fn log_path(&self) -> PathBuf {
        self.0.dir.join("log.jsonl")
    }

    /// Reads the log. **Broken lines are skipped** — one corrupt line must not stop the rest from being reverted.
    fn read_log(&self) -> Vec<Record> {
        let Ok(text) = std::fs::read_to_string(self.log_path()) else {
            return Vec::new();
        };
        text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    fn write_log(&self, log: &[Record]) -> std::io::Result<()> {
        let mut text = String::new();
        for record in log {
            text.push_str(&serde_json::to_string(record).unwrap_or_default());
            text.push('\n');
        }
        std::fs::create_dir_all(&self.0.dir)?;
        std::fs::write(self.log_path(), text)
    }

    fn next_number(&self, log: &[Record]) -> u64 {
        log.last()
            .and_then(|r| r.backup.split('-').next()?.parse::<u64>().ok())
            .map(|n| n + 1)
            .unwrap_or(1)
    }

    /// Past the cap, drop from the oldest. The backup files go too —
    /// otherwise the cache directory keeps growing.
    fn trim(&self, log: &mut Vec<Record>) {
        while log.len() > KEEP {
            let dropped = log.remove(0);
            let _ = std::fs::remove_file(self.0.dir.join(&dropped.backup));
        }
    }
}

/// Where the history lives. Respects `XDG_CACHE_HOME` — tests move the location with it too.
fn home() -> PathBuf {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache).join("zyris-code");
    }
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".cache/zyris-code"),
        None => std::env::temp_dir().join("zyris-code"),
    }
}

/// Turns a working directory into one directory name. **Readable, not a hash** —
/// someone looking into the cache must know which repo it is to decide whether to delete it.
fn slug(cwd: &Path) -> String {
    let mut out = String::new();
    for ch in cwd.to_string_lossy().chars() {
        match ch {
            c if c.is_alphanumeric() => out.push(c),
            _ if out.ends_with('-') => {}
            _ => out.push('-'),
        }
    }
    let name = out.trim_matches('-');
    // Very long paths hit the filesystem name limit. Keep the tail — the repo name is there.
    let cut = name.char_indices().rev().nth(NAME_LIMIT - 1).map(|(i, _)| i).unwrap_or(0);
    let name = &name[cut..];
    if name.is_empty() {
        "root".into()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Moves the cache to a temp directory. **Must not write to the real home.**
    ///
    /// `set_var` is process-global, so these tests queue up on one lock.
    fn scoped() -> (tempfile::TempDir, tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        static ENV: Mutex<()> = Mutex::new(());
        let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let cache = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CACHE_HOME", cache.path());
        (cache, work, guard)
    }

    fn write(at: &Path, text: &str) {
        std::fs::write(at, text).unwrap();
    }

    /// Reverting must put the original content back exactly.
    #[test]
    fn reverting_puts_the_old_content_back() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("a.rs");
        write(&file, "before\n");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file);
        write(&file, "after\n");

        assert_eq!(undo.revert_last().unwrap(), file);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "before\n");
    }

    /// **A newly created file is deleted.** Reverting to an empty file would leave a file that never existed.
    #[test]
    fn reverting_a_created_file_removes_it() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("new.rs");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file); // doesn't exist yet
        write(&file, "새로 만든 것\n");

        undo.revert_last().unwrap();
        assert!(!file.exists(), "만든 파일이 남았다");
    }

    /// Pressing repeatedly keeps walking back.
    #[test]
    fn reverting_twice_walks_back_two_edits() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("a.rs");
        write(&file, "하나\n");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file);
        write(&file, "둘\n");
        undo.snapshot(&file);
        write(&file, "셋\n");

        undo.revert_last().unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "둘\n");
        undo.revert_last().unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "하나\n");
        assert!(undo.is_empty(), "다 되돌렸으면 비어야 한다");
    }

    /// **A file edited many times is still one row.** What's wanted isn't "edited five times" but
    /// how many lines ultimately changed versus the start.
    #[test]
    fn a_file_edited_twice_counts_from_the_oldest_backup() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("a.rs");
        write(&file, "하나\n");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file);
        write(&file, "하나\n둘\n");
        undo.snapshot(&file);
        write(&file, "하나\n둘\n셋\n");

        let changed = undo.changed();
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed[0].path, file);
        assert_eq!(changed[0].edits, 2);
        assert_eq!((changed[0].added, changed[0].removed), (2, 0));
        assert!(!changed[0].created);
    }

    /// Creating a file that didn't exist must be marked as such — reverting deletes it.
    #[test]
    fn a_created_file_is_marked_as_created() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("새것.rs");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file); // doesn't exist yet
        write(&file, "한 줄\n");

        let changed = undo.changed();
        assert_eq!(changed.len(), 1);
        assert!(changed[0].created);
        assert_eq!((changed[0].added, changed[0].removed), (1, 0));
    }

    /// Most recently touched comes first. The one just edited must not be found at the bottom of the list.
    #[test]
    fn the_most_recently_touched_file_comes_first() {
        let (_cache, work, _g) = scoped();
        let (a, b) = (work.path().join("a.rs"), work.path().join("b.rs"));
        write(&a, "가\n");
        write(&b, "나\n");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&a);
        write(&a, "가가\n");
        undo.snapshot(&b);
        write(&b, "나나\n");
        // Editing a once more brings a back to the front.
        undo.snapshot(&a);
        write(&a, "가가가\n");

        let paths: Vec<PathBuf> = undo.changed().into_iter().map(|c| c.path).collect();
        assert_eq!(paths, vec![a, b]);
    }

    /// A deleted file must still be comparable. Panicking here would hide the whole list.
    #[test]
    fn a_file_deleted_afterwards_still_shows_up() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("a.rs");
        write(&file, "하나\n둘\n");

        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file);
        std::fs::remove_file(&file).unwrap();

        let changed = undo.changed();
        assert_eq!(changed.len(), 1);
        assert_eq!((changed[0].added, changed[0].removed), (0, 2));
    }

    /// With nothing to revert it says so. Silently succeeding would look like the press did nothing.
    #[test]
    fn reverting_with_nothing_to_undo_says_so() {
        let (_cache, work, _g) = scoped();
        let undo = Undo::for_dir(work.path());
        assert!(undo.is_empty());
        let why = undo.revert_last().unwrap_err();
        assert!(why.contains("없습니다"), "{why}");
    }

    /// **Nothing is created inside the user's repo.** Creating `.zyris-code/`
    /// ends up in commits unless it's added to `.gitignore`.
    #[test]
    fn nothing_is_written_inside_the_working_directory() {
        let (_cache, work, _g) = scoped();
        let file = work.path().join("a.rs");
        write(&file, "before\n");

        Undo::for_dir(work.path()).snapshot(&file);

        let left: Vec<String> = std::fs::read_dir(work.path())
            .unwrap()
            .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
        assert_eq!(left, vec!["a.rs".to_string()], "작업 디렉터리에 뭔가 생겼다: {left:?}");
    }

    /// If two working directories' histories mixed, the wrong file would come back.
    #[test]
    fn two_working_directories_keep_separate_histories() {
        let (_cache, work, _g) = scoped();
        let other = tempfile::tempdir().unwrap();
        let file = work.path().join("a.rs");
        write(&file, "before\n");

        Undo::for_dir(work.path()).snapshot(&file);
        assert!(Undo::for_dir(other.path()).is_empty(), "남의 기록이 보인다");
    }

    /// The name must be human-readable — opening the cache you must know which repo it is.
    #[test]
    fn the_directory_name_is_readable() {
        assert_eq!(slug(Path::new("/home/ruma/zyris-code")), "home-ruma-zyris-code");
        assert_eq!(slug(Path::new("/")), "root");
    }

    /// A very long path must still produce a usable name within the filesystem limit.
    #[test]
    fn a_very_long_path_still_makes_a_usable_name() {
        let long = format!("/{}", "가나다라마바사".repeat(60));
        let name = slug(Path::new(&long));
        assert!(name.chars().count() <= NAME_LIMIT, "{}칸", name.chars().count());
        assert!(!name.is_empty());
    }
}
