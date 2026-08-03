//! 재등록이 일어나는 순간을 알아채고 사람에게 말한다.
//!
//! **등록 코드는 우리가 못 그린다.** 상류가 stdout에 `println!`으로 찍고
//! (`zyris::enroll::http`) 값으로 내주는 길이 없다. 한때 `EnrollmentUi`라는 훅을 로컬
//! zyris에 만들어 썼는데, 그 코드가 어디에도 올라가 있지 않아 워킹트리가 사라지자 이
//! 리포가 통째로 빌드되지 않았다 — **상류에 없는 API에 기대지 않는다.**
//!
//! 그래도 첫 등록은 멀쩡하다. **TUI는 `on_connect`에서 뜨므로** 처음 켤 때는 화면이 아직
//! 없고, 상류가 찍는 상자가 그대로 터미널에 남는다. 덮어쓸 프레임이 없다.
//!
//! 깨지는 것은 **화면이 떠 있는데 재등록이 일어날 때**다(자격이 revoke되거나 리프레시가
//! 영영 실패한 뒤). ratatui가 다음 프레임에 그 위를 덮어 코드가 한 번 깜박이고 사라진다.
//! 그때 사람에게는 "아무 일도 안 일어나는 화면"만 남는다.
//!
//! 여기서 하는 일은 그 순간을 알아채고 화면에 한 줄 남기는 것뿐이다. 코드를 그리지는
//! 못해도, **무슨 일이 벌어졌고 무엇을 하면 되는지**는 말할 수 있다.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use zyris::enroll::CredentialStore;
use zyris::runtime::credentials::{Credentials, CredentialsError};

use crate::app::Frame;
use crate::tools::bridge::Bridge;

/// 이 노드가 쓸 자격.
///
/// 순서는 상류 `credentials::from_env`와 **같아야 한다** — 사람이 명시한 것이 언제나 이기고,
/// 사람에게 물어야 하는 등록은 맨 나중이다. 다른 점은 등록 경로를 `Watch`로 감싸는 것
/// 하나뿐이다. 저장소를 우리가 만들어 쥐고 있으므로 자격 파일 경로를 짐작할 필요가 없다.
///
/// **스코프는 여기 오기 전에 정해져 있어야 한다.** `Enroller`가 `config.scopes`를 이 자리에서
/// 복사해 가므로, 나중에 `Runner::request_scopes`로 준 것은 등록 요청에 실리지 않는다.
pub fn source(
    config: &zyris::runtime::RunConfig,
    bridge: &Bridge,
) -> Result<(Arc<dyn Credentials>, Option<Reauth>), String> {
    use zyris::runtime::credentials::{StaticToken, TokenFile};

    // 사람이 토큰을 직접 준 자리에는 버릴 자격도, 다시 물을 상대도 없다.
    if let Some(token) = StaticToken::from_env().map_err(|e| e.to_string())? {
        return Ok((Arc::new(token), None));
    }
    if let Some(file) = TokenFile::from_env() {
        return Ok((Arc::new(file), None));
    }

    // 자격 파일은 `$ZYRIS_CONFIG_DIR` 아래에 떨어진다. `main.rs`가 그 변수를 이 앱
    // 디렉터리로 채워 두었다(`conn::credential_dir`).
    let store = Arc::new(
        zyris::enroll::FileCredentialStore::for_server(&config.url, &config.profile)
            .map_err(|e| e.to_string())?,
    );
    let store = store as Arc<dyn CredentialStore>;

    let enroller = zyris::enroll::Enroller::new(
        &config.url,
        config.node_name.clone(),
        config.platform().to_string(),
        config.scopes.clone(),
        store.clone(),
    )
    .map_err(|e| e.to_string())?;
    let creds: Arc<dyn Credentials> =
        Arc::new(zyris::runtime::credentials::DeviceGrant::new(enroller));

    let creds = Arc::new(Watch::new(creds, store.clone(), bridge.clone()));
    let reauth = Reauth { store, spent: Arc::new(AtomicBool::new(false)) };
    Ok((creds, Some(reauth)))
}

/// 자격을 버리고 다시 승인받게 하는 손잡이. **프로세스당 한 번만 쓴다.**
///
/// 승인 때 정해진 권한은 토큰을 갱신해도 넓어지지 않는다. 기능이 늘어 권한이 하나 더
/// 필요해지면 자격을 버리는 것 말고는 길이 없다 — 화면 쪽이 붙은 뒤에 그것을 알아채고
/// (`conn::needs_reenrollment`) 여기를 부른다.
#[derive(Clone)]
pub struct Reauth {
    store: Arc<dyn CredentialStore>,
    /// 이 프로세스에서 이미 버려 봤는가. **사람이 또 좁게 승인할 수 있다** — 매번 버리면
    /// 켤 때마다가 아니라 붙을 때마다 브라우저를 요구하는 고리가 된다.
    spent: Arc<AtomicBool>,
}

