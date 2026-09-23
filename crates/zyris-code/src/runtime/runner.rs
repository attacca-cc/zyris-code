//! The dial/reconnect loop this node runs, so nothing above it has to write one.
//!
//! Nothing here is clever, and that is the point: backoff with jitter, a healthy connection resets
//! it, one re-enrollment when Attacca refuses the credential, graceful shutdown on `Ctrl-C`, and
//! exit codes a supervisor can act on. Each of those is a small decision that is easy to get subtly
//! wrong once and then carry forever — a node pinned at the backoff ceiling after a nightly server
//! restart, a restart loop printing enrollment codes into a log nobody reads.
//!
//! **Why this loop exists when the library has `Node::connect`.** `connect` redials with one fixed
//! credential behind a `Link` and ends on a 401. A `zc_` never expires, so the fixed credential is
//! fine; the 401 is not. Attacca answers a revoked credential — and every `zna_`/`znt_` an earlier
//! build stored — with exactly that, and this app's answer is to forget the credential and draw a
//! fresh enrollment code, which only a loop that asks [`Credentials::bearer`] before every dial can
//! do.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use zyris::{Capabilities, ConnectError, Connection, Node, NodeKind};

use crate::runtime::credentials::{token_prefix, Credentials, CredentialsError};

/// A connection that stayed up this long counts as healthy, so its eventual drop restarts the
/// backoff from the bottom. Without this a nightly server restart would leave every node pinned at
/// the ceiling forever.
const DEFAULT_STABLE_AFTER: Duration = Duration::from_secs(30);
const DEFAULT_BACKOFF_MIN: Duration = Duration::from_secs(1);
const DEFAULT_BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Long enough for a closing frame to reach the server, so it retires the node promptly instead of
/// waiting for the heartbeat to lapse.
const CLOSE_GRACE: Duration = Duration::from_millis(200);

/// Everything a node needs to know before it dials.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// The websocket URL. The enrollment endpoints are derived from it, so this is the only address
    /// a node is configured with.
    pub url: String,
    pub node_name: String,
    pub kind: NodeKind,
    /// Names the credential file, so one machine can hold separate identities against the same
    /// deployment without them clobbering each other.
    pub profile: String,
    /// What this node *asks* for at enrollment. The user narrows it at approval time and may grant
    /// none of it.
    ///
    /// **This is settled before anything reads it and there is no builder method to change it
    /// later.** Upstream had `Runner::request_scopes`, which existed because the old `Runner`
    /// resolved its credential source lazily; a device grant copies the scopes it will ask for when
    /// it is built, so the setter had to run before that. Here the credential source is built by
    /// `enroll::source` from this very config, which is why `main.rs` writes `$ZYRIS_SCOPES`
    /// before it calls [`RunConfig::from_env`]. Reverse that order and the approval screen appears
    /// asking for nothing.
    pub scopes: Vec<String>,
    /// Set when `$ZYRIS_SCOPES` was present. Read through [`Self::scopes_pinned()`].
    scopes_pinned: bool,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    pub stable_after: Duration,
}

impl Default for RunConfig {
    fn default() -> RunConfig {
        RunConfig {
            url: zyris::DEFAULT_SERVER_URL.to_string(),
            // Upstream forked this on its own `hostname` feature. There is no fork to keep here:
            // a crate cannot `cfg` on a dependency's feature, and `hostname` is one of `zyris`'s
            // defaults, which this repo does not turn off. `main.rs` sets `$ZYRIS_NODE_NAME` from
            // `conn::default_node_name()` anyway, so this is the floor under that, not the answer.
            node_name: zyris::machine_name().unwrap_or_else(|| "zyris-node".to_string()),
            kind: NodeKind::Service,
            profile: "default".to_string(),
            // Empty on purpose. A node that announces tools needs no access to its owner's account,
            // and a default that asks for some would have every node asking by accident.
            scopes: Vec::new(),
            scopes_pinned: false,
            backoff_min: DEFAULT_BACKOFF_MIN,
            backoff_max: DEFAULT_BACKOFF_MAX,
            stable_after: DEFAULT_STABLE_AFTER,
        }
    }
}

