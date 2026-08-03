//! 재등록이 일어나는 순간을 알아채고 **코드를 화면에 그린다.**
//!
//! 상류(zyris)는 `EnrollmentUi` 훅을 제공한다(`Enroller::with_ui`, PR #6). 이 훅에 우리
//! 화면을 꽂으면, 등록 코드가 stdout 상자로 나가는 대신 `Frame::Enroll`로 화면에 도착한다 —
//! 예전의 "코드가 화면 뒤 터미널로 새는" 문제가 구조적으로 사라진다. 화면이 없으면
//! (첫 실행이 화면보다 먼저인 극단) 예전처럼 상자로 찍는다.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use zyris::enroll::{AuthorizeResponse, CredentialStore, EnrollmentUi, TokenResponse};
use zyris::runtime::credentials::Credentials;

use crate::app::{EnrollPhase, EnrollView, Frame};
use crate::tools::bridge::Bridge;

/// 이 노드가 쓸 자격.
///
/// 순서는 상류 `credentials::from_env`와 **같아야 한다** — 사람이 명시한 것이 언제나 이기고,
/// 사람에게 물어야 하는 등록은 맨 나중이다. 다른 점은 등록 경로를 화면과 잇는 것
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

    // 등록 코드는 이 화면이 그린다. 화면이 없으면 상자로 빠진다(`ScreenEnroll::show`).
    let enroller = zyris::enroll::Enroller::new(
        &config.url,
        config.node_name.clone(),
        config.platform().to_string(),
        config.scopes.clone(),
        store.clone(),
    )
    .map_err(|e| e.to_string())?
    .with_ui(Arc::new(ScreenEnroll { bridge: bridge.clone() }));
    let creds: Arc<dyn Credentials> =
        Arc::new(zyris::runtime::credentials::DeviceGrant::new(enroller));

    let reauth = Reauth { store, spent: Arc::new(AtomicBool::new(false)) };
    Ok((creds, Some(reauth)))
}

/// 등록 코드를 화면으로 옮기는 훅. 상류의 폴링 루프가 이 메서드를 부른다.
///
/// `show`가 화면에 닿으면 그 순간부터 화면이 표시를 소유한다 — stdout에는 아무것도
/// 안 나간다. 닿지 않으면(화면이 아직 없거나 이미 죽은 경우) 예전처럼 상자로 찍는다.
pub struct ScreenEnroll {
    bridge: Bridge,
}

impl EnrollmentUi for ScreenEnroll {
    fn show(&self, response: &AuthorizeResponse) {
        let view = EnrollView {
            code: response.user_code.clone(),
            uri: response.verification_uri.clone(),
            expires_at: std::time::Instant::now()
                + Duration::from_secs(response.expires_in.max(0) as u64),
            phase: EnrollPhase::Waiting,
        };
        if !self.bridge.reaches_screen(Frame::Enroll(view)) {
            // 화면이 없으면 상자로 찍는다 — 예전과 같은 길이다. 훅이 있어도
            // 첫 실행(화면이 뜨기 전)은 이것이 전부다.
            println!("{}", zyris::enroll::authorization_notice(response));
        }
    }

    fn lapsed(&self) {
        self.bridge.frame(Frame::EnrollPhase(EnrollPhase::Lapsed));
    }

    fn denied(&self) {
        self.bridge.frame(Frame::EnrollPhase(EnrollPhase::Denied));
    }

    fn authorized(&self, _response: &TokenResponse) {
        self.bridge.frame(Frame::EnrollDone);
    }
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
    /// **지금 도는 연결은 그대로 둔다.** 여기서 끊으면 상류가 등록을 화면 위에 띄우고,
    /// 사람은 등록 코드를 **화면에서** 본다 — 예전처럼 터미널로 새지 않는다. 그래도
    /// 자격은 비워 두므로, 다음에 켤 때(또는 지금 연결이 끊겼다 붙을 때) 깨끗하게 묻는다.
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use zyris::enroll::MemoryCredentialStore;