impl Reauth {
    /// 이미 해 봤는가. 판정(`conn::needs_reenrollment`)에 넣는 값이다.
    pub fn spent(&self) -> bool {
        self.spent.load(Ordering::SeqCst)
    }

    /// 자격을 버린다. 실제로 버렸으면 참이다.
    ///
    /// **지금 도는 연결은 그대로 둔다.** 여기서 끊으면 상류가 등록 코드를 TUI 위에 찍고,
    /// 사람은 화면이 깨진 채로 코드를 못 본다. 다음에 켤 때 화면 없이 깨끗하게 묻는다.
    pub async fn discard_once(&self) -> bool {
        if self.spent.swap(true, Ordering::SeqCst) {
            return false;
        }
        match self.store.clear().await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "자격을 버리지 못했다");
                false
            }
        }
    }
}

/// 화면이 떠 있는 동안 등록 코드가 터미널로 새면 하는 말.
pub const REENROLLED: &str = "**인증이 만료되어 다시 등록해야 합니다.** 새 등록 코드가 이 \
     창 뒤 터미널에 찍혔습니다. 화면에 가려 보이지 않으니 zyris-code를 껐다 다시 켜 주세요.";

/// 자격을 감싸 **등록이 임박한 순간**을 알아챈다.
///
/// 판정은 하나다: 자격 저장소가 비어 있으면 상류는 등록으로 간다. 저장소를 들고 있으므로
/// 경로를 짐작할 필요가 없다 — 예전에는 "자격 파일이 있는지"로 코드가 뜰 때를 알아맞히려
/// 했고, 그건 상류가 무엇을 언제 찍는지 추측하는 일이었다.
pub struct Watch {
    inner: Arc<dyn Credentials>,
    store: Arc<dyn CredentialStore>,
    bridge: Bridge,
    /// **한 번만 말한다.** `bearer()`는 dial 직전마다 불리므로, 못 붙는 동안 재시도가
    /// 도는 내내 같은 줄이 쌓인다.
    said: AtomicBool,
}

impl Watch {
    pub fn new(
        inner: Arc<dyn Credentials>,
        store: Arc<dyn CredentialStore>,
        bridge: Bridge,
    ) -> Watch {
        Watch { inner, store, bridge, said: AtomicBool::new(false) }
    }

    /// 화면이 떠 있는데 등록으로 갈 참이면 한 줄 남긴다.
    ///
    /// **화면이 없으면 아무 말도 안 한다.** 그때는 상류의 상자가 그대로 보이고, 그 위에
    /// 우리 줄을 덧붙이면 상자의 테두리만 흐려진다.
    async fn warn_if_screen_is_up(&self) {
        if !self.bridge.has_screen() || self.said.load(Ordering::SeqCst) {
            return;
        }
        // 못 읽는 저장소는 상류도 못 읽는다 — 그쪽도 등록으로 간다(`load_forgiving`).
        let empty = matches!(self.store.load().await, Ok(None) | Err(_));
        if empty && !self.said.swap(true, Ordering::SeqCst) {
            tracing::warn!("화면이 떠 있는 동안 재등록이 시작됐다. 코드는 터미널로 나간다");
            self.bridge.frame(Frame::Notice(REENROLLED.to_string()));
        }
    }
}

#[async_trait]
impl Credentials for Watch {
    async fn bearer(&self) -> Result<String, CredentialsError> {
        self.warn_if_screen_is_up().await;
        self.inner.bearer().await
    }