impl RunConfig {
    /// Read the `ZYRIS_*` variables, falling back to [`RunConfig::default`] for each.
    ///
    /// | Variable | Falls back to |
    /// |---|---|
    /// | `ZYRIS_SERVER_URL` | `zyris::DEFAULT_SERVER_URL` |
    /// | `ZYRIS_NODE_NAME` | this machine's hostname |
    /// | `ZYRIS_PROFILE` | `default` |
    /// | `ZYRIS_SCOPES` | nothing |
    pub fn from_env() -> RunConfig {
        let default = RunConfig::default();
        let scopes = std::env::var("ZYRIS_SCOPES").ok().map(|raw| {
            raw.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
        });
        RunConfig {
            url: env_or("ZYRIS_SERVER_URL", default.url),
            node_name: env_or("ZYRIS_NODE_NAME", default.node_name),
            profile: env_or("ZYRIS_PROFILE", default.profile),
            scopes_pinned: scopes.is_some(),
            scopes: scopes.unwrap_or(default.scopes),
            ..default
        }
    }

    /// Whether `$ZYRIS_SCOPES` is where [`scopes`](Self::scopes) came from.
    ///
    /// Kept even though nothing in this app branches on it yet, because it is the only record of
    /// *who decided*: an operator who set the variable outranks any compiled-in default, and code
    /// that overrides it without checking here would make the variable a lie. The flag stays
    /// private and this reads it, so the distinction cannot be lost by a field going unused.
    pub fn scopes_pinned(&self) -> bool {
        self.scopes_pinned
    }

    /// What this machine reports itself as at enrollment. A hint the server never verifies.
    pub fn platform(&self) -> &'static str {
        match std::env::consts::OS {
            "linux" => "linux",
            "macos" => "macos",
            "windows" => "windows",
            _ => "other",
        }
    }
}

fn env_or(key: &str, fallback: String) -> String {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty()).unwrap_or(fallback)
}

/// A node could not be started, or could not keep running.
///
/// Written out rather than derived, the same way [`CredentialsError`] is: `thiserror` would be a
/// dependency bought for three `Display` lines.
#[derive(Debug)]
pub enum RunError {
    Credentials(CredentialsError),
    /// The node itself could not be assembled — two capabilities claiming one name, a descriptor
    /// the wire rejects. **Nothing in this file produces it any more**: the node is built by the
    /// caller through `zyris::NodeBuilder` now (see [`Runner::new`]). It stays so that caller can
    /// report a build failure through the same exit-code table as everything else here, instead of
    /// inventing a second one.
    Build(zyris::Error),
    /// The server refused this node in a way retrying will not fix — a revoked credential, an
    /// unsupported protocol version.
    Refused(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Credentials(error) => write!(f, "{error}"),
            RunError::Build(error) => write!(f, "could not build the node: {error}"),
            RunError::Refused(reason) => write!(f, "the server refused this node: {reason}"),
        }
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RunError::Credentials(error) => Some(error),
            RunError::Build(error) => Some(error),
            RunError::Refused(_) => None,
        }
    }
}

impl From<CredentialsError> for RunError {
    fn from(error: CredentialsError) -> RunError {
        RunError::Credentials(error)
    }
}

impl From<zyris::Error> for RunError {
    fn from(error: zyris::Error) -> RunError {
        RunError::Build(error)
    }
}

impl RunError {
    /// Exit 2 when a human has to do something, 1 otherwise.
    ///
    /// The distinction is what stops a supervisor from restart-looping on a condition no restart
    /// can fix, printing a fresh enrollment code into a log nobody reads each time around.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            RunError::Credentials(CredentialsError::NeedsOperator(_)) => ExitCode::from(2),
            RunError::Build(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        }
    }
}

/// What a refused dial means to this loop.
///
/// [`Refusal::of`] is **exhaustive on [`ConnectError`] on purpose**: a variant upstream adds later
/// must stop the build here rather than be quietly folded into "unreachable, back off" — that is
/// the folding that turns a revocation into a node reconnecting forever and never coming back.
/// Upstream keeps the same habit in its own `Ending::of`, and for the same reason.
enum Refusal {
    /// Attacca will not take this credential. Worth forgetting it and enrolling once more.
    Reenroll,
    /// Retrying gets the same answer. A different build or a different person fixes it.
    Fatal,
    /// The network or the server, briefly. Back off and dial again.
    Backoff,
}

