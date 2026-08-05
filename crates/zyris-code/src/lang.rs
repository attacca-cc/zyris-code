//! Produces the screen's words in Korean and English. Changed with `/lang`.
//!
//! **All phrases are gathered in this one place.** Split per-widget with conditionals, one language
//! would inevitably get edited alone, leaving half the screen in the other language. With a single
//! function here holding both languages side by side, both are in view when editing.
//!
//! ## Why it lives in two places
//!
//! - `State.lang` — used by the drawing side. Since `apply` must stay pure, it has to be carried as
//!   state, and screen tests being able to fix a language and look at it is thanks to this too.
//! - `lang::current()` — used where there is no screen (the shell notice in `notice.rs`, errors the
//!   tools return). Carrying it as an argument that far would string a `lang` through functions that aren't even pure.
//!
//! `/lang` sets both together. If they diverged, the conversation window would be English while only the shell notice stayed Korean.
//!
//! ## Which words come here
//!
//! **Only what people read.** Tool descriptions read by the agent are always English (the doc
//! comments in `tools/`), and code comments and test names are always Korean. What this file divides is only the screen.

use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::instructions::Found;
use crate::plugin::Plugin;
use crate::tools::gate::Grants;
use crate::tools::skill::SkillInfo;
use crate::undo::Changed;

/// **The default is English.** This repo is written in Korean, but the people receiving the app
/// aren't — a screen in a language they can't read makes it unusable. Korean comes from the locale or a person's choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    Ko,
    #[default]
    En,
}

/// The current language. Used where there is no screen.
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
    /// From what the person typed. **Both languages' names are accepted** — typing `/lang` with a
    /// Korean word on an English screen is natural, and so is `/lang english` on a Korean one.
    pub fn parse(text: &str) -> Option<Lang> {
        match text.trim().to_ascii_lowercase().as_str() {
            "ko" | "kr" | "korean" | "한글" | "한국어" => Some(Lang::Ko),
            "en" | "eng" | "english" | "영어" => Some(Lang::En),
            _ => None,
        }
    }

    /// The name written to the setting.
    pub fn code(self) -> &'static str {
        match self {
            Lang::Ko => "ko",
            Lang::En => "en",
        }
    }

    /// The name shown to people. **Written in its own language** — since it's for picking from a
    /// list, a name in a language you can't read right now leaves you unable to tell what you'd choose.
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

/// Which language to start with at launch.
///
/// Order: `$ZYRIS_CODE_LANG` → last choice → system locale → Korean.
///
/// **What a person gave always wins.** The last choice comes next because a `/lang` change must
/// survive into the next run to count as a "setting".
pub fn startup() -> Lang {
    if let Some(given) = std::env::var("ZYRIS_CODE_LANG").ok().and_then(|v| Lang::parse(&v)) {
        return given;
    }
    if let Some(saved) = load() {
        return saved;
    }
    from_locale(std::env::var("LC_ALL").or_else(|_| std::env::var("LANG")).ok().as_deref())
}

/// `ko_KR.UTF-8` → Korean. An unknown locale is treated as English — offering a Korean screen to
/// someone who can't read Korean is worse than the reverse.
pub fn from_locale(locale: Option<&str>) -> Lang {
    match locale {
        Some(l) if l.to_ascii_lowercase().starts_with("ko") => Lang::Ko,
        Some(l) if !l.trim().is_empty() => Lang::En,
        // Environments with no locale at all (docker, systemd) stay at the default, English.
        _ => Lang::En,
    }
}

/// The file where the chosen language lives. Same directory as the credentials.
fn store() -> Option<std::path::PathBuf> {
    crate::conn::credential_dir().map(|dir| dir.join("lang"))
}

pub fn load() -> Option<Lang> {
    Lang::parse(&std::fs::read_to_string(store()?).ok()?)
}

/// Saves the choice. **The app keeps running even if this fails** — it's already changed for this run.
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
// Screen phrases
// ─────────────────────────────────────────────────────────────────────────────

