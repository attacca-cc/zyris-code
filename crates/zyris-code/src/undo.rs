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
use sha2::{Digest, Sha256};

/// Maximum kept. Beyond that, the oldest are dropped.
const KEEP: usize = 200;
/// Maximum length of a directory name. Most filesystems cap at 255 bytes.
const NAME_LIMIT: usize = 120;
/// How long an operation waits for another window's turn at the same log.
///
/// **Short.** This is spent inside a tool call or on a keypress; giving up and proceeding is the
/// failure the caller had before this lock existed, and stalling would be worse.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

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
    /// **Which window made this edit.** An entry from another *live* window is not this window's to
    /// revert — `/undo` here would silently take back work happening in a window beside it.
    ///
    /// `0` for entries written before this field existed, and `0` is never a live window's pid, so
    /// those read as "nobody is claiming this" — the permissive direction, which keeps yesterday's
    /// run undoable.
    #[serde(default)]
    pid: u32,
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
        Undo::under(&home(), cwd)
    }

    /// The same thing with the cache root given. **Tests use this.**
    ///
    /// `std::env::set_var` is process-global, so tests that each pointed `XDG_CACHE_HOME` at their
    /// own temp directory were overwriting one another's — passing alone and failing in the suite,
    /// at random. Handing the root in removes the shared variable from the picture, the same way
    /// `plugin::install_into` takes its directory.
    pub fn under(cache_root: &Path, cwd: &Path) -> Undo {
        Undo(Arc::new(Inner { dir: cache_root.join("undo").join(slug(cwd)), lock: Mutex::new(()) }))
    }

    /// Called **right before** the write.
    ///
    /// **Failure doesn't block the edit.** Blocking work because the safety net failed would create
    /// unfixable files — the same call as `Preview` failing not blocking approval. Instead, that edit
    /// isn't recorded, and `/undo` reverts the one before it.
    pub fn snapshot(&self, path: &Path) {
        if let Err(e) = self.try_snapshot(path) {
            tracing::warn!(path = %path.display(), "could not write the undo record: {e}");
        }
    }

    fn try_snapshot(&self, path: &Path) -> std::io::Result<()> {
        let _guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(&self.0.dir)?;
        // **Across processes, not just across tasks.** The mutex above is per process; two windows
        // in one directory share this log, and a read-modify-write of it under no OS lock is how
        // one window's whole history was replaced by the other's single line.
        let _file = crate::instance::FileLock::take(&self.lock_path(), LOCK_WAIT);

        let before = std::fs::read(path);
        let existed = before.is_ok();
        let mut log = self.read_log();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let backup = self.write_backup(&log, &name, before.unwrap_or_default())?;

        log.push(Record {
            at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
            path: path.to_string_lossy().into_owned(),
            backup,
            existed,
            pid: std::process::id(),
        });
        self.trim(&mut log);
        self.write_log(&log)
    }

    /// Writes the pre-change content under a name nobody else has taken.
    ///
    /// **`create_new`, retried on a collision.** The number alone is not enough: it is derived from
    /// the log this process just read, so two windows that read the same log pick the same number,
    /// and the second plain `write` quietly overwrote the first's snapshot — one undo point then
    /// restoring the wrong content. `create_new` fails instead, and the number is stepped until it
    /// does not.
    fn write_backup(
        &self,
        log: &[Record],
        name: &str,
        content: Vec<u8>,
    ) -> std::io::Result<String> {
        let mut number = next_number(log);
        loop {
            let backup = format!("{number:06}-{name}.bak");
            let opened = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.0.dir.join(&backup));
            match opened {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(&content)?;
                    return Ok(backup);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => number += 1,
                Err(e) => return Err(e),
            }
        }
    }

    /// The lock shared **between processes**. The `Mutex` inside `Inner` only orders this one.
    fn lock_path(&self) -> PathBuf {
        self.0.dir.join(".lock")
    }

    /// Reverts the last single edit **this window made.** Returns the reverted file's path.
    pub fn revert_last(&self) -> Result<PathBuf, String> {
        let _guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _file = crate::instance::FileLock::take(&self.lock_path(), LOCK_WAIT);
        let mut log = self.read_log();
        // **Another live window's edit is not this window's to revert.** Two windows in one
        // directory share this log, and `/undo` used to pop whatever was newest — often the other
        // window's edit of another file, which silently took back work still in progress. An entry
        // from a window that has since ended is fair game: that is the ordinary case of undoing
        // what yesterday's run changed.
        let live = live_pids();
        let Some(at) = last_revertable(&log, &live) else {
            return Err(crate::lang::current().nothing_to_undo().to_string());
        };
        // **Nor is a file another live window has edited since.** Restoring this entry's backup
        // puts back the content from before *both* edits, so the other window's later change to
        // the same file would vanish without a trace. Refused, and the entry kept, until that
        // window has ended.
        if log[at + 1..].iter().any(|later| later.path == log[at].path && live.contains(&later.pid))
        {
            return Err(crate::lang::current().undo_after_other_window(&log[at].path));
        }
        let last = log.remove(at);
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

    /// Whether there is anything this window may revert.
    pub fn is_empty(&self) -> bool {
        let _guard = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _file = crate::instance::FileLock::take(&self.lock_path(), LOCK_WAIT);
        last_revertable(&self.read_log(), &live_pids()).is_none()
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
        let _file = crate::instance::FileLock::take(&self.lock_path(), LOCK_WAIT);
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
        // **Atomic.** A truncating `write` has a window in which the file is empty, and a window
        // that read it there got an empty log — appended its own single record and wrote that back,
        // throwing the whole history (up to `KEEP` entries) away. A temp file plus a rename is one
        // step and has no such window.
        crate::atomic::write_atomic(&self.log_path(), text.as_bytes(), None)
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

/// The number to try first: one past the last entry in the log.
///
/// Only a starting point — `write_backup` steps past a number another window has already taken.
fn next_number(log: &[Record]) -> u64 {
    log.last()
        .and_then(|r| r.backup.split('-').next()?.parse::<u64>().ok())
        .map(|n| n + 1)
        .unwrap_or(1)
}

/// The newest entry this window may revert — its own, or one whose window has since ended.
///
/// Kept apart from `trim` so the rule is one sentence in one place: an entry is skipped only while
/// the process that wrote it is still running. The live set is read once rather than per entry: a
/// log holds up to `KEEP` records, and asking the registry about each would be two hundred
/// directory scans for one `/undo`.
fn last_revertable(log: &[Record], live: &[u32]) -> Option<usize> {
    log.iter().rposition(|record| record.pid == std::process::id() || !live.contains(&record.pid))
}

/// The pids of every live window, read once per operation.
fn live_pids() -> Vec<u32> {
    crate::instance::live(&crate::instance::cache_root()).into_iter().map(|row| row.pid).collect()
}

/// Where the history lives. Respects `XDG_CACHE_HOME` — tests move the location with it too.
///
/// **One definition with the instance registry and the edit locks** (`instance::cache_root`). Two
/// copies of this rule would put the undo history somewhere other than the locks that protect it.
fn home() -> PathBuf {
    crate::instance::cache_root()
}

/// Turns a working directory into one directory name. **Readable, and unique.**
///
/// Someone looking into the cache must know which repo it is to decide whether to delete it, so the
/// readable part stays. But the readable part alone is not enough: every non-alphanumeric character
/// became `-`, so `/home/a-b/c` and `/home/a/b-c` — and `/x/my app` and `/x/my-app` — landed in one
/// directory, and two different repositories shared one undo history. The hash of the full path is
/// what makes the mapping one-to-one; the words in front keep it legible.
fn slug(cwd: &Path) -> String {
    let mut hash = Sha256::new();
    hash.update(cwd.to_string_lossy().as_bytes());
    let digest = format!("{:x}", hash.finalize());
    format!("{}-{}", readable(cwd), &digest[..8])
}

/// The readable part: the path with everything that is not a letter or a digit turned into `-`.
fn readable(cwd: &Path) -> String {
    let mut out = String::new();
    for ch in cwd.to_string_lossy().chars() {
        match ch {
            c if c.is_alphanumeric() => out.push(c),
            _ if out.ends_with('-') => {}
            _ => out.push('-'),
        }
    }
    let name = out.trim_matches('-');
    // Very long paths hit the filesystem name limit. Keep the tail — the repo name is there — and
    // leave room for the `-` and the eight hex digits `slug` puts after it, so the whole name still
    // fits. Nothing is lost to the cut: the hash still tells two long paths apart.
    let budget = NAME_LIMIT.saturating_sub(9);
    let cut = name.char_indices().rev().nth(budget - 1).map(|(i, _)| i).unwrap_or(0);
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
        assert!(!file.exists(), "the created file was left behind");
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
        assert!(undo.is_empty(), "once everything is undone it must be empty");
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
        // Asserted against the current language — a Korean literal here only passed because
        // another test had set the global language first.
        assert_eq!(why, crate::lang::current().nothing_to_undo());
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
        assert_eq!(
            left,
            vec!["a.rs".to_string()],
            "something was created in the working directory: {left:?}"
        );
    }

    /// If two working directories' histories mixed, the wrong file would come back.
    #[test]
    fn two_working_directories_keep_separate_histories() {
        let (_cache, work, _g) = scoped();
        let other = tempfile::tempdir().unwrap();
        let file = work.path().join("a.rs");
        write(&file, "before\n");

        Undo::for_dir(work.path()).snapshot(&file);
        assert!(Undo::for_dir(other.path()).is_empty(), "history from another run is visible");
    }

    /// The name must be human-readable — opening the cache you must know which repo it is — and
    /// **end in a short hash**, which is what stops two directories that read the same from being
    /// one directory.
    #[test]
    fn the_directory_name_is_readable_and_unique() {
        let name = slug(Path::new("/home/ruma/zyris-code"));
        assert!(name.starts_with("home-ruma-zyris-code-"), "{name}");
        assert!(slug(Path::new("/")).starts_with("root-"), "{}", slug(Path::new("/")));
        // **The collisions the readable form alone allowed.** `/home/a-b/c` and `/home/a/b-c` used
        // to be one directory, and so did `/x/my app` and `/x/my-app`.
        assert_ne!(slug(Path::new("/home/a-b/c")), slug(Path::new("/home/a/b-c")));
        assert_ne!(slug(Path::new("/x/my app")), slug(Path::new("/x/my-app")));
    }

    /// A very long path must still produce a usable name within the filesystem limit.
    #[test]
    fn a_very_long_path_still_makes_a_usable_name() {
        let long = format!("/{}", "가나다라마바사".repeat(60));
        let name = slug(Path::new(&long));
        assert!(name.chars().count() <= NAME_LIMIT, "{} columns", name.chars().count());
        assert!(!name.is_empty());
    }

    /// **An entry another live window wrote is not this window's to revert.** `/undo` popped the
    /// newest entry whatever it was, so pressing it in one window took back the other window's edit
    /// of another file. An entry from a window that has since ended is still fair game — which is
    /// how yesterday's run stays undoable.
    #[test]
    fn another_live_windows_entry_is_left_alone() {
        let (cache, work, _g) = scoped();
        let ours = work.path().join("ours.rs");
        write(&ours, "before\n");
        let undo = Undo::for_dir(work.path());
        undo.snapshot(&ours);
        write(&ours, "after\n");

        // A second live window, as the registry sees one: a row whose lock this process holds
        // under a pid that is not this one's.
        let other: u32 = std::process::id() + 1;
        let held = crate::instance::fake_live(&cache.path().join("zyris-code"), other, "/tmp");

        // Their edit sits on top of ours: a backup and a log line of their own.
        let theirs = work.path().join("theirs.rs");
        write(&theirs, "theirs changed\n");
        write(&undo.0.dir.join("999999-theirs.rs.bak"), "theirs before\n");
        let log = undo.0.dir.join("log.jsonl");
        let mut text = std::fs::read_to_string(&log).unwrap();
        text.push_str(&format!(
            "{}\n",
            serde_json::json!({
                "at": 0,
                "path": theirs,
                "backup": "999999-theirs.rs.bak",
                "existed": true,
                "pid": other,
            })
        ));
        std::fs::write(&log, text).unwrap();

        // `/undo` takes this window's edit and leaves the live window's file exactly as it was.
        assert_eq!(undo.revert_last().unwrap(), ours);
        assert_eq!(std::fs::read_to_string(&ours).unwrap(), "before\n");
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "theirs changed\n");
        // With nothing of its own left, it says so rather than reaching into theirs.
        assert!(undo.revert_last().is_err(), "it reverted another live window's edit");

        drop(held);
    }

    /// **An edit another live window made on top of ours is not erased by our `/undo`.** Our
    /// entry's backup is the file from before both edits; restoring it took the other window's
    /// change away with ours and said nothing.
    #[test]
    fn a_file_another_live_window_edited_since_is_not_reverted() {
        let (cache, work, _g) = scoped();
        let file = work.path().join("shared.rs");
        write(&file, "original\n");
        let undo = Undo::for_dir(work.path());
        undo.snapshot(&file);
        write(&file, "ours\n");

        let other: u32 = std::process::id() + 1;
        let held = crate::instance::fake_live(&cache.path().join("zyris-code"), other, "/tmp");
        write(&undo.0.dir.join("999999-shared.rs.bak"), "ours\n");
        let log = undo.0.dir.join("log.jsonl");
        let mut text = std::fs::read_to_string(&log).unwrap();
        text.push_str(&format!(
            "{}\n",
            serde_json::json!({
                "at": 0,
                "path": file,
                "backup": "999999-shared.rs.bak",
                "existed": true,
                "pid": other,
            })
        ));
        std::fs::write(&log, text).unwrap();
        write(&file, "ours and theirs\n");

        let why = undo.revert_last().expect_err("it erased another live window's edit");
        assert_eq!(why, crate::lang::current().undo_after_other_window(&file.to_string_lossy()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ours and theirs\n");
        drop(held);
    }
}
