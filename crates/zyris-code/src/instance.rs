//! Which zyris-code windows are alive on this machine, and the locks that keep them out of each
//! other's way.
//!
//! **Several windows share one user's state.** The credentials, the settings, the undo history and
//! the working tree are all per-user, not per-window, so a second window can overwrite the first
//! one's work in ways neither of them can see. What was missing was any record of *which windows
//! are running*; with one, "is another window already here?" becomes a question with an answer.
//!
//! **Liveness is a held lock, not a timer and not a pid table.** Each window keeps an exclusive
//! lock on `<cache>/instances/<pid>.lock` for as long as it lives, and describes itself in
//! `<pid>.json` beside it. Any other window asks whether an entry's owner is still there by trying
//! to take that lock — it succeeds only if the owner is gone. No clock, no `/proc`, no platform
//! branch, and a machine that is rebooted leaves nothing behind, because the lock died with the
//! process.
//!
//! **The lock is not on the row itself.** On Windows a file lock is mandatory: a locked byte range
//! cannot be read through any other handle, so a locked `<pid>.json` was a row nobody else could
//! read, and every other window saw an empty registry. The row stays unlocked and readable; the
//! lock lives on a file nobody ever reads.
//!
//! | what is at stake | who asks |
//! |---|---|
//! | a node name two windows would both answer to | `conn::default_node_name` |
//! | a session another window is already serving | `conn::session_awaiting_answer` |
//! | an undo entry another window wrote | `undo::revert_last` |
//! | one file two windows are editing | `tools::edit::LocalEdit` |

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where this user's zyris-code cache lives. **One definition** — the undo history, this registry
/// and the edit locks all sit under it, and a second copy of this rule would drift.
pub fn cache_root() -> PathBuf {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache).join("zyris-code");
    }
    // `$HOME` is not portable — `conn::user_home` answers with `USERPROFILE` too. Falling through
    // to the temp directory on Windows puts this where the system periodically empties it.
    match crate::conn::user_home() {
        Some(home) => home.join(".cache/zyris-code"),
        None => std::env::temp_dir().join("zyris-code"),
    }
}

/// What a window wrote about itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Row {
    pub pid: u32,
    /// The node name this window asked Attacca for, so a second window can tell whether its own
    /// name would collide.
    pub node_name: String,
    pub cwd: PathBuf,
}

/// One window's entry: the row, and the lock file held open and locked for as long as it lives.
/// Dropping it releases both.
pub struct Instance {
    path: PathBuf,
    lock: PathBuf,
    /// Never read — it exists so the lock is held. Closing it releases the lock.
    _file: fs::File,
}

impl Instance {
    /// Registers this process. `None` when there is nowhere to write or the registry is not
    /// writable — **a window that cannot say it is here still runs**, it just cannot be seen.
    pub fn register(cache_root: &Path, node_name: &str, cwd: &Path) -> Option<Instance> {
        let dir = cache_root.join("instances");
        fs::create_dir_all(&dir).ok()?;
        // **Rows whose owner is gone are cleared first.** Left behind, a stale row would make every
        // window believe the machine is busier than it is — and its basename would keep forcing the
        // `-<hash>` suffix on a name that no longer collides.
        prune(&dir);
        let pid = std::process::id();
        let path = dir.join(format!("{pid}.json"));
        let lock = lock_path(&path);
        // **The lock first, then the row.** Written the other way round, another window's `prune`
        // could find the row in the moment before it was locked, take the free lock, and delete a
        // live window's row. Once the lock is held, `prune` leaves the row it guards alone.
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock)
            .ok()?;
        // Every stale row was just pruned, so nothing else should hold it. A failure here means the
        // filesystem grew a lock this process cannot take, and the honest answer is then "not
        // registered".
        file.try_lock().ok()?;
        let row = Row { pid, node_name: node_name.to_string(), cwd: cwd.to_path_buf() };
        let text = serde_json::to_vec(&row).ok()?;
        if crate::atomic::write_atomic(&path, &text, None).is_err() {
            let _ = fs::remove_file(&lock);
            return None;
        }
        Some(Instance { path, lock, _file: file })
    }

    /// Where the entry lives, so a test can look at it.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // The row goes before its lock, so nobody finds a row whose lock file is already gone.
        // Removing a file this process still holds open works on Windows too — std opens every
        // file with `FILE_SHARE_DELETE`.
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(&self.lock);
    }
}

/// The lock file that says whether the row at `row` has a live owner.
fn lock_path(row: &Path) -> PathBuf {
    row.with_extension("lock")
}

