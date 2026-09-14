//! Where this node keeps its account credential between runs.
//!
//! **This layer used to be upstream's** — `zyris::enroll::store` and `zyris::enroll::file_store` —
//! and moved here when zyris became a library with no program layer of its own. The seam is
//! unchanged, because the reason for it is: what holds a credential varies far more than the
//! enrollment flow does. A laptop wants a file under `$HOME`, a pod wants a k8s Secret, a desktop
//! app wants the OS keychain, and a test wants nothing at all. That is a trait, not a path, which
//! is why [`CredentialStore`] is the seam and [`FileCredentialStore`] is merely the default behind
//! it.
//!
//! What gets stored is [`zyris::AccountCredential`], and **it is field-for-field the old
//! `StoredCredential` with the same `version = 1`**. That is the load-bearing fact of the whole
//! move: a `wss-<server>-<profile>.json` written by any earlier release of this app deserializes
//! into the new type unchanged, so **nobody is logged out by the migration**. Do not change the
//! JSON, and do not change the file name — see [`FileCredentialStore::for_server`].
//!
//! Be honest about the threat model of the file backend: a file on disk cannot be protected from
//! anyone who can `sudo -u` the account that owns it. For a shared service account the right answer
//! is a static `znt_` out of a secret manager, which is exactly why
//! [`StaticToken`](crate::runtime::credentials::StaticToken) stays supported. What this backend
//! *can* do is stop a credential from leaking through a permissive umask or a restored tarball,
//! and it does.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use zyris::AccountCredential;

/// Bumped only on an incompatible change. A file from the future is refused rather than guessed
/// at, because guessing wrong here means a node that authenticates as something unintended.
///
/// **This must stay equal to the version `zyris::AccountCredential::new` stamps.** Upstream keeps
/// its own copy of the number private, so there is nothing to import; the two agree at 1 today and
/// that agreement is the only reason existing credential files still load. If upstream ever bumps
/// its version, this has to move with it — otherwise every enrolled user is silently told to
/// enroll again.
const CREDENTIAL_VERSION: u32 = 1;
const LOCK_WAIT: Duration = Duration::from_secs(10);
const LOCK_POLL: Duration = Duration::from_millis(25);
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// What went wrong reaching a credential, in the only two shades the caller can act on.
///
/// The distinction is load-bearing: on startup a node *discards* a credential it cannot use and
/// enrolls again, but a credential it must not use is an operator's problem and has to be said out
/// loud. Collapsing the two would turn a leaked secret into a silent re-enrollment.
///
/// Written out rather than derived: upstream used `thiserror`, and one derive is not worth adding
/// a dependency this crate does not otherwise have. Same reasoning as
/// [`CredentialsError`](crate::runtime::credentials::CredentialsError) next door.
#[derive(Debug)]
pub enum CredentialStoreError {
    /// The credential may exist, but using it would be wrong and someone needs to know — a
    /// world-readable key file, a config directory that cannot be located. Never discarded silently.
    Refused(String),
    /// Unreadable, corrupt, or written by a version this build does not understand. Safe to throw
    /// away and enroll again.
    Unusable(String),
    /// Anything a third-party backend wants to report. Treated like [`Refused`](Self::Refused) —
    /// a store that fails in a way this crate cannot classify does not get its contents deleted.
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl std::fmt::Display for CredentialStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialStoreError::Refused(message) | CredentialStoreError::Unusable(message) => {
                f.write_str(message)
            }
            CredentialStoreError::Other(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for CredentialStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CredentialStoreError::Other(error) => Some(error.as_ref()),
            CredentialStoreError::Refused(_) | CredentialStoreError::Unusable(_) => None,
        }
    }
}

impl CredentialStoreError {
    pub fn other(error: impl std::error::Error + Send + Sync + 'static) -> CredentialStoreError {
        CredentialStoreError::Other(Box::new(error))
    }

    /// Whether the caller may respond by clearing the credential and starting over.
    pub fn is_discardable(&self) -> bool {
        matches!(self, CredentialStoreError::Unusable(_))
    }
}