impl Refusal {
    fn of(error: &ConnectError) -> Refusal {
        match error {
            // A revoked credential, one typed wrong, and one from before credentials existed
            // (`zna_`/`znt_`) all arrive as 401, or as `Revoked` when the revocation closes a live
            // connection. The device-grant source answers by forgetting the credential, so the next
            // `bearer` enrolls and the following dial presents a fresh one.
            ConnectError::Unauthorized | ConnectError::Revoked => Refusal::Reenroll,
            // A build speaking the wrong major will speak it just as wrong in a second; a build
            // with no TLS provider compiled in cannot grow one at runtime.
            ConnectError::VersionMismatch { .. } | ConnectError::NoTlsProvider => Refusal::Fatal,
            ConnectError::Unreachable(_) => Refusal::Backoff,
        }
    }
}

type ConnectHook = Arc<dyn Fn(Connection) -> BoxFuture<'static, ()> + Send + Sync>;

/// A node, its credentials, and the loop that keeps them connected.
pub struct Runner {
    config: RunConfig,
    node: Node,
    credentials: Arc<dyn Credentials>,
    on_connect: Option<ConnectHook>,
}

impl Runner {
    /// Take an already-built node and keep it dialled.
    ///
    /// **The node arrives assembled, which is the one shape change from upstream.** The old
    /// `Runner` collected capabilities itself into a private `CapabilitySet` and built the node at
    /// `try_run`. That type is not public any more, and the public replacement,
    /// `zyris::Capabilities::add`, is `async` — so there is no way to collect capabilities behind a
    /// synchronous builder method on this type. `zyris::NodeBuilder` is synchronous and does
    /// exactly that job, so `capability`, `capability_arc` and `kind` are its methods now, not
    /// this one's. `tools::announce` takes and returns that builder.
    ///
    /// Building outside the loop is still load-bearing for the same reason it was upstream: a
    /// `Node` is reusable across connections and owns the capability impls, so rebuilding one per
    /// attempt would hand every reconnect a freshly initialised capability that had forgotten
    /// whatever the last one knew.
    pub fn new(config: RunConfig, node: Node, credentials: Arc<dyn Credentials>) -> Runner {
        Runner { config, node, credentials, on_connect: None }
    }

    pub fn config(&self) -> &RunConfig {
        &self.config
    }

    /// What this node offers, available before [`run`](Self::run) so an application can hold onto
    /// it and change the set while the node is connected. See [`zyris::Capabilities`].
    pub fn capabilities(&self) -> Capabilities {
        self.node.capabilities()
    }

