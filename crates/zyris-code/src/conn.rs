//! Attacca와의 대화. 세션은 **첫 메시지에서** 만든다.

use anyhow::{anyhow, Result};
// `AttaccaApi`는 트레이트다. 메서드를 부르려면 클라이언트 타입만으로는 안 되고 트레이트가
// 스코프에 있어야 한다 — 없으면 "method not found"로 막힌다.
use zyris_attacca::{
    AttaccaApi, AttaccaApiClient, ZHistoryQuery, ZNewJob, ZNewProject, ZNewSession, ZNewWork,
    ZSessionFilter, ZTurnFrame,
};

use crate::app::Frame;
use crate::event::entry_from;
use crate::mode::Route;

/// 지금 붙는 에이전트의 이름.
///
/// **원래 목적지는 `zyris-code`다**(`prompts/agents/zyris_code.yml`의 `name`). 그런데 그
/// 에이전트는 아직 개발 중이고, 정의가 콘텐츠 리포의 `develop`에만 있어서 지금 붙으면
/// 전송이 `Agent not found`로 실패한다 — 턴 경로가 기본 브랜치만 보기 때문이다(CLAUDE.md의
/// "걸려 있는 문제" 절). 그때까지는 `Main Agent`로 개발한다.
///
/// 되돌리는 것은 이 상수 한 줄이다.
pub const DEFAULT_AGENT: &str = "Main Agent";

/// 이 앱이 실제로 부르는 attacca 호출에 필요한 권한 전부.
///
/// attacca의 `zyris_gateway.rs`가 호출마다 `require(ApiScope::…)`로 잰다:
///
/// | 부르는 것 | 필요한 권한 |
/// |---|---|
/// | `me` | 없음 |
/// | `list_agents` | `agents:read` |
/// | `list_projects` | `projects:read` |
/// | `create_project` | `projects:write` |
/// | `list_sessions`·`session_usage`·`session_history` | `sessions:read` |
/// | `create_session_with`·`send_message`·`cancel_turn` | `sessions:write` |
/// | `turn_events` | `events:read` |
/// | `create_work`·`work_message`·`stop_work`·`continue_work` | `works:write` |
/// | `list_works`·`get_work`·`work_tasks` | `works:read` |
/// | `create_job` | `jobs:write` |
/// | `list_jobs`·`get_job` | `jobs:read` |
///
/// **하나라도 빠지면 그 목록만 조용히 빈 채로 돌아온다.** 오류가 아니라 빈 결과라,
/// 사람은 자기 계정에 에이전트나 프로젝트가 없는 줄 안다. 실제로 그렇게 걸렸다.
///
/// 요청할 때와 확인할 때가 **같은 목록이어야 한다.** 갈라지면 요청하지도 않은 것을
/// 없다고 말하거나, 없는데도 아무 말을 안 한다.
///
/// **하나라도 없으면 앱이 제 일을 못 한다.** 모자라면 자격을 버리고 다시 승인받는다
/// (`needs_reenrollment`).
///
/// **여기에 새 스코프를 더하기 전에 서버가 그것을 아는지 먼저 재 볼 것.** 모르는 스코프가
/// 하나라도 있으면 등록 요청이 통째로 막힌다 — axum의 `Json` 추출기가 열거형을 못 읽어
/// 422로 거절하는 것이라 **승인 화면까지 가지도 못한다.** 2026-08-03에 `nodes:write`를
/// 넣었다가 정확히 그렇게 막혔고, 오류 본문이 배포본이 받는 목록 전체를 알려 준다:
///
/// ```text
/// POST /api/zyris/v1/device/authorize {"scopes":[…,"nodes:write"], …}
///   → 422 … unknown variant `nodes:write`, expected one of `agents:read`, … `events:read`
/// ```
pub const REQUIRED_SCOPES: [&str; 10] = [
    "agents:read",
    "projects:read",
    // `/project <이름>`이 쓴다. 2026-08-03에 배포본에 재 보고 넣었다 — 200이었다.
    "projects:write",
    "sessions:read",
    "sessions:write",
    "events:read",
    // `work` 캐퍼빌리티가 쓴다(`tools/work.rs`). 큰 일을 attacca에 넘기는 길이다.
    // work 모드도 같은 것을 쓴다 — `create_work`가 `works:write`, 계획 대화를 기다리는
    // `get_work`가 `works:read`다.
    "works:read",
    "works:write",
    // job 모드(`Session::open_job`). **2026-08-03에 배포본에 직접 재 보고 넣었다** —
    // 위에 적어 둔 대로, 서버가 모르는 스코프 하나면 등록이 통째로 422다:
    //
    // ```text
    // POST /api/zyris/v1/device/authorize {"scopes":[…,"jobs:read","jobs:write"], …}
    //   → 200 {"device_code":"zdc_…","user_code":"…"}
    // ```
    "jobs:read",
    "jobs:write",
];

/// 이 프로그램의 이름. 자격 디렉터리가 이것으로 갈린다.
pub const APP: &str = "zyris-code";

/// zyris를 쓰는 모든 프로그램이 같이 쓰던 옛 자리. 여기 남은 자격은 첫 실행에 옮겨 온다.
pub const LEGACY_APP: &str = "zyris";

/// 자격이 사는 디렉터리. `/cwd`가 보여준다.
///
/// **`~/.config/zyris/`가 아니다.** 거기는 zyris를 쓰는 모든 프로그램이 같이 쓰던 자리라,
/// 프로필을 안 정한 둘이 서로의 신원 위에 등록했다. 사람이 "내 로그인이 어디 있나"를
/// 물을 때 옛 경로를 말하면 엉뚱한 파일을 지우게 된다.
pub fn credential_home() -> String {
    credential_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "자격을 둘 디렉터리를 찾지 못했습니다".to_string())
}

/// 자격을 둘 자리. **우리가 계산한다.**
///
/// 상류에는 앱별 디렉터리를 정하는 길이 없다(`RunConfig::app`은 zyris `main`에 없다). 대신
/// zyris의 `config_dir()`이 **`$ZYRIS_CONFIG_DIR`을 가장 먼저 본다** — 그러니 그 변수를
/// 이 값으로 채우면(`main.rs`) 자격이 이 앱 자리에 떨어진다.
pub fn credential_dir() -> Option<std::path::PathBuf> {
    config_home_for(APP)
}

/// 옛 자리. 첫 실행에 여기 있는 자격을 옮겨 온다.
pub fn legacy_credential_dir() -> Option<std::path::PathBuf> {
    // 사람이 `$ZYRIS_CONFIG_DIR`을 준 경우에는 옮길 옛 자리랄 것이 없다 — 그 사람은
    // 어디에 둘지 이미 정했고, 우리가 다른 디렉터리를 뒤질 이유가 없다.
    if given_config_dir().is_some() {
        return None;
    }
    config_home_for(LEGACY_APP)
}

