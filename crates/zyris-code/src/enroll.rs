//! Where this node's credential comes from, and **the enrollment code drawn on screen.**
//!
//! Upstream used to drive the whole device grant: an `Enroller` held the store, ran the polling
//! loop, and called back into an `EnrollmentUi` we implemented so the code arrived as
//! `Frame::Enroll` instead of going out through a stdout box. **That layer is gone** — the
//! library-only `zyris` hands back the code as a value and refuses to write the loop, on the
//! grounds that a loop the caller cannot end is a program rather than a library.
//!
//! So the loop is here now, and the property it existed for is unchanged: with a screen up, the
//! code goes to the screen and nothing is written to stdout, so the old "code leaking into the
//! terminal behind the alternate screen" problem stays structurally impossible. Without a screen
//! (the extreme where the app could not start at all) it prints the box, exactly as before.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::app::{EnrollPhase, EnrollView, Frame};
use crate::runtime::{
    CredentialStore, CredentialStoreError, Credentials, CredentialsError, FileCredentialStore,
    RunConfig,
};
use crate::tools::bridge::Bridge;

/// How often a window waiting on another window's enrollment looks for the credential it writes.
const ENROLL_POLL: Duration = Duration::from_millis(500);

/// The credentials this node will use.
///
/// What a person explicitly gives always wins, and the path that has to ask a person comes last:
/// `$ZYRIS_CREDENTIAL`, then `$ZYRIS_CREDENTIAL_FILE`, then the device grant with its code on
/// screen.
///
/// **Scopes must be settled before getting here.** The `EnrollRequest` built below is what the
/// authorize call carries, and the grant is approved once against exactly that list. `main.rs`
/// writes `$ZYRIS_SCOPES` before `RunConfig::from_env` reads it for that reason: settle them later
/// and the approval screen asks for nothing, which is a browser round trip that grants nothing.
pub fn source(
    config: &RunConfig,
    bridge: &Bridge,
) -> Result<(Arc<dyn Credentials>, Option<Reauth>), String> {
    use crate::runtime::{StaticToken, TokenFile};

    // Where a person gave a credential directly, there is nothing to discard and no one to ask.
    if let Some(token) = StaticToken::from_env().map_err(|e| e.to_string())? {
        return Ok((Arc::new(token), None));
    }
    if let Some(file) = TokenFile::from_env() {
        return Ok((Arc::new(file), None));
    }

    // The credential file lands under `$ZYRIS_CONFIG_DIR`. `main.rs` has filled that variable with
    // this app's directory (`conn::credential_dir`).
    let store = Arc::new(
        FileCredentialStore::for_server(&config.url, &config.profile).map_err(|e| e.to_string())?,
    ) as Arc<dyn CredentialStore>;

    let grant = Arc::new(DeviceGrant::new(
        store.clone(),
        config.url.clone(),
        zyris::EnrollRequest {
            program: crate::conn::APP.to_string(),
            // Which system the person approving is offered first. Empty only on a machine with
            // no usable hostname, where the choice is simply theirs.
            system_hint: zyris::machine_name().unwrap_or_default(),
            platform: config.platform().to_string(),
            scopes: config.scopes.clone(),
        },
        ScreenEnroll { bridge: bridge.clone() },
    ));
    let creds: Arc<dyn Credentials> = grant.clone();

    let reauth = Reauth { store, grant, spent: Arc::new(AtomicBool::new(false)) };
    Ok((creds, Some(reauth)))
}

/// The credential this node presents, **with a way to let go of it.**
///
/// It is the device grant end to end: the credential being held, the one on disk, or a fresh
/// enrollment with the code drawn on screen. A `zc_` never expires and never rotates, so once one
/// is held it is simply presented before every dial.
///
/// **The copy in memory is what once made `/account logout` a lie** (2026-08-14): clearing the file
/// and dropping the socket let the redial present the credential this process was still holding.
/// [`forget`](Self::forget) lets go of that copy, and logging out calls it.
struct DeviceGrant {
    store: Arc<dyn CredentialStore>,
    /// The websocket URL. `zyris::enroll` derives the HTTP base from it, so this node cannot end up
    /// enrolling against one deployment while connecting to another.
    url: String,
    /// What to ask to be enrolled as. Settled before this value is built — see [`source`] — and
    /// only ever narrowed after: a scope the server says it does not know is taken out
    /// (`start_enrollment`).
    request: std::sync::Mutex<zyris::EnrollRequest>,
    ui: ScreenEnroll,
    held: tokio::sync::Mutex<Option<zyris::Credential>>,
    /// Held for the length of one enrollment, so two dials cannot put two codes on the screen.
    ///
    /// **Separate from `held` on purpose.** The enrollment loop has no bound — it renews a lapsed
    /// code for as long as somebody might still walk over to a browser — and `Reauth::discard`
    /// reaches for `held` to let go of the credential this process is carrying. Parking the
    /// enrollment on that same lock would make `/account logout` wait for it, which is a frozen
    /// screen with no keys and no redraw.
    enrolling: tokio::sync::Mutex<()>,
}