/// Where this node's credentials live between runs.
///
/// Async because the interesting backends are: a keychain prompts, a secret manager is a network
/// call. The default file backend does blocking IO inside these methods, which is what shipped
/// before the trait existed and is bounded by one small read or write per process lifetime.
#[async_trait]
pub trait CredentialStore: Send + Sync + 'static {
    /// Hold an inter-process transaction boundary when this backend needs one. Non-file stores
    /// default to no lock; their own implementation already owns consistency.
    async fn lock(&self) -> Result<CredentialLock, CredentialStoreError> {
        Ok(CredentialLock { file: None })
    }
    /// The stored credential, or `None` when this node has never enrolled.
    async fn load(&self) -> Result<Option<AccountCredential>, CredentialStoreError>;
    /// Write, replacing whatever was there. Callers persist *before* using a credential, so a
    /// backend that can be atomic should be.
    async fn save(&self, credential: &AccountCredential) -> Result<(), CredentialStoreError>;
    /// Forget a credential the server will never honour again, so the next start enrolls cleanly
    /// instead of looping on it. Clearing nothing is success, not an error.
    async fn clear(&self) -> Result<(), CredentialStoreError>;
    /// Where this backend keeps things, for the one debug line a node logs at startup. Must never
    /// contain a secret — this is a path or a URL, not a token.
    fn describe(&self) -> String;
}

/// Released on drop, including cancellation and error paths.
pub struct CredentialLock {
    file: Option<fs::File>,
}

impl Drop for CredentialLock {
    fn drop(&mut self) {
        if let Some(file) = &self.file {
            let _ = fs::File::unlock(file);
        }
    }
}

/// Keeps a credential for exactly as long as the process lives.
///
/// It exists so the enrollment flow can be exercised end to end without touching a filesystem —
/// `enroll.rs`'s tests build every `AccountGrant` and `Reauth` on top of this. A node using it in
/// earnest re-enrolls on every restart, which is a real choice for a short-lived worker and a
/// mistake for anything else.
#[derive(Debug, Default)]
pub struct MemoryCredentialStore {
    held: std::sync::Mutex<Option<AccountCredential>>,
}

impl MemoryCredentialStore {
    pub fn new() -> MemoryCredentialStore {
        MemoryCredentialStore::default()
    }
}

#[async_trait]
impl CredentialStore for MemoryCredentialStore {
    async fn load(&self) -> Result<Option<AccountCredential>, CredentialStoreError> {
        Ok(self.held.lock().expect("credential mutex poisoned").clone())
    }

    async fn save(&self, credential: &AccountCredential) -> Result<(), CredentialStoreError> {
        *self.held.lock().expect("credential mutex poisoned") = Some(credential.clone());
        Ok(())
    }

    async fn clear(&self) -> Result<(), CredentialStoreError> {
        *self.held.lock().expect("credential mutex poisoned") = None;
        Ok(())
    }

    fn describe(&self) -> String {
        "memory (this credential is lost on restart)".to_string()
    }
}

/// What the file backend can fail with, before it is classified into a [`CredentialStoreError`].
///
/// Hand-written for the same reason as the type above: no `thiserror` in this crate.
#[derive(Debug)]
pub enum StoreError {
    NoConfigDir,
    Permissive { path: String, mode: u32 },
    UnknownVersion { found: u32 },
    Corrupt(serde_json::Error),
    Io(io::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NoConfigDir => {
                f.write_str("could not determine a config directory; set ZYRIS_CONFIG_DIR")
            }
            // Says the fix, not just the fault: the person reading this is at a shell.
            StoreError::Permissive { path, mode } => write!(
                f,
                "credential file {path} is readable by other users (mode {mode:04o}); \
                 run: chmod 600 {path}"
            ),
            StoreError::UnknownVersion { found } => write!(
                f,
                "credential file was written by a newer version \
                 ({found}, expected {CREDENTIAL_VERSION})"
            ),
            StoreError::Corrupt(error) => write!(f, "credential file is corrupt: {error}"),
            StoreError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Corrupt(error) => Some(error),
            StoreError::Io(error) => Some(error),
            StoreError::NoConfigDir
            | StoreError::Permissive { .. }
            | StoreError::UnknownVersion { .. } => None,
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> StoreError {
        StoreError::Io(error)
    }
}

impl From<StoreError> for CredentialStoreError {
    fn from(error: StoreError) -> CredentialStoreError {
        match error {
            // Both need a human: one is an exposed secret, the other is a machine with nowhere to
            // put one. Neither is a reason to quietly enroll again.
            StoreError::Permissive { .. } | StoreError::NoConfigDir => {
                CredentialStoreError::Refused(error.to_string())
            }
            // A file this build cannot read is a reason to enroll again, not to die. `Io` is here
            // rather than under `Other` deliberately: `NotFound` is already reported as "no
            // credential", so what remains is a file that exists and cannot be read, and looping on
            // it forever helps nobody.
            StoreError::UnknownVersion { .. } | StoreError::Corrupt(_) | StoreError::Io(_) => {
                CredentialStoreError::Unusable(error.to_string())
            }
        }
    }
}

/// A credential kept in one file, private to the user running the node.
pub struct FileCredentialStore {
    path: PathBuf,
}

impl FileCredentialStore {
    /// The conventional location for a given deployment URL and profile.
    ///
    /// One file per `(deployment, profile)` so two nodes started concurrently against different
    /// servers cannot clobber each other, and no locking is needed to make that true.
    ///
    /// **The name is not ours to change.** Every release of this app so far wrote
    /// `<server>-<profile>.json` by this exact rule, and `conn::migrate_credentials` recognises the
    /// files to move by the same suffix. Change it and a person with a perfectly good credential
    /// gets the enrollment screen.
    pub fn for_server(server_url: &str, profile: &str) -> Result<FileCredentialStore, StoreError> {
        Ok(FileCredentialStore { path: config_dir()?.join(file_name(server_url, profile)) })
    }