    /// Run once per established connection, concurrently with the connection itself.
    ///
    /// This is the *consume* half of a node: the server announces its own capabilities on the same
    /// websocket, so a node is not only a tool provider. The hook is spawned and its outcome is
    /// ignored — a node whose token lacks a scope should still serve the tools it announced.
    ///
    /// `zyris::NodeBuilder::on_connect` exists too and fires on the link's own reconnects. This one
    /// is the hook for *this* loop's connections, which is the only kind this app makes.
    pub fn on_connect<F, Fut>(mut self, hook: F) -> Self
    where
        F: Fn(Connection) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.on_connect = Some(Arc::new(move |conn| Box::pin(hook(conn))));
        self
    }

    /// Connect, stay connected, and translate whatever happens into an exit code.
    ///
    /// Returns only when the node is shut down deliberately (`Ctrl-C`, success) or hits something
    /// no retry can fix. Every failure along the way is logged as it happens, so the caller has
    /// nothing left to report.
    pub async fn run(self) -> ExitCode {
        match self.try_run().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                tracing::error!(%error, "zyris node stopped");
                error.exit_code()
            }
        }
    }

    /// [`run`](Self::run) without the exit-code translation, for a caller that has its own idea of
    /// what to do when a node gives up. `main.rs` is one: it has a screen to tear down first.
    pub async fn try_run(self) -> Result<(), RunError> {
        let credentials = self.credentials.clone();

        // `capabilities` is counted through `descriptors()` because `Capabilities::len` is
        // upstream-private. One descriptor is one capability, so the number is the one the old
        // line printed; the vector it builds to get there is paid for once, at startup.
        tracing::info!(
            node = %self.config.node_name,
            url = %self.config.url,
            credentials = %credentials.describe(),
            scopes = ?self.config.scopes,
            capabilities = self.node.capabilities().descriptors().len(),
            built = %build_line(),
            "starting zyris node"
        );

        let mut backoff = self.config.backoff_min;
        // Whether a refusal has already been answered with a re-enrollment, so a server that refuses
        // even a credential just approved still ends the process.
        let mut reenrolled_after_refusal = false;

        loop {
            // Freshness is decided immediately before each dial rather than by a timer task: a
            // token that is valid now is valid for the handshake, and a connection that outlives
            // its token is handled server-side by the heartbeat.
            let bearer = match credentials.bearer().await {
                Ok(bearer) => bearer,
                // The credential *source* is unreachable — a Secret not mounted yet, a vault
                // restarting, an enrollment server that timed out. That is a startup race, not a
                // dead node, so it backs off like any other transient failure instead of killing
                // the process.
                Err(e @ CredentialsError::Unavailable(_)) => {
                    tracing::warn!(error = %e, "credential source unavailable");
                    backoff = self.wait_then_widen(backoff).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            tracing::info!(token = %token_prefix(&bearer), "connecting");
            // `dial`, not `connect`: one attempt, and the loop around it is ours. See the module
            // comment for why.
            let error = match self.node.dial(&self.config.url, &bearer).await {
                Ok(conn) => {
                    reenrolled_after_refusal = false;
                    let up = Instant::now();
                    tracing::info!(
                        node_id = %conn.info().node_id,
                        conn_id = %conn.info().conn_id,
                        "connected"
                    );
                    if let Some(hook) = &self.on_connect {
                        tokio::spawn(hook(conn.clone()));
                    }
                    if self.serve_until_closed(&conn).await {
                        return Ok(());
                    }
                    if up.elapsed() >= self.config.stable_after {
                        backoff = self.config.backoff_min;
                    }
                    // The same wait the refusal paths below take. Spelled out here rather than
                    // shared at the bottom of the loop so the classification match stays flat.
                    backoff = self.wait_then_widen(backoff).await;
                    continue;
                }
                Err(error) => error,
            };

            match Refusal::of(&error) {
                Refusal::Reenroll if !reenrolled_after_refusal => {
                    tracing::warn!(%error, "credential refused; forgetting it and enrolling once more");
                    reenrolled_after_refusal = true;
                    match credentials.forget_refused().await {
                        // The next `bearer` enrolls, or adopts a credential another window has just
                        // written, so dial again at once.
                        Ok(true) => continue,
                        Ok(false) => return Err(RunError::Refused(error.to_string())),
                        // Nothing was forgotten, so the one attempt is not spent.
                        Err(forget_error @ CredentialsError::Unavailable(_)) => {
                            tracing::warn!(error = %forget_error, "could not forget the credential");
                            reenrolled_after_refusal = false;
                        }
                        Err(forget_error) => return Err(forget_error.into()),
                    }
                }
                // The one re-enrollment is spent, so this really is the end. Saying so is what
                // stops a supervisor restart-looping on it.
                Refusal::Reenroll | Refusal::Fatal => {
                    return Err(RunError::Refused(error.to_string()))
                }
                Refusal::Backoff => tracing::warn!(%error, "connect failed"),
            }

            backoff = self.wait_then_widen(backoff).await;
        }
    }

    /// Hold a live connection until it drops or the operator interrupts. `true` means shut down.
    async fn serve_until_closed(&self, conn: &Connection) -> bool {
        tokio::select! {
            reason = conn.closed() => {
                tracing::warn!(%reason, "disconnected");
                false
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutting down");
                conn.close("node shutting down");
                tokio::time::sleep(CLOSE_GRACE).await;
                true
            }
        }
    }

    async fn wait_then_widen(&self, backoff: Duration) -> Duration {
        let wait = jitter(backoff);
        tracing::info!(seconds = wait.as_secs_f64(), "reconnecting");
        tokio::time::sleep(wait).await;
        (backoff * 2).min(self.config.backoff_max)
    }
}

/// ±20%, so a server restart does not bring every node back in the same instant.
///
/// `RandomState` rather than an `rand` dependency: a fresh one hashes differently on every call,
/// which is the entire requirement here. Backoff jitter does not need a real RNG, and this app has
/// no other use for one.
fn jitter(base: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u8(0);
    let factor = 0.8 + (hasher.finish() % 400) as f64 / 1000.0;
    base.mul_f64(factor)
}

/// **Which binary is running, and how old it is** — one line in the log, and an answer that until
/// now nothing could give.
///
/// A `zyris-code` on `$PATH` can be a build from days ago, and nothing said so: the version is the
/// same for every build of a release, so "the fix did not work" can really mean "the fix was never
/// in the binary that ran". That happened on this machine — a copy in `~/.local/bin` predated a
/// whole round of work, and the running process was yet another inode that `cargo build` had since
/// replaced. The file the process was started from is `current_exe()` (`/proc/self/exe` on Linux)
/// and its modification time is when that build landed, so printing the path and the age together
/// answers "am I testing what I just built?" in one line of `/tmp/zyris-code.log`.
fn build_line() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return "unknown".to_string();
    };
    let age = std::fs::metadata(&exe)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| std::time::SystemTime::now().duration_since(at).ok())
        .map(|age| ago(age.as_secs()))
        .unwrap_or_else(|| "?".to_string());
    format!("{} ({age} old)", exe.display())
}

