//! Writing a file so nobody ever reads half of it.
//!
//! **Several windows write the same user-level files** — `config.json`, `lang`, `github.json`,
//! `mcp-enabled.json` — and every one of them used to be a plain `std::fs::write`. That truncates
//! first: a reader that arrives in between sees an empty file, and `serde_json` answers "corrupt",
//! so the other window silently falls back to defaults and rewrites them over the first one's
//! change. Writing to a temporary file beside the target and renaming it over the target is one
//! `rename(2)`, which is atomic: a reader sees either the whole old file or the whole new one.
//!
//! **The mode is set before the bytes go in.** `github.json` holds tokens for every repository the
//! account can reach, and the old order — write, then `chmod 600` — left it readable under a
//! permissive umask for as long as the two calls were apart.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes two temp files made by the same process in the same millisecond.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path`, atomically: a temp file beside it, fsynced, then renamed over it.
///
/// `mode` is the Unix permission to create the temp file with — `Some(0o600)` for a file holding a
/// secret, `None` where the umask is the right answer. It is applied **before** the content is
/// written, so the secret is never briefly on disk under a laxer mode. On Windows it is ignored.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let temp = dir.join(format!(".{name}.{}.{}.tmp", std::process::id(), seq));

    let written = (|| -> io::Result<()> {
        use std::io::Write;
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
        file.write_all(bytes)?;
        file.sync_all()?;
        // Closed before the rename: Windows will not rename over a file another handle holds open.
        drop(file);
        fs::rename(&temp, path)
    })();

    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_written_is_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("file.json");
        write_atomic(&path, b"{\"a\":1}", None).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        // Overwriting replaces the content whole, and leaves no temp file behind.
        write_atomic(&path, b"short", None).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "short");
        let left: Vec<String> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
        assert_eq!(left, vec!["file.json".to_string()], "{left:?}");
    }

    /// **The mode is on before the bytes are.** A token file written and then chmod-ed is readable
    /// by anybody under a permissive umask for as long as the two calls are apart.
    #[cfg(unix)]
    #[test]
    fn a_secret_is_private_from_the_first_byte() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("github.json");
        write_atomic(&path, b"{\"token\":\"x\"}", Some(0o600)).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // And rewriting it does not lose that.
        write_atomic(&path, b"{\"token\":\"y\"}", Some(0o600)).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