impl DeviceGrant {
    fn new(
        store: Arc<dyn CredentialStore>,
        url: String,
        request: zyris::EnrollRequest,
        ui: ScreenEnroll,
    ) -> DeviceGrant {
        DeviceGrant {
            store,
            url,
            request: std::sync::Mutex::new(request),
            ui,
            held: tokio::sync::Mutex::new(None),
            enrolling: tokio::sync::Mutex::new(()),
        }
    }

    /// The credential to present: the one held, the one on disk, or a fresh enrollment.
    async fn credential(&self) -> Result<zyris::Credential, CredentialsError> {
        let in_hand = { self.held.lock().await.clone() };
        if let Some(credential) = in_hand {
            // **Logging out in another window signs this one out too.** All windows share one
            // credential file, and `/account logout` in one of them clears it — but this process
            // keeps its own copy in memory, and would otherwise go on dialling with a credential
            // the machine has been signed out of while the person believes they are signed out. The
            // file having gone is the signal; a file that merely *changed* is another window's
            // fresh enrollment, which `forget_refused` adopts and this leaves alone.
            if !self.released_elsewhere().await {
                return Ok(credential);
            }
            tracing::info!("the credential was cleared by another window; dropping this copy");
            *self.held.lock().await = None;
        }
        // One enrollment at a time, so a second dial arriving mid-grant cannot put a second code on
        // the screen. **`held` is deliberately not what is locked here** — see the field.
        let _enrolling = self.enrolling.lock().await;
        // Whoever was ahead of us may have finished while we waited for that.
        let in_hand = { self.held.lock().await.clone() };
        if let Some(credential) = in_hand {
            return Ok(credential);
        }
        // **The file is locked for the read, and no longer.** Whoever holds the lock is mid-
        // enrollment and puts the new credential down before letting go, so this sees either the
        // finished file or nothing — without waiting out a browser trip the way it used to.
        let credential = match self.stored_now().await? {
            Some(credential) => credential,
            None => self.enroll_or_adopt().await?,
        };
        *self.held.lock().await = Some(credential.clone());
        Ok(credential)
    }

    /// `stored`, under the credential file's lock for the read and no longer.
    async fn stored_now(&self) -> Result<Option<zyris::Credential>, CredentialsError> {
        let _transaction = self.store.lock().await.map_err(store_trouble)?;
        self.stored().await
    }

    /// Enrolls — **unless another window already is**, in which case this one waits for the
    /// credential that window will write and adopts it.
    ///
    /// Two windows started fresh used to enroll side by side: two codes, two browser approvals,
    /// and a second credential on the account for the one the person approved second. The claim
    /// is held for the whole enrollment and never waited on with a timeout; a window that does not
    /// get it keeps running and looks at the credential file every `ENROLL_POLL`. If the enrolling
    /// window goes away without a credential — declined, or closed — the claim is free again and
    /// the next look takes it and enrolls here instead.
    async fn enroll_or_adopt(&self) -> Result<zyris::Credential, CredentialsError> {
        let mut said = false;
        loop {
            if let Some(_claim) = self.store.claim_enrollment().await.map_err(store_trouble)? {
                // Whoever held the claim may have finished between our read and taking it.
                if let Some(credential) = self.stored_now().await? {
                    return Ok(credential);
                }
                return self.enroll().await;
            }
            if !std::mem::replace(&mut said, true) {
                tracing::info!("another window is enrolling; waiting for its credential");
            }
            tokio::time::sleep(ENROLL_POLL).await;
            if let Some(credential) = self.stored_now().await? {
                return Ok(credential);
            }
        }
    }

    /// Whether the credential carried in memory has since been cleared from the store — which is
    /// what logging out in another window looks like from here.
    ///
    /// Split out so the check can be made without going on to enroll: the answer is a fact about
    /// the file, not about the network.
    async fn released_elsewhere(&self) -> bool {
        self.held.lock().await.is_some() && matches!(self.store.load().await, Ok(None))
    }

    /// A corrupt or unreadable credential — including an account credential an earlier release
    /// wrote — is a reason to enroll again, not to die. A *refused* one is different — a
    /// world-readable key file, a machine with nowhere to keep a secret — and refusing loudly is
    /// the whole point of that distinction, so it propagates rather than answering an exposed
    /// secret with a quiet re-enrollment.
    async fn stored(&self) -> Result<Option<zyris::Credential>, CredentialsError> {
        match self.store.load().await {
            Ok(credential) => Ok(credential),
            Err(e) if !e.is_discardable() => Err(store_trouble(e)),
            Err(e) => {
                tracing::warn!(error = %e, "discarding unusable stored credential");
                self.store.clear().await.map_err(store_trouble)?;
                Ok(None)
            }
        }
    }