/// 사람이 정한 자리. **빈 값은 안 준 것으로 친다** — `ZYRIS_CONFIG_DIR=`로 지우려 한
/// 사람에게 빈 경로를 돌려주면 자격이 작업 디렉터리에 떨어진다.
fn given_config_dir() -> Option<std::ffi::OsString> {
    std::env::var_os("ZYRIS_CONFIG_DIR").filter(|v| !v.is_empty())
}

/// `$ZYRIS_CONFIG_DIR` → 플랫폼의 사용자 설정 자리 아래 `app`.
///
/// zyris의 `enroll::config_dir()`과 **같은 갈래**를 따른다. 갈라지면 우리가 채운 변수와
/// 상류가 읽는 자리가 어긋나 자격이 두 군데로 흩어진다.
fn config_home_for(app: &str) -> Option<std::path::PathBuf> {
    if let Some(given) = given_config_dir() {
        // 사람이 정확히 그 자리를 뜻한 것이다. 앱 이름을 덧붙이지 않는다.
        return Some(std::path::PathBuf::from(given));
    }
    let base = platform_config_base()?;
    Some(base.join(app))
}

#[cfg(target_os = "macos")]
fn platform_config_base() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join("Library/Application Support"))
}

#[cfg(target_os = "windows")]
fn platform_config_base() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_config_base() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
}

/// 옛 자리에 남은 이 프로필의 자격을 **옮겨 온다.** 옮긴 개수를 돌려준다.
///
/// **복사가 아니라 이동이다.** 살아 있는 리프레시 토큰이 디스크에 둘이면 언젠가 둘 다
/// 제시되고, attacca는 유예 30초를 넘긴 재사용을 사슬이 샌 것으로 보아 **노드를 통째로
/// revoke한다**(`zyris_enrollment_service.rs`의 `RefreshAttempt::Reused`). 둘 다 죽는다.
///
/// **이미 여기 있는 것은 안 덮어쓴다.** 이 앱이 이미 등록해 둔 자격이 옛 파일보다 뒤이고,
/// 덮어쓰면 지금 붙어 있는 신원을 잃는다. 그때는 옛 파일도 지우지 않는다 — 우리가 안 가진
/// 자격을 지우는 것은 남의 것을 버리는 일이다.
pub fn migrate_credentials(from: &std::path::Path, into: &std::path::Path, profile: &str) -> usize {
    let suffix = format!("-{}.json", slugify_profile(profile));
    let Ok(entries) = std::fs::read_dir(from) else {
        return 0;
    };
    let mut moved = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };
        if !name_str.ends_with(&suffix) || !entry.path().is_file() {
            continue;
        }
        let target = into.join(&name);
        if target.exists() {
            continue;
        }
        if std::fs::create_dir_all(into).is_err() {
            continue;
        }
        // 같은 파일시스템이면 rename 한 번이다. 홈이 여러 마운트로 갈려 있으면 EXDEV로
        // 실패하므로 복사한 **뒤에 지운다** — 지우기가 실패하면 두 벌이 남으니 그때는
        // 옮기지 않은 것으로 친다.
        let done = match std::fs::rename(entry.path(), &target) {
            Ok(()) => true,
            Err(_) => match std::fs::copy(entry.path(), &target) {
                Ok(_) => match std::fs::remove_file(entry.path()) {
                    Ok(()) => true,
                    Err(_) => {
                        let _ = std::fs::remove_file(&target);
                        false
                    }
                },
                Err(_) => false,
            },
        };
        if done {
            // 자격이다. 옮기면서 권한이 느슨해지면 다음 실행에 상류가 읽기를 거부한다.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600));
            }
            moved += 1;
        }
    }
    moved
}