/// Whether somebody holds the lock guarding the row at `row`. A lock file that does not exist, or
/// that we can take ourselves, has no owner.
fn is_held(row: &Path) -> bool {
    // Opened without `create`: a missing lock file is an answer ("nobody"), not something to make.
    let Ok(file) = fs::OpenOptions::new().read(true).write(true).open(lock_path(row)) else {
        return false;
    };
    // Dropping the handle on return releases the lock again if we took it.
    matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock))
}

/// Every window currently alive, with what it said about itself.
pub fn live(cache_root: &Path) -> Vec<Row> {
    let dir = cache_root.join("instances");
    let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // **A row is alive exactly while its lock is held** — the same test `prune` uses to decide
        // a row is stale.
        if !is_held(&path) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue };
        if let Ok(row) = serde_json::from_str::<Row>(&text) {
            out.push(row);
        }
    }
    out
}

/// Whether a process is one of the live windows. **A pid nobody registered is not live** — an undo
/// entry from a finished run reads that way, and so does a pid that some other program has since
/// reused, which is the conservative direction to be wrong in.
pub fn is_live(cache_root: &Path, pid: u32) -> bool {
    live(cache_root).iter().any(|row| row.pid == pid)
}

/// Removes rows whose owner is gone, so the registry does not grow for ever.
///
/// ponytail: a pid reused in the instant between our lock release and a new window's lock could
/// leave that window unregistered; pid reuse that fast is not worth an inode check.
fn prune(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let lock = lock_path(&path);
        // **Only a row whose lock we could take ourselves is deleted**, and the lock is held across
        // both removals, so a window registering under that pid waits until both are gone.
        match fs::OpenOptions::new().read(true).write(true).open(&lock) {
            Ok(file) => {
                if file.try_lock().is_ok() {
                    let _ = fs::remove_file(&path);
                    let _ = fs::remove_file(&lock);
                }
            }
            // No lock file at all: a row left by a build that locked the row itself, or one whose
            // owner died between the two removals in `Drop`. Nobody can be holding it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let _ = fs::remove_file(&path);
            }
            Err(_) => {}
        }
    }
}

/// A stand-in for another live window, for tests anywhere in the crate: a row under `pid` and its
/// lock, held by this process until the returned handle is dropped.
#[cfg(test)]
pub(crate) fn fake_live(cache_root: &Path, pid: u32, cwd: &str) -> fs::File {
    let dir = cache_root.join("instances");
    fs::create_dir_all(&dir).unwrap();
    let row = dir.join(format!("{pid}.json"));
    let file = fs::File::create(lock_path(&row)).unwrap();
    file.try_lock().unwrap();
    let text = serde_json::json!({ "pid": pid, "node_name": "app", "cwd": cwd });
    fs::write(&row, text.to_string()).unwrap();
    file
}

/// An advisory lock over a path, **shared between processes** — the in-process `Mutex` beside it
/// in `undo` and `edit` is not.
pub struct FileLock {
    /// Never read. Closing the handle is what releases the lock.
    _file: fs::File,
}