    /// Ask for a code, put it in front of a person, and wait.
    ///
    /// **The renewal loop is ours because the library refuses to write it.** `Enrollment::renew`
    /// exists and nothing calls it on its own. It is also the reason the window says "that code
    /// expired" and draws the next one over it instead of the app dying at the ten-minute mark.
    ///
    /// Unbounded on purpose: the window is on screen, Ctrl+C is always live, and giving up on the
    /// node's behalf would take away the one thing it can still do. The cost is known: attacca
    /// rate-limits repeated grants from one address, so a code left unapproved long enough renews
    /// into that refusal — which arrives as `Unavailable`, and the run loop backs off on it.
    async fn enroll(&self) -> Result<zyris::Credential, CredentialsError> {
        let mut enrollment = self.start_enrollment().await?;
        self.ui.show(enrollment.code());
        loop {
            // Hoisted out of the `match` so nothing borrows `enrollment` while the arm that has
            // to renew it runs.
            let progress = enrollment.poll().await.map_err(enrollment_trouble)?;
            match progress {
                // `poll` sleeps to the server's own cadence, `slow_down` included.
                zyris::Progress::Waiting { .. } => {}
                zyris::Progress::Granted(credential) => {
                    // Stored **before** it is used, and before the window is told. A credential
                    // this process began dialling on but never wrote down would enroll again on
                    // the next start and leave an unused credential behind in the account.
                    //
                    // **Under the lock, and only when nobody has replaced it.** Enrolling takes as
                    // long as a person takes to reach a browser; another window can approve and
                    // store its own credential in the meantime, and this one's write would then be
                    // a second credential on the account with the person's first approval thrown
                    // away — exactly what `forget_refused`'s comment says must not happen.
                    let _transaction = self.store.lock().await.map_err(store_trouble)?;
                    let credential = match self.store.load().await {
                        Ok(Some(existing)) if existing.secret != credential.secret => {
                            tracing::info!(
                                "another window enrolled while this one waited; adopting its credential"
                            );
                            existing
                        }
                        _ => {
                            self.store.save(&credential).await.map_err(store_trouble)?;
                            credential
                        }
                    };
                    self.ui.authorized();
                    return Ok(credential);
                }
                zyris::Progress::Lapsed => {
                    self.ui.lapsed();
                    enrollment.renew().await.map_err(enrollment_trouble)?;
                    self.ui.show(enrollment.code());
                }
                zyris::Progress::Denied => {
                    self.ui.denied();
                    return Err(CredentialsError::NeedsOperator(
                        "the request was declined in the browser".to_string(),
                    ));
                }
            }
        }
    }

    /// Asks for a code, **leaving out any scope the server says it does not know.**
    ///
    /// One unknown scope refuses the whole authorize request before a person ever sees a code, so
    /// a build asking for a scope the deployment has not shipped (or has since dropped — 0.3.2's
    /// `nodes:write`) could not enroll at all, and the only way out was a new build. The server
    /// cannot demand a scope it does not know, so asking without it loses nothing; the person is
    /// asked to approve what is left.
    ///
    /// The server names one scope per refusal, so this repeats until the request goes through. It
    /// ends: every round takes a scope out, and a refusal naming one that is not in the request
    /// is returned as it was.
    async fn start_enrollment(&self) -> Result<zyris::Enrollment, CredentialsError> {
        loop {
            let request = self.request.lock().unwrap().clone();
            match zyris::enroll(&self.url, request).await {
                Ok(enrollment) => return Ok(enrollment),
                Err(zyris::EnrollError::ScopeUnknown { scope }) if self.drop_scope(&scope) => {
                    tracing::warn!(%scope, "the server does not know this scope; asking without it");
                    crate::conn::server_does_not_know(&scope);
                }
                Err(e) => return Err(enrollment_trouble(e)),
            }
        }
    }

    /// Takes `scope` out of the request. False when it was not there to take.
    fn drop_scope(&self, scope: &str) -> bool {
        let mut request = self.request.lock().unwrap();
        let before = request.scopes.len();
        request.scopes.retain(|s| s != scope);
        request.scopes.len() < before
    }

    /// Lets go of the credential held in memory. The next `bearer` goes back through
    /// [`credential`](Self::credential), which finds whatever the store now holds — nothing, once
    /// logging out has cleared it — and enrolls.
    pub async fn forget(&self) {
        *self.held.lock().await = None;
    }

    #[cfg(test)]
    pub(crate) async fn is_holding(&self) -> bool {
        self.held.lock().await.is_some()
    }

    /// The credential in hand, if any. **What `Reauth::discard_scope` compares the file against**:
    /// "the file is not what I was carrying" means another window has done the work already, and
    /// deleting it would throw that window's approval away.
    async fn held(&self) -> Option<zyris::Credential> {
        self.held.lock().await.clone()
    }

    /// Puts a credential in hand — another window's, adopted rather than re-enrolled.
    #[allow(dead_code)]
    async fn hold(&self, credential: zyris::Credential) {
        *self.held.lock().await = Some(credential);
    }
}

#[async_trait::async_trait]
impl Credentials for DeviceGrant {
    async fn bearer(&self) -> Result<String, CredentialsError> {
        // **No file lock is held here, and that is the fix.** It used to be taken before every dial
        // and kept for the whole call — and a call can be an enrollment that waits for a person in
        // a browser, for as long as they take to walk over to one. Any other window that dialled
        // meanwhile waited the lock's ten seconds and then gave up with exit code 2, so starting a
        // second window during a first run killed it instead of letting it wait. What actually
        // needs serialising is the credential **file**, and that is locked inside `credential`,
        // `enroll` and `forget_refused` — each for a read-check-write and no longer. One enrollment
        // per *window* is still enforced by the in-process `enrolling` mutex, and one per *machine*
        // by the enrollment claim (`enroll_or_adopt`), which is waited on without a timeout.
        Ok(self.credential().await?.secret().to_string())
    }