/// 자격 파일 이름에 들어가는 프로필 조각.
///
/// **zyris의 `file_store::slugify`를 그대로 베껴 둔 것이다.** 파일 이름을 짓는 쪽은 상류이고
/// 여기는 그것을 알아보기만 하므로, 규칙이 갈라지면 옮길 파일을 못 알아보고 조용히 지나친다
/// — 사람 눈에는 "다시 등록하라는 화면"으로만 보인다. 상류가 바뀌면 여기도 같이 고친다.
fn slugify_profile(profile: &str) -> String {
    let mut out = String::with_capacity(profile.len());
    let mut prev_dash = false;
    for ch in profile.chars() {
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

/// 받은 권한에서 **모자란 것**. 요청 목록과 확인 목록은 언제나 `REQUIRED_SCOPES` 하나다.
pub fn missing_scopes(granted: &[String]) -> Vec<&'static str> {
    REQUIRED_SCOPES.iter().copied().filter(|s| !granted.iter().any(|g| g == s)).collect()
}

/// 자격을 버리고 다시 승인받아야 하는가. **순수 판정이다.**
///
/// 승인 때 정해진 권한은 토큰을 갱신해도 넓어지지 않는다. 그러니 기능이 늘어 권한이 하나
/// 더 필요해지면 길은 하나뿐이다 — 자격을 버리고 다시 묻는 것.
///
/// **프로세스당 한 번이다.** 사람이 또 좁게 승인할 수 있는데, 그때마다 다시 물으면 브라우저를
/// 계속 요구하는 고리가 된다. 한 번 해 봤으면 그 다음은 말로만 알린다.
pub fn needs_reenrollment(granted: &[String], already_tried: bool) -> bool {
    !already_tried && !missing_scopes(granted).is_empty()
}

/// 권한이 모자랄 때 사람에게 할 말.
///
/// **"부족합니다"만으로는 길이 없다.** 승인 때 정해진 권한은 토큰을 갱신해도 넓어지지
/// 않으므로, 할 수 있는 일은 자격을 버리고 다시 승인받는 것 하나뿐이다. 그 방법을 적는다.
pub fn missing_scopes_message(missing: &[&str]) -> String {
    format!(
        "**권한이 모자랍니다: {}**. 승인할 때 정해진 권한은 나중에 넓어지지 않습니다. \
         zyris-code를 다시 켜면 새 등록 코드가 뜨니, 승인 화면에서 권한을 **모두** \
         체크해 주세요.",
        missing.join(", ")
    )
}

/// 모자란 권한 때문에 자격을 버렸을 때 할 말.
///
/// **"다시 켜면 된다"까지 말해야 한다.** 여기서 연결을 끊고 그 자리에서 다시 묻지 않는
/// 이유는, 상류가 등록 코드를 stdout에 찍어 TUI 위에 겹치기 때문이다 — 화면이 없는
/// 다음 실행이 코드를 보여주기에 훨씬 낫다.
pub fn scopes_will_be_asked_again(missing: &[&str]) -> String {
    format!(
        "**권한이 모자랍니다: {}**. 다시 승인받을 수 있도록 이 컴퓨터의 자격을 비웠습니다. \
         zyris-code를 껐다 켜면 새 등록 코드가 뜨니, 승인 화면에서 권한을 **모두** 체크해 \
         주세요. 지금 연결은 그대로 쓸 수 있습니다.",
        missing.join(", ")
    )
}

/// 붙을 에이전트. `ZYRIS_CODE_AGENT`로 덮어쓴다.
pub fn agent_name() -> String {
    std::env::var("ZYRIS_CODE_AGENT").unwrap_or_else(|_| DEFAULT_AGENT.to_string())
}

/// 서버에 등록할 노드 이름.
///
/// **호스트 이름만 쓰면 이 머신의 다른 노드와 같은 신원이 된다.** 같은 컴퓨터에서
/// `zyris-daemon`이 돌고 있으면 둘 다 `arch`로 등록되고, attacca는
/// `slug_with_suffix`로 한쪽에 `-2`를 붙여 떼어 놓는다 — **어느 쪽이 `arch`를 갖는지는
/// 붙는 순서에 달려 있어**, 도구 이름(`zyris__arch__…`)이 실행마다 바뀔 수 있다.
///
/// 그래서 이 앱은 자기 이름을 달고 등록한다: `arch zyris-code`.
///
/// **길이에 걸린다.** attacca의 `slugify_node_name`은 영숫자만 남기고 나머지를 하이픈으로
/// 접은 뒤 **16자에서 자른다**(`ZYRIS_NODE_SLUG_MAX_LEN`). `arch zyris-code`는
/// `arch-zyris-code`(15자)로 딱 들어가지만, 호스트 이름이 길면 뒤쪽 `zyris-code`가 잘려
/// 나가 도로 호스트 이름만 남는다. 그때는 **구별되는 쪽을 앞에 둔다.**
pub fn node_name() -> String {
    let host = zyris::machine_name().unwrap_or_else(|| "node".to_string());
    let natural = format!("{host} {SUFFIX}");
    if slug_of(&natural).contains(SUFFIX) {
        natural
    } else {
        // 잘려서 앱 이름이 사라졌다. 순서를 뒤집으면 적어도 무엇인지는 남는다.
        format!("{SUFFIX} {host}")
    }
}

/// attacca가 이 노드에 줄 슬러그. 도구 이름의 가운데 조각이다.
///
/// **겹치면 서버가 `-2`를 붙인다**(`slug_with_suffix`). 그러니 여기 값이 언제나 실제와
/// 같지는 않다 — 같은 이름의 노드가 둘이면 어느 쪽이 맨 이름을 갖는지는 붙은 순서다.
pub fn node_slug() -> String {
    slug_of(&std::env::var("ZYRIS_NODE_NAME").unwrap_or_else(|_| node_name()))
}

/// 이름 뒤에 붙는 이 앱의 표시. 슬러그에서 이것이 살아남아야 구별이 된다.
const SUFFIX: &str = "zyris-code";

/// attacca의 `slugify_node_name`과 **같은 규칙**이다(`attacca-domain/src/zyris_node.rs`).
///
/// 서버가 무엇을 만들지 여기서 미리 알아야 이름이 잘리는지 판단할 수 있다. 규칙이
/// 갈라지면 이 판단이 틀리므로, 바뀌면 여기도 같이 고쳐야 한다.
fn slug_of(name: &str) -> String {
    const MAX: usize = 16;
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
        if slug.len() >= MAX {
            break;
        }
    }
    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "node".to_string()
    } else {
        trimmed
    }
}

#[derive(Debug, Default)]
pub struct Session {
    id: Option<String>,
    /// 지금 보고 있는 프로젝트. **여기서 여는 것은 전부 이 프로젝트로 간다** —
    /// 세션도, job도, work도.
    ///
    /// **한 번 쓰고 지우면 안 된다.** 예전에는 `＋ 새 세션`이 채우고 첫 생성이 지우는
    /// 일회용이었는데, 그러면 프로젝트를 골라 놓고 job을 걸었을 때 `project_id`가 이미
    /// 비어 서버가 **기본 프로젝트에** 만든다. 실제로 그렇게 걸렸다.
    ///
    /// `None`은 "아직 안 골랐다"이고 그때만 서버의 기본 프로젝트가 맞다.
    project: Option<String>,
    /// 세션마다 붙는 시스템 지시. 스킬이 하나도 없으면 `None`이다.
    preamble: Option<String>,
    /// 다음 메시지가 **새로 열** 것. `None`이면 지금 세션에 이어 붙인다.
    ///
    /// **`id`를 비우는 것으로 대신할 수 없다.** work·job 모드로 갔다가 아무 말도 안 하고
    /// 돌아오는 일이 흔한데, 그때 `id`를 이미 버렸으면 하던 대화를 잃는다. 예약과
    /// "세션이 없다"는 서로 다른 상태다.
    pending_open: Option<Route>,
}

/// 세션을 열고 나서 알아야 할 것.
///
/// **`sent`이 요점이다.** `create_job`·`create_work`는 여는 요청이 **첫 메시지를 먹으므로**
/// (`ZNewJob::message`·`ZNewWork::message`) 뒤이어 `send_message`를 또 부르면 같은 말이
/// 두 번 들어간다. 세션을 그냥 만든 길에서는 아직 아무것도 안 갔다.
#[derive(Debug, Clone)]
pub struct Opened {
    pub id: String,
    /// 첫 메시지가 여는 요청에 이미 실려 갔는가.
    pub sent: bool,
    /// 방금 무엇을 열었는가. 아무것도 새로 안 열었으면 `None`이다 — 화면이 그때만 말한다.
    pub announced: Option<(Route, String)>,
}

impl Session {
    /// `preamble`은 이 세션의 시스템 지시다 — 지금은 스킬 목록이 실린다.
    ///
    /// **세션을 만들 때 한 번 정해지고 뒤에 바꿀 수 없다**(attacca의 `ZNewSession`).
    /// 그래서 나중에 붙는 MCP 도구는 여기 실리지 않는다 — 그쪽은 도구 목록으로 간다.
    pub fn new(preamble: Option<String>) -> Self {
        Session { preamble, ..Default::default() }
    }

    /// 이미 만들어졌다면 그 id.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// 다른 세션으로 갈아탄다.
    ///
    /// **예약을 지운다.** 목록에서 세션을 고른 것은 "저기로 간다"는 뜻이라, 그 말을 job으로
    /// 흘려보내면 고른 자리로 안 간다.
    ///
    /// **프로젝트는 지우는 것이 아니라 고른 세션의 것으로 바꾼다.** 비우면 그 다음에 여는
    /// job이 기본 프로젝트로 떨어진다.
    ///
    /// 어느 프로젝트인지 모르는 자리도 있다(시작할 때 답 기다리는 세션으로 들어가는 길).
    /// 그때는 `None`을 주고 **알던 것을 그대로 둔다** — 모른다고 비우면 그것이 곧 기본
    /// 프로젝트로 떨어지는 길이다.
    pub fn switch_to(&mut self, id: String, project_id: Option<String>) {
        self.id = Some(id);
        if let Some(p) = project_id {
            self.project = Some(p);
        }
        self.pending_open = None;
    }