impl FileLock {
    /// Takes the lock, waiting up to `wait` for whoever holds it. `None` when it could not be
    /// taken inside that. **Not an error at the call sites**: whoever could not get it is about to
    /// do the same read-modify-write a moment later, and saying so would be noise.
    pub fn take(path: &Path, wait: Duration) -> Option<FileLock> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).ok()?;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // Said out loud: nothing is ever written through this handle, and truncating would only
            // make two processes race to empty a file neither reads.
            .truncate(false)
            .open(path)
            .ok()?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Some(FileLock { _file: file }),
                Err(fs::TryLockError::WouldBlock) => {
                    if started.elapsed() >= wait {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(fs::TryLockError::Error(error)) => {
                    tracing::warn!(path = %path.display(), "could not take the lock: {error}");
                    return None;
                }
            }
        }
    }

    /// `take`, for async code. **The wait runs on the blocking pool**: `take` sleeps the thread it
    /// is on for up to `wait`, and on a runtime worker that stalls every other task scheduled
    /// there — the UI's included — for as long as another window holds the file.
    pub async fn take_async(path: PathBuf, wait: Duration) -> Option<FileLock> {
        tokio::task::spawn_blocking(move || FileLock::take(&path, wait)).await.ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A window that registered is live; one that dropped is not.** This is the whole of the
    /// mechanism every other caller leans on.
    #[test]
    fn a_dropped_window_is_no_longer_live() {
        let dir = tempfile::tempdir().unwrap();
        let here = tempfile::tempdir().unwrap();
        let me = Instance::register(dir.path(), "app", here.path()).expect("it must register");
        assert!(is_live(dir.path(), std::process::id()));
        let rows = live(dir.path());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].node_name, "app");
        assert_eq!(rows[0].cwd, here.path());

        drop(me);
        assert!(!is_live(dir.path(), std::process::id()), "the row outlived the window");
        assert!(live(dir.path()).is_empty());
    }

    /// **A pid that never registered is not live.** An undo entry from a finished run must not
    /// look like a running window, or `/undo` would refuse to walk back into it.
    #[test]
    fn a_pid_nobody_registered_is_not_live() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_live(dir.path(), std::process::id()));
        assert!(!is_live(dir.path(), 1));
        // And an entry left behind by a process that is gone is cleared at the next registration.
        fs::create_dir_all(dir.path().join("instances")).unwrap();
        let stale = dir.path().join("instances").join("999999.json");
        fs::write(&stale, br#"{"pid":999999,"node_name":"gone","cwd":"/tmp"}"#).unwrap();
        assert!(!is_live(dir.path(), 999999));
        let here = tempfile::tempdir().unwrap();
        let me = Instance::register(dir.path(), "app", here.path()).unwrap();
        assert!(!stale.exists(), "a stale row was left to pile up");
        drop(me);
    }

    /// **The lock is held while the window lives and free afterwards, and the row stays readable
    /// throughout.** A second window asking cannot take the lock in the first case and can in the
    /// second. The row being readable is the Windows half: a locked file there cannot be read
    /// through another handle, and a locked row was a window nobody else could see.
    #[test]
    fn the_registry_lock_is_held_for_the_windows_life() {
        let dir = tempfile::tempdir().unwrap();
        let here = tempfile::tempdir().unwrap();
        let me = Instance::register(dir.path(), "app", here.path()).unwrap();
        let at = dir.path().join("instances").join(format!("{}.json", std::process::id()));

        let held = fs::OpenOptions::new().read(true).write(true).open(lock_path(&at)).unwrap();
        assert!(held.try_lock().is_err(), "another handle took a live window's lock");
        drop(held);
        let text = fs::read_to_string(&at).expect("a live window's row must be readable");
        assert!(text.contains("\"app\""), "{text}");

        drop(me);
        // Both files are removed on drop, so there is nothing left to hold — the same answer.
        assert!(!at.exists());
        assert!(!lock_path(&at).exists());
    }

    /// **A row whose lock file nobody holds is stale**, and the next registration clears both.
    #[test]
    fn a_row_with_a_free_lock_is_pruned() {
        let root = tempfile::tempdir().unwrap();
        // Written by hand and never locked: a lock taken and released here could still be held
        // for a moment by a child another test forks, which inherits the descriptor until exec.
        fs::create_dir_all(root.path().join("instances")).unwrap();
        let row = root.path().join("instances").join("4243.json");
        fs::write(&row, br#"{"pid":4243,"node_name":"gone","cwd":"/gone"}"#).unwrap();
        fs::write(lock_path(&row), b"").unwrap();
        assert!(!is_live(root.path(), 4243));

        let here = tempfile::tempdir().unwrap();
        let me = Instance::register(root.path(), "app", here.path()).unwrap();
        assert!(!row.exists() && !lock_path(&row).exists(), "a stale row was left behind");
        drop(me);
    }

    /// **Waiting for a held lock does not stall the runtime.** On a single-threaded runtime a
    /// thread-sleeping wait would keep the ticker below from running at all until it gave up.
    #[tokio::test(flavor = "current_thread")]
    async fn waiting_for_a_lock_leaves_the_runtime_free() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().join("x.lock");
        let held = FileLock::take(&at, Duration::ZERO).expect("a free lock");

        let ticks = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter = ticks.clone();
        let ticker = tokio::spawn(async move {
            loop {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        let got = FileLock::take_async(at.clone(), Duration::from_millis(300)).await;
        assert!(got.is_none(), "a held lock was taken");
        assert!(ticks.load(std::sync::atomic::Ordering::Relaxed) > 5, "the runtime was blocked");
        ticker.abort();

        drop(held);
        assert!(FileLock::take_async(at, Duration::ZERO).await.is_some());
    }

    /// Two different directories register side by side; the registry is keyed by pid, not by path,
    /// so both are visible at once. The other window is there **before** this one registers, so
    /// this one's `prune` must leave a row whose lock is held alone.
    #[test]
    fn two_windows_are_both_visible() {
        let root = tempfile::tempdir().unwrap();
        let other = fake_live(root.path(), 4242, "/elsewhere/app");
        let a = tempfile::tempdir().unwrap();
        let me = Instance::register(root.path(), "app", a.path()).unwrap();

        let rows = live(root.path());
        assert_eq!(rows.len(), 2, "{rows:?}");
        drop(other);
        drop(me);
    }
}