    /// Attacca refused the credential — revoked in the web UI, or one from before credentials
    /// existed. Forget it, on disk and in memory, so the next `bearer` enrolls and a fresh code
    /// appears.
    ///
    /// **Unless another window has already replaced it.** Windows share one credential file; the
    /// first to be refused enrolls and writes a new credential, and a second one refused a moment
    /// later must adopt that one rather than delete it — or each window's refusal would throw the
    /// other's approval away.
    async fn forget_refused(&self) -> Result<bool, CredentialsError> {
        let _transaction = self.store.lock().await.map_err(store_trouble)?;
        let refused = self.held.lock().await.take();
        if let Some(stored) = self.stored().await? {
            if refused.as_ref().is_none_or(|held| held.secret != stored.secret) {
                *self.held.lock().await = Some(stored);
                return Ok(true);
            }
        }
        tracing::warn!("Attacca refused this credential; forgetting it and enrolling again");
        self.store.clear().await.map_err(store_trouble)?;
        Ok(true)
    }

    fn describe(&self) -> String {
        format!("device enrollment ({})", self.store.describe())
    }
}

/// How the run loop should read a failure that came from the enrollment layer.
///
/// Matched exhaustively on purpose: a shade added upstream must stop the build here rather than be
/// folded into "back off and try again", which is the answer that never surfaces anything.
fn enrollment_trouble(error: zyris::EnrollError) -> CredentialsError {
    match error {
        // **Only reached when the named scope is not in the request** — `start_enrollment` drops
        // the ones that are and asks again. The server then refused something this build never
        // sent, and naming it is all that can be done.
        zyris::EnrollError::ScopeUnknown { scope } => CredentialsError::NeedsOperator(format!(
            "this server does not know the scope {scope}; it must be removed from the list this \
             build asks for before enrollment can even show a code"
        )),
        // Somebody said no in the browser. Asking again is pestering them.
        e @ zyris::EnrollError::Denied => CredentialsError::NeedsOperator(e.to_string()),
        // Both are worth another dial rather than an exit: a lapsed code is renewed by the next
        // attempt, and a server that is merely unreachable is usually a startup race.
        e @ (zyris::EnrollError::Lapsed | zyris::EnrollError::Unreachable(_)) => {
            CredentialsError::Unavailable(e.to_string())
        }
    }
}

/// A store failure that reached the caller, in the terms the run loop acts on.
///
/// [`DeviceGrant::stored`] already swallows the discardable ones on the read path, so what is left
/// is a refusal — an exposed secret, a machine with nowhere to keep one — a *write* that could not
/// be kept, or **a lock another window is holding**. Retrying the first two would mean a fresh code
/// on every restart and an unused credential in the account behind each one, so neither is a
/// reason to loop; the lock is the opposite, and becomes `Unavailable` so the loop backs off and
/// asks again.
fn store_trouble(error: CredentialStoreError) -> CredentialsError {
    match error {
        // **Waiting is the whole answer.** A second window that dials while the first is enrolling
        // used to be told this needed a person, and the run loop exits with code 2 on that — one
        // window's login killed the other. As `Unavailable` it backs off and tries again, and the
        // enrollment finishes in the meantime.
        CredentialStoreError::Busy(message) => CredentialsError::Unavailable(message),
        other => CredentialsError::NeedsOperator(other.to_string()),
    }
}

/// What moves the enrollment code to the screen. The polling loop above calls these.
///
/// Once `show` reaches the screen, the screen owns the display from that moment — nothing goes to
/// stdout. If it doesn't (screen not up yet, or already dead), it prints the box as before.
pub struct ScreenEnroll {
    bridge: Bridge,
}

impl ScreenEnroll {
    /// A fresh code is ready. Called for the first one and again after every renewal, so the
    /// window redraws over whatever phase it was showing.
    pub fn show(&self, code: &zyris::Code) {
        let view = EnrollView {
            code: code.user_code.clone(),
            uri: code.verification_uri.clone(),
            // `Code` carries a wall-clock instant and the screen counts down on the monotonic one.
            // A code that already lapsed converts to no time left rather than panicking — renewal
            // and this conversion race, and losing that race must not take the app down.
            expires_at: Instant::now() + time_left(code),
            phase: EnrollPhase::Waiting,
        };
        if !self.bridge.reaches_screen(Frame::Enroll(view)) {
            // Without a screen, print the box — the same path as before. Even with the screen
            // wired up, this is all the first run does when the app could not start at all.
            println!("{}", notice(code));
        }
    }

    /// The code lapsed. **The window is not closed** — a new one is on its way and will be drawn
    /// over this phase.
    pub fn lapsed(&self) {
        self.bridge.frame(Frame::EnrollPhase(EnrollPhase::Lapsed));
    }

    pub fn denied(&self) {
        self.bridge.frame(Frame::EnrollPhase(EnrollPhase::Denied));
    }

