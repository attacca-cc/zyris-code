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
    /// What to ask to be enrolled as. Settled before this value is built — see [`source`].
    request: zyris::EnrollRequest,
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
            request,
            ui,
            held: tokio::sync::Mutex::new(None),
            enrolling: tokio::sync::Mutex::new(()),
        }
    }

    /// The credential to present: the one held, the one on disk, or a fresh enrollment.
    async fn credential(&self) -> Result<zyris::Credential, CredentialsError> {
        if let Some(credential) = self.held.lock().await.clone() {
            return Ok(credential);
        }
        // One enrollment at a time, so a second dial arriving mid-grant cannot put a second code on
        // the screen. **`held` is deliberately not what is locked here** — see the field.
        let _enrolling = self.enrolling.lock().await;
        // Whoever was ahead of us may have finished while we waited for that.
        if let Some(credential) = self.held.lock().await.clone() {
            return Ok(credential);
        }
        let credential = match self.stored().await? {
            Some(credential) => credential,
            None => self.enroll().await?,
        };
        *self.held.lock().await = Some(credential.clone());
        Ok(credential)
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
        let mut enrollment =
            zyris::enroll(&self.url, self.request.clone()).await.map_err(enrollment_trouble)?;
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
                    self.store.save(&credential).await.map_err(store_trouble)?;
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

    #[cfg(test)]
    pub(crate) async fn hold(&self, credential: zyris::Credential) {
        *self.held.lock().await = Some(credential);
    }
}

#[async_trait::async_trait]
impl Credentials for DeviceGrant {
    async fn bearer(&self) -> Result<String, CredentialsError> {
        // Held through the re-read and any enrollment, so a second window starting at the same
        // moment waits for this one's approval instead of asking for a second credential.
        let _transaction = self.store.lock().await.map_err(store_trouble)?;
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
        // **The 2026-08-03 incident, and this message is the only thing that explains it.** One
        // scope the deployment does not know refuses the *whole* authorize request with a 422, so
        // nobody ever reaches the approval screen. Naming the scope is the difference between
        // "enrollment is broken" and one line to delete from `conn::REQUIRED_SCOPES`.
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

/// A store failure that reached the caller needs a person.
///
/// [`DeviceGrant::stored`] already swallows the discardable ones on the read path, so what is left
/// is either a refusal — an exposed secret, a machine with nowhere to keep one — or a *write* that
/// could not be kept. Retrying the second would mean a fresh code on every restart and an unused
/// credential in the account behind each one, so neither is a reason to loop.
fn store_trouble(error: CredentialStoreError) -> CredentialsError {
    CredentialsError::NeedsOperator(error.to_string())
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
    pub async fn discard_once(&self) -> bool {
        if self.spent.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.discard().await
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
