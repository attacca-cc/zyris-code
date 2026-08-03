//! 화면 말을 한국어와 영어로 낸다. `/lang`으로 바꾼다.
//!
//! **문구는 여기 한 군데에 모은다.** 위젯마다 조건문으로 갈라 쓰면 한쪽 언어만 고치는 일이
//! 반드시 생기고, 그러면 화면 절반이 다른 말로 남는다. 여기 함수 하나가 두 언어를 나란히
//! 들고 있으면 고칠 때 둘 다 눈에 들어온다.
//!
//! ## 두 군데에 있는 이유
//!
//! - `State.lang` — 그리는 쪽이 쓴다. `apply`가 순수해야 하므로 상태로 들고 있어야 하고,
//!   화면 테스트가 언어를 정해 놓고 볼 수 있는 것도 이것 덕이다.
//! - `lang::current()` — 화면이 없는 자리가 쓴다(`notice.rs`의 셸 알림, 도구가 돌려주는
//!   오류). 거기까지 인자로 나르면 순수하지도 않은 함수들에 `lang` 하나가 줄줄이 붙는다.
//!
//! 둘은 `/lang`이 함께 세운다. 갈라지면 대화창은 영어인데 셸 알림만 한국어로 남는다.
//!
//! ## 어떤 말이 여기 오는가
//!
//! **사람이 읽는 것만.** 에이전트가 읽는 도구 설명은 언제나 영어이고(`tools/`의 doc 주석),
//! 코드 주석과 테스트 이름은 언제나 한국어다. 이 파일이 가르는 것은 화면뿐이다.

use std::sync::atomic::{AtomicU8, Ordering};

/// **기본은 영어다.** 이 리포는 한국어로 쓰지만 앱을 받는 사람은 그렇지 않다 — 못 읽는
/// 말로 뜨는 화면은 아무것도 못 하게 만든다. 한국어는 로케일이 말해 주거나 사람이 고른다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    Ko,
    #[default]
    En,
}

/// 지금 언어. 화면이 없는 자리에서 쓴다.
static CURRENT: AtomicU8 = AtomicU8::new(1);

pub fn current() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        0 => Lang::Ko,
        _ => Lang::En,
    }
}

pub fn set(lang: Lang) {
    CURRENT.store(
        match lang {
            Lang::Ko => 0,
            Lang::En => 1,
        },
        Ordering::Relaxed,
    );
}

impl Lang {
    /// 사람이 친 말에서. **양쪽 언어의 이름을 다 받는다** — 영어 화면에서 `/lang 한글`을
    /// 치는 것이 자연스럽고, 한국어 화면에서 `/lang english`를 치는 것도 그렇다.
    pub fn parse(text: &str) -> Option<Lang> {
        match text.trim().to_ascii_lowercase().as_str() {
            "ko" | "kr" | "korean" | "한글" | "한국어" => Some(Lang::Ko),
            "en" | "eng" | "english" | "영어" => Some(Lang::En),
            _ => None,
        }
    }

    /// 설정에 적어 두는 이름.
    pub fn code(self) -> &'static str {
        match self {
            Lang::Ko => "ko",
            Lang::En => "en",
        }
    }

    /// 사람에게 보여줄 이름. **제 언어로 적는다** — 목록에서 고르는 것이라, 지금 못 읽는
    /// 언어로 적혀 있으면 무엇을 고르는지 알 수 없다.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Ko => "한국어",
            Lang::En => "English",
        }
    }

    fn pick(self, ko: &'static str, en: &'static str) -> &'static str {
        match self {
            Lang::Ko => ko,
            Lang::En => en,
        }
    }
}

/// 켤 때 어느 언어로 시작할까.
///
/// 순서: `$ZYRIS_CODE_LANG` → 지난번에 고른 것 → 시스템 로케일 → 한국어.
///
/// **사람이 준 것이 언제나 이긴다.** 그다음이 지난번 선택인 이유는, `/lang`으로 바꾼 것이
/// 다음 실행에 그대로 남아야 "설정"이라 할 수 있기 때문이다.
pub fn startup() -> Lang {
    if let Some(given) = std::env::var("ZYRIS_CODE_LANG").ok().and_then(|v| Lang::parse(&v)) {
        return given;
    }
    if let Some(saved) = load() {
        return saved;
    }
    from_locale(std::env::var("LC_ALL").or_else(|_| std::env::var("LANG")).ok().as_deref())
}