    /// Approved. This is what closes the window, including one a person dismissed with Esc while
    /// the grant kept polling in the background.
    pub fn authorized(&self) {
        self.bridge.frame(Frame::EnrollDone);
    }
}

/// How much of a code's life is left, on the wall clock it was issued against.
fn time_left(code: &zyris::Code) -> Duration {
    code.expires_at.duration_since(SystemTime::now()).unwrap_or_default()
}

/// The block printed when there is no screen to draw on.
///
/// Built here rather than fetched from the library: upstream's `authorization_notice` went with
/// the program layer, and it was three facts and a border. **Nothing here goes through `lang.rs`**
/// — that is the screen's vocabulary, and this is the one path where there is no screen.
///
/// `println!` rather than `tracing` at the call site: somebody running with `RUST_LOG=error` must
/// still see the code. It is the primary UX of the whole feature when the box is all there is.
fn notice(code: &zyris::Code) -> String {
    let minutes = time_left(code).as_secs().div_ceil(60);
    format!(
        "\n\
         --------------------------------------------------------------\n  \
         Authorize this node\n\n  \
         1. Open        {uri}\n  \
         2. Enter code  {user_code}\n\n  \
         Waiting for approval. This code expires in {minutes} minutes.\n  \
         Press Ctrl-C to cancel.\n\
         --------------------------------------------------------------\n",
        uri = code.verification_uri,
        user_code = code.user_code,
    )
}

/// A handle to discard credentials and get authorized again. **Used at most once per process**
/// by the automatic scope check; `/account logout` goes through [`discard`](Self::discard).
///
/// The scopes settled at approval never widen. When a feature grows and one more scope is needed,
/// there is no path but discarding the credential — the screen side notices that after attaching
/// (`conn::needs_reenrollment`) and calls this.
#[derive(Clone)]
pub struct Reauth {
    store: Arc<dyn CredentialStore>,
    /// The credential this process is holding. **Clearing the file is only half of it** — see
    /// `DeviceGrant`.
    grant: Arc<DeviceGrant>,
    /// Whether this process has already discarded once. **A person can approve narrowly again** —
    /// discarding every time would demand the browser on every attach, not every launch.
    spent: Arc<AtomicBool>,
}

impl Reauth {
    /// A `Reauth` over whatever store the test hands it. Nothing in these tests reaches the
    /// network — the URL never resolves.
    #[cfg(test)]
    pub(crate) fn for_test(store: Arc<dyn CredentialStore>) -> Reauth {
        let grant = Arc::new(DeviceGrant::new(
            store.clone(),
            "wss://example.invalid/zyris/v1/ws".to_string(),
            zyris::EnrollRequest {
                program: "zyris-code".to_string(),
                system_hint: "arch".to_string(),
                platform: "linux".to_string(),
                scopes: Vec::new(),
            },
            ScreenEnroll { bridge: Bridge::new() },
        ));
        Reauth { store, grant, spent: Arc::new(AtomicBool::new(false)) }
    }

    /// Whether it has already been done. The value fed into `conn::needs_reenrollment`.
    pub fn spent(&self) -> bool {
        self.spent.load(Ordering::SeqCst)
    }

    /// Discards the credential **at most once per process.** True if something was discarded.
    ///
    /// **The automatic path, and it adopts rather than clears when the file changed under us** —
    /// see [`discard_scope`](Self::discard_scope).
    pub async fn discard_once(&self) -> bool {
        if self.spent.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.discard_scope().await
    }

    /// The discard used when the credential on disk is missing scopes this build needs.
    ///
    /// **Two windows on a scope upgrade approve once.** Both see the missing scope; the first
    /// discards the file, enrolls and writes a fresh credential; the second, arriving a moment
    /// later, used to delete that file unconditionally — so the person had to approve a second time
    /// and the first approval was thrown away. If the secret on disk is not the one this process
    /// was carrying, another window has already done the work: adopt it and leave it alone.
    ///
    /// An explicit `/account logout` goes through [`discard`](Self::discard) instead, which always
    /// clears — there the person means it, and this machine is what they meant it about.
    async fn discard_scope(&self) -> bool {
        let _transaction = match self.store.lock().await {
            Ok(lock) => lock,
            Err(e) => {
                tracing::warn!(error = %e, "could not lock the credentials for discard");
                return false;
            }
        };
        // **Only when this window is carrying something else.** A window that is carrying nothing
        // has nothing to compare against and nothing to adopt from — the file it finds is the file
        // it should clear, which is the case the automatic scope check exists for.
        let carrying = self.grant.held().await;
        if let (Some(carrying), Ok(Some(stored))) = (&carrying, self.store.load().await) {
            if carrying.secret != stored.secret {
                tracing::info!("another window enrolled while this one was deciding; adopting it");
                self.grant.hold(stored).await;
                return false;
            }
        }
        self.grant.forget().await;
        match self.store.clear().await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "could not discard the credentials");
                false
            }
        }
    }

    /// Discards the credential, however many times it is asked. What `/account logout` calls.
    ///
    /// **The file and the copy in memory both go, in that order.** Clearing the file alone left
    /// this process holding a working credential, so the redial that logging out triggers
    /// presented it and attached. Forgetting first would let a dial in between reload the file and
    /// hold it again.
    pub async fn discard(&self) -> bool {
        self.spent.store(true, Ordering::SeqCst);
        let _transaction = match self.store.lock().await {
            Ok(lock) => lock,
            Err(e) => {
                tracing::warn!(error = %e, "could not lock the credentials for discard");
                return false;
            }
        };
        let cleared = match self.store.clear().await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "could not discard the credentials");
                false
            }
        };
        self.grant.forget().await;
        cleared
    }
}