    /// 프로젝트 목록에서 하나를 열었다. **세션을 고르기 전에도 기억해 둔다** — 목록만
    /// 열어 보고 Esc로 닫은 뒤 job을 걸어도 그 프로젝트로 가야 한다.
    pub fn enter_project(&mut self, project_id: String) {
        self.project = Some(project_id);
    }

    /// 지금 프로젝트. `/cwd`와 테스트가 본다.
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    /// 새 세션을 예약한다. 아직 서버에는 아무것도 만들지 않는다.
    pub fn stage_new(&mut self, project_id: String) {
        self.id = None;
        self.project = Some(project_id);
        self.pending_open = None;
    }

    /// 모드가 정한 곳으로 다음 메시지가 가도록 맞춘다.
    ///
    /// **`Route::Session`은 지금 대화를 그대로 둔다.** 기본↔계획이 세션을 안 건드리는 것이
    /// 그 뜻이고, work·job에 있다가 기본으로 돌아오는 것도 "그것에게 답한다"는 뜻이지
    /// 새 대화를 여는 것이 아니다.
    ///
    /// 반대로 work·job으로 **들어가는 것은 언제나 새로 여는 것**이다. 이미 열어 둔 job이
    /// 있어도 또 건다 — 모드를 다시 고른 것은 그러겠다는 뜻이다.
    pub fn set_route(&mut self, route: Route) {
        self.pending_open = match route {
            Route::Session => None,
            other => Some(other),
        };
    }

    /// 다음 메시지가 새로 열 것. 화면이 무슨 말을 할지 정하는 데 쓴다.
    pub fn pending_open(&self) -> Option<Route> {
        self.pending_open
    }

    /// 기본 프로젝트에 새 세션을 예약한다. `/agent`이 부른다.
    ///
    /// **세션의 에이전트는 만들 때 정해지고 바꾸는 API가 없다**(`ZNewSession.agent_id`,
    /// `send_message`에는 에이전트 인자가 없다). 그러니 에이전트를 바꾸려면 세션을 새로
    /// 여는 수밖에 없다. 여기서도 서버에는 아무것도 만들지 않는다 — 실제 생성은 첫
    /// 메시지에서다. **앞 세션은 지우지 않는다**: ←의 목록으로 돌아갈 수 있다.
    pub fn stage_new_default(&mut self) {
        self.id = None;
        // **프로젝트는 그대로 둔다.** `/agent`은 에이전트를 바꾼 것이지 프로젝트를 떠난
        // 것이 아니다 — 여기서 비우면 다음 세션이 기본 프로젝트에 생긴다.
        self.pending_open = None;
    }

    /// 전용 에이전트를 찾는다.
    ///
    /// **찾지 못하면 다른 에이전트로 폴백하지 않는다.** 조용히 폴백하면 상태줄에는
    /// 이름이 뜨는데 전송이 `Agent not found`로 실패하는, 원인을 찾기 어려운 상태가 된다.
    pub async fn agent_id(api: &AttaccaApiClient) -> Result<String> {
        Session::agent_id_named(api, &agent_name()).await
    }

    /// 이름으로 에이전트를 찾는다. `/agent`과 시작 시점이 같은 길을 쓴다.
    pub async fn agent_id_named(api: &AttaccaApiClient, wanted: &str) -> Result<String> {
        let agents =
            api.list_agents().await.map_err(|e| anyhow!("에이전트 목록을 읽지 못했습니다: {e}"))?;
        agents.into_iter().find(|a| a.name == wanted).map(|a| a.id).ok_or_else(|| {
            anyhow!("'{wanted}' 에이전트가 계정에 없습니다. /agent으로 목록을 볼 수 있습니다.")
        })
    }

    /// 세션 id를 준다. 없으면 지금 만든다.
    ///
    /// `title`은 반드시 `None`이다 — 여기서 제목을 주면 영구가 되고, attacca가 첫
    /// 메시지로 제목을 붙이는 동작이 막힌다.
    pub async fn ensure(&mut self, api: &AttaccaApiClient, agent_id: &str) -> Result<String> {
        if let Some(id) = &self.id {
            return Ok(id.clone());
        }
        let session = api
            .create_session_with(ZNewSession {
                agent_id: agent_id.to_string(),
                title: None,
                // 예약해 둔 프로젝트가 있으면 거기에 만든다. 없으면 기본 프로젝트다.
                project_id: self.project.clone(),
                preamble: self.preamble.clone(),
            })
            .await
            .map_err(|e| anyhow!("thread를 만들지 못했습니다: {e}"))?;
        self.id = Some(session.id.clone());
        Ok(session.id)
    }

    /// 모드가 정한 곳을 연다. **보내기 직전에 딱 한 번 부른다.**
    ///
    /// 셋 다 끝에는 평범한 세션 id가 나오므로(`ZJob::session_id`,
    /// `ZWork::planner_session_id`) 부른 쪽은 무엇을 열었는지 몰라도 그대로 스트림을 연다.
    pub async fn open_for(
        &mut self,
        api: &AttaccaApiClient,
        agent_id: &str,
        message: &str,
        mode: crate::mode::Mode,
    ) -> Result<Opened> {
        // **예약은 한 번 쓰면 없어진다.** 안 지우면 job 모드에 머무는 동안 말할 때마다
        // job이 하나씩 생겨, 되묻는 말에 답할 자리가 영영 안 생긴다.
        //
        // **예약이 없어도 대화가 아직 없으면 모드가 정한다.** 예약은 모드가 *바뀌는*
        // 순간에만 걸리므로 안 걸리는 자리가 여럿이다 — 켜자마자 첫 마디, `/agent`으로
        // 새 쓰레드를 예약한 뒤, `＋ 새 세션` 뒤. 거기서 세션만 만들면 **하단 바는 job인데
        // 열리는 것은 맨 세션**이 된다. 실제로 그렇게 걸렸다.
        let route = match self.pending_open.take() {
            Some(staged) => staged,
            // 이어 갈 대화가 있으면 이어 간다. 모드는 *새로 열 때* 무엇을 열지만 정한다.
            None if self.id.is_some() => Route::Session,
            None => mode.route(),
        };
        match route {
            Route::Job => self.open_job(api, agent_id, message).await,
            Route::Work => self.open_work(api, agent_id, message).await,
            Route::Session => {
                let id = self.ensure(api, agent_id).await?;
                Ok(Opened { id, sent: false, announced: None })
            }
        }
    }

