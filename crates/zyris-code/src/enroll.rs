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
/// The order is upstream's old `credentials::from_env` and must stay it — what a person explicitly
/// gives always wins, and the path that has to ask a person comes last. The only difference is
/// that the enrollment path is wired to the screen; since we hold the store ourselves, no path
/// guessing is needed.
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

    // Where a person gave a token directly, there is nothing to discard and no one to ask again.
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

    // **Not `Account` on its own** — see `AccountGrant`. The rotation behaviour is the library's;
    // what is added here is a screen to draw the code on and a way for logging out to reach the
    // copy this process is holding.
    let grant = Arc::new(AccountGrant::new(
        store.clone(),
        config.url.clone(),
        zyris::EnrollRequest {
            name: config.node_name.clone(),
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
/// It is the device grant end to end: load what is stored, or enroll and show a code; wrap the
/// result in a [`zyris::Account`], which rotates the refresh token and hands each rotation back
/// for storing; and answer `bearer` from it before every dial.
///
/// **The account is kept in memory, and that is what made `/account logout` a lie.** Logging out
/// cleared the credential file and dropped the socket so the runner would redial — and the redial
/// presented the token this process was **still holding** and attached. Logged out on disk,
/// connected on the wire, and no enrollment code, which is exactly what was reported
/// (2026-08-14). Clearing the file cannot be the whole of logging out while a live process holds a
/// working token.
///
/// So this type exists for one method beyond the trait: [`forget`](Self::forget).
struct AccountGrant {
    store: Arc<dyn CredentialStore>,
    /// The websocket URL. `Account` and `zyris::enroll` both derive the HTTP base from it, so this
    /// node cannot end up enrolling against one deployment while connecting to another.
    url: String,
    /// What to ask to be enrolled as. Settled before this value is built — see [`source`].
    request: zyris::EnrollRequest,
    ui: ScreenEnroll,
    held: tokio::sync::Mutex<Option<Arc<zyris::Account>>>,
    /// Held for the length of one enrollment, so two dials cannot put two codes on the screen.
    ///
    /// **Separate from `held` on purpose.** The loop below has no bound — it renews a lapsed code
    /// for as long as somebody might still walk over to a browser — and `Reauth::discard` reaches
    /// for `held` to let go of the token this process is carrying. Parking the enrollment on that
    /// same lock made `/account logout` wait for the enrollment to finish, which is a frozen screen
    /// with no keys and no redraw. Two locks, so the one a person can reach is never the one a
    /// stranger's browser is holding.
    enrolling: tokio::sync::Mutex<()>,
    /// Why a rotation could not be stored, once one could not be.
    ///
    /// **This is the difference between stopping and being revoked.** `zyris::Account` does not
    /// adopt a rotation its hook refused, and it reports the refusal as `Unreachable` — the same
    /// shade a server that blipped produces, which the run loop answers by backing off and dialling
    /// again. Here that answer is wrong and dangerous: the server has *already* spent the old
    /// refresh token, so every later attempt re-presents a spent one, and attacca reads a replay
    /// past its 30-second grace as a leaked chain — `revoke_all_for_node`, which kills every node
    /// under this credential rather than only this one.
    ///
    /// Upstream could not reach that state: its `Enroller` persisted with `?`, so a store failure
    /// was `NeedsOperator` and the process exited 2 on the first one. Recording it here is how that
    /// answer survives the move — the hook is our code, so it is the one place that can still tell
    /// a full disk from a slow server.
    store_broke: Arc<tokio::sync::Mutex<Option<String>>>,
}

/// Persist a rotated credential, and **remember a refusal as well as returning it.**
///
/// Extracted from the hook so a test can drive it, which is the half that was missing. The
/// rotation test below sets `store_broke` by hand and asserts the node then stops — which proves
/// *"if the flag is set, we stop"* and says nothing about *"a store that refuses sets the flag"*.
/// The second is the one that fires in the real incident: a full disk, a config directory that
/// lost its write bit, a home on a mount that went away.
///
/// **Remembering is usually the only trace there is.** [`zyris::Account::bearer`] does not
/// propagate a hook failure while it still holds a usable token — it logs "could not rotate;
/// carrying on with the credential held" and answers `Ok`. Rotation falls due at 80% of the access
/// token's life, so for the last fifth of every hour the refusal reaches this app through nothing
/// but this flag. By then the server has already spent the refresh token that produced `rotated`,
/// so every dial after this is a replay, and attacca answers a replay past its 30-second grace
/// with `revoke_all_for_node`: every node under this credential, not only this one.
///
/// **A free function over the two `Arc`s, not a method.** The hook is `'static` and runs inside
/// `Account::bearer`; [`AccountGrant::refresh`] holds the `held` lock across that call, so a hook
/// reaching back into `self` would deadlock. Being free also keeps the flag shared across every
/// account the grant builds — including the throwaway one `refresh` ages with `due_now`, which is
/// the account most likely to rotate.
async fn persist_rotation(
    store: &Arc<dyn CredentialStore>,
    broke: &Arc<tokio::sync::Mutex<Option<String>>>,
    rotated: &zyris::AccountCredential,
) -> Result<(), zyris::RotateError> {
    match store.save(rotated).await {
        Ok(()) => Ok(()),
        Err(e) => {
            *broke.lock().await = Some(e.to_string());
            Err(zyris::RotateError(e.to_string()))
        }
    }
}

impl AccountGrant {
    fn new(
        store: Arc<dyn CredentialStore>,
        url: String,
        request: zyris::EnrollRequest,
        ui: ScreenEnroll,
    ) -> AccountGrant {
        AccountGrant {
            store,
            url,
            request,
            ui,
            held: tokio::sync::Mutex::new(None),
            enrolling: tokio::sync::Mutex::new(()),
            store_broke: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// The account to ask for a bearer: the one being held, the one on disk, or a fresh enrollment.
    ///
    /// This is the whole startup decision tree, and going through it whenever nothing usable is
    /// held means there is no timer task and no second code path.
    ///
    /// **An account whose credential is due for rotation is dropped and the file read again**, and
    /// that re-read is the whole reason two windows in one directory are survivable. Upstream's
    /// `Held` went back through `Enroller::obtain()` whenever the access token was spent, and
    /// `obtain` opened by reading the file — so the second window found the pair the first had just
    /// written and simply used it. Holding an `Account` for the life of the process instead means
    /// both windows carry the same credential, reach 80% of the *same* `access_expires_at` at the
    /// same instant, and rotate from the same single-use refresh token. That is not a race to lose
    /// occasionally, it is an appointment — and attacca answers a replay past its 30-second grace
    /// with `revoke_all_for_node`. CLAUDE.md has a section on this — "nothing locks credential
    /// rotation", spelled `### 자격 회전을 잠그는 것은 아무것도 없다` there, and quoted so it can be
    /// searched for. Nothing here is a lock either; re-reading is only what keeps the odds where
    /// that note says they are.
    async fn account(&self) -> Result<Arc<zyris::Account>, CredentialsError> {
        if let Some(account) = self.usable().await {
            return Ok(account);
        }
        // One enrollment at a time, so a second dial arriving mid-grant cannot put a second code on
        // the screen. **`held` is deliberately not what is locked here** — see the field.
        let _enrolling = self.enrolling.lock().await;
        // Whoever was ahead of us may have finished while we waited for that.
        if let Some(account) = self.usable().await {
            return Ok(account);
        }

        let credential = match self.stored().await? {
            Some(credential) => credential,
            None => self.enroll().await?,
        };
        let account = Arc::new(self.restore(credential));
        *self.held.lock().await = Some(account.clone());
        Ok(account)
    }

    /// The held account, if there is one and it is not already due to rotate.
    ///
    /// Dropping a due one here rather than in [`account`](Self::account) keeps the two places that
    /// ask the same question answering it the same way.
    async fn usable(&self) -> Option<Arc<zyris::Account>> {
        let mut held = self.held.lock().await;
        let account = held.as_ref()?.clone();
        if account.credential().await.should_refresh(now_unix(), ACCESS_LIFETIME_SECS) {
            *held = None;
            return None;
        }
        Some(account)
    }

    /// A corrupt or unreadable credential is a reason to enroll again, not to die. A *refused* one
    /// is different — a world-readable key file, a machine with nowhere to keep a secret — and
    /// refusing loudly is the whole point of that distinction, so it propagates rather than
    /// answering an exposed secret with a quiet re-enrollment.
    async fn stored(&self) -> Result<Option<zyris::AccountCredential>, CredentialsError> {
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

    /// Wrap a credential in an account that writes every rotation back to the store.
    ///
    /// **The hook runs before the rotation is adopted, and that ordering is the load-bearing part.**
    /// A refresh token is single-use: a process that started presenting a pair which never reached
    /// disk would present the spent one on its next start, and attacca reads a replay past its
    /// 30-second grace as a leaked chain — `revoke_all_for_node`, which kills every node under this
    /// credential rather than only this one. `zyris::Account` enforces the order; all we owe it is
    /// a hook that fails when the write failed.
    fn restore(&self, credential: zyris::AccountCredential) -> zyris::Account {
        let store = self.store.clone();
        let broke = self.store_broke.clone();
        zyris::Account::restore(&self.url, credential)
            .on_rotate(move |rotated| {
                let store = store.clone();
                let broke = broke.clone();
                // **Delegation only, and it has to stay that way.** No test reaches this line:
                // firing it needs a server that actually rotates, so replacing the body with
                // `Ok(())` leaves the whole suite green. What the tests below pin is
                // `persist_rotation` itself and the two doors that read what it records — so the
                // one thing they cannot see is this call going missing. Keep the closure a single
                // delegation, and any logic that belongs here belongs in that function instead.
                async move { persist_rotation(&store, &broke, &rotated).await }
            })
            .build()
    }

    /// The reason this node must stop, if a rotation could not be stored.
    ///
    /// **Asked whether or not the dial would have worked.** A hook that failed means the server
    /// rotated and the pair on disk is spent, which is true no matter what the call it happened
    /// inside went on to return. `NeedsOperator` is the shade that ends the process (exit 2)
    /// instead of backing off into a replay — the same answer upstream gave, from the same fact.
    async fn cannot_go_on(&self) -> Option<CredentialsError> {
        let why = self.store_broke.lock().await.clone()?;
        Some(CredentialsError::NeedsOperator(format!(
            "a rotated credential could not be stored ({why}), so the one on disk is spent and \
             dialling again would replay it. Fix that path and start this again."
        )))
    }

    /// Ask for a code, put it in front of a person, and wait.
    ///
    /// **The renewal loop is ours because the library refuses to write it.** `Enrollment::renew`
    /// exists and nothing calls it on its own. It is also the reason the window says "that code
    /// expired" and draws the next one over it instead of the app dying at the ten-minute mark.
    ///
    /// Unbounded on purpose. Upstream stopped after three rounds because it was printing into a
    /// terminal somebody had walked away from; here the window is on screen, Ctrl+C is always
    /// live, and giving up on the node's behalf would take away the one thing it can still do.
    /// The cost is known: attacca rate-limits repeated grants from one address (`too many pending
    /// enrollments from this address`), so a code left unapproved long enough eventually renews
    /// into that refusal — which arrives as `Unavailable`, and the run loop backs off on it.
    async fn enroll(&self) -> Result<zyris::AccountCredential, CredentialsError> {
        let mut enrollment =
            zyris::enroll(&self.url, self.request.clone()).await.map_err(enrollment_trouble)?;
        self.ui.show(enrollment.code());
        loop {
            // Hoisted out of the `match` so nothing borrows `enrollment` while the arm that has
            // to renew it runs.
            let progress = enrollment.poll().await.map_err(enrollment_trouble)?;
            match progress {
                // `poll` sleeps to the server's own cadence, `slow_down` included. A sleep here
                // would be a second clock, disagreeing with the one the server asked for.
                zyris::Progress::Waiting { .. } => {}
                zyris::Progress::Granted(credential) => {
                    // Stored **before** it is used, and before the window is told. A credential
                    // this process began dialling on but never wrote down would enroll again on
                    // the next start and leave a dead node row behind each time; and a window that
                    // closed on `EnrollDone` while the save was still to fail would have said the
                    // approval took when the next thing on screen is that it did not.
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

    /// Lets go of the account held in memory. The next `bearer` goes back through
    /// [`account`](Self::account), which finds whatever the store now holds — nothing, once logging
    /// out has cleared it — and enrolls.
    pub async fn forget(&self) {
        *self.held.lock().await = None;
    }

    /// Whether an account is being held right now. For the test that logging out lets go of it —
    /// there is no other way to see the thing that made logging out a lie.
    #[cfg(test)]
    pub(crate) async fn is_holding(&self) -> bool {
        self.held.lock().await.is_some()
    }

    #[cfg(test)]
    pub(crate) async fn hold(&self, credential: zyris::AccountCredential) {
        *self.held.lock().await = Some(Arc::new(self.restore(credential)));
    }
}

#[async_trait::async_trait]
impl Credentials for AccountGrant {
    async fn bearer(&self) -> Result<String, CredentialsError> {
        let account = self.account().await?;
        let asked = account.bearer().await;
        // Before the answer is read, because a rotation that could not be stored is fatal whatever
        // this call returned — it may well have returned a perfectly good token, held over from
        // before the rotation the disk refused.
        if let Some(stop) = self.cannot_go_on().await {
            return Err(stop);
        }
        match asked {
            Ok(bearer) => Ok(bearer),
            // The server disowned this credential while we were holding it. Clearing the store and
            // letting go of the account sends the next dial back through `account`, which finds
            // nothing stored and enrolls — so a node whose grant chain was revoked shows a fresh
            // code instead of presenting a dead token until somebody deletes the file by hand.
            // `Unavailable` rather than `Fatal` is what buys that next dial: the run loop backs off
            // and comes round again, where `Fatal` would end the process.
            Err(zyris::EnrollError::Revoked) => {
                tracing::warn!("this credential was revoked; enrolling again");
                if let Err(e) = self.store.clear().await {
                    tracing::warn!(error = %e, "could not discard the revoked credential");
                }
                self.forget().await;
                Err(CredentialsError::Unavailable(
                    "this credential was revoked; asking for a new one".to_string(),
                ))
            }
            Err(e) => Err(enrollment_trouble(e)),
        }
    }

    async fn refresh(&self) -> Result<bool, CredentialsError> {
        let mut held = self.held.lock().await;
        // Nothing held means nothing was presented, so the refusal was not about a token of ours.
        let Some(account) = held.take() else { return Ok(false) };

        let forced = self.restore(due_now(account.credential().await));
        let asked = forced.bearer().await;
        if let Some(stop) = self.cannot_go_on().await {
            return Err(stop);
        }
        match asked {
            Ok(_) => {
                *held = Some(Arc::new(forced));
                Ok(true)
            }
            // The same conclusion the startup path reaches, and for the same reason: a node whose
            // grant chain was revoked while it was connected must not be left presenting the dead
            // token until a human deletes the file. The store is cleared here and nothing is put
            // back in `held`, so the next `bearer` enrolls and a fresh code appears.
            Err(zyris::EnrollError::Revoked) => {
                tracing::warn!("this credential was rejected on rotation; enrolling again");
                if let Err(e) = self.store.clear().await {
                    tracing::warn!(error = %e, "could not discard the revoked credential");
                }
                Ok(true)
            }
            Err(e) => Err(enrollment_trouble(e)),
        }
    }

    fn describe(&self) -> String {
        format!("device enrollment ({})", self.store.describe())
    }
}

/// The same credential, stamped as already due for rotation.
///
/// **[`zyris::Account`] has no "rotate now"**: it decides from the credential's own expiry, at 80%
/// of a one-hour life. Upstream's `Enroller::force_refresh` could call the refresh endpoint
/// outright, and that is what answered the 401 a slept laptop or a drifted clock produces —
/// without it a node exits permanently on a condition it could have fixed itself, because the
/// transport marks 401 non-retriable and the run loop would spend its one rotation on a no-op.
///
/// So the copy handed to a throwaway account is aged instead. **Nothing false reaches disk**: the
/// stored credential is untouched and the only thing `on_rotate` ever writes is what came back
/// rotated. An expiry of *now* also means a rotation that fails leaves nothing presentable, so the
/// failure is reported rather than papered over with the token that was just refused.
/// The access-token lifetime attacca issues, used only to ask when a rotation is due.
///
/// **A copy of a constant `zyris::Account` keeps private** (`ACCESS_LIFETIME_SECS` in
/// `zyris-core/src/account.rs`). It has to be the same number: this is what decides when to drop a
/// held account and read the file again, and `Account` uses it to decide when to rotate. Read it
/// too large and the re-read never happens before the rotation it exists to get ahead of; read it
/// too small and every dial re-reads the file for nothing. If upstream changes it, change this.
const ACCESS_LIFETIME_SECS: i64 = 3600;

fn due_now(credential: zyris::AccountCredential) -> zyris::AccountCredential {
    zyris::AccountCredential::new(
        credential.access_token,
        credential.refresh_token,
        credential.node_id,
        credential.node_name,
        credential.owner_email,
        now_unix(),
    )
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// How the run loop should read a failure that came from the enrollment or account layer.
///
/// Matched exhaustively on purpose: a shade added upstream must stop the build here rather than be
/// folded into "back off and try again", which is the answer that never surfaces anything.
fn enrollment_trouble(error: zyris::EnrollError) -> CredentialsError {
    match error {
        // **The 2026-08-03 incident, and this message is the only thing that explains it.** One
        // scope the deployment does not know refuses the *whole* authorize request: attacca's axum
        // `Json` extractor cannot read an enum variant it has never heard of and answers 422, so
        // nobody ever reaches the approval screen. Naming the scope is the difference between
        // "enrollment is broken" and one line to delete from `conn::REQUIRED_SCOPES`.
        zyris::EnrollError::ScopeUnknown { scope } => CredentialsError::NeedsOperator(format!(
            "this server does not know the scope {scope}; it must be removed from the list this \
             build asks for before enrollment can even show a code"
        )),
        // Somebody said no in the browser. Asking again is pestering them.
        e @ zyris::EnrollError::Denied => CredentialsError::NeedsOperator(e.to_string()),
        // All three are worth another dial rather than an exit. `Lapsed` and `Revoked` reaching
        // here mean the caller has already cleared what it had, so the next attempt enrolls; and a
        // server that is merely unreachable is usually a startup race, not a dead node.
        e @ (zyris::EnrollError::Lapsed
        | zyris::EnrollError::Revoked
        | zyris::EnrollError::Unreachable(_)) => CredentialsError::Unavailable(e.to_string()),
    }
}

/// A store failure that reached the caller needs a person.
///
/// [`AccountGrant::stored`] already swallows the discardable ones on the read path, so what is left
/// is either a refusal — an exposed secret, a machine with nowhere to keep one — or a *write* that
/// could not be kept. Retrying the second would mean a fresh code on every restart and a dead node
/// row in the account behind each one, so neither is a reason to loop.
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

/// A handle to discard credentials and get authorized again. **Used at most once per process.**
///
/// The scopes settled at approval don't widen when the token is refreshed. When a feature grows
/// and one more scope is needed, there is no path but discarding the credentials — the screen side
/// notices that after attaching (`conn::needs_reenrollment`) and calls this.
#[derive(Clone)]
pub struct Reauth {
    store: Arc<dyn CredentialStore>,
    /// The account this process is holding. **Clearing the file is only half of it** — see
    /// `AccountGrant`.
    grant: Arc<AccountGrant>,
    /// Whether this process has already discarded once. **A person can approve narrowly again** —
    /// discarding every time would demand the browser on every attach, not every launch.
    spent: Arc<AtomicBool>,
}

impl Reauth {
    /// A `Reauth` over whatever store the test hands it. Nothing in these tests reaches the
    /// network — the URL never resolves — but the grant has to exist, because discarding has two
    /// halves and one of them is the account this process is holding.
    #[cfg(test)]
    pub(crate) fn for_test(store: Arc<dyn CredentialStore>) -> Reauth {
        let grant = Arc::new(AccountGrant::new(
            store.clone(),
            "wss://example.invalid/zyris/v1/ws".to_string(),
            zyris::EnrollRequest {
                name: "arch zyris-code".to_string(),
                platform: "linux".to_string(),
                scopes: Vec::new(),
            },
            ScreenEnroll { bridge: Bridge::new() },
        ));
        Reauth { store, grant, spent: Arc::new(AtomicBool::new(false)) }
    }

    /// Whether it has already been done. The value fed into the decision (`conn::needs_reenrollment`).
    pub fn spent(&self) -> bool {
        self.spent.load(Ordering::SeqCst)
    }

    /// Discards the credentials **at most once per process.** True if something was discarded.
    ///
    /// The limit is for the automatic path: when the granted scopes come back short
    /// (`conn::needs_reenrollment`), asking again every time would demand a browser on every
    /// reconnect, because the person may approve narrowly again.
    ///
    /// **A person asking to log out is not that path** — see `discard`.
    pub async fn discard_once(&self) -> bool {
        if self.spent.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.discard().await
    }

    /// Discards the credentials, however many times it is asked.
    ///
    /// **`/account logout` must not be silently refused.** It used to go through `discard_once`,
    /// so once the automatic scope check had spent the one allowance, pressing logout cleared
    /// nothing and reported failure — with the credentials still on disk and still working. A
    /// person asking to log out means it every time.
    ///
    /// **The file and the copy in memory both go, in that order.** Clearing the file alone left
    /// this process holding a working access token, so the redial that logging out triggers
    /// presented it and attached — no enrollment code, still connected, credential gone from disk.
    /// The order matters because forgetting first would let a dial in between reload the file and
    /// cache it again.
    pub async fn discard(&self) -> bool {
        // Anything automatic afterwards would be pointless: there is nothing left to discard.
        self.spent.store(true, Ordering::SeqCst);
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
mod tests_discard {
    use super::*;
    use crate::runtime::MemoryCredentialStore;

    fn stored() -> zyris::AccountCredential {
        zyris::AccountCredential::new(
            "a".into(),
            "r".into(),
            "n".into(),
            "arch zyris-code".into(),
            "e@example.com".into(),
            i64::MAX,
        )
    }

    /// A credential due to rotate at a chosen moment. `access_expires_at` is what
    /// `should_refresh` reads, so this is the only knob a test needs to say "due" or "not yet".
    fn expiring_at(at: i64) -> zyris::AccountCredential {
        zyris::AccountCredential::new(
            "a".into(),
            "r".into(),
            "n".into(),
            "arch zyris-code".into(),
            "e@example.com".into(),
            at,
        )
    }

    /// **A rotation that could not be stored stops this node instead of dialling again.**
    ///
    /// `zyris::Account` reports a hook refusal as `Unreachable`, which the run loop answers with
    /// backoff and another dial — but the server has already spent the refresh token by then, so
    /// every retry is a replay, and attacca answers a replay past its 30-second grace with
    /// `revoke_all_for_node`: every node under the credential, not just this one. `NeedsOperator`
    /// is what ends the process instead, which is the answer upstream's `Enroller` gave by
    /// persisting with `?`.
    #[tokio::test]
    async fn a_rotation_that_could_not_be_stored_stops_the_node() {
        let store = std::sync::Arc::new(MemoryCredentialStore::default());
        store.save(&stored()).await.unwrap();
        let reauth = Reauth::for_test(store.clone());
        *reauth.grant.store_broke.lock().await = Some("no space left on device".to_string());

        // The credential is nowhere near expiry, so this asks nothing of the network: the token it
        // would have handed back is perfectly good, and that is exactly the case being locked.
        match reauth.grant.bearer().await {
            Err(CredentialsError::NeedsOperator(why)) => {
                assert!(why.contains("no space left on device"), "the reason is lost: {why}");
            }
            other => panic!("a spent credential must not be dialled with again: {other:?}"),
        }
    }

    /// **`refresh` consults the same memory, and nothing used to check that it did.**
    ///
    /// `bearer` and `refresh` are two doors onto the same credential and both must refuse to open
    /// once a rotation has gone unwritten. Only `bearer`'s guard was pinned: deleting `refresh`'s
    /// outright left all 986 tests green, which is how a guard disappears in a refactor nobody
    /// reviews twice.
    ///
    /// **`refresh` is the door that matters more here.** It runs after the server has already
    /// refused a dial, so by definition something is wrong with the credential — and it *forces* a
    /// rotation (`due_now`) rather than waiting for one. A refusal to store that rotation therefore
    /// arrives on the path most likely to retry, and a retry is a replay.
    #[tokio::test]
    async fn a_refresh_after_a_refused_rotation_stops_the_node_too() {
        let store = std::sync::Arc::new(MemoryCredentialStore::default());
        store.save(&stored()).await.unwrap();
        let reauth = Reauth::for_test(store.clone());
        reauth.grant.hold(stored()).await;
        *reauth.grant.store_broke.lock().await = Some("no space left on device".to_string());

        match reauth.grant.refresh().await {
            Err(CredentialsError::NeedsOperator(why)) => {
                assert!(why.contains("no space left on device"), "the reason is lost: {why}");
            }
            other => panic!(
                "refresh handed back a credential whose rotation was never written, so the run \
                 loop will present a spent token: {other:?}"
            ),
        }
    }

    /// **A store that refuses a rotation is remembered, not only refused.**
    ///
    /// The test above starts from `store_broke` already set, so between them they lock the two
    /// halves: this one that a refusal fills the flag, that one that a filled flag stops the node.
    /// On its own either is comfortable and wrong — a hook that returned `Err` without recording it
    /// passes the test above and still loses the account. `Account::bearer` keeps answering `Ok`
    /// from the token it is holding for the last fifth of that token's life, so in the common case
    /// the refusal reaches nobody at all, and the next rotation replays a refresh token the server
    /// has already spent.
    #[tokio::test]
    async fn a_store_that_refuses_a_rotation_is_remembered_not_just_refused() {
        /// Every write fails, the way a full disk or a config directory that lost its write bit
        /// does.
        #[derive(Debug)]
        struct RefusingStore;

        #[async_trait::async_trait]
        impl CredentialStore for RefusingStore {
            async fn load(&self) -> Result<Option<zyris::AccountCredential>, CredentialStoreError> {
                Ok(None)
            }
            async fn save(
                &self,
                _credential: &zyris::AccountCredential,
            ) -> Result<(), CredentialStoreError> {
                Err(CredentialStoreError::Refused("no space left on device".to_string()))
            }
            async fn clear(&self) -> Result<(), CredentialStoreError> {
                Ok(())
            }
            fn describe(&self) -> String {
                "a store that refuses".to_string()
            }
        }

        let store: Arc<dyn CredentialStore> = Arc::new(RefusingStore);
        let broke = Arc::new(tokio::sync::Mutex::new(None));

        let refused = persist_rotation(&store, &broke, &stored()).await;

        assert!(
            refused.is_err(),
            "a rotation that was not written must not be reported as adopted"
        );
        let remembered = broke.lock().await.clone();
        let why = remembered.expect(
            "the refusal was returned but not remembered, so the run loop will read it as an \
             outage and dial again with a spent credential",
        );
        assert!(why.contains("no space left on device"), "the reason is lost: {why}");
    }

    /// **A credential due to rotate is dropped and the file is read again.**
    ///
    /// Two windows in one directory share one credential file. Upstream went back to disk whenever
    /// its token was spent, so the second window found the pair the first had just written and used
    /// it. Holding an account for the life of the process instead puts both windows on the same
    /// `access_expires_at`, so both rotate from the same single-use refresh token at the same
    /// instant — an appointment rather than a race, and the answer to it is `revoke_all_for_node`.
    #[tokio::test]
    async fn a_credential_due_to_rotate_is_not_handed_back_from_memory() {
        let store = std::sync::Arc::new(MemoryCredentialStore::default());
        let reauth = Reauth::for_test(store.clone());

        // Well inside its life: nothing to re-read, so what is held is what is used.
        reauth.grant.hold(expiring_at(now_unix() + ACCESS_LIFETIME_SECS)).await;
        assert!(reauth.grant.usable().await.is_some(), "a fresh credential is still good");

        // Past the 80% mark, which is where `Account` would rotate. The held one goes, and with it
        // the guarantee that this window rotates from a pair another window has already spent.
        reauth.grant.hold(expiring_at(now_unix() + 60)).await;
        assert!(reauth.grant.usable().await.is_none(), "a due credential was handed back");
        assert!(!reauth.grant.is_holding().await, "the due account is still held");
    }

    /// **Clearing the file is only half of logging out.**
    ///
    /// The token this process is already holding is what the next dial presents, so wiping the
    /// credential file and dropping the socket left the redial attaching on the held token: no
    /// enrollment code, still connected, nothing on disk. Reported 2026-08-14.
    #[tokio::test]
    async fn logging_out_lets_go_of_the_token_this_process_is_holding() {
        let store = std::sync::Arc::new(MemoryCredentialStore::default());
        store.save(&stored()).await.unwrap();
        let reauth = Reauth::for_test(store.clone());
        reauth.grant.hold(stored()).await;

        assert!(reauth.discard().await);
        assert!(store.load().await.unwrap().is_none(), "the file was not cleared");
        assert!(!reauth.grant.is_holding().await, "the token in memory would still attach");
    }

    /// **A person asking to log out means it every time.**
    ///
    /// Logging out went through `discard_once`, which allows one discard per process for the
    /// automatic scope check. Once that allowance was spent, pressing logout cleared nothing and
    /// reported failure — with the credentials still on disk and still working.
    #[tokio::test]
    async fn asking_to_log_out_twice_still_logs_out() {
        let store = std::sync::Arc::new(MemoryCredentialStore::default());
        store.save(&stored()).await.unwrap();
        let reauth = Reauth::for_test(store.clone());

        // The automatic path spends its one allowance.
        assert!(reauth.discard_once().await);
        assert!(!reauth.discard_once().await, "the automatic path is once per process");

        // A person can still log out afterwards.
        store.save(&stored()).await.unwrap();
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

    fn stored() -> zyris::AccountCredential {
        zyris::AccountCredential::new(
            "a".into(),
            "r".into(),
            "n".into(),
            "arch zyris-code".into(),
            "e@example.com".into(),
            i64::MAX,
        )
    }

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

    /// **Credentials are discarded at most once per process.** A person can approve narrowly again,
    /// but discarding on every attach makes a loop that demands the browser each time.
    #[tokio::test]
    async fn a_credential_is_discarded_at_most_once_per_process() {
        let store = Arc::new(MemoryCredentialStore::new());
        store.save(&stored()).await.unwrap();
        let reauth = Reauth::for_test(store.clone());

        assert!(!reauth.spent());
        assert!(reauth.discard_once().await, "the first one is discarded");
        assert!(store.load().await.unwrap().is_none(), "the credential must actually be empty");

        // Even if a fresh credential arrives in between, the second time is left untouched.
        store.save(&stored()).await.unwrap();
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