impl Lang {
    // ── Bottom bar · activity line
    pub fn mode_normal(self) -> &'static str {
        self.pick("기본", "normal")
    }
    pub fn mode_plan(self) -> &'static str {
        self.pick("계획", "plan")
    }
    /// **Not translated.** These two must stay exactly the names attacca uses on its own screen, so
    /// what's opened here can be found in that list — same reason sessions are written as `thread`.
    /// Moved to the Korean for "work", it would blend with the "working…" just above in the activity line and blur what each points at.
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
        self.pick("쉬는 중", "Taking a break")
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
    /// Connected. The activity line shows this briefly, then settles to `idle()` —
    /// the transition a user sees is connecting → connected → taking a break.
    pub fn connected(self) -> &'static str {
        self.pick("연결됨", "Connected")
    }
    /// What to show in the activity line while a command runs.
    pub fn running_command(self, command: &str, secs: u64) -> String {
        match self {
            Lang::Ko => format!("▶ {command}  ·  {secs}초"),
            Lang::En => format!("▶ {command}  ·  {secs}s"),
        }
    }
    /// What to show in the activity line while waiting for a question.
    pub fn waiting_answer(self) -> &'static str {
        self.pick("대기 중 — 답을 고르세요", "Waiting — answer the question")
    }
    /// The hint attached to the right of the activity line while waiting for a question.
    pub fn waiting_answer_hint(self) -> &'static str {
        self.pick("↑↓ 이동 · Enter 고르기", "↑↓ move · Enter choose")
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

    /// When entering `work`·`job`. **It says in advance what the next message becomes** — if you
    /// think only the mode changed and keep writing your ongoing talk, that becomes the goal.
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

    /// When switching to work mode with a session open. **The mode no longer opens new things** —
    /// the ongoing conversation continues as-is, and a new work opens in a new thread.
    pub fn mode_continues_work(self) -> &'static str {
        self.pick(
            "모드가 **work**입니다. 지금 대화는 그대로 이어갑니다 — \
             새 work는 ←의 새 쓰레드에서 엽니다.",
            "Mode is **work**. This conversation continues as-is — \
             start a new work from a new thread (←).",
        )
    }
    pub fn mode_continues_job(self) -> &'static str {
        self.pick(
            "모드가 **job**입니다. 지금 대화는 그대로 이어갑니다 — \
             새 job은 ←의 새 쓰레드에서 엽니다.",
            "Mode is **job**. This conversation continues as-is — \
             start a new job from a new thread (←).",
        )
    }

    /// After opening. **It says which one opened by id** — that's what's needed to find it on the attacca side.
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

    // ── Lists (picker)
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
    /// The mark attached to the language currently in use, in the list.
    pub fn in_use(self) -> &'static str {
        self.pick("지금", "in use")
    }
    pub fn new_project(self) -> &'static str {
        self.pick("＋ 새 프로젝트", "+ New project")
    }
    /// Choosing it opens a form for the name and description — the list has no place to type.
    pub fn new_project_note(self) -> &'static str {
        self.pick("이름과 설명을 적습니다", "type a name and description")
    }
    // ── New project form
    pub fn project_form_title(self) -> &'static str {
        self.pick("새 프로젝트", "New project")
    }
    pub fn project_name(self) -> &'static str {
        self.pick("이름", "Name")
    }
    pub fn project_name_placeholder(self) -> &'static str {
        self.pick("프로젝트 이름", "project name")
    }
    pub fn project_description(self) -> &'static str {
        self.pick("설명", "Description")
    }
    pub fn project_description_placeholder(self) -> &'static str {
        self.pick("무엇을 하는 곳인지", "what it is for")
    }
    pub fn project_form_keys(self) -> &'static str {
        self.pick(
            "Tab 다음 칸 · Enter 만들기 · Esc 닫기",
            "Tab next field · Enter create · Esc close",
        )
    }
    /// **An empty name isn't created** — it's unclear what's being made, and a nameless row in the
    /// list would have no way to be removed.
    pub fn project_name_required(self) -> &'static str {
        self.pick("이름을 적어 주세요.", "Type a name.")
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

    // ── Sidebar
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

    // ── Question screen
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

    // ── Approval screen
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

    // ── Enrollment code window
    pub fn enroll_title(self) -> &'static str {
        self.pick("Attacca 연결", "Connect to Attacca")
    }
    pub fn enroll_steps(self) -> &'static str {
        self.pick(
            "브라우저에서 이 주소를 열고, 아래 코드를 입력해 승인해 주세요:",
            "Open this address in your browser and enter the code below:",
        )
    }
    pub fn enroll_expires(self, secs: u64) -> String {
        let minutes = secs.div_ceil(60);
        match self {
            Lang::Ko => format!("코드는 {minutes}분 후 만료됩니다."),
            Lang::En => format!("Code expires in {minutes} minute(s)."),
        }
    }
    pub fn enroll_lapsed(self) -> &'static str {
        self.pick(
            "코드가 만료됐습니다. 새 코드를 요청하는 중입니다…",
            "That code expired. Requesting a new one…",
        )
    }
    pub fn enroll_denied(self) -> &'static str {
        self.pick(
            "브라우저에서 거부했습니다. Esc를 눌러 닫으세요.",
            "The request was declined in the browser. Press Esc to close.",
        )
    }
    pub fn enroll_keys(self) -> &'static str {
        self.pick("Esc 닫기", "Esc close")
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

    // ── Shell notice · no screen (notice.rs · main.rs · the connection bails in app.rs)
    pub fn connection_lost(self) -> &'static str {
        self.pick("연결이 끊겼습니다.", "Connection lost.")
    }
    pub fn connect_failed(self, why: &str) -> String {
        match self {
            Lang::Ko => format!("연결에 실패했습니다: {why}"),
            Lang::En => format!("Failed to connect: {why}"),
        }
    }
    pub fn previous_error(self, why: &str) -> String {
        match self {
            Lang::Ko => format!("직전 오류: {why}"),
            Lang::En => format!("Previous error: {why}"),
        }
    }
    pub fn log_location(self, path: &str) -> String {
        match self {
            Lang::Ko => format!("자세한 것은 로그에 있습니다: {path}"),
            Lang::En => format!("See the log for details: {path}"),
        }
    }
    pub fn waiting_for_approval(self) -> &'static str {
        self.pick(
            "브라우저에서 승인하면 저절로 이어집니다. 그만두려면 Ctrl+C를 누르세요.",
            "Approve in the browser and it continues on its own. Press Ctrl+C to quit.",
        )
    }
    pub fn server_unreachable(self, secs: u64, why: &str) -> String {
        match self {
            Lang::Ko => format!("서버에 연결하지 못했습니다 ({secs}초째): {why}"),
            Lang::En => format!("Couldn't reach the server ({secs}s in): {why}"),
        }
    }
    /// Another window already holding this credential. **The approval window may be in that window** —
    /// saying only \"connected elsewhere\" would send the person looking for the code in the wrong place.
    pub fn another_window_notice(self) -> &'static str {
        self.pick(
            "다른 zyris-code 창이 이미 같은 자격으로 붙어 있습니다. 승인 창이 그 창에 떠 있을 수 있습니다.",
            "Another zyris-code window is already using the same credential. The approval window may be there.",
        )
    }

    // ── Commands (`/clear` · `/cwd` · `/grants` · `/agent` · `/undo` · `/changes`)
    pub fn clear_done(self) -> &'static str {
        self.pick(
            "화면을 지웠습니다. thread의 기록은 그대로입니다.",
            "Screen cleared. The thread's history is untouched.",
        )
    }
    pub fn cwd_text(self, cwd: &Path, node: &str, slug: &str, cred: &str) -> String {
        match self {
            Lang::Ko => format!(
                "도구는 `{}`에서 돕니다.\n\n\
                 이 노드는 **{node}**로 등록돼 있습니다 — 도구 이름은 `zyris__{slug}__…`입니다. \
                 `ZYRIS_NODE_NAME`으로 바꿉니다.\n\n\
                 자격은 `{cred}`에 있습니다.",
                cwd.display(),
            ),
            Lang::En => format!(
                "Tools run in `{}`.\n\n\
                 This node is registered as **{node}** — tool names are `zyris__{slug}__…`. \
                 Change it with `ZYRIS_NODE_NAME`.\n\n\
                 Credentials live in `{cred}`.",
                cwd.display(),
            ),
        }
    }
    pub fn grants_none_closed(self) -> &'static str {
        self.pick("열어 둔 곳이 없었습니다.", "Nothing was open.")
    }
    pub fn grants_closed(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("{n}곳을 닫았습니다. 이제 그 밖을 만지려면 다시 물어봅니다."),
            Lang::En => format!("Closed {n}. Touching anything else outside asks again."),
        }
    }
    pub fn grants_text(self, grants: &Grants) -> String {
        let roots = grants.roots();
        if roots.is_empty() {
            return match self {
                Lang::Ko => "작업 디렉터리 밖으로 열어 둔 곳이 없습니다.\n\n\
                             승인 화면에서 `a`를 누르면 그 디렉터리가 이 thread 동안 열립니다."
                    .into(),
                Lang::En => "Nothing is open outside the working directory.\n\n\
                             Pressing `a` on the approval screen opens that directory for this thread."
                    .into(),
            };
        }
        let mut s = match self {
            Lang::Ko => format!("작업 디렉터리 밖으로 {}곳이 열려 있습니다.\n", roots.len()),
            Lang::En => {
                format!("{} directories are open outside the working directory.\n", roots.len())
            }
        };
        for root in roots {
            s.push_str(&format!("\n- `{}`", root.display()));
        }
        // **끄면 잊는다는 것을 같이 말한다.** 디스크에 남는 줄 알면 필요 이상으로 겁을 낸다.
        s.push_str(match self {
            Lang::Ko => {
                "\n\n여기와 그 아래는 묻지 않고 만집니다. `/grants close`로 전부 닫습니다 — \
                         앱을 끄면 어차피 잊습니다."
            }
            Lang::En => {
                "\n\nEverything here and below is touched without asking. `/grants close` \
                         shuts them all — the app forgets them on quit anyway."
            }
        });
        s
    }
    pub fn agent_staged(self, name: &str) -> String {
        match self {
            Lang::Ko => format!(
                "에이전트: **{name}** · 다음 메시지에서 새 thread가 열립니다. \
                 앞 thread는 ←의 목록에 그대로 있습니다."
            ),
            Lang::En => format!(
                "Agent: **{name}** · a new thread opens with your next message. \
                 The previous thread stays in the ← list."
            ),
        }
    }
    pub fn agent_list_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("에이전트 목록을 읽지 못했습니다: {e}"),
            Lang::En => format!("Couldn't read the agent list: {e}"),
        }
    }
    pub fn agent_cannot_send(self) -> &'static str {
        self.pick("에이전트를 찾지 못해 보낼 수 없습니다.", "No agent — can't send.")
    }
    pub fn send_failed(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("전송하지 못했습니다: {e}"),
            Lang::En => format!("Couldn't send: {e}"),
        }
    }
    pub fn stopping_turn(self) -> &'static str {
        self.pick("진행 중인 턴을 멈추는 중입니다…", "Stopping the running turn…")
    }
    pub fn undo_log_not_ready(self) -> &'static str {
        self.pick("되돌림 기록을 아직 열지 못했습니다.", "Undo history isn't ready yet.")
    }
    pub fn reverted(self, path: &str) -> String {
        match self {
            Lang::Ko => format!("되돌렸습니다: `{path}`"),
            Lang::En => format!("Reverted: `{path}`"),
        }
    }
    pub fn nothing_to_undo(self) -> &'static str {
        self.pick("되돌릴 편집이 없습니다.", "Nothing to undo.")
    }
    pub fn undo_failed(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("되돌리지 못했습니다: {e}"),
            Lang::En => format!("Couldn't revert: {e}"),
        }
    }
    pub fn changes_text(self, changed: &[Changed], cwd: &Path) -> String {
        if changed.is_empty() {
            return self
                .pick(
                    "이 디렉터리에서 바꾼 파일이 없습니다.",
                    "No files were changed in this directory.",
                )
                .to_string();
        }
        let mut s = match self {
            Lang::Ko => {
                format!("바꾼 파일 {}개입니다. 최근에 손댄 것이 위입니다.\n", changed.len())
            }
            Lang::En => format!("{} files changed. Most recent first.\n", changed.len()),
        };
        for c in changed {
            let shown = c.path.strip_prefix(cwd).unwrap_or(&c.path);
            // 만든 파일이라는 것이 +N보다 먼저다 — 되돌리면 지워진다는 뜻이라 무게가 다르다.
            let note = if c.created {
                self.pick(" · 새로 만든 것", " · created").to_string()
            } else if c.edits > 1 {
                match self {
                    Lang::Ko => format!(" · {}번 고침", c.edits),
                    Lang::En => format!(" · edited {} times", c.edits),
                }
            } else {
                String::new()
            };
            s.push_str(&format!("\n- `{}`  +{} −{}{note}", shown.display(), c.added, c.removed));
        }
        // **되돌릴 수 있는 범위와 같다는 것을 말해 둔다.** 이 목록을 보고 `/undo`를 누르므로,
        // 둘의 범위가 다르면 눌러 보고서야 안다.
        s.push_str(self.pick(
            "\n\n`/undo`가 이 기록을 뒤에서부터 되돌립니다. 앱을 껐다 켜도 남습니다.",
            "\n\n`/undo` reverts this log from the end. It survives an app restart.",
        ));
        s
    }
    pub fn mcp_report_text(self, report: &[(String, Result<usize, String>)]) -> String {
        if report.is_empty() {
            return self
                .pick(
                    "붙은 MCP 서버가 없습니다. `.mcp.json`이나 \
                     `~/.config/zyris-code/mcp.json`에 적습니다.",
                    "No MCP servers are attached. Write one in `.mcp.json` or \
                     `~/.config/zyris-code/mcp.json`.",
                )
                .to_string();
        }
        let mut s = String::from(self.pick("MCP 서버입니다.\n", "MCP servers:\n"));
        for (name, outcome) in report {
            s.push_str(&match outcome {
                Ok(n) => match self {
                    Lang::Ko => format!("\n- `{name}` — 도구 {n}개"),
                    Lang::En => format!("\n- `{name}` — {n} tools"),
                },
                Err(why) => match self {
                    Lang::Ko => format!("\n- `{name}` — 못 띄웠습니다: {why}"),
                    Lang::En => format!("\n- `{name}` — couldn't start it: {why}"),
                },
            });
        }
        s
    }
    pub fn rules_text(self, found: &[Found]) -> String {
        if found.is_empty() {
            return self
                .pick(
                    "이 디렉터리와 그 위쪽에 `CLAUDE.md`도 `AGENTS.md`도 없습니다.",
                    "No `CLAUDE.md` or `AGENTS.md` in this directory or above it.",
                )
                .to_string();
        }
        let mut s = String::from(self.pick(
            "이 thread에 실린 지침입니다. 아래로 갈수록 구체적입니다.\n",
            "Instructions loaded into this thread. The later ones are more specific.\n",
        ));
        for f in found {
            let count = f.text.chars().count();
            s.push_str(&match self {
                Lang::Ko => format!("\n- `{}` — {count}자", f.path.display()),
                Lang::En => format!("\n- `{}` — {count} chars", f.path.display()),
            });
        }
        s.push_str(self.pick(
            "\n\n파일을 고쳤으면 `/agent`이나 ←의 목록으로 새 thread를 열어야 반영됩니다.",
            "\n\nAfter editing the files, open a new thread with `/agent` or the ← list for it \
             to take effect.",
        ));
        s
    }
    pub fn skills_text(self, skills: &[SkillInfo]) -> String {
        if skills.is_empty() {
            return self
                .pick(
                    "쓸 수 있는 스킬이 없습니다. `.zyris-code/skills/`나 \
                     `~/.config/zyris-code/skills/`에 둡니다.",
                    "No skills available. Put them in `.zyris-code/skills/` or \
                     `~/.config/zyris-code/skills/`.",
                )
                .to_string();
        }
        let mut s = String::from(self.pick("쓸 수 있는 스킬입니다.\n", "Skills available:\n"));
        for skill in skills {
            s.push_str(&format!("\n- **{}** — {}", skill.name, skill.description));
        }
        s
    }

    // ── Plugin (`/plugin`)
    pub fn plugin_added(self, p: &Plugin, contents: &str) -> String {
        match self {
            Lang::Ko => format!(
                "**{}**을 받았습니다.{}\n\n{contents}\n\n\
                 다음에 zyris-code를 다시 띄우면 붙습니다 — 도구도 스킬도 시작할 때 읽습니다.",
                p.name,
                if p.description.is_empty() { String::new() } else { format!(" {}", p.description) },
            ),
            Lang::En => format!(
                "Installed **{}**.{}\n\n{contents}\n\n\
                 It attaches on the next launch of zyris-code — tools and skills are read at startup.",
                p.name,
                if p.description.is_empty() { String::new() } else { format!(" {}", p.description) },
            ),
        }
    }
    pub fn plugin_removed(self, name: &str) -> String {
        match self {
            Lang::Ko => format!("`{name}`을 지웠습니다. 다시 띄우면 빠집니다."),
            Lang::En => format!("Removed `{name}`. It drops out on the next launch."),
        }
    }
    pub fn plugin_update_text(self, done: &[(String, Result<String, String>)]) -> String {
        if done.is_empty() {
            return self.pick("받아 둔 플러그인이 없습니다.", "No fetched plugins.").to_string();
        }
        let mut s = String::from(self.pick("갱신했습니다.\n", "Updated.\n"));
        for (name, outcome) in done {
            s.push_str(&match outcome {
                Ok(out) if out.contains("up to date") || out.contains("최신") => match self {
                    Lang::Ko => format!("\n- `{name}` — 이미 최신입니다"),
                    Lang::En => format!("\n- `{name}` — already up to date"),
                },
                Ok(_) => match self {
                    Lang::Ko => format!("\n- `{name}` — 새로 받았습니다"),
                    Lang::En => format!("\n- `{name}` — fetched the update"),
                },
                Err(why) => format!("\n- `{name}` — {why}"),
            });
        }
        s.push_str(
            self.pick("\n\n다시 띄우면 반영됩니다.", "\n\nIt takes effect on the next launch."),
        );
        s
    }
    pub fn plugin_unknown(self, why: &str) -> String {
        match self {
            Lang::Ko => format!(
                "`/plugin {why}`\n\n             - `/plugin` — 받아 둔 것 보기\n\
                 \x20            - `/plugin add owner/repo` — 받기\n\
                 \x20            - `/plugin remove 이름` — 지우기\n\
                 \x20            - `/plugin update [이름]` — 갱신"
            ),
            Lang::En => format!(
                "`/plugin {why}`\n\n             - `/plugin` — list what's fetched\n\
                 \x20            - `/plugin add owner/repo` — install\n\
                 \x20            - `/plugin remove name` — remove\n\
                 \x20            - `/plugin update [name]` — update"
            ),
        }
    }
    pub fn plugin_no_git(self) -> &'static str {
        self.pick(
            "git이 없습니다. 플러그인은 git으로 받아 옵니다.",
            "git is missing. Plugins are fetched with git.",
        )
    }
    pub fn plugin_git_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("git을 돌리지 못했습니다: {e}"),
            Lang::En => format!("Couldn't run git: {e}"),
        }
    }
    pub fn plugin_git_failed(self) -> &'static str {
        self.pick("git이 실패했습니다", "git failed")
    }
    pub fn plugin_source_unclear(self, text: &str) -> String {
        match self {
            Lang::Ko => format!(
                "`{text}`에서 받을 곳을 못 찾았습니다. `owner/repo`나 clone할 수 있는 주소를 주세요."
            ),
            Lang::En => format!(
                "Couldn't find a source in `{text}`. Give an `owner/repo` or a clonable address."
            ),
        }
    }
    pub fn plugin_already_there(self, name: &str) -> String {
        match self {
            Lang::Ko => format!("`{name}`은 이미 있습니다. 갱신은 `/plugin update {name}`입니다."),
            Lang::En => {
                format!("`{name}` is already installed. Update with `/plugin update {name}`.")
            }
        }
    }
    pub fn plugin_dir_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("플러그인 자리를 못 만들었습니다: {e}"),
            Lang::En => format!("Couldn't create the plugin directory: {e}"),
        }
    }
    pub fn plugin_no_manifest(self, name: &str) -> String {
        match self {
            Lang::Ko => {
                format!("`{name}`에 plugin.json이 없어 플러그인이 아닙니다. 받은 것은 지웠습니다.")
            }
            Lang::En => format!(
                "`{name}` has no plugin.json, so it isn't a plugin. The fetched copy was removed."
            ),
        }
    }
    pub fn plugin_manifest_unreadable(self, name: &str) -> String {
        match self {
            Lang::Ko => format!("`{name}`의 plugin.json을 읽지 못했습니다."),
            Lang::En => format!("Couldn't read `{name}`'s plugin.json."),
        }
    }
    pub fn plugin_remove_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("지우지 못했습니다: {e}"),
            Lang::En => format!("Couldn't remove it: {e}"),
        }
    }
    pub fn plugin_not_found(self, name: &str) -> String {
        match self {
            Lang::Ko => format!("받아 둔 플러그인 중에 `{name}`이 없습니다."),
            Lang::En => format!("`{name}` isn't among the fetched plugins."),
        }
    }
    pub fn plugin_contents_text(self, p: &Plugin) -> String {
        let mut s = String::new();
        for spec in &p.mcp {
            s.push_str(&match self {
                Lang::Ko => {
                    format!("- MCP `{}` — 다음 실행 때 `{}`을 돌립니다\n", spec.slug, spec.command)
                }
                Lang::En => {
                    format!("- MCP `{}` — runs `{}` on the next launch\n", spec.slug, spec.command)
                }
            });
        }
        if p.skills.is_some() {
            s.push_str(self.pick("- 스킬이 딸려 있습니다\n", "- ships a skill\n"));
        }
        if s.is_empty() {
            s.push_str(self.pick("- 얹는 것이 없습니다\n", "- adds nothing\n"));
        }
        s
    }
    pub fn plugin_list_text(self, found: &[Plugin]) -> String {
        if found.is_empty() {
            return self
                .pick(
                    "플러그인이 없습니다. `/plugin add owner/repo`로 받습니다.",
                    "No plugins. Install one with `/plugin add owner/repo`.",
                )
                .to_string();
        }
        let mut s = String::from(self.pick("플러그인입니다.\n", "Plugins:\n"));
        for p in found {
            s.push_str(&format!(
                "\n- **{}**{}{}",
                p.name,
                if p.fetched() { "" } else { self.pick(" (직접 둔 것)", " (hand-placed)") },
                if p.description.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", p.description)
                },
            ));
        }
        s
    }

    // ── Question screen actions
    pub fn action_back(self) -> &'static str {
        self.pick("← 이전", "← back")
    }
    pub fn action_next(self) -> &'static str {
        self.pick("다음 →", "next →")
    }
    pub fn action_skip(self) -> &'static str {
        self.pick("건너뛰기 →", "skip →")
    }
    pub fn action_submit(self) -> &'static str {
        self.pick("제출", "submit")
    }
    pub fn action_edit(self) -> &'static str {
        self.pick("고치기", "edit")
    }
    pub fn action_reject(self) -> &'static str {
        self.pick("답하지 않기", "decline to answer")
    }
    pub fn question_refused(self) -> &'static str {
        self.pick("이 질문에는 답하지 않겠습니다.", "I won't answer this question.")
    }
    pub fn all_skipped(self) -> &'static str {
        self.pick("모두 건너뛰었습니다.", "Skipped them all.")
    }
    /// The marker that flags a typed-in answer. **Must match between the composed answer
    /// (`question::answer_text`) and the timeline's detection (`rows`)** — both use the same lang.
    pub fn free_mark(self) -> &'static str {
        self.pick("직접 입력:", "Typed:")
    }

    // ── Tool detail (event.rs writes the text · rows.rs styles it)
    pub fn detail_args(self) -> &'static str {
        self.pick("인자", "Args")
    }
    pub fn detail_output(self) -> &'static str {
        self.pick("출력", "Output")
    }
    pub fn detail_result(self) -> &'static str {
        self.pick("결과", "Result")
    }
    pub fn detail_error(self) -> &'static str {
        self.pick("오류", "Error")
    }
    /// The section heads in the order `tool_detail` writes them. **rows.rs styles by these names,
    /// so the writer and the reader must agree** — both sides use the same lang.
    pub fn tool_sections(self) -> [&'static str; 4] {
        [self.detail_args(), self.detail_output(), self.detail_result(), self.detail_error()]
    }
    pub fn detail_timed_out(self) -> &'static str {
        self.pick("시간이 다 됐습니다", "Timed out")
    }
    pub fn detail_exit_code(self, code: i64) -> String {
        match self {
            Lang::Ko => format!("종료 코드 {code}"),
            Lang::En => format!("Exit code {code}"),
        }
    }
    pub fn detail_no_output(self) -> &'static str {
        self.pick("(출력 없음)", "(no output)")
    }
    pub fn detail_clipped(self) -> &'static str {
        self.pick("… (잘렸습니다)", "… (clipped)")
    }
    pub fn default_shell(self) -> &'static str {
        self.pick("기본 셸", "default shell")
    }
    pub fn tool_count(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("도구 {n}개"),
            Lang::En => format!("{n} tools"),
        }
    }
    pub fn step_count(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("{n}단계"),
            Lang::En => format!("{n} steps"),
        }
    }
    pub fn diff_skip(self, n: u32) -> String {
        match self {
            Lang::Ko => format!(" … {n}줄 생략"),
            Lang::En => format!(" … {n} lines skipped"),
        }
    }
    pub fn diff_omitted(self, n: u32) -> String {
        match self {
            Lang::Ko => format!("@@ {n}줄 생략 @@"),
            Lang::En => format!("@@ {n} lines omitted @@"),
        }
    }

    // ── Lists (picker)
    pub fn pick_more(self, up: bool, n: usize) -> String {
        let arrow = if up { "↑" } else { "↓" };
        match self {
            Lang::Ko => format!("  {arrow} {n}개 더"),
            Lang::En => format!("  {arrow} {n} more"),
        }
    }
    pub fn picker_close(self) -> &'static str {
        self.pick("← 닫기", "← close")
    }
    pub fn loading(self) -> &'static str {
        self.pick("불러오는 중…", "Loading…")
    }
    pub fn picker_back(self) -> &'static str {
        self.pick("← 뒤로", "← back")
    }
    pub fn picker_esc_close(self) -> &'static str {
        self.pick("Esc 닫기", "Esc close")
    }
    pub fn picker_keys(self, back: &str) -> String {
        match self {
            Lang::Ko => format!("↑↓ 이동 · Enter 고르기 · {back}"),
            Lang::En => format!("↑↓ move · Enter choose · {back}"),
        }
    }
    pub fn cannot_choose(self) -> &'static str {
        self.pick("지금은 고를 수 없습니다", "Can't choose right now")
    }

    // ── Conn (errors that surface in the status bar · timeline)
    pub fn server_timeout(self, secs: u64) -> String {
        match self {
            Lang::Ko => format!("서버가 {secs}초 안에 답하지 않았습니다"),
            Lang::En => format!("The server didn't answer within {secs}s"),
        }
    }
    pub fn no_credential_dir(self) -> &'static str {
        self.pick(
            "자격을 둘 디렉터리를 찾지 못했습니다",
            "Couldn't find a directory for credentials",
        )
    }
    pub fn missing_scopes(self, missing: &str) -> String {
        match self {
            Lang::Ko => format!(
                "**권한이 모자랍니다: {missing}**. 승인할 때 정해진 권한은 나중에 넓어지지 않습니다.\n\n\
                 다시 연결되면 등록 코드 창이 뜨니, 승인 화면에서 권한을 **모두** 체크해 주세요."
            ),
            Lang::En => format!(
                "**Not enough permissions: {missing}**. Permissions fixed at approval time \
                 never widen.\n\nA fresh enrollment-code window appears on reconnect — check \
                 **every** scope there."
            ),
        }
    }
    pub fn scopes_asked_again(self, missing: &str) -> String {
        match self {
            Lang::Ko => format!(
                "**권한이 모자랍니다: {missing}**. 다시 승인받을 수 있도록 이 컴퓨터의 자격을 비웠습니다.\n\n\
                 다시 연결되면 등록 코드 창이 뜨니, 승인 화면에서 권한을 **모두** 체크해 주세요. \
                 지금 연결은 그대로 쓸 수 있습니다."
            ),
            Lang::En => format!(
                "**Not enough permissions: {missing}**. Dropped this machine's credentials so \
                 you can be approved again.\n\nA fresh enrollment-code window appears on reconnect \
                 — check **every** scope there. The current connection keeps working."
            ),
        }
    }
    pub fn agent_not_found(self, name: &str) -> String {
        match self {
            Lang::Ko => {
                format!("'{name}' 에이전트가 계정에 없습니다. /agent으로 목록을 볼 수 있습니다.")
            }
            Lang::En => {
                format!("No agent named '{name}' on this account. See the list with /agent.")
            }
        }
    }
    pub fn thread_create_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("thread를 만들지 못했습니다: {e}"),
            Lang::En => format!("Couldn't create the thread: {e}"),
        }
    }
    pub fn job_create_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("job을 걸지 못했습니다: {e}"),
            Lang::En => format!("Couldn't queue the job: {e}"),
        }
    }
    pub fn job_no_session(self, id: &str) -> String {
        match self {
            Lang::Ko => format!(
                "job **{id}**은 걸렸는데 세션이 아직 없어 여기서 못 봅니다. \
                 attacca에서 열어 보세요."
            ),
            Lang::En => format!(
                "Job **{id}** was queued but has no session yet, so it can't be watched here. \
                 Open it in attacca."
            ),
        }
    }
    pub fn work_create_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("work를 만들지 못했습니다: {e}"),
            Lang::En => format!("Couldn't create the work: {e}"),
        }
    }
    pub fn project_create_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("프로젝트를 만들지 못했습니다: {e}"),
            Lang::En => format!("Couldn't create the project: {e}"),
        }
    }
    pub fn project_list_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("프로젝트 목록을 읽지 못했습니다: {e}"),
            Lang::En => format!("Couldn't read the project list: {e}"),
        }
    }
    pub fn thread_list_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("thread 목록을 읽지 못했습니다: {e}"),
            Lang::En => format!("Couldn't read the thread list: {e}"),
        }
    }
    pub fn history_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("지난 기록을 읽지 못했습니다: {e}"),
            Lang::En => format!("Couldn't read the past history: {e}"),
        }
    }
    pub fn untitled(self) -> &'static str {
        self.pick("제목 없음", "untitled")
    }

    // ── Edit tool errors (returned to the agent, shown in the timeline)
    pub fn edit_mkdir_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("상위 디렉터리를 만들지 못했습니다: {e}"),
            Lang::En => format!("Couldn't create the parent directory: {e}"),
        }
    }
    pub fn edit_changed_after_read(self, path: &str, base: &str, now: &str) -> String {
        match self {
            Lang::Ko => format!(
                "'{path}'이(가) 읽은 뒤 바뀌었습니다 (base_version {base} ≠ 지금 {now}). \
                 file_io.read로 지금 내용을 다시 읽고, 새 버전 토큰을 base_version으로 다시 시도하세요."
            ),
            Lang::En => format!(
                "'{path}' changed after it was read (base_version {base} ≠ current {now}). \
                 Re-read the current content with file_io.read and retry with the new version \
                 token as base_version."
            ),
        }
    }
    pub fn edit_exists_no_base(self, path: &str) -> String {
        match self {
            Lang::Ko => format!(
                "'{path}'은(는) 이미 있는 파일입니다 — 덮어쓰려면 base_version을 주세요. \
                 읽은 응답의 stat.modified_unix_ms:stat.size 또는 code_edit.version의 version을 \
                 그대로 넘기세요."
            ),
            Lang::En => format!(
                "'{path}' already exists — pass base_version to overwrite it. Use the \
                 stat.modified_unix_ms:stat.size of the read response or code_edit.version's \
                 version as-is."
            ),
        }
    }
    pub fn edit_write_error(self, e: &str) -> String {
        match self {
            Lang::Ko => format!("쓰지 못했습니다: {e}"),
            Lang::En => format!("Couldn't write: {e}"),
        }
    }
    pub fn edit_not_found(self, needle: &str) -> String {
        match self {
            Lang::Ko => format!(
                "'{needle}'을(를) 파일에서 찾지 못했습니다. file_io.read로 지금 내용을 다시 읽으세요."
            ),
            Lang::En => format!(
                "'{needle}' wasn't found in the file. Read the current content with file_io.read."
            ),
        }
    }
    pub fn edit_ambiguous(self, n: usize) -> String {
        match self {
            Lang::Ko => format!(
                "앞뒤를 더 붙여 한 곳만 가리키거나 replace_all을 켜세요. (파일에 {n}번 나옵니다)"
            ),
            Lang::En => format!(
                "Add more context to point at one spot, or turn replace_all on. (It appears \
                 {n} times in the file)"
            ),
        }
    }
    pub fn edit_read_error(self, path: &str, e: &str) -> String {
        match self {
            Lang::Ko => format!("'{path}'을(를) 읽지 못했습니다: {e}"),
            Lang::En => format!("Couldn't read '{path}': {e}"),
        }
    }
    pub fn edit_stat_error(self, path: &str, e: &str) -> String {
        match self {
            Lang::Ko => format!("'{path}'을(를) 확인하지 못했습니다: {e}"),
            Lang::En => format!("Couldn't stat '{path}': {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Both languages' names are accepted.** Typing `/lang` with a Korean word on an English screen
    /// is natural, and so is the reverse — accepting only the current screen language would leave someone who chose wrong with no way back.
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

    /// The locale is only a guess. **Unknown means English** — offering a Korean screen to someone
    /// who can't read Korean is worse than the reverse.
    #[test]
    fn the_locale_is_a_guess_that_errs_towards_english() {
        assert_eq!(from_locale(Some("ko_KR.UTF-8")), Lang::Ko);
        assert_eq!(from_locale(Some("KO")), Lang::Ko);
        assert_eq!(from_locale(Some("en_US.UTF-8")), Lang::En);
        assert_eq!(from_locale(Some("fr_FR")), Lang::En);
        assert_eq!(from_locale(None), Lang::En, "모르면 영어다");
        assert_eq!(from_locale(Some("  ")), Lang::En);
    }

    /// The name written and the name read back must match — if they diverge, the saved setting can't be read.
    #[test]
    fn what_is_written_is_what_is_read_back() {
        for lang in [Lang::Ko, Lang::En] {
            assert_eq!(Lang::parse(lang.code()), Some(lang));
        }
    }

    /// **A language names itself in its own language.** Written in a language you can't read now,
    /// you can't tell what you're choosing.
    #[test]
    fn a_language_names_itself() {
        assert_eq!(Lang::Ko.name(), "한국어");
        assert_eq!(Lang::En.name(), "English");
    }

    /// Both languages **must both be present.** Filling only one side leaves half the screen in the
    /// other language.
    ///
    /// **`mode_work`·`mode_job` are deliberately left out.** They're exactly the names attacca uses on
    /// its own screen, so both languages are the same, and putting them here would trip a "not translated" check. `the_english_side_has_no_hangul_left_in_it` below guards them instead.
    #[test]
    fn no_message_is_left_in_one_language_only() {
        let ko = Lang::Ko;
        let en = Lang::En;
        let pairs: Vec<(&str, &str)> = vec![
            (ko.working(), en.working()),
            (ko.idle(), en.idle()),
            (ko.stopping(), en.stopping()),
            (ko.connected(), en.connected()),
            (ko.waiting_answer(), en.waiting_answer()),
            (ko.new_thread(), en.new_thread()),
            (ko.projects(), en.projects()),
            (ko.new_project(), en.new_project()),
            (ko.project_form_title(), en.project_form_title()),
            (ko.project_form_keys(), en.project_form_keys()),
            (ko.project_name_required(), en.project_name_required()),
            (ko.agents(), en.agents()),
            (ko.commands(), en.commands()),
            (ko.mode_normal(), en.mode_normal()),
            (ko.mode_plan(), en.mode_plan()),
            (ko.approve_keys(), en.approve_keys()),
            (ko.esc_stops(), en.esc_stops()),
            (ko.enroll_title(), en.enroll_title()),
            (ko.enroll_steps(), en.enroll_steps()),
            (ko.enroll_lapsed(), en.enroll_lapsed()),
            (ko.enroll_denied(), en.enroll_denied()),
            (ko.enroll_keys(), en.enroll_keys()),
            (ko.clear_done(), en.clear_done()),
            (ko.grants_none_closed(), en.grants_none_closed()),
            (ko.agent_cannot_send(), en.agent_cannot_send()),
            (ko.undo_log_not_ready(), en.undo_log_not_ready()),
            (ko.nothing_to_undo(), en.nothing_to_undo()),
            (ko.action_back(), en.action_back()),
            (ko.action_next(), en.action_next()),
            (ko.action_skip(), en.action_skip()),
            (ko.action_submit(), en.action_submit()),
            (ko.action_edit(), en.action_edit()),
            (ko.action_reject(), en.action_reject()),
            (ko.question_refused(), en.question_refused()),
            (ko.all_skipped(), en.all_skipped()),
            (ko.detail_args(), en.detail_args()),
            (ko.detail_output(), en.detail_output()),
            (ko.detail_result(), en.detail_result()),
            (ko.detail_error(), en.detail_error()),
            (ko.detail_timed_out(), en.detail_timed_out()),
            (ko.detail_no_output(), en.detail_no_output()),
            (ko.detail_clipped(), en.detail_clipped()),
            (ko.default_shell(), en.default_shell()),
            (ko.picker_close(), en.picker_close()),
            (ko.picker_back(), en.picker_back()),
            (ko.picker_esc_close(), en.picker_esc_close()),
            (ko.cannot_choose(), en.cannot_choose()),
            (ko.untitled(), en.untitled()),
            (ko.no_credential_dir(), en.no_credential_dir()),
            (ko.connection_lost(), en.connection_lost()),
            (ko.waiting_for_approval(), en.waiting_for_approval()),
            (ko.another_window_notice(), en.another_window_notice()),
            (ko.free_mark(), en.free_mark()),
        ];
        for (k, e) in pairs {
            assert_ne!(k, e, "번역이 안 된 문구가 있다: {k}");
            assert!(!k.is_empty() && !e.is_empty());
        }
    }

    /// The English screen must have **not a single Hangul character.** Mixed in, an untranslated spot wouldn't show.
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
            en.enroll_title(),
            en.enroll_steps(),
            en.enroll_lapsed(),
            en.enroll_denied(),
            en.enroll_keys(),
            en.connected(),
            en.waiting_answer(),
            en.mode_continues_work(),
            en.mode_continues_job(),
            en.project_form_title(),
            en.project_name(),
            en.project_name_placeholder(),
            en.project_description(),
            en.project_description_placeholder(),
            en.project_form_keys(),
            en.project_name_required(),
            en.connection_lost(),
            en.waiting_for_approval(),
            en.another_window_notice(),
            en.clear_done(),
            en.grants_none_closed(),
            en.agent_cannot_send(),
            en.undo_log_not_ready(),
            en.nothing_to_undo(),
            en.action_back(),
            en.action_next(),
            en.action_skip(),
            en.action_submit(),
            en.action_edit(),
            en.action_reject(),
            en.question_refused(),
            en.all_skipped(),
            en.free_mark(),
            en.detail_args(),
            en.detail_output(),
            en.detail_result(),
            en.detail_error(),
            en.detail_timed_out(),
            en.detail_no_output(),
            en.detail_clipped(),
            en.default_shell(),
            en.picker_close(),
            en.picker_back(),
            en.picker_esc_close(),
            en.cannot_choose(),
            en.untitled(),
            en.no_credential_dir(),
        ];
        for text in said {
            assert!(
                !text.chars().any(|c| ('가'..='힣').contains(&c)),
                "영어 문구에 한글이 남았다: {text}"
            );
        }
        // **Arguments-filled phrases.** These take data, so they can't sit in the array above —
        // the call itself is the check.
        let with_args = [
            en.agent_list_error("x"),
            en.connect_failed("x"),
            en.previous_error("x"),
            en.log_location("/tmp/zyris-code.log"),
            en.server_unreachable(5, "x"),
            en.cwd_text(std::path::Path::new("/home/ruma"), "node", "slug", "cred"),
            en.grants_closed(2),
            en.agent_staged("Main Agent"),
            en.reverted("src/x.rs"),
            en.undo_failed("x"),
            en.server_timeout(15),
            en.missing_scopes("a, b"),
            en.scopes_asked_again("a"),
            en.agent_not_found("x"),
            en.thread_create_error("x"),
            en.job_create_error("x"),
            en.job_no_session("j1"),
            en.work_create_error("x"),
            en.project_create_error("x"),
            en.project_list_error("x"),
            en.thread_list_error("x"),
            en.history_error("x"),
            en.edit_mkdir_error("x"),
            en.edit_write_error("x"),
            en.edit_not_found("abc"),
            en.edit_ambiguous(3),
            en.edit_read_error("p", "x"),
            en.edit_stat_error("p", "x"),
            en.plugin_removed("x"),
            en.plugin_unknown("x"),
            en.detail_exit_code(1),
            en.tool_count(3),
            en.diff_skip(2),
            en.diff_omitted(2),
            en.pick_more(true, 3),
            en.picker_keys("← close"),
            en.changes_text(&[], std::path::Path::new("/")),
            en.mcp_report_text(&[]),
            en.rules_text(&[]),
            en.skills_text(&[]),
            en.plugin_list_text(&[]),
            en.plugin_update_text(&[]),
            en.grants_text(&Grants::default()),
            en.plugin_contents_text(&Plugin {
                name: "x".into(),
                description: String::new(),
                mcp: vec![crate::mcp::bridge::ServerSpec {
                    slug: "m".into(),
                    command: "cmd".into(),
                    args: vec![],
                    env: Default::default(),
                }],
                skills: None,
                root: "/tmp".into(),
            }),
            en.plugin_added(
                &Plugin {
                    name: "x".into(),
                    description: String::new(),
                    mcp: vec![],
                    skills: None,
                    root: "/tmp".into(),
                },
                "contents",
            ),
        ];
        for text in with_args {
            assert!(
                !text.chars().any(|c| ('가'..='힣').contains(&c)),
                "영어 문구에 한글이 남았다: {text}"
            );
        }
        assert!(!en.queued(3).chars().any(|c| ('가'..='힣').contains(&c)));
        assert!(!en.threads_in("proj").chars().any(|c| ('가'..='힣').contains(&c)));
    }

    /// In Korean, thread is **sseuredeu**. Keeping the English word would make it stick out in the list.
    #[test]
    fn thread_reads_as_sseurede_in_korean() {
        assert!(Lang::Ko.new_thread().contains("쓰레드"), "{}", Lang::Ko.new_thread());
        assert!(Lang::Ko.threads_in("proj").contains("쓰레드"));
        assert!(Lang::Ko.approve_keys().contains("쓰레드"));
    }
}