    /// **첫 메시지가 시킬 일이 된다**(`ZNewJob::message`). 그래서 `sent`이 참이다.
    async fn open_job(
        &mut self,
        api: &AttaccaApiClient,
        agent_id: &str,
        message: &str,
    ) -> Result<Opened> {
        let job = api
            .create_job(ZNewJob {
                message: message.to_string(),
                // **고른 에이전트로 돌린다.** 비우면 Main Agent로 가는데, 그러면 `/agent`으로
                // 고른 것이 화면에만 남고 실제로 도는 것은 다른 에이전트가 된다.
                agent_id: Some(agent_id.to_string()),
                project_id: self.project.clone(),
                // 배포본의 시간대를 그대로 쓴다. 이 머신의 시간대를 억지로 밀어 넣으면
                // 같은 계정의 다른 job과 답이 갈린다.
                timezone: None,
                // **둘 다 끄고 둔다.** `planning`은 job을 work로 넘기는 것이고 `plan_mode`는
                // job 안에서 계획을 받고 멈추는 것인데, 여기서는 모드가 이미 그 갈래다 —
                // job 모드는 "시켜 놓는다"이고, 계획이 필요하면 계획 모드나 work 모드다.
                planning: false,
                plan_mode: false,
                data: vec![],
            })
            .await
            .map_err(|e| anyhow!("job을 걸지 못했습니다: {e}"))?;

        let id = job.session_id.clone().ok_or_else(|| {
            anyhow!(
                "job **{}**은 걸렸는데 세션이 아직 없어 여기서 못 봅니다. \
                 attacca에서 열어 보세요.",
                job.id
            )
        })?;
        self.id = Some(id.clone());
        Ok(Opened { id, sent: true, announced: Some((Route::Job, job.id)) })
    }

    /// **첫 메시지가 목표가 된다**(`ZNewWork::message`). 그래서 `sent`이 참이다.
    async fn open_work(
        &mut self,
        api: &AttaccaApiClient,
        agent_id: &str,
        message: &str,
    ) -> Result<Opened> {
        let work = api
            .create_work(ZNewWork {
                message: message.to_string(),
                agent_id: Some(agent_id.to_string()),
                // **work의 태스크는 프로젝트의 체크아웃에서 돈다.** 여기가 비면 기본
                // 프로젝트가 되고, 그것이 무엇을 바꿔도 되는지를 정한다.
                project_id: self.project.clone(),
            })
            .await
            .map_err(|e| anyhow!("work를 만들지 못했습니다: {e}"))?;

        let id = planner_session(api, &work).await?;
        self.id = Some(id.clone());
        Ok(Opened { id, sent: true, announced: Some((Route::Work, work.id)) })
    }
}

/// work의 계획 대화를 집는다. **없으면 잠깐 기다려 본다.**
///
/// `create_work`는 계획 턴을 걸고 돌아오지만 `planner_session_id`가 그 응답에 이미 들어
/// 있으리라는 보장이 없다 — 서버가 세션을 만드는 것과 work 행을 돌려주는 것이 같은
/// 트랜잭션이 아니다. 여기서 포기하면 사람 눈에는 **말이 그냥 사라진 것**으로 보인다.
///
/// 그렇다고 오래 붙들 수도 없다. 기다리는 동안 화면은 아무 말도 못 한다.
async fn planner_session(api: &AttaccaApiClient, work: &zyris_attacca::ZWork) -> Result<String> {
    if let Some(id) = &work.planner_session_id {
        return Ok(id.clone());
    }
    for _ in 0..PLANNER_TRIES {
        tokio::time::sleep(PLANNER_WAIT).await;
        match api.get_work(work.id.clone()).await {
            Ok(fresh) => {
                if let Some(id) = fresh.planner_session_id {
                    return Ok(id);
                }
            }
            // 한 번 실패했다고 그만두지 않는다. 다음 바퀴에 다시 물어본다.
            Err(e) => tracing::debug!(error = %e, work = %work.id, "work를 다시 읽지 못했다"),
        }
    }
    Err(anyhow!(
        "work **{}**은 만들었는데 계획 대화가 아직 안 열려 여기서 못 봅니다. \
         attacca에서 열어 보세요.",
        work.id
    ))
}

/// 계획 대화를 기다리는 시간 — 넉넉잡아 3초. 넘기면 기다리는 것이 아니라 멈춰 있는 것이다.
const PLANNER_TRIES: u32 = 6;
const PLANNER_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// 와이어 프레임을 앱 프레임으로. 렌더하지 않는 이벤트도 **커서는 넘긴다.**
pub fn frame_from(f: ZTurnFrame) -> Frame {
    match f {
        ZTurnFrame::Event { cursor, event } => Frame::Event { cursor, entry: entry_from(&event) },
        ZTurnFrame::Delta { kind, text } => Frame::Delta { kind, text },
        ZTurnFrame::Status { running } => Frame::Status { running },
    }
}

/// 프로젝트를 만든다. 만든 것의 `(id, 이름)`을 준다.
///
/// **이름이 비면 부르지 않는다** — 서버가 무엇을 만들지 모르고, 목록에 이름 없는 줄이
/// 하나 생기면 지우는 길이 이 앱에 없다.
pub async fn create_project(api: &AttaccaApiClient, name: &str) -> Result<(String, String)> {
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!("프로젝트 이름을 같이 적어 주세요: `/project 이름`"));
    }
    let p = api
        .create_project(ZNewProject { name: name.to_string(), description: None })
        .await
        .map_err(|e| anyhow!("프로젝트를 만들지 못했습니다: {e}"))?;
    Ok((p.id, p.name))
}

/// 프로젝트 목록을 픽커가 쓰는 모양으로.
pub async fn projects(api: &AttaccaApiClient) -> Result<Vec<(String, String, bool)>> {
    let items =
        api.list_projects().await.map_err(|e| anyhow!("프로젝트 목록을 읽지 못했습니다: {e}"))?;
    Ok(items.into_iter().map(|p| (p.id, p.name, p.is_default)).collect())
}

/// 한 프로젝트의 세션 목록. 제목이 없는 세션은 첫 메시지 전이라 그렇게 말해 준다.
pub async fn sessions(
    api: &AttaccaApiClient,
    project_id: &str,
) -> Result<Vec<(String, String, bool)>> {
    let items = api
        .list_sessions(ZSessionFilter { project_id: Some(project_id.to_string()), limit: Some(50) })
        .await
        .map_err(|e| anyhow!("thread 목록을 읽지 못했습니다: {e}"))?;
    Ok(items
        .into_iter()
        .map(|s| {
            let title =
                s.title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "제목 없음".into());
            (s.id, title, s.running)
        })
        .collect())
}