    /// An exact path, for a node whose location is decided by something other than this module.
    pub fn at(path: impl Into<PathBuf>) -> FileCredentialStore {
        FileCredentialStore { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn lock(&self) -> Result<CredentialLock, CredentialStoreError> {
        let lock_path = self.path.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(CredentialStoreError::other)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                    .map_err(CredentialStoreError::other)?;
            }
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // **Said out loud, because it is the intent and not a default.** Nothing is ever
            // written through this handle — it exists so `try_lock` has something to hold — and
            // truncating it would only mean two processes racing to empty a file neither reads.
            .truncate(false)
            .open(&lock_path)
            .map_err(CredentialStoreError::other)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(CredentialStoreError::other)?;
        }
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(CredentialLock { file: Some(file) }),
                Err(fs::TryLockError::WouldBlock) => {
                    if started.elapsed() >= LOCK_WAIT {
                        return Err(CredentialStoreError::Refused(format!(
                            "timed out waiting for credential lock {}; try again",
                            lock_path.display()
                        )));
                    }
                    tokio::time::sleep(LOCK_POLL).await;
                }
                Err(fs::TryLockError::Error(error)) => {
                    return Err(CredentialStoreError::other(error))
                }
            }
        }
    }

    async fn load(&self) -> Result<Option<AccountCredential>, CredentialStoreError> {
        Ok(load(&self.path)?)
    }

    async fn save(&self, credential: &AccountCredential) -> Result<(), CredentialStoreError> {
        Ok(save(&self.path, credential)?)
    }

    async fn clear(&self) -> Result<(), CredentialStoreError> {
        Ok(clear(&self.path)?)
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// The file name for a deployment and profile, split out of [`FileCredentialStore::for_server`] so
/// the naming rule can be pinned by a test without setting `$ZYRIS_CONFIG_DIR`. Environment
/// variables are process-global and the tests in this crate run beside `conn`'s, which reads that
/// same variable — `conn` refuses to set it in tests for exactly this reason.
fn file_name(server_url: &str, profile: &str) -> String {
    format!("{}-{}.json", slugify(server_url), slugify(profile))
}

/// `$ZYRIS_CONFIG_DIR`, else the platform's per-user config location.
///
/// **In this app the variable is always set**: `main.rs` fills it with `conn::credential_dir()`,
/// which is the one place that decides where this app's credentials live. The fallback branches
/// below still join `"zyris"` rather than `"zyris-code"` — that is the *old* shared location, the
/// one `conn::migrate_credentials` moves files out of, and inventing a new answer here would only
/// give this app a second opinion about a question `conn` already settles.
///
/// Under `systemd` with `ProtectHome=yes` there is no usable `$HOME`. That case fails loudly and
/// names the variable to set, rather than silently writing a credential into the working directory
/// — which is how secrets end up committed.
pub fn config_dir() -> Result<PathBuf, StoreError> {
    if let Some(dir) = std::env::var_os("ZYRIS_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    #[cfg(target_os = "macos")]
    let base =
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));

    base.map(|base| base.join("zyris")).ok_or(StoreError::NoConfigDir)
}