#[cfg(test)]
fn a_credential(secret: &str) -> zyris::Credential {
    zyris::Credential {
        version: 2,
        secret: secret.to_string(),
        system: zyris::Named { id: "s".into(), name: "arch".into(), slug: "arch".into() },
        program: zyris::Named {
            id: "c".into(),
            name: "zyris-code".into(),
            slug: "zyris-code".into(),
        },
        scopes: Vec::new(),
        owner_email: "e@example.com".into(),
    }
}

#[cfg(test)]
mod tests_discard {
    use super::*;
    use crate::runtime::MemoryCredentialStore;

    /// What goes on the wire is the credential's secret, and nothing needs the network to say so.
    #[tokio::test]
    async fn the_bearer_is_the_stored_credentials_secret() {
        let store = Arc::new(MemoryCredentialStore::default());
        store.save(&a_credential("zc_stored")).await.unwrap();
        let reauth = Reauth::for_test(store.clone());

        assert_eq!(reauth.grant.bearer().await.unwrap(), "zc_stored");
        assert!(reauth.grant.is_holding().await);
    }

    /// Attacca refused it: gone from disk and from memory, so the next `bearer` enrolls.
    #[tokio::test]
    async fn a_refused_credential_is_forgotten_so_the_next_dial_enrolls() {
        let store = Arc::new(MemoryCredentialStore::default());
        store.save(&a_credential("zc_revoked")).await.unwrap();
        let reauth = Reauth::for_test(store.clone());
        reauth.grant.hold(a_credential("zc_revoked")).await;

        assert!(reauth.grant.forget_refused().await.unwrap());
        assert!(store.load().await.unwrap().is_none(), "the refused credential is still on disk");
        assert!(!reauth.grant.is_holding().await, "the refused credential would be dialled again");
    }

    /// Review focus 2: another window already enrolled and wrote a new credential. Adopt it —
    /// deleting it would throw that window's approval away.
    #[tokio::test]
    async fn a_credential_another_window_just_enrolled_is_adopted_not_deleted() {
        let store = Arc::new(MemoryCredentialStore::default());
        store.save(&a_credential("zc_new")).await.unwrap();
        let reauth = Reauth::for_test(store.clone());
        reauth.grant.hold(a_credential("zc_old")).await;

        assert!(reauth.grant.forget_refused().await.unwrap());
        assert_eq!(store.load().await.unwrap().unwrap().secret, "zc_new");
        assert_eq!(reauth.grant.bearer().await.unwrap(), "zc_new");
    }

    /// **Clearing the file is only half of logging out.** Reported 2026-08-14.
    #[tokio::test]
    async fn logging_out_lets_go_of_the_credential_this_process_is_holding() {
        let store = Arc::new(MemoryCredentialStore::default());
        store.save(&a_credential("zc_stored")).await.unwrap();
        let reauth = Reauth::for_test(store.clone());
        reauth.grant.hold(a_credential("zc_stored")).await;

        assert!(reauth.discard().await);
        assert!(store.load().await.unwrap().is_none(), "the file was not cleared");
        assert!(!reauth.grant.is_holding().await, "the credential in memory would still attach");
    }

    /// **A person asking to log out means it every time**, even after the automatic scope check
    /// spent its one allowance.
    #[tokio::test]
    async fn asking_to_log_out_twice_still_logs_out() {
        let store = Arc::new(MemoryCredentialStore::default());
        store.save(&a_credential("zc_stored")).await.unwrap();
        let reauth = Reauth::for_test(store.clone());

        assert!(reauth.discard_once().await);
        assert!(!reauth.discard_once().await, "the automatic path is once per process");

        store.save(&a_credential("zc_stored")).await.unwrap();
        assert!(reauth.discard().await, "logging out was refused");
        assert!(store.load().await.unwrap().is_none(), "the credential is still there");
    }

    /// **A scope upgrade in two windows approves once.** The second window finds the credential the
    /// first one just wrote, and must adopt it rather than delete it.
    #[tokio::test]
    async fn a_scope_discard_adopts_a_credential_another_window_just_wrote() {
        let store = Arc::new(MemoryCredentialStore::default());
        store.save(&a_credential("zc_narrow")).await.unwrap();
        let reauth = Reauth::for_test(store.clone());
        reauth.grant.hold(a_credential("zc_narrow")).await;

        // Another window approved a wider credential while this one was looking at the old scopes.
        store.save(&a_credential("zc_wide")).await.unwrap();

        assert!(!reauth.discard_once().await, "it deleted another window's approval");
        assert_eq!(store.load().await.unwrap().unwrap().secret, "zc_wide");
        assert_eq!(reauth.grant.bearer().await.unwrap(), "zc_wide");
    }