/// 세션의 지난 기록. 갈아탈 때 화면을 채우는 데 쓴다.
///
/// `after`를 비우면 전체다 — `turn_events`와 반대이니 헷갈리지 말 것.
pub async fn history(
    api: &AttaccaApiClient,
    session_id: &str,
) -> Result<Vec<zyris_attacca::ZSessionEvent>> {
    api.session_history(session_id.to_string(), ZHistoryQuery::default())
        .await
        .map_err(|e| anyhow!("지난 기록을 읽지 못했습니다: {e}"))
}

/// 답을 기다리고 있는 세션을 찾는다.
///
/// 질문에 답하지 않은 채 앱을 끄면 서버는 계속 기다린다. 다시 켰을 때 사람이 그 세션을
/// 손으로 찾아 들어가야 한다면 사실상 답할 길이 없는 것과 같다 — 켜자마자 집어 준다.
///
/// 막혀 있는 세션은 `running`이 서 있으므로 목록만으로 좁혀진다. 히스토리는 그 몇 개만
/// 읽는다.
pub async fn session_awaiting_answer(api: &AttaccaApiClient) -> Option<String> {
    let sessions =
        api.list_sessions(ZSessionFilter { project_id: None, limit: Some(50) }).await.ok()?;
    for s in sessions.into_iter().filter(|s| s.running).take(5) {
        let events = history(api, &s.id).await.ok()?;
        // 답을 기다리는 질문이 하나라도 있으면 그 세션이다.
        let pending = events.iter().rev().take(50).any(|e| {
            matches!(
                crate::event::entry_from(e).map(|x| x.kind),
                Some(crate::event::EntryKind::Question { answered: false, .. })
            )
        });
        if pending {
            return Some(s.id);
        }
    }
    None
}

/// 세션 사용량. 배포가 미터링을 안 하면 `capability_not_announced`가 오는데,
/// 그건 오류가 아니라 "이 배포에는 그 기능이 없다"이므로 조용히 비운다.
pub async fn usage(api: &AttaccaApiClient, session_id: &str) -> Option<crate::sidebar::Usage> {
    let u = api.session_usage(session_id.to_string()).await.ok()?;
    Some(crate::sidebar::Usage {
        model: u.model,
        context_tokens: u.context_tokens,
        total_tokens: u.total_tokens,
        credits_used: u.credits_used,
    })
}

