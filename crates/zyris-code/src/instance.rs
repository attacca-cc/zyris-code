//! Which zyris-code windows are alive on this machine, and the locks that keep them out of each
//! other's way.
//!
//! **Several windows share one user's state.** The credentials, the settings, the undo history and
//! the working tree are all per-user, not per-window, so a second window can overwrite the first
//! one's work in ways neither of them can see. What was missing was any record of *which windows
//! are running*; with one, "is another window already here?" becomes a question with an answer.
//!
//! **Liveness is a held lock, not a timer and not a pid table.** Each window writes
//! `<cache>/instances/<pid>.json` and keeps an exclusive lock on it for as long as it lives. Any
//! other window asks whether an entry's owner is still there by trying to take that lock — it
//! succeeds only if the owner is gone. No clock, no `/proc`, no platform branch, and a machine
//! that is rebooted leaves nothing behind, because the lock died with the process.
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

/// One window's entry, held open and locked for as long as it lives. Dropping it releases both.
pub struct Instance {
    path: PathBuf,
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
        let path = dir.join(format!("{}.json", std::process::id()));
        let row = Row {
            pid: std::process::id(),
            node_name: node_name.to_string(),
            cwd: cwd.to_path_buf(),
        };
        let text = serde_json::to_vec(&row).ok()?;
        if crate::atomic::write_atomic(&path, &text, None).is_err() {
            return None;
        }
        let file = fs::OpenOptions::new().read(true).write(true).open(&path).ok()?;
        // Written a moment ago, and every stale row was just pruned, so nothing else can hold it.
        // A failure here means the filesystem grew a lock this process cannot take, and the honest
        // answer is then "not registered".
        file.try_lock().ok()?;
        Some(Instance { path, _file: file })
    }

    /// Where the entry lives, so a test can look at it.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
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
        // **A row is alive exactly while its lock is held.** We may take it ourselves only when
        // nobody else has it — the same test `prune` uses to decide a row is stale. Dropping the
        // handle at the end of the loop releases it again.
        let Ok(file) = fs::OpenOptions::new().read(true).write(true).open(&path) else { continue };
        if !matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock)) {
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
fn prune(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(file) = fs::OpenOptions::new().read(true).write(true).open(&path) else { continue };
        // **We could take the lock, so nobody holds it, so nobody is there.** The handle is let go
        // before the file is removed, so the removal is not fought by our own lock.
        if !matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock)) {
            drop(file);
            let _ = fs::remove_file(&path);
        }
    }
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

    /// **The lock is held while the window lives and free afterwards.** A second window asking
    /// cannot take it in the first case and can in the second.
    #[test]
    fn the_registry_lock_is_held_for_the_windows_life() {
        let dir = tempfile::tempdir().unwrap();
        let here = tempfile::tempdir().unwrap();
        let me = Instance::register(dir.path(), "app", here.path()).unwrap();
        let at = dir.path().join("instances").join(format!("{}.json", std::process::id()));

        let held = fs::OpenOptions::new().read(true).write(true).open(&at).unwrap();
        assert!(held.try_lock().is_err(), "another handle took a live window's lock");
        drop(held);

        drop(me);
        let free = fs::OpenOptions::new().read(true).write(true).open(&at);
        // The row is removed on drop, so the file itself is gone — which is the same answer.
        assert!(free.is_err());
    }

    /// Two different directories register side by side; the registry is keyed by pid, not by path,
    /// so both are visible at once.
    #[test]
    fn two_windows_are_both_visible() {
        let root = tempfile::tempdir().unwrap();
        let a = tempfile::tempdir().unwrap();
        let me = Instance::register(root.path(), "app", a.path()).unwrap();
        // A second row, written and locked by hand, stands in for a second process.
        let at = root.path().join("instances").join("4242.json");
        fs::write(&at, br#"{"pid":4242,"node_name":"app","cwd":"/elsewhere/app"}"#).unwrap();
        let file = fs::OpenOptions::new().read(true).write(true).open(&at).unwrap();
        file.try_lock().unwrap();

        let rows = live(root.path());
        assert_eq!(rows.len(), 2, "{rows:?}");
        drop(file);
        drop(me);
    }
}