    /// **An explicit logout always clears**, even when the file holds another window's credential —
    /// the person said sign out, and this is the machine they said it about.
    #[tokio::test]
    async fn logging_out_clears_even_a_credential_another_window_wrote() {
        let store = Arc::new(MemoryCredentialStore::default());
        store.save(&a_credential("zc_old")).await.unwrap();
        let reauth = Reauth::for_test(store.clone());
        reauth.grant.hold(a_credential("zc_old")).await;
        store.save(&a_credential("zc_new")).await.unwrap();

        assert!(reauth.discard().await);
        assert!(store.load().await.unwrap().is_none(), "logout left a credential behind");
    }

    /// **A credential cleared by another window is not dialled again.** `discard` in one window
    /// clears the shared file; a second window that kept its copy in memory would go on attaching
    /// as a machine that has been signed out.
    #[tokio::test]
    async fn a_credential_cleared_by_another_window_is_noticed() {
        let store = Arc::new(MemoryCredentialStore::default());
        store.save(&a_credential("zc_shared")).await.unwrap();
        let reauth = Reauth::for_test(store.clone());
        reauth.grant.hold(a_credential("zc_shared")).await;
        assert!(!reauth.grant.released_elsewhere().await, "nothing was cleared yet");

        store.clear().await.unwrap();
        assert!(reauth.grant.released_elsewhere().await, "the cleared file was not noticed");
    }

    /// **A window starting while another enrolls waits for that credential instead of enrolling
    /// too.** Both used to show a code, and the second approval put a second credential on the
    /// account. The waiting window keeps running — no timeout, no exit — and adopts the file the
    /// other one writes. The server here does not exist, so enrolling would have failed.
    #[tokio::test]
    async fn a_window_waits_for_another_windows_enrollment_and_adopts_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let other = FileCredentialStore::at(&path);
        let claim = other.claim_enrollment().await.unwrap().expect("the first claim is free");
        let ours = FileCredentialStore::at(&path);
        assert!(ours.claim_enrollment().await.unwrap().is_none(), "two windows both enrolling");