/// 이 세션의 제목. 아직 없으면 `None` — 첫 메시지 뒤에 붙는다.
pub async fn session_title(api: &AttaccaApiClient, session_id: &str) -> Option<String> {
    let sessions =
        api.list_sessions(ZSessionFilter { project_id: None, limit: Some(100) }).await.ok()?;
    sessions
        .into_iter()
        .find(|s| s.id == session_id)
        .and_then(|s| s.title)
        .filter(|t| !t.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zyris_attacca::{ZDeltaKind, ZSessionEvent};

    /// **자격은 이 앱 디렉터리로 간다.** `~/.config/zyris/`는 zyris를 쓰는 모든 프로그램이
    /// 같이 쓰던 자리라, 프로필을 안 정한 둘이 서로의 신원 위에 등록했다.
    ///
    /// 환경변수를 흔들면 병렬로 도는 다른 테스트를 밟으므로, 여기서는 갈래를 정하는
    /// 규칙만 본다 — 옛 자리와 새 자리는 **마지막 한 칸만** 다르고 그 위는 같아야 한다.
    #[test]
    fn credentials_live_under_this_apps_own_name() {
        if given_config_dir().is_some() {
            // 자리를 정해 주면 갈래 규칙이 아니라 그 자리를 그대로 쓴다 —
            // 그 규칙은 a_given_config_dir_wins_and_is_taken_literally가 본다.
            return;
        }
        let (Some(ours), Some(legacy)) = (config_home_for(APP), config_home_for(LEGACY_APP)) else {
            // 홈이 없는 환경(systemd `ProtectHome=yes`)에서는 둘 다 없는 것이 맞다.
            assert!(config_home_for(APP).is_none() && config_home_for(LEGACY_APP).is_none());
            return;
        };
        assert_eq!(ours.file_name().unwrap(), "zyris-code");
        assert_eq!(legacy.file_name().unwrap(), "zyris");
        assert_eq!(ours.parent(), legacy.parent(), "두 자리는 같은 부모 아래에 있어야 한다");
    }

    /// **사람이 준 자리가 이긴다.** 그리고 그 자리에는 앱 이름을 덧붙이지 않는다 — 그
    /// 사람은 정확히 그 디렉터리를 뜻한 것이다.
    #[test]
    fn a_given_config_dir_wins_and_is_taken_literally() {
        // `given_config_dir`이 읽는 값을 흉내 내는 대신, 그 값이 있을 때의 규칙만 본다.
        // (환경변수를 실제로 세우면 같은 프로세스의 다른 테스트가 그것을 본다.)
        let given: Option<std::ffi::OsString> = Some("/somewhere/else".into());
        let picked = given.clone().map(std::path::PathBuf::from).unwrap();
        assert_eq!(picked, std::path::Path::new("/somewhere/else"));
        // 빈 값은 안 준 것으로 친다 — 빈 경로를 그대로 쓰면 자격이 작업 디렉터리에 떨어진다.
        let empty: Option<std::ffi::OsString> = Some(std::ffi::OsString::new());
        assert!(empty.filter(|v| !v.is_empty()).is_none());
    }

    /// **옛 자격은 옮겨 오고, 옛 자리는 빈다.** 복사로 두면 살아 있는 리프레시 토큰이
    /// 디스크에 둘이 되고, 재사용을 본 attacca가 노드를 통째로 revoke한다.
    #[test]
    fn a_legacy_credential_moves_and_leaves_nothing_behind() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        let name = "wss-attacca-cc-zyris-v1-ws-zyris-code.json";
        std::fs::write(old.path().join(name), "{\"refresh_token\":\"r\"}").unwrap();
        // 다른 프로필의 것은 남의 것이다. 건드리면 그 프로그램이 로그아웃된다.
        std::fs::write(old.path().join("wss-attacca-cc-default.json"), "{}").unwrap();

        assert_eq!(migrate_credentials(old.path(), new.path(), "zyris-code"), 1);
        assert!(new.path().join(name).exists(), "새 자리에 있어야 한다");
        assert!(!old.path().join(name).exists(), "옛 자리는 비어야 한다");
        assert!(old.path().join("wss-attacca-cc-default.json").exists(), "남의 것은 그대로다");
    }

    /// **이미 여기 있으면 안 덮어쓴다.** 지금 붙어 있는 신원을 옛 파일로 덮으면 그 자격을
    /// 잃는다. 그때는 옛 파일도 지우지 않는다 — 우리가 안 가진 자격을 버리는 셈이다.
    #[test]
    fn migration_never_overwrites_what_is_already_here() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        let name = "wss-attacca-cc-zyris-v1-ws-zyris-code.json";
        std::fs::write(old.path().join(name), "옛것").unwrap();
        std::fs::write(new.path().join(name), "지금것").unwrap();

        assert_eq!(migrate_credentials(old.path(), new.path(), "zyris-code"), 0);
        assert_eq!(std::fs::read_to_string(new.path().join(name)).unwrap(), "지금것");
        assert!(old.path().join(name).exists(), "안 가져왔으면 지우지도 않는다");
    }

    /// **모자라면 한 번은 다시 묻는다.** 승인 때 정해진 권한은 갱신으로 넓어지지 않으므로,
    /// 자격을 버리고 다시 승인받는 것 말고는 길이 없다.
    #[test]
    fn a_narrow_approval_is_asked_again_exactly_once() {
        let narrow: Vec<String> = vec!["agents:read".into()];
        assert!(needs_reenrollment(&narrow, false), "모자라면 다시 묻는다");
        // **두 번은 안 묻는다.** 사람이 또 좁게 승인할 수 있고, 매번 물으면 브라우저를
        // 계속 요구하는 고리가 된다.
        assert!(!needs_reenrollment(&narrow, true), "한 번 해 봤으면 말로만 알린다");
    }

    /// 다 받았으면 아무 일도 없어야 한다. **평소의 길에서 브라우저가 뜨면 그게 사고다.**
    #[test]
    fn a_full_approval_is_left_alone() {
        let all: Vec<String> = REQUIRED_SCOPES.iter().map(|s| s.to_string()).collect();
        assert!(missing_scopes(&all).is_empty());
        assert!(!needs_reenrollment(&all, false));
        // 더 넓게 받은 것도 모자란 것이 아니다.
        let more: Vec<String> =
            all.iter().cloned().chain(std::iter::once("jobs:read".to_string())).collect();
        assert!(!needs_reenrollment(&more, false));
    }

    /// 무엇이 없는지 **이름으로** 말해야 한다. "부족합니다"만으로는 할 수 있는 일이 없다.
    #[test]
    fn what_is_missing_is_named() {
        let narrow: Vec<String> = vec!["agents:read".into(), "projects:read".into()];
        let missing = missing_scopes(&narrow);
        assert!(missing.contains(&"events:read"), "{missing:?}");
        assert!(!missing.contains(&"agents:read"), "{missing:?}");
        assert!(missing_scopes_message(&missing).contains("events:read"));
    }

    /// 파일 이름을 짓는 쪽은 상류다. **규칙이 갈라지면 옮길 파일을 못 알아본다.**
    #[test]
    fn the_profile_slug_matches_what_zyris_writes() {
        // zyris `enroll/file_store.rs`의 테스트에서 그대로 가져온 값들이다.
        assert_eq!(slugify_profile("zyris-code"), "zyris-code");
        assert_eq!(slugify_profile("///"), "default");
        assert_eq!(slugify_profile(""), "default");
        assert_eq!(slugify_profile("Two  Words"), "two-words");
    }

    /// **슬러그 규칙이 attacca와 같아야 한다.** 여기가 갈라지면 이름이 잘리는지에 대한
    /// 판단이 틀리고, 그러면 앱 이름이 사라진 채로 등록된다.
    #[test]
    fn the_slug_rule_matches_what_attacca_does() {
        // `attacca-domain/src/zyris_node.rs`의 테스트에서 그대로 가져온 값들이다.
        assert_eq!(slug_of("Allen's Desktop!!"), "allen-s-desktop");
        assert_eq!(slug_of("   "), "node");
        assert_eq!(slug_of("a-very-long-machine-name-here"), "a-very-long-mach");
    }

    /// `HOSTNAME`은 프로세스 전역이라 이 둘은 한 줄로 세운다.
    static HOST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// **호스트 이름만으로 등록하면 이 머신의 다른 노드와 같은 신원이 된다.**
    #[test]
    fn the_node_name_carries_this_app() {
        let _g = HOST.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HOSTNAME", "arch");
        assert_eq!(node_name(), "arch zyris-code");
        assert_eq!(slug_of(&node_name()), "arch-zyris-code");
    }

    /// **호스트 이름이 길면 뒤쪽이 잘려 나간다.** 그대로 두면 도로 호스트 이름만 남아
    /// 구별이 사라진다 — 그때는 구별되는 쪽을 앞에 둔다.
    #[test]
    fn a_long_hostname_does_not_swallow_the_app_name() {
        let _g = HOST.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HOSTNAME", "a-very-long-machine-name-here");
        let name = node_name();
        let slug = slug_of(&name);
        assert!(slug.contains("zyris-code"), "앱 이름이 잘려 나갔다: {name} → {slug}");
        assert!(slug.len() <= 16, "{slug}");
    }

    /// 에이전트를 바꾸면 **다음 메시지에서 새 세션이 열린다.** 세션의 에이전트는 만들 때
    /// 정해지고 바꾸는 API가 없어서(`ZNewSession`), 앞 세션을 계속 쓰면 안 바뀐다.
    #[test]
    fn staging_a_new_default_session_forgets_the_current_one() {
        let mut s = Session::new(None);
        s.switch_to("abc".into(), None);
        assert_eq!(s.id(), Some("abc"));
        s.stage_new_default();
        assert_eq!(s.id(), None, "앞 세션을 계속 쓰면 에이전트가 안 바뀐다");
    }

    /// **서버에 지금 만들지 않는다.** 예약만 하고 실제 생성은 첫 메시지에서다 —
    /// 열어만 보고 마는 빈 세션이 계정에 쌓이면 안 된다.
    #[test]
    fn staging_creates_nothing_by_itself() {
        let mut s = Session::new(None);
        s.stage_new_default();
        assert_eq!(s.id(), None);
    }

    /// **기본↔계획은 하던 대화를 안 건드린다.** 이것이 깨지면 대화 도중에 계획 모드를
    /// 켜는 순간 얘기가 끊겨, 계획 모드가 쓸모 있는 유일한 방식이 사라진다.
    #[test]
    fn routing_to_a_session_leaves_the_conversation_alone() {
        let mut s = Session::new(None);
        s.switch_to("abc".into(), None);
        s.set_route(Route::Session);
        assert_eq!(s.id(), Some("abc"), "계획 모드가 하던 대화를 버렸다");
        assert_eq!(s.pending_open(), None);
    }

    /// work·job으로 들어가면 **다음 메시지가 새로 연다**고 예약된다.
    #[test]
    fn routing_to_work_or_job_stages_an_open() {
        for route in [Route::Work, Route::Job] {
            let mut s = Session::new(None);
            s.set_route(route);
            assert_eq!(s.pending_open(), Some(route));
        }
    }

    /// **예약은 하던 대화를 버리지 않는다.** work·job에 들렀다가 아무 말 없이 돌아오는 일이
    /// 흔한데, 그때 세션을 이미 버렸으면 되돌아갈 곳이 없다.
    #[test]
    fn staging_an_open_keeps_the_conversation_to_come_back_to() {
        let mut s = Session::new(None);
        s.switch_to("abc".into(), None);
        s.set_route(Route::Job);
        assert_eq!(s.id(), Some("abc"), "예약만 했는데 하던 대화를 잃었다");
        s.set_route(Route::Session);
        assert_eq!(s.id(), Some("abc"));
        assert_eq!(s.pending_open(), None, "돌아왔는데 예약이 남아 있다");
    }

    /// 목록에서 세션을 고른 것은 "저기로 간다"는 뜻이다. **예약이 남아 있으면 고른 자리로
    /// 안 가고 job이 하나 더 생긴다.**
    #[test]
    fn picking_a_session_by_hand_cancels_a_staged_open() {
        let mut s = Session::new(None);
        s.set_route(Route::Job);
        s.switch_to("고른-세션".into(), None);
        assert_eq!(s.pending_open(), None);
        assert_eq!(s.id(), Some("고른-세션"));

        let mut s = Session::new(None);
        s.set_route(Route::Work);
        s.stage_new("프로젝트-1".into());
        assert_eq!(s.pending_open(), None);

        let mut s = Session::new(None);
        s.set_route(Route::Work);
        s.stage_new_default();
        assert_eq!(s.pending_open(), None);
    }

    /// **고른 프로젝트는 붙어 있어야 한다.** 예전에는 `pending_project`가 일회용이라
    /// 첫 생성이 지웠고, 그래서 프로젝트를 골라 놓고 job을 걸면 `project_id`가 이미 비어
    /// **서버가 기본 프로젝트에 만들었다.** 실제로 그렇게 걸렸다.
    #[test]
    fn the_chosen_project_sticks_to_everything_opened_after_it() {
        // 목록에서 프로젝트를 열기만 해도 기억한다 — 세션을 안 고르고 Esc해도 그대로다.
        let mut s = Session::new(None);
        s.enter_project("프로젝트-1".into());
        assert_eq!(s.project(), Some("프로젝트-1"));

        // 그 안의 세션을 고르면 그 세션의 프로젝트로 간다.
        s.switch_to("세션-1".into(), Some("프로젝트-2".into()));
        assert_eq!(s.project(), Some("프로젝트-2"));

        // 어느 프로젝트인지 모르는 자리는 **알던 것을 안 지운다.**
        s.switch_to("세션-2".into(), None);
        assert_eq!(s.project(), Some("프로젝트-2"), "모른다고 비우면 기본 프로젝트로 떨어진다");

        // `/agent`은 에이전트를 바꾼 것이지 프로젝트를 떠난 것이 아니다.
        s.stage_new_default();
        assert_eq!(s.project(), Some("프로젝트-2"));

        // job·work를 예약해도 그대로다 — 그 예약이 쓸 값이 바로 이것이다.
        s.set_route(Route::Job);
        assert_eq!(s.project(), Some("프로젝트-2"));
    }

    /// 아직 아무 프로젝트도 안 골랐으면 `None`이고, **그때만** 서버의 기본 프로젝트가 맞다.
    #[test]
    fn a_fresh_session_has_no_project_of_its_own() {
        assert_eq!(Session::new(None).project(), None);
    }

    /// **모드가 정하는 것은 예약이 없을 때도 통해야 한다.**
    ///
    /// 예약은 모드가 *바뀌는* 순간에만 걸린다. 그래서 안 걸리는 자리가 여럿이다 — 켜자마자
    /// 첫 마디, `/agent` 뒤, `＋ 새 세션` 뒤. 거기서 세션만 만들면 **하단 바는 job인데
    /// 열리는 것은 맨 세션**이 된다. 실제로 그렇게 걸렸다.
    ///
    /// 여기서는 `open_for`를 부를 수 없으므로(서버가 필요하다) 그 판정을 그대로 흉내 낸다.
    /// 갈라지면 이 테스트가 지키는 것이 없어지니, `open_for`를 고치면 여기도 고칠 것.
    #[test]
    fn with_no_conversation_yet_the_mode_decides_what_opens() {
        use crate::mode::Mode;
        let route_for = |staged: Option<Route>, has_id: bool, mode: Mode| match staged {
            Some(r) => r,
            None if has_id => Route::Session,
            None => mode.route(),
        };

        // 대화가 없고 예약도 없다 → 모드가 정한다.
        assert_eq!(route_for(None, false, Mode::Job), Route::Job, "job 모드인데 맨 세션이 열린다");
        assert_eq!(route_for(None, false, Mode::Work), Route::Work);
        assert_eq!(route_for(None, false, Mode::Normal), Route::Session);
        assert_eq!(route_for(None, false, Mode::Plan), Route::Session);

        // 이어 갈 대화가 있으면 이어 간다 — 말할 때마다 job이 하나씩 생기면 안 된다.
        assert_eq!(route_for(None, true, Mode::Job), Route::Session);

        // 예약이 있으면 그것이 이긴다. 하던 대화가 있어도 새로 연다.
        assert_eq!(route_for(Some(Route::Job), true, Mode::Normal), Route::Job);
    }

    /// 렌더하지 않는 이벤트여도 커서는 넘겨야 한다 — 재개 위치를 놓치면 안 된다.
    #[test]
    fn a_hidden_event_still_carries_its_cursor() {
        let f = ZTurnFrame::Event {
            cursor: 99,
            event: ZSessionEvent {
                seq: 5,
                cursor: 99,
                kind: "recall".into(),
                payload: json!({"kind": "recall", "content": "…"}),
                created_at: None,
            },
        };
        match frame_from(f) {
            Frame::Event { cursor, entry } => {
                assert_eq!(cursor, 99);
                assert!(entry.is_none(), "recall은 렌더하지 않는다");
            }
            other => panic!("이벤트 프레임이어야 한다: {other:?}"),
        }
    }

    #[test]
    fn a_delta_frame_keeps_its_kind() {
        let f = ZTurnFrame::Delta { kind: ZDeltaKind::Reasoning, text: "생각".into() };
        match frame_from(f) {
            Frame::Delta { kind, text } => {
                assert_eq!(kind, ZDeltaKind::Reasoning);
                assert_eq!(text, "생각");
            }
            other => panic!("델타 프레임이어야 한다: {other:?}"),
        }
    }

    /// zyris-code가 준비되면 여기만 바꾸면 된다. 그때 이 테스트가 같이 빨개져서
    /// 바꿨다는 사실이 드러난다.
    #[test]
    fn the_default_agent_is_main_agent_while_zyris_code_is_in_progress() {
        assert_eq!(DEFAULT_AGENT, "Main Agent");
    }

    #[test]
    fn a_status_frame_carries_running() {
        match frame_from(ZTurnFrame::Status { running: true }) {
            Frame::Status { running } => assert!(running),
            other => panic!("상태 프레임이어야 한다: {other:?}"),
        }
    }
}