    async fn refresh(&self) -> Result<bool, CredentialsError> {
        let refreshed = self.inner.refresh().await;
        // 리프레시가 자격을 버렸으면 다음 `bearer()`가 등록으로 간다. 그 판정은 저장소를
        // 다시 읽어 하므로 여기서는 **다시 말할 수 있게 풀어 주기만** 한다.
        self.said.store(false, Ordering::SeqCst);
        refreshed
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use zyris::enroll::{MemoryCredentialStore, StoredCredential};

    use crate::app::Action;

    /// 무엇을 하든 자격은 그대로 흘려보낸다. **감시가 인증을 바꾸면 안 된다.**
    struct Fixed;

    #[async_trait]
    impl Credentials for Fixed {
        async fn bearer(&self) -> Result<String, CredentialsError> {
            Ok("znt_test".to_string())
        }
        async fn refresh(&self) -> Result<bool, CredentialsError> {
            Ok(true)
        }
        fn describe(&self) -> String {
            "고정 토큰".to_string()
        }
    }

    fn stored() -> StoredCredential {
        StoredCredential::new(
            "a".into(),
            "r".into(),
            "n".into(),
            "arch zyris-code".into(),
            "e@example.com".into(),
            i64::MAX,
        )
    }

    /// 화면을 붙인 다리와, 그 화면이 받는 통.
    fn with_screen() -> (Bridge, mpsc::UnboundedReceiver<Action>) {
        let bridge = Bridge::new();
        let (tx, rx) = mpsc::unbounded_channel();
        bridge.attach(tx);
        (bridge, rx)
    }

    /// **화면이 떠 있는데 자격이 없으면** 코드가 터미널로 새는 것이니 한 줄 남긴다.
    #[tokio::test]
    async fn a_reenrollment_behind_the_screen_is_said_out_loud() {
        let (bridge, mut screen) = with_screen();
        let watch = Watch::new(Arc::new(Fixed), Arc::new(MemoryCredentialStore::new()), bridge);

        assert_eq!(watch.bearer().await.unwrap(), "znt_test");
        let said = screen.try_recv().expect("한 줄 남겨야 한다");
        // **무슨 말을 하는지가 요점이다.** 코드를 못 그리는 대신 무엇을 하면 되는지 말한다.
        assert!(
            matches!(said, Action::Frame(Frame::Notice(text)) if text.contains("다시 켜")),
            "다시 켜라는 말이 있어야 한다"
        );
    }

    /// **자격이 있으면 등록으로 안 간다.** 평소의 길에서 말이 나오면 그게 소음이다.
    #[tokio::test]
    async fn nothing_is_said_when_the_credential_is_there() {
        let (bridge, mut screen) = with_screen();
        let store = Arc::new(MemoryCredentialStore::new());
        store.save(&stored()).await.unwrap();
        let watch = Watch::new(Arc::new(Fixed), store, bridge);

        watch.bearer().await.unwrap();
        assert!(screen.try_recv().is_err(), "평소의 길에서는 조용해야 한다");
    }

    /// **첫 등록은 화면이 없다**(TUI는 `on_connect`에서 뜬다). 그때는 상류의 상자가 그대로
    /// 보이므로 우리가 끼어들 이유가 없다.
    #[tokio::test]
    async fn the_first_enrollment_has_no_screen_to_warn() {
        let bridge = Bridge::new();
        let watch =
            Watch::new(Arc::new(Fixed), Arc::new(MemoryCredentialStore::new()), bridge.clone());
        watch.bearer().await.unwrap();

        let (tx, mut screen) = mpsc::unbounded_channel();
        bridge.attach(tx);
        assert!(screen.try_recv().is_err(), "화면이 붙기 전에 한 말은 아무 데도 안 남는다");
    }

    /// **자격은 프로세스당 한 번만 버린다.** 사람이 또 좁게 승인할 수 있는데, 붙을 때마다
    /// 버리면 그때마다 브라우저를 요구하는 고리가 된다.
    #[tokio::test]
    async fn a_credential_is_discarded_at_most_once_per_process() {
        let store = Arc::new(MemoryCredentialStore::new());
        store.save(&stored()).await.unwrap();
        let reauth = Reauth { store: store.clone(), spent: Arc::new(AtomicBool::new(false)) };

        assert!(!reauth.spent());
        assert!(reauth.discard_once().await, "첫 번째는 버린다");
        assert!(store.load().await.unwrap().is_none(), "자격이 실제로 비어야 한다");

        // 그 사이에 새 자격을 받았어도 두 번째는 손대지 않는다.
        store.save(&stored()).await.unwrap();
        assert!(!reauth.discard_once().await, "두 번째는 안 버린다");
        assert!(store.load().await.unwrap().is_some(), "새로 받은 자격은 그대로다");
        assert!(reauth.spent(), "해 봤다는 것이 판정에 들어간다");
    }

    /// **한 번만 말한다.** `bearer()`는 dial 직전마다 불리므로, 못 붙는 동안 재시도가 도는
    /// 내내 같은 줄이 쌓이면 대화가 그것으로 덮인다.
    #[tokio::test]
    async fn the_same_warning_is_not_repeated_every_dial() {
        let (bridge, mut screen) = with_screen();
        let watch = Watch::new(Arc::new(Fixed), Arc::new(MemoryCredentialStore::new()), bridge);

        watch.bearer().await.unwrap();
        watch.bearer().await.unwrap();
        watch.bearer().await.unwrap();
        assert!(screen.try_recv().is_ok(), "첫 번째는 말한다");
        assert!(screen.try_recv().is_err(), "그 뒤로는 조용하다");
    }
}