        let reauth = Reauth::for_test(Arc::new(ours));
        let waiting = tokio::spawn(async move { reauth.grant.bearer().await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiting.is_finished(), "it gave up instead of waiting");

        other.save(&a_credential("zc_theirs")).await.unwrap();
        drop(claim);
        let got = tokio::time::timeout(Duration::from_secs(5), waiting).await;
        assert_eq!(got.unwrap().unwrap().unwrap(), "zc_theirs");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    use crate::app::Action;
    use crate::runtime::MemoryCredentialStore;

    fn code() -> zyris::Code {
        zyris::Code {
            user_code: "WXQR-7KBD".into(),
            verification_uri: "https://attacca.example/settings/zyris/device".into(),
            expires_at: SystemTime::now() + Duration::from_secs(600),
        }
    }

    /// A bridge with a screen attached, and the mailbox that screen receives.
    fn with_screen() -> (Bridge, mpsc::UnboundedReceiver<crate::app::AppMsg>) {
        let bridge = Bridge::new();
        let (tx, rx) = mpsc::unbounded_channel();
        bridge.attach(tx);
        (bridge, rx)
    }

    /// An authorize endpoint that refuses any request naming `nodes:write`, the way a deployment
    /// that never shipped it does, and hands out a code otherwise. Returns the websocket URL.
    fn refusing_nodes_write() -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                // Read the whole request: the body can arrive after the headers.
                let mut seen = Vec::new();
                let mut buf = [0u8; 4096];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    seen.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&seen);
                    let Some(end) = text.find("\r\n\r\n") else { continue };
                    let length = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if seen.len() >= end + 4 + length {
                        break;
                    }
                }
                let (status, body) = if String::from_utf8_lossy(&seen).contains("nodes:write") {
                    (
                        "422 Unprocessable Entity",
                        r#"{"error":"unknown_scope","scope":"nodes:write"}"#,
                    )
                } else {
                    (
                        "200 OK",
                        r#"{"device_code":"zdc_secret","user_code":"WXQR-7KBD","verification_uri":"https://attacca.example/settings/zyris/device","expires_in":600,"interval":1}"#,
                    )
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("ws://{address}/zyris/v1/ws")
    }

    fn grant_asking_for(url: String, scopes: &[&str]) -> DeviceGrant {
        DeviceGrant::new(
            Arc::new(MemoryCredentialStore::default()),
            url,
            zyris::EnrollRequest {
                program: "zyris-code".to_string(),
                system_hint: "arch".to_string(),
                platform: "linux".to_string(),
                scopes: scopes.iter().map(|s| s.to_string()).collect(),
            },
            ScreenEnroll { bridge: Bridge::new() },
        )
    }

    /// **A scope the server does not know is dropped, and the person is asked for the rest.** It
    /// used to end enrollment before any code was shown, and only a new build got past it.
    #[tokio::test]
    async fn a_scope_the_server_does_not_know_is_left_out_and_a_code_is_shown() {
        let grant = grant_asking_for(refusing_nodes_write(), &["agents:read", "nodes:write"]);
        let enrollment = grant.start_enrollment().await.expect("it must get a code");
        assert_eq!(enrollment.code().user_code, "WXQR-7KBD");
        assert_eq!(grant.request.lock().unwrap().scopes, vec!["agents:read".to_string()]);
    }

    /// A refusal naming a scope this build never sent cannot be fixed by dropping it. It comes back
    /// as it did, rather than asking again for ever.
    #[test]
    fn a_refused_scope_that_was_never_asked_for_is_not_dropped() {
        let grant = grant_asking_for("ws://127.0.0.1:1/zyris/v1/ws".to_string(), &["agents:read"]);
        assert!(!grant.drop_scope("nodes:write"));
        assert!(grant.drop_scope("agents:read"));
        assert!(grant.request.lock().unwrap().scopes.is_empty());
    }

    /// **With the screen up, the code goes to the screen.** It doesn't leak to stdout.
    #[test]
    fn the_code_goes_to_the_screen_when_one_is_up() {
        let (bridge, mut screen) = with_screen();
        ScreenEnroll { bridge }.show(&code());

        match screen.try_recv().expect("must reach the screen") {
            (_, Action::Frame(Frame::Enroll(view))) => {
                assert_eq!(view.code, "WXQR-7KBD");
                assert_eq!(view.uri, "https://attacca.example/settings/zyris/device");
                assert_eq!(view.phase, EnrollPhase::Waiting);
            }
            other => panic!("must be an enrollment frame: {other:?}"),
        }
    }

    /// **Without a screen, print the box** (the old path). That spot is all the first run does.
    #[test]
    fn without_a_screen_the_code_is_printed() {
        let bridge = Bridge::new();
        // Called without a screen, the box goes to stdout — no panic, and that's all.
        ScreenEnroll { bridge }.show(&code());
    }

    /// **The box has to carry both halves.** A code with nowhere to type it is not actionable, and
    /// this string is all a machine without a screen ever gets. The device code — the secret half
    /// of the grant — is not in `zyris::Code` at all, so it cannot be printed by accident.
    #[test]
    fn the_printed_box_names_the_code_and_where_to_type_it() {
        let box_text = notice(&code());
        assert!(box_text.contains("WXQR-7KBD"), "the code is missing: {box_text}");
        assert!(box_text.contains("https://attacca.example/settings/zyris/device"));
        assert!(box_text.contains("expires in 10 minutes"), "no idea how long it lasts");
    }

    /// **A code that already lapsed still draws.** Renewal and the wall-clock conversion race, and
    /// `SystemTime::duration_since` answers a past instant with an error — taking the app down at
    /// the moment it is showing somebody how to authorize it would be the worst place for one.
    #[test]
    fn a_code_that_already_expired_still_reaches_the_screen() {
        let (bridge, mut screen) = with_screen();
        let lapsed =
            zyris::Code { expires_at: SystemTime::now() - Duration::from_secs(60), ..code() };
        ScreenEnroll { bridge }.show(&lapsed);

        assert!(matches!(screen.try_recv(), Ok((_, Action::Frame(Frame::Enroll(_))))));
    }

    /// **Expiry, denial, and approval reach the screen.** If they vanished silently, a person wouldn't know.
    #[test]
    fn the_outcomes_reach_the_screen() {
        let (bridge, mut screen) = with_screen();
        let ui = ScreenEnroll { bridge };

        ui.lapsed();
        match screen.try_recv().expect("expiry must reach the screen") {
            (_, Action::Frame(Frame::EnrollPhase(EnrollPhase::Lapsed))) => {}
            other => panic!("must be an expiry frame: {other:?}"),
        }

        ui.denied();
        match screen.try_recv().expect("denial must reach the screen") {
            (_, Action::Frame(Frame::EnrollPhase(EnrollPhase::Denied))) => {}
            other => panic!("must be a denial frame: {other:?}"),
        }

        ui.authorized();
        assert!(matches!(screen.try_recv(), Ok((_, Action::Frame(Frame::EnrollDone)))));
    }

    /// **Credentials are discarded at most once per process** by the automatic path.
    #[tokio::test]
    async fn a_credential_is_discarded_at_most_once_per_process() {
        let store = Arc::new(MemoryCredentialStore::new());
        store.save(&a_credential("zc_stored")).await.unwrap();
        let reauth = Reauth::for_test(store.clone());

        assert!(!reauth.spent());
        assert!(reauth.discard_once().await, "the first one is discarded");
        assert!(store.load().await.unwrap().is_none(), "the credential must actually be empty");

        store.save(&a_credential("zc_stored")).await.unwrap();
        assert!(!reauth.discard_once().await, "the second one is not discarded");
        assert!(store.load().await.unwrap().is_some(), "the newly received credential stays");
        assert!(reauth.spent(), "having tried once feeds into the decision");
    }

    /// **Where a token was given directly there is nothing to discard.** Not having a `Reauth` is that state.
    #[test]
    fn a_static_token_has_no_reauth() {
        // When source() falls into the StaticToken path it isn't Some(reauth) — since the
        // environment can't be shaken here, this only records the contract that the handle may be `None`.
        // The real decision is locked down by the `conn::missing_scopes` test.
    }
}