/// Read a stored credential, or `None` when there is none yet.
///
/// A file with group or other bits set is **refused**, the way `ssh` refuses a world-readable
/// private key. This is the cheapest possible mitigation for a credential restored from a tarball
/// or created under a permissive umask, and refusing is safer than silently repairing: the
/// operator should know it was exposed.
fn load(path: &Path) -> Result<Option<AccountCredential>, StoreError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(StoreError::Permissive { path: path.display().to_string(), mode });
        }
    }

    let credential: AccountCredential =
        serde_json::from_slice(&bytes).map_err(StoreError::Corrupt)?;
    if credential.version != CREDENTIAL_VERSION {
        return Err(StoreError::UnknownVersion { found: credential.version });
    }
    Ok(Some(credential))
}

/// Write atomically: temp file, `sync_all`, rename. A node killed mid-write must not come back to
/// a half-written credential, because that is indistinguishable from a corrupt one and would
/// force a re-enrollment that needed a human.
fn save(path: &Path, credential: &AccountCredential) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }

    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("credential");
    let temp = path.with_file_name(format!(".{name}.{}.{stamp}.{id}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(credential).map_err(StoreError::Corrupt)?;
    let written = (|| -> Result<(), StoreError> {
        use std::io::Write;
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Set before writing, so the secret is never briefly on disk under the umask's mode.
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// Forget a credential the server no longer honours, so the next start enrolls cleanly instead of
/// looping on a token that will never work again.
fn clear(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Reduce a URL or profile name to something safe as a filename component.
///
/// **`conn::slugify_profile` is a verbatim copy of this.** It has to be: `conn` recognises the
/// files to migrate out of the old shared directory by the suffix this produces, so if the two
/// rules ever diverge it walks past a file it should have moved and the person sees an enrollment
/// screen with no explanation. Change one, change both.
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential() -> AccountCredential {
        AccountCredential::new(
            "zna_access".into(),
            "znr_refresh".into(),
            "node-id".into(),
            "hello node".into(),
            "allen@example.com".into(),
            1_000_000,
        )
    }

    #[tokio::test]
    async fn memory_store_round_trips() {
        let store = MemoryCredentialStore::new();
        assert_eq!(store.load().await.unwrap(), None);
        store.save(&credential()).await.unwrap();
        assert_eq!(store.load().await.unwrap().unwrap(), credential());
        store.clear().await.unwrap();
        assert_eq!(store.load().await.unwrap(), None);
        // Clearing nothing is success, so a node that never enrolled can still take the
        // credential-was-rejected branch without dying on the cleanup.
        store.clear().await.unwrap();
    }

    /// The one classification the startup path branches on.
    #[test]
    fn only_unusable_may_be_thrown_away() {
        assert!(CredentialStoreError::Unusable("corrupt".into()).is_discardable());
        assert!(!CredentialStoreError::Refused("mode 0644".into()).is_discardable());
        assert!(
            !CredentialStoreError::other(std::io::Error::other("keychain locked")).is_discardable()
        );
    }

    /// `bearer` immediately before each dial is the whole mid-connection-expiry story, so the
    /// skew boundary is the thing worth pinning. The method is upstream's now — this stays as a
    /// characterization test, because our dial loop is built on the answer it gives.
    #[test]
    fn bearer_expires_early_by_the_skew_allowance() {
        let credential = credential();
        assert_eq!(credential.bearer(0, 30), Some("zna_access"));
        assert_eq!(credential.bearer(999_969, 30), Some("zna_access"));
        // 30 seconds out with a 30-second allowance: refuse, rather than race the handshake.
        assert_eq!(credential.bearer(999_970, 30), None);
        assert_eq!(credential.bearer(2_000_000, 30), None);
    }

    #[test]
    fn refresh_fires_at_eighty_percent_of_the_lifetime() {
        let credential = credential();
        // A one-hour token expiring at 1_000_000 was issued at 996_400; 80% of the way is 999_280.
        assert!(!credential.should_refresh(999_279, 3600));
        assert!(credential.should_refresh(999_280, 3600));
    }

    /// The credential written by every release before this migration must still load, which is
    /// only true while the JSON and the version number stay exactly as they were.
    #[test]
    fn a_credential_file_written_by_an_older_release_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wss-attacca-cc-zyris-v1-ws-zyris-code.json");
        fs::write(
            &path,
            br#"{"version":1,"access_token":"zna_access","refresh_token":"znr_refresh",
                "node_id":"node-id","node_name":"hello node","owner_email":"allen@example.com",
                "access_expires_at":1000000}"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(load(&path).unwrap().unwrap(), credential());
    }

    #[tokio::test]
    async fn save_then_load_round_trips_through_the_trait() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::at(dir.path().join("nested").join("creds.json"));
        assert_eq!(store.load().await.unwrap(), None, "a missing file is not an error");

        store.save(&credential()).await.unwrap();
        assert_eq!(store.load().await.unwrap().unwrap(), credential());

        store.clear().await.unwrap();
        assert_eq!(store.load().await.unwrap(), None);
        store.clear().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_refresh_rotates_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let store = FileCredentialStore::at(&path);
        store.save(&credential()).await.unwrap();

        let rotate = |path: PathBuf| async move {
            let store = FileCredentialStore::at(path);
            let _transaction = store.lock().await.unwrap();
            let mut current = store.load().await.unwrap().unwrap();
            if current.refresh_token != "znr_refresh" {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            current.refresh_token = "znr_rotated".into();
            store.save(&current).await.unwrap();
            true
        };
        let (first, second) = tokio::join!(rotate(path.clone()), rotate(path.clone()));

        assert_eq!(u8::from(first) + u8::from(second), 1);
        assert_eq!(store.load().await.unwrap().unwrap().refresh_token, "znr_rotated");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_is_private_and_a_permissive_one_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let store = FileCredentialStore::at(&path);
        store.save(&credential()).await.unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777, 0o700);

        // A credential restored from a tarball, or written under a lax umask, must be refused
        // rather than silently used — and refused in the shade that never gets thrown away, or the
        // node would answer an exposed secret by quietly enrolling again.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let error = store.load().await.unwrap_err();
        assert!(matches!(error, CredentialStoreError::Refused(_)));
        assert!(!error.is_discardable());
    }

    #[tokio::test]
    async fn a_file_from_the_future_is_refused_rather_than_guessed_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let mut future = credential();
        future.version = CREDENTIAL_VERSION + 1;
        save(&path, &future).unwrap();

        let error = FileCredentialStore::at(&path).load().await.unwrap_err();
        assert!(error.is_discardable(), "a file this build cannot read is re-enrollable");
    }

    #[tokio::test]
    async fn corrupt_json_is_discardable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        fs::write(&path, b"{not json").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(FileCredentialStore::at(&path).load().await.unwrap_err().is_discardable());
    }

    /// One file per (deployment, profile), so concurrent starts against different servers cannot
    /// clobber each other and no locking is needed.
    ///
    /// Upstream's version of this test set `$ZYRIS_CONFIG_DIR` around `for_server`. It does not
    /// here: the variable is process-global and `conn`'s tests read it, so setting it would make
    /// them skip. The naming rule is what mattered, and `file_name` is that rule.
    #[test]
    fn paths_are_per_deployment_and_per_profile() {
        let prod = file_name("wss://attacca.example/zyris/v1/ws", "default");
        let staging = file_name("wss://staging.attacca.example/zyris/v1/ws", "default");
        let other = file_name("wss://attacca.example/zyris/v1/ws", "builder");
        assert_ne!(prod, staging);
        assert_ne!(prod, other);

        let store = FileCredentialStore::at(Path::new("/cfg").join(&prod));
        assert!(store.path().starts_with("/cfg"));
        assert!(store.describe().ends_with(".json"));
    }

    /// The file name existing installs already have on disk. Spelled out rather than derived, so
    /// that a change to `slugify` has to change this line too and cannot pass unnoticed.
    #[test]
    fn the_name_on_disk_is_the_one_earlier_releases_wrote() {
        assert_eq!(
            file_name("wss://attacca.cc/api/zyris/v1/ws", "zyris-code"),
            "wss-attacca-cc-api-zyris-v1-ws-zyris-code.json"
        );
    }

    #[test]
    fn slugify_is_filename_safe() {
        assert_eq!(slugify("wss://attacca.example/zyris/v1/ws"), "wss-attacca-example-zyris-v1-ws");
        assert_eq!(slugify("///"), "default");
        assert_eq!(slugify(""), "default");
        assert!(!slugify("a/../../etc/passwd").contains('/'));
    }
}