    use crate::app::Action;

    fn stored() -> zyris::enroll::StoredCredential {
        zyris::enroll::StoredCredential::new(
            "a".into(),
            "r".into(),
            "n".into(),
            "arch zyris-code".into(),
            "e@example.com".into(),
            i64::MAX,
        )
    }

    fn authorize() -> AuthorizeResponse {
        AuthorizeResponse {
            device_code: "zdc_secret".into(),
            user_code: "WXQR-7KBD".into(),
            verification_uri: "https://attacca.example/settings/zyris/device".into(),
            expires_in: 600,
            interval: 5,
        }
    }

    /// 화면을 붙인 다리와, 그 화면이 받는 통.
    fn with_screen() -> (Bridge, mpsc::UnboundedReceiver<crate::app::AppMsg>) {
        let bridge = Bridge::new();
        let (tx, rx) = mpsc::unbounded_channel();
        bridge.attach(tx);
        (bridge, rx)
    }

    /// **화면이 떠 있으면 코드가 화면으로 간다.** stdout으로 새지 않는다.
    #[test]
    fn the_code_goes_to_the_screen_when_one_is_up() {
        let (bridge, mut screen) = with_screen();
        ScreenEnroll { bridge }.show(&authorize());

        match screen.try_recv().expect("화면에 가야 한다") {
            (_, Action::Frame(Frame::Enroll(view))) => {
                assert_eq!(view.code, "WXQR-7KBD");
                assert_eq!(view.uri, "https://attacca.example/settings/zyris/device");
                assert_eq!(view.phase, EnrollPhase::Waiting);
            }
            other => panic!("등록 프레임이어야 한다: {other:?}"),
        }
    }

    /// **화면이 없으면 상자로 찍는다**(예전 길). 그 자리가 첫 실행의 전부다.
    #[test]
    fn without_a_screen_the_code_is_printed() {
        let bridge = Bridge::new();
        // 화면 없이 부르면 stdout으로 상자가 나간다 — 패닉이 없고 다만 그뿐이다.
        ScreenEnroll { bridge }.show(&authorize());
    }

    /// **만료·거부·승인이 화면에 닿는다.** 조용히 사라지면 사람은 무슨 일인지 모른다.
    #[test]
    fn the_outcomes_reach_the_screen() {
        let (bridge, mut screen) = with_screen();
        let ui = ScreenEnroll { bridge };

        ui.lapsed();
        match screen.try_recv().expect("만료가 화면에 가야 한다") {
            (_, Action::Frame(Frame::EnrollPhase(EnrollPhase::Lapsed))) => {}
            other => panic!("만료 프레임이어야 한다: {other:?}"),
        }

        ui.denied();
        match screen.try_recv().expect("거부가 화면에 가야 한다") {
            (_, Action::Frame(Frame::EnrollPhase(EnrollPhase::Denied))) => {}
            other => panic!("거부 프레임이어야 한다: {other:?}"),
        }

        ui.authorized(&TokenResponse {
            access_token: "zna_x".into(),
            refresh_token: "znr_x".into(),
            expires_in: 3600,
            scope: String::new(),
            node_id: "n".into(),
            node_name: "hello node".into(),
            owner_email: "allen@example.com".into(),
        });
        assert!(matches!(screen.try_recv(), Ok((_, Action::Frame(Frame::EnrollDone)))));
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

    /// **토큰을 직접 준 자리에는 버릴 자격이 없다.** `Reauth`가 없는 것이 그 상태다.
    #[test]
    fn a_static_token_has_no_reauth() {
        // source()가 StaticToken 경로로 빠지면 Some(reauth)가 아니다 — 환경변수를
        // 흔들 수 없으므로 여기서는 손잡이가 `None`일 수 있다는 계약만 적는다.
        // 실제 판정은 `conn::missing_scopes` 테스트가 잠근다.
    }
}