/// `3s`, `1m`, `1h07m`, `3d` — how long ago something was written, coarsely. **Coarse on
/// purpose:** this is read to answer "is this a fresh build?", not to do arithmetic.
fn ago(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    /// The stamp is coarse and that is the point — it answers "is this the build I just made?".
    #[test]
    fn the_binarys_age_is_reported_coarsely() {
        assert_eq!(super::ago(3), "3s");
        assert_eq!(super::ago(90), "1m");
        assert_eq!(super::ago(3600), "1h00m");
        assert_eq!(super::ago(3600 + 7 * 60), "1h07m");
        assert_eq!(super::ago(86_400 * 3), "3d");
    }

    use super::*;

    #[test]
    fn jitter_stays_within_twenty_percent_and_actually_varies() {
        let base = Duration::from_secs(10);
        let samples: Vec<Duration> = (0..64).map(|_| jitter(base)).collect();
        for sample in &samples {
            assert!(*sample >= base.mul_f64(0.8) && *sample <= base.mul_f64(1.2), "{sample:?}");
        }
        assert!(
            samples.iter().any(|s| *s != samples[0]),
            "every node backing off by the same amount is the thundering herd this exists to avoid"
        );
    }

    #[test]
    fn defaults_point_at_the_default_deployment() {
        let config = RunConfig::default();
        assert_eq!(config.url, zyris::DEFAULT_SERVER_URL);
        assert!(config.scopes.is_empty(), "a bare default must not ask for account access");
        assert!(!config.scopes_pinned(), "nobody has said anything about scopes yet");
        assert!(!config.node_name.is_empty());
    }

    /// The two mistakes are not symmetric: reading an outage as a revocation drags a person back to
    /// a terminal, and reading a revocation as an outage is a node that reconnects forever and
    /// never comes back. This is the table that keeps them apart.
    #[test]
    fn a_refusal_is_classified_by_what_another_dial_could_possibly_change() {
        let reenroll = [ConnectError::Unauthorized, ConnectError::Revoked];
        for error in reenroll {
            assert!(matches!(Refusal::of(&error), Refusal::Reenroll), "{error}");
        }

        let mismatch =
            ConnectError::VersionMismatch { ours: "1".to_string(), theirs: Some("2".to_string()) };
        let fatal = [mismatch, ConnectError::NoTlsProvider];
        for error in fatal {
            assert!(matches!(Refusal::of(&error), Refusal::Fatal), "{error}");
        }

        let blip = ConnectError::Unreachable(zyris::TransportError::Closed);
        assert!(matches!(Refusal::of(&blip), Refusal::Backoff), "{blip}");
    }
}