/// `ko_KR.UTF-8` → 한국어. 모르는 로케일은 영어로 본다 — 한국어를 못 읽는 사람에게
/// 한국어 화면을 내미는 쪽이 그 반대보다 나쁘다.
pub fn from_locale(locale: Option<&str>) -> Lang {
    match locale {
        Some(l) if l.to_ascii_lowercase().starts_with("ko") => Lang::Ko,
        Some(l) if !l.trim().is_empty() => Lang::En,
        // 로케일이 아예 없는 환경(도커, systemd)에서는 기본인 영어로 둔다.
        _ => Lang::En,
    }
}

/// 골라 둔 언어가 사는 파일. 자격과 같은 디렉터리다.
fn store() -> Option<std::path::PathBuf> {
    crate::conn::credential_dir().map(|dir| dir.join("lang"))
}

pub fn load() -> Option<Lang> {
    Lang::parse(&std::fs::read_to_string(store()?).ok()?)
}

/// 고른 것을 남긴다. **실패해도 앱은 그대로 돈다** — 이번 실행에는 이미 바뀌어 있다.
pub fn save(lang: Lang) {
    let Some(at) = store() else { return };
    if let Some(dir) = at.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&at, lang.code()) {
        tracing::warn!(error = %e, "고른 언어를 남기지 못했다");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 화면 문구
// ─────────────────────────────────────────────────────────────────────────────

impl Lang {
    // ── 하단 바·활동 줄
    pub fn mode_normal(self) -> &'static str {
        self.pick("기본", "normal")
    }
    pub fn mode_plan(self) -> &'static str {
        self.pick("계획", "plan")
    }
    /// **번역하지 않는다.** 이 둘은 attacca가 제 화면에서 부르는 이름 그대로여야, 여기서
    /// 연 것을 저쪽 목록에서 찾을 수 있다 — 세션을 `thread`라고 적어 두는 것과 같은 이유다.
    /// `작업`으로 옮기면 바로 위 활동 줄의 `작업 중…`과 섞여 무엇을 가리키는지 흐려진다.
    pub fn mode_work(self) -> &'static str {
        "work"
    }
    pub fn mode_job(self) -> &'static str {
        "job"
    }
    pub fn working(self) -> &'static str {
        self.pick("작업 중…", "Working…")
    }
    pub fn stopping(self) -> &'static str {
        self.pick("멈추는 중…", "Stopping…")
    }
    pub fn idle(self) -> &'static str {
        self.pick("쉬는 중", "Idle")
    }
    pub fn esc_stops(self) -> &'static str {
        self.pick("Esc 정지", "Esc stops")
    }
    pub fn ctrl_c_quits(self) -> &'static str {
        self.pick("Ctrl+C 종료", "Ctrl+C quits")
    }
    pub fn queued(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("대기 {n}개"),
            Lang::En => format!("{n} queued"),
        }
    }
    pub fn quit_armed(self) -> &'static str {
        self.pick("한 번 더 Ctrl+C를 누르면 끝냅니다", "Press Ctrl+C again to quit")
    }

    pub fn mode_now(self, mode: &str) -> String {
        match self {
            Lang::Ko => format!(
                "지금은 **{mode}** 모드입니다. Shift+Tab으로 돌리거나 \
                 `/mode 기본`·`/mode 계획`·`/mode work`·`/mode job`으로 바꿉니다."
            ),
            Lang::En => format!(
                "Mode is **{mode}**. Cycle it with Shift+Tab, or set it with \
                 `/mode normal`, `/mode plan`, `/mode work`, `/mode job`."
            ),
        }
    }
    pub fn mode_changed(self, mode: &str) -> String {
        match self {
            Lang::Ko => format!("**{mode}** 모드로 바꿨습니다."),
            Lang::En => format!("Mode is now **{mode}**."),
        }
    }

    /// `work`·`job`으로 들어갔을 때. **다음 메시지가 무엇이 되는지 미리 말한다** —
    /// 모드만 바뀐 줄 알고 하던 얘기를 이어 쓰면 그것이 목표가 되어 버린다.
    pub fn mode_opens_work(self) -> &'static str {
        self.pick(
            "다음에 보내는 말이 **work의 목표**가 됩니다. attacca가 계획을 세워 \
             태스크로 쪼갭니다 — 관문 둘은 사람이 열어야 합니다.",
            "Your next message becomes a **work goal**. Attacca plans it into tasks; \
             the two gates need a person to open them.",
        )
    }
    pub fn mode_opens_job(self) -> &'static str {
        self.pick(
            "다음에 보내는 말이 **job**이 됩니다. 시켜 놓으면 끝까지 해냅니다 — \
             되묻는 것이 있으면 그대로 답하면 됩니다.",
            "Your next message becomes a **job** — hand it over and it runs to the end. \
             If it asks something back, just answer here.",
        )
    }

    /// 열고 나서. **어느 것이 열렸는지 id로 말한다** — attacca 쪽에서 찾으려면 그게 필요하다.
    pub fn opened_work(self, id: &str) -> String {
        match self {
            Lang::Ko => format!("work **{id}**을 열었습니다. 여기서 계획을 두고 얘기하면 됩니다."),
            Lang::En => format!("Opened work **{id}**. Talk the plan over right here."),
        }
    }
    pub fn opened_job(self, id: &str) -> String {
        match self {
            Lang::Ko => format!("job **{id}**을 걸었습니다. 도는 것을 여기서 봅니다."),
            Lang::En => format!("Queued job **{id}**. Watch it run right here."),
        }
    }

    pub fn connecting(self) -> &'static str {
        self.pick("연결 중…", "Connecting…")
    }
    pub fn disconnected(self, why: &str) -> String {
        match self {
            Lang::Ko => format!("연결이 끊겼습니다 ({why}). 다시 붙는 중입니다."),
            Lang::En => format!("Disconnected ({why}). Reconnecting."),
        }
    }

    // ── 목록(픽커)
    pub fn new_thread(self) -> &'static str {
        self.pick("＋ 새 쓰레드", "+ New thread")
    }
    pub fn projects(self) -> &'static str {
        self.pick("프로젝트", "Projects")
    }
    pub fn threads_in(self, project: &str) -> String {
        match self {
            Lang::Ko => format!("쓰레드  ·  {project}"),
            Lang::En => format!("Threads  ·  {project}"),
        }
    }
    pub fn agents(self) -> &'static str {
        self.pick("에이전트", "Agents")
    }
    pub fn commands(self) -> &'static str {
        self.pick("명령", "Commands")
    }
    pub fn language(self) -> &'static str {
        self.pick("화면 말", "Language")
    }
    /// 목록에서 지금 쓰는 언어에 붙는 표시.
    pub fn in_use(self) -> &'static str {
        self.pick("지금", "in use")
    }
    pub fn new_project(self) -> &'static str {
        self.pick("＋ 새 프로젝트", "+ New project")
    }
    /// 고르면 무슨 일이 벌어지는지 그 자리에서 말한다. **누르면 바로 만들지 않는다** —
    /// 이름을 받아야 하는데 목록에는 글자를 칠 자리가 없어서, `/project `를 입력란에
    /// 넣어 주고 이름은 사람이 친다.
    pub fn new_project_note(self) -> &'static str {
        self.pick("이름을 이어서 칩니다", "type a name after it")
    }
    pub fn project_created(self, name: &str) -> String {
        match self {
            Lang::Ko => format!(
                "프로젝트 **{name}**을 만들고 그 안으로 들어왔습니다. \
                 여기서 여는 thread·job·work는 이 프로젝트의 것이 됩니다."
            ),
            Lang::En => format!(
                "Created project **{name}** and moved into it. \
                 Threads, jobs and works you open here belong to it."
            ),
        }
    }
    pub fn default_project(self) -> &'static str {
        self.pick("기본", "default")
    }
    pub fn running(self) -> &'static str {
        self.pick("작업 중", "running")
    }

    pub fn unknown_command(self, what: &str, help: &str) -> String {
        match self {
            Lang::Ko => format!("`/{what}`은 모르는 명령입니다.\n\n{help}"),
            Lang::En => format!("`/{what}` is not a command.\n\n{help}"),
        }
    }

    // ── 사이드바
    pub fn usage(self) -> &'static str {
        self.pick("사용량", "Usage")
    }
    pub fn credits(self) -> &'static str {
        self.pick("크레딧", "Credits")
    }
    pub fn context(self) -> &'static str {
        self.pick("컨텍스트", "Context")
    }
    pub fn total_tokens(self) -> &'static str {
        self.pick("총 토큰", "Tokens")
    }
    pub fn shells(self) -> &'static str {
        self.pick("셸", "Shells")
    }
    pub fn tasks(self) -> &'static str {
        self.pick("태스크", "Tasks")
    }
    pub fn none(self) -> &'static str {
        self.pick("없음", "None")
    }

    // ── 질문 화면
    pub fn type_your_own(self) -> &'static str {
        self.pick("✎ 직접 입력", "✎ Type your own")
    }
    pub fn type_here(self) -> &'static str {
        self.pick("여기에 직접 적으세요 (Enter로 확정)", "Type here (Enter to confirm)")
    }
    pub fn typing_keys(self) -> &'static str {
        self.pick("Enter 입력 끝 · Esc 취소", "Enter to finish · Esc to cancel")
    }
    pub fn choosing_keys(self) -> &'static str {
        self.pick(
            "↑↓ 이동 · Enter 고르기/실행 · 클릭도 됨 · Esc 접기",
            "↑↓ move · Enter choose/run · click works too · Esc folds",
        )
    }
    pub fn review_keys(self) -> &'static str {
        self.pick("↑↓ 이동 · Enter 실행 · 클릭도 됨", "↑↓ move · Enter runs · click works too")
    }
    pub fn answered(self) -> &'static str {
        self.pick("답한 내용", "Your answer")
    }
    pub fn skipped(self) -> &'static str {
        self.pick("건너뜀", "skipped")
    }

    // ── 승인 화면
    pub fn approve_keys(self) -> &'static str {
        self.pick(
            "  y 허용 / n 거부 / a 이 디렉터리는 이번 쓰레드 내내 허용",
            "  y allow / n deny / a allow this directory for the whole thread",
        )
    }
    pub fn approve_head(self) -> &'static str {
        self.pick(
            "작업 디렉터리 밖입니다. 승인이 필요합니다",
            "Outside the working directory. This needs your approval",
        )
    }
    pub fn approve_root(self, cwd: &str) -> String {
        match self {
            Lang::Ko => format!("여기서 도는 곳은 {cwd}"),
            Lang::En => format!("Tools run in {cwd}"),
        }
    }
    pub fn approve_more_waiting(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("  뒤에 {n}개가 더 기다립니다"),
            Lang::En => format!("  {n} more waiting behind this"),
        }
    }
    pub fn approve_gave_up(self) -> &'static str {
        self.pick("  기다리다 돌아갔습니다", "  The server gave up waiting")
    }
    pub fn approve_next_time(self) -> &'static str {
        self.pick(
            "  허용하면 다음 시도에 바로 실행됩니다",
            "  Allowing it runs on the next attempt",
        )
    }
    pub fn approve_expired(self) -> &'static str {
        self.pick(
            "서버가 이 호출을 포기했습니다. 허용하면 다음 호출부터 적용됩니다.",
            "The server gave up on this call. Allowing it applies from the next one.",
        )
    }

    // ── `/lang`
    pub fn lang_now(self) -> String {
        match self {
            Lang::Ko => {
                format!("화면 말: **{}**. `/lang en`으로 영어로 바꿉니다.", Lang::Ko.name())
            }
            Lang::En => {
                format!("Interface language: **{}**. Use `/lang ko` for Korean.", Lang::En.name())
            }
        }
    }
    pub fn lang_changed(self) -> &'static str {
        self.pick(
            "화면 말을 한국어로 바꿨습니다. 다음에 켤 때도 이대로입니다.",
            "Interface language is now English. It stays this way next time.",
        )
    }
    pub fn lang_unknown(self, given: &str) -> String {
        match self {
            Lang::Ko => format!("`{given}`가 무슨 말인지 모르겠습니다. `ko` 또는 `en`입니다."),
            Lang::En => format!("Cannot tell what `{given}` means. Use `ko` or `en`."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **양쪽 언어의 이름을 다 받는다.** 영어 화면에서 `/lang 한글`을 치는 것이 자연스럽고,
    /// 그 반대도 마찬가지다 — 지금 화면 말로만 받으면 잘못 고른 사람이 되돌아올 길이 없다.
    #[test]
    fn either_language_can_be_named_in_either_language() {
        for said in ["ko", "KO", "한글", "한국어", "korean"] {
            assert_eq!(Lang::parse(said), Some(Lang::Ko), "{said}");
        }
        for said in ["en", "English", "영어", " eng "] {
            assert_eq!(Lang::parse(said), Some(Lang::En), "{said}");
        }
        assert_eq!(Lang::parse("일본어"), None);
        assert_eq!(Lang::parse(""), None);
    }

    /// 로케일은 짐작일 뿐이다. **모르면 영어로 본다** — 한국어를 못 읽는 사람에게 한국어
    /// 화면을 내미는 쪽이 그 반대보다 나쁘다.
    #[test]
    fn the_locale_is_a_guess_that_errs_towards_english() {
        assert_eq!(from_locale(Some("ko_KR.UTF-8")), Lang::Ko);
        assert_eq!(from_locale(Some("KO")), Lang::Ko);
        assert_eq!(from_locale(Some("en_US.UTF-8")), Lang::En);
        assert_eq!(from_locale(Some("fr_FR")), Lang::En);
        assert_eq!(from_locale(None), Lang::En, "모르면 영어다");
        assert_eq!(from_locale(Some("  ")), Lang::En);
    }

    /// 적어 두는 이름과 읽는 이름이 같아야 한다 — 갈라지면 저장한 설정을 못 읽는다.
    #[test]
    fn what_is_written_is_what_is_read_back() {
        for lang in [Lang::Ko, Lang::En] {
            assert_eq!(Lang::parse(lang.code()), Some(lang));
        }
    }

    /// **언어 이름은 제 언어로 적는다.** 지금 못 읽는 말로 적혀 있으면 무엇을 고르는지
    /// 알 수 없다.
    #[test]
    fn a_language_names_itself() {
        assert_eq!(Lang::Ko.name(), "한국어");
        assert_eq!(Lang::En.name(), "English");
    }

    /// 두 언어가 **둘 다 있어야 한다.** 한쪽만 채우면 화면 절반이 다른 말로 남는다.
    ///
    /// **`mode_work`·`mode_job`은 일부러 뺐다.** 둘은 attacca가 제 화면에서 쓰는 이름
    /// 그대로여서 두 언어가 같고, 여기 넣으면 "번역이 안 됐다"고 걸린다.
    /// 아래 `the_english_side_has_no_hangul_left_in_it`이 대신 지킨다.
    #[test]
    fn no_message_is_left_in_one_language_only() {
        let ko = Lang::Ko;
        let en = Lang::En;
        let pairs: Vec<(&str, &str)> = vec![
            (ko.working(), en.working()),
            (ko.idle(), en.idle()),
            (ko.stopping(), en.stopping()),
            (ko.new_thread(), en.new_thread()),
            (ko.projects(), en.projects()),
            (ko.agents(), en.agents()),
            (ko.commands(), en.commands()),
            (ko.mode_normal(), en.mode_normal()),
            (ko.mode_plan(), en.mode_plan()),
            (ko.approve_keys(), en.approve_keys()),
            (ko.esc_stops(), en.esc_stops()),
        ];
        for (k, e) in pairs {
            assert_ne!(k, e, "번역이 안 된 문구가 있다: {k}");
            assert!(!k.is_empty() && !e.is_empty());
        }
    }

    /// 영어 화면에는 **한글이 한 글자도 없어야 한다.** 섞이면 안 옮긴 자리가 티가 안 난다.
    #[test]
    fn the_english_side_has_no_hangul_left_in_it() {
        let en = Lang::En;
        let said = [
            en.working(),
            en.idle(),
            en.stopping(),
            en.new_thread(),
            en.projects(),
            en.agents(),
            en.commands(),
            en.mode_work(),
            en.mode_job(),
            en.approve_keys(),
            en.approve_expired(),
            en.esc_stops(),
            en.quit_armed(),
            en.lang_changed(),
        ];
        for text in said {
            assert!(
                !text.chars().any(|c| ('가'..='힣').contains(&c)),
                "영어 문구에 한글이 남았다: {text}"
            );
        }
        assert!(!en.queued(3).chars().any(|c| ('가'..='힣').contains(&c)));
        assert!(!en.threads_in("proj").chars().any(|c| ('가'..='힣').contains(&c)));
    }

    /// 한국어에서 thread는 **쓰레드**다. 영어 낱말을 그대로 두면 목록에서 튄다.
    #[test]
    fn thread_reads_as_sseurede_in_korean() {
        assert!(Lang::Ko.new_thread().contains("쓰레드"), "{}", Lang::Ko.new_thread());
        assert!(Lang::Ko.threads_in("proj").contains("쓰레드"));
        assert!(Lang::Ko.approve_keys().contains("쓰레드"));
    }
}
