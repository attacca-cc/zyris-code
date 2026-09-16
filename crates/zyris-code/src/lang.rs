//! Produces the screen's words in Korean and English. Changed with `/config lang`.
//!
//! **All phrases are gathered in this one place.** Split per-widget with conditionals, one language
//! would inevitably get edited alone, leaving half the screen in the other language. With a single
//! function here holding both languages side by side, both are in view when editing.
//!
//! ## Why it lives in two places
//!
//! - `State.lang` — used by the drawing side. Since `apply` must stay pure, it has to be carried as
//!   state, and screen tests being able to fix a language and look at it is thanks to this too.
//! - `lang::current()` — used where there is no screen: the shell notice in `notice.rs`, which
//!   goes to the terminal a person is looking at. **Not for anything a tool returns** — the agent
//!   is what reads a tool's answer, and it does not read Korean (2026-09-14, user decision: the
//!   tools answered in Korean on a Korean machine, which is the wrong reader).
//!
//! `/config lang` sets both together. If they diverged, the conversation window would be English while only the shell notice stayed Korean.
//!
//! ## Which words come here
//!
//! **Only what people read.** Tool descriptions read by the agent are always English (the doc
//! comments in `tools/`), and code comments and test names are always Korean. What this file divides is only the screen.

use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::instructions::Found;
use crate::mode::{Mode, Route};
use crate::plugin::Plugin;
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
    /// From what the person typed. **Both languages' names are accepted** — typing `/config lang` with a
    /// Korean word on an English screen is natural, and so is `/config lang english` on a Korean one.
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
/// **What a person gave always wins.** The last choice comes next because a language change must
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
        tracing::warn!(error = %e, "couldn't save the chosen language");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Screen phrases
// ─────────────────────────────────────────────────────────────────────────────

/// The pieces `/status` shows, gathered in one place so the wording function stays
/// under clippy's argument budget.
pub struct StatusInfo<'a> {
    pub session_id: Option<&'a str>,
    pub project: Option<&'a str>,
    pub agent: &'a str,
    pub mode: &'a str,
    pub cwd: &'a Path,
    pub usage: &'a crate::usage::Usage,
    pub pending: Option<Route>,
}

impl Lang {
    // ── Bottom bar · activity line
    pub fn mode_normal(self) -> &'static str {
        self.pick("일반", "normal")
    }
    pub fn mode_plan(self) -> &'static str {
        self.pick("계획", "plan")
    }
    /// The natural Korean words — **일** (work) and **작업** (job), chosen with the user on
    /// 2026-08-07. The English names still work on the input side (`mode_named`), and attacca's
    /// own screen keeps using `work`·`job`, so the two stay findable there.
    pub fn mode_work(self) -> &'static str {
        self.pick("일", "work")
    }
    pub fn mode_job(self) -> &'static str {
        self.pick("작업", "job")
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
    /// What rides on the end of the activity line while the session has a plan: `(2/5)`.
    ///
    /// **The same in both languages.** It is a count, and a word in front of it would take room
    /// from the line that has to say what is happening.
    pub fn todo_count(self, done: usize, total: usize) -> String {
        format!(" ({done}/{total})")
    }
    /// The line that stands in for the tasks that did not fit.
    pub fn todo_more(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("↓ {n}개 더"),
            Lang::En => format!("↓ {n} more"),
        }
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
    /// What to show instead of the connected notice when the terminal cannot tell
    /// Shift+Enter apart from Enter.
    ///
    /// A terminal without the Kitty keyboard protocol sends Shift+Enter as a single
    /// `\r`, so the app cannot separate the two — say up front that Alt+Enter (which
    /// works everywhere) is the way to insert a newline.
    pub fn kitty_shift_enter_hint(self) -> &'static str {
        self.pick(
            "연결됨 ‒ 이 터미널은 Shift+Enter를 구별하지 못합니다. 줄바꿈은 Alt+Enter를 쓰세요.",
            "Connected ‒ this terminal can't tell Shift+Enter apart from Enter. Use Alt+Enter for a newline.",
        )
    }
    /// A span of seconds, in at most two units.
    ///
    /// **Seconds alone stop being readable long before a build finishes.** `110s` is a number
    /// somebody has to divide in their head; `1분 50초` is a moment. Hours appear only once there
    /// is one, and then the seconds are dropped — two units is as much as anybody takes in at a
    /// glance, and the third is noise.
    ///
    /// The words are the language's: a Korean screen says `1분 50초`, an English one `1m 50s`.
    pub fn duration(self, secs: u64) -> String {
        let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
        match self {
            Lang::Ko => match (h, m) {
                (0, 0) => format!("{s}초"),
                (0, m) => format!("{m}분 {s}초"),
                (h, m) => format!("{h}시간 {m}분"),
            },
            Lang::En => match (h, m) {
                (0, 0) => format!("{s}s"),
                (0, m) => format!("{m}m {s}s"),
                (h, m) => format!("{h}h {m}m"),
            },
        }
    }
    /// The head of a report row. **A failure says so in words as well as in colour** — colour
    /// alone is not a message.
    pub fn report_head(self, ok: bool) -> &'static str {
        match (self, ok) {
            (Lang::Ko, true) => "작업 결과 ∙ 성공",
            (Lang::Ko, false) => "작업 결과 ∙ 실패",
            (Lang::En, true) => "Job result ∙ ok",
            (Lang::En, false) => "Job result ∙ failed",
        }
    }
    /// What to show in the activity line while a command runs: **the tool that is running, and
    /// the run's own subtitle — never the command.**
    ///
    /// The command is as long as the agent wrote it. One `python3 -c`, one heredoc or one
    /// `sed -n '1,200p' …` fills the line right to its edge, pushes the `Esc 정지` hint off the
    /// end of it, and still does not say what the work is for. `tool` (`exec`) is what is
    /// running; `what` is the newest `work_summary` of the run — the very words the work card's
    /// head is wearing, and the server's own one-line answer to "what is being done". Both are
    /// short by construction (user decision, 2026-09-15).
    pub fn running_tool(self, tool: &str, what: &str, secs: u64) -> String {
        let took = self.duration(secs);
        let what = clip_columns(what.trim(), ACTIVITY_WIDTH);
        match what.is_empty() {
            true => format!("▶ {tool}  ∙  {took}"),
            false => format!("▶ {tool}  ∙  {what}  ∙  {took}"),
        }
    }
    /// Says once, on the status line, that a background job finished. **It says so on success
    /// too** — not knowing it is done leaves a person waiting.
    pub fn job_ended(self, id: &str, ok: bool, secs: u64) -> String {
        let took = self.duration(secs);
        match (self, ok) {
            (Lang::Ko, true) => format!("배경 {id} 끝남 ∙ 성공 ∙ {took}"),
            (Lang::Ko, false) => format!("배경 {id} 끝남 ∙ 실패 ∙ {took}"),
            (Lang::En, true) => format!("background {id} done ∙ ok ∙ {took}"),
            (Lang::En, false) => format!("background {id} done ∙ failed ∙ {took}"),
        }
    }
    /// Background jobs on the activity line. With several, only the count and the oldest one —
    /// they don't all fit on a single line.
    pub fn background_job(self, count: usize, id: &str, label: &str, secs: u64) -> String {
        let head = match (self, count) {
            (Lang::Ko, 1) => "배경".to_string(),
            (Lang::Ko, n) => format!("배경 {n}개"),
            (Lang::En, 1) => "background".to_string(),
            (Lang::En, n) => format!("background ×{n}"),
        };
        // **Clipped for the same reason a command is not shown at all.** The label defaults to
        // the job's own command (`jobs.rs`), so a backgrounded build would otherwise sprawl
        // across this line exactly the way a running one used to.
        let label = clip_columns(label.trim(), ACTIVITY_WIDTH);
        format!("{head}  {id} {label}  ∙  {}", self.duration(secs))
    }
    /// One row of `/jobs`, when that job belongs to a conversation other than the one on screen.
    ///
    /// `∙` rather than `·`: the middle dot is ambiguous width, so a terminal set up for CJK draws
    /// it two columns wide and shoves the rest of the row along (`tests/width.rs`).
    pub fn jobs_row_elsewhere(self, row: &str) -> String {
        match self {
            Lang::Ko => format!("{row}  ∙  다른 대화"),
            Lang::En => format!("{row}  ∙  another conversation"),
        }
    }
    pub fn jobs_none(self) -> &'static str {
        self.pick("배경에서 도는 것이 없습니다.", "Nothing running in the background.")
    }
    pub fn jobs_header(self) -> &'static str {
        self.pick("배경에서 도는 것:", "Running in the background:")
    }
    pub fn jobs_hint(self) -> &'static str {
        self.pick(
            "\n\n/jobs stop <id> 로 멈춥니다. 앱을 끄면 전부 같이 멈춥니다.",
            "\n\n/jobs stop <id> kills one. Quitting the app kills them all.",
        )
    }
    pub fn jobs_stopped(self, id: &str) -> String {
        match self {
            Lang::Ko => format!("배경 {id}을 멈췄습니다."),
            Lang::En => format!("Stopped background {id}."),
        }
    }
    pub fn jobs_unknown(self, id: &str) -> String {
        match self {
            Lang::Ko => format!("배경 {id}이 없습니다. /jobs로 확인해 주세요."),
            Lang::En => format!("No background job {id}. Check /jobs."),
        }
    }
    /// One row of `/jobs`. The seconds suffix belongs here, not at the call site.
    pub fn jobs_row(self, id: &str, label: &str, secs: u64) -> String {
        format!("\n  {id}  {label}  ∙  {}", self.duration(secs))
    }
    /// What to show in the activity line while waiting for a question.
    pub fn waiting_answer(self) -> &'static str {
        self.pick("대기 중 ‒ 답을 고르세요", "Waiting ‒ answer the question")
    }
    /// The hint attached to the right of the activity line while waiting for a question.
    pub fn waiting_answer_hint(self) -> &'static str {
        self.pick("↑↓ 이동 ∙ Enter 고르기", "↑↓ move ∙ Enter choose")
    }

    pub fn mode_now(self, mode: &str) -> String {
        match self {
            Lang::Ko => format!(
                "지금은 **{mode}** 모드입니다. Shift+Tab으로 돌리거나 \
                 `/mode 일반`∙`/mode 계획`∙`/mode 일`∙`/mode 작업`으로 바꿉니다."
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

    /// **Says what will happen next, not just that something opened.** A plan-mode job answers with
    /// a plan and then waits, and a person who does not know that is left watching a job that looks
    /// like it stopped halfway.
    pub fn opened_plan(self, id: &str) -> String {
        match self {
            Lang::Ko => format!("계획 **{id}**을 걸었습니다. 조사한 뒤 계획을 내놓고 기다립니다."),
            Lang::En => {
                format!("Planning **{id}**. It will investigate, hand back a plan, and wait.")
            }
        }
    }

    // ── The plan waiting to be approved
    pub fn plan_title(self) -> &'static str {
        self.pick("계획", "Plan")
    }
    /// **Says both keys, because nothing else on screen does.** Approving is Enter on an empty
    /// draft, which is not a thing anybody guesses.
    pub fn plan_keys(self, open: bool) -> String {
        let fold = match (self, open) {
            (Lang::Ko, true) => "Ctrl+P 접기",
            (Lang::Ko, false) => "Ctrl+P 펼치기",
            (Lang::En, true) => "Ctrl+P to fold",
            (Lang::En, false) => "Ctrl+P to open",
        };
        match self {
            // **`∙`, not `·`.** The middle dot is East Asian Ambiguous: one column here and two
            // on a terminal set for CJK, which shifts everything after it on the row.
            Lang::Ko => format!("Enter 승인 ∙ 고칠 점은 그냥 적으세요 ∙ {fold}"),
            Lang::En => format!("Enter approves ∙ type to ask for changes ∙ {fold}"),
        }
    }
    pub fn plan_more(self, n: usize) -> String {
        match self {
            // **`,` rather than an em dash.** `—` is East Asian Ambiguous, and a row that shifts
            // by a column on a CJK terminal cannot be repaired by redrawing it.
            Lang::Ko => format!("↓ {n}줄 더, 전체는 위 대화에 있습니다"),
            Lang::En => format!("↓ {n} more lines, the whole plan is in the conversation above"),
        }
    }
    /// What Enter on an empty draft sends. **The agent reads this as the decision**, so it has to
    /// say approval and nothing else — attacca tells it the message is the decision, and anything
    /// hedged reads as changes.
    pub fn plan_approved(self) -> &'static str {
        self.pick("승인합니다. 계획대로 진행해 주세요.", "Approved, go ahead with the plan.")
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
            Lang::Ko => format!("쓰레드  ∙  {project}"),
            Lang::En => format!("Threads  ∙  {project}"),
        }
    }
    pub fn agents(self) -> &'static str {
        self.pick("에이전트", "Agents")
    }
    pub fn commands(self) -> &'static str {
        self.pick("명령", "Commands")
    }
    /// Title of the list `@` opens.
    pub fn files(self) -> &'static str {
        self.pick("파일", "Files")
    }
    /// Title of the list `Ctrl+R` opens.
    pub fn history(self) -> &'static str {
        self.pick("보낸 말", "History")
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
            "Tab 다음 칸 ∙ Enter 만들기 ∙ Esc 닫기",
            "Tab next field ∙ Enter create ∙ Esc close",
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
                 여기서 여는 thread∙job∙work는 이 프로젝트의 것이 됩니다."
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

    // ── Usage (bottom bar)
    pub fn credits(self) -> &'static str {
        self.pick("크레딧", "Credits")
    }
    pub fn context(self) -> &'static str {
        self.pick("컨텍스트", "Context")
    }
    pub fn total_tokens(self) -> &'static str {
        self.pick("총 토큰", "Tokens")
    }

    // ── Question screen
    pub fn type_your_own(self) -> &'static str {
        self.pick("✎ 직접 입력", "✎ Type your own")
    }
    pub fn type_here(self) -> &'static str {
        self.pick("여기에 직접 적으세요 (Enter로 확정)", "Type here (Enter to confirm)")
    }
    pub fn typing_keys(self) -> &'static str {
        self.pick("Enter 입력 끝 ∙ Esc 취소", "Enter to finish ∙ Esc to cancel")
    }
    pub fn choosing_keys(self) -> &'static str {
        self.pick(
            "↑↓ 이동 ∙ Enter 고르기/실행 ∙ 클릭도 됨 ∙ Esc 접기",
            "↑↓ move ∙ Enter choose/run ∙ click works too ∙ Esc folds",
        )
    }
    pub fn review_keys(self) -> &'static str {
        self.pick("↑↓ 이동 ∙ Enter 실행 ∙ 클릭도 됨", "↑↓ move ∙ Enter runs ∙ click works too")
    }
    pub fn answered(self) -> &'static str {
        self.pick("답한 내용", "Your answer")
    }
    pub fn skipped(self) -> &'static str {
        self.pick("건너뜀", "skipped")
    }

    // ── Enrollment code window
    pub fn enroll_title(self) -> &'static str {
        self.pick("Attacca 연결", "Connect to Attacca")
    }
    pub fn enroll_steps(self) -> &'static str {
        self.pick(
            "아래 코드를 복사해 이 주소에서 승인해 주세요 (Ctrl+클릭으로 열립니다):",
            "Copy the code below and approve it at this address (Ctrl+click opens it):",
        )
    }
    /// **Said where the code is, because that is where it is decided.** Approving hands this
    /// computer's files and shell to whichever account approved — so whose account it is matters
    /// more here than anywhere else in the app, and afterwards there is nothing to warn about.
    pub fn enroll_warning(self) -> &'static str {
        self.pick(
            "다른 사람의 계정과 연결하지 마세요. 승인한 계정이 이 컴퓨터의 파일과 셸을 쓰게 됩니다.",
            "Do not connect this to someone else's account. Whoever approves it gets to read and \
             change this computer's files and run commands on it.",
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

    // ── The confirmation shown when the language is changed
    pub fn lang_changed(self) -> &'static str {
        self.pick(
            "언어를 한국어로 바꿨습니다. 다음에 켤 때도 이대로입니다.",
            "Interface language is now English. It stays this way next time.",
        )
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
    /// The screen never came up — the shell notice is all the person gets. `main` says this and
    /// exits instead of sitting on the waiting line with a frozen cursor (the 2026-08-07 report).
    pub fn screen_failed(self, why: &str) -> String {
        match self {
            Lang::Ko => format!("화면을 띄우지 못했습니다: {why}"),
            Lang::En => format!("Could not start the screen: {why}"),
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
    /// **This window has just taken the node from another one** (`conn::claim_instance_lock`). Said
    /// because the other window is about to go quiet, and because the enrollment-code window may be
    /// over there — "connected elsewhere" would send the person looking in the wrong place.
    pub fn another_window_notice(self) -> &'static str {
        self.pick(
            "다른 zyris-code 창이 같은 자격으로 붙어 있었습니다. 이 창이 노드를 넘겨받았고, 그 창은 잠시 뒤 물러납니다. 등록 코드 창이 그쪽에 떠 있을 수 있습니다.",
            "Another zyris-code window was attached with the same credential. This window has taken the node over, and that one stands by shortly. The enrollment-code window may be there.",
        )
    }
    /// **This window has just given the node up to a later one**, so it is not reconnecting — it is
    /// waiting for that window to end. Said once per hand-over, with the one thing the person might
    /// want instead: taking the node back now (`/reconnect` writes this window's pid into the slot,
    /// and the other window stands by in turn).
    pub fn stood_by_notice(self) -> &'static str {
        self.pick(
            "다른 창이 이 노드를 가져갔습니다. 그 창이 끝나면 자동으로 다시 붙습니다 ∙ /reconnect 로 지금 가져올 수 있습니다.",
            "Another window has taken this node. This one reattaches by itself once that window ends ∙ /reconnect takes it back now.",
        )
    }
    /// A window that took a slot of its own. **Said because the first launch of a slot asks for
    /// approval again** — an enrollment window appearing for no visible reason reads as the app
    /// having logged itself out.
    pub fn window_slot_notice(self, slot: usize) -> String {
        match self {
            Lang::Ko => format!(
                "다른 창이 이미 붙어 있어 이 창은 {slot}번 노드로 따로 등록합니다. \
                 처음 한 번만 승인이 필요하고, 그 뒤로는 이 창이 자기 도구 호출만 받습니다."
            ),
            Lang::En => format!(
                "Another window is already attached, so this one registers as node {slot} of its \
                 own. It asks for approval once; after that this window only gets its own tool calls."
            ),
        }
    }

    // ── Commands (`/clear` · `/cwd` · `/config` · `/agent` · `/undo` · `/changes`)
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
                 이 노드는 **{node}**로 등록돼 있습니다 ‒ 도구 이름은 `zyris__{slug}__…`입니다. \
                 `ZYRIS_NODE_NAME`으로 바꿉니다.\n\n\
                 자격은 `{cred}`에 있습니다.",
                cwd.display(),
            ),
            Lang::En => format!(
                "Tools run in `{}`.\n\n\
                 This node is registered as **{node}** ‒ tool names are `zyris__{slug}__…`. \
                 Change it with `ZYRIS_NODE_NAME`.\n\n\
                 Credentials live in `{cred}`.",
                cwd.display(),
            ),
        }
    }
    // ── Commands (`/account`)
    /// What `/account` prints — who the connection is: name, email, id, billing, scopes.
    /// `plan` and `credits` are absent on deployments that don't meter.
    pub fn account_text(
        self,
        name: &str,
        email: &str,
        user_id: &str,
        plan: Option<&str>,
        credits: Option<&str>,
        scopes: &[String],
    ) -> String {
        let scopes = if scopes.is_empty() {
            match self {
                Lang::Ko => "없음".into(),
                Lang::En => "none".into(),
            }
        } else {
            scopes.join(", ")
        };
        match self {
            Lang::Ko => format!(
                "**{name}** ({email})\n\n\
                 아이디: `{user_id}`\n\
                 {plan_line}\
                 {credits_line}\
                 부여된 권한: {scopes}\n\n\
                 `/account logout` ‒ 이 기기에서 로그아웃합니다. \
                 다음 실행 때 다시 승인을 받습니다.",
                plan_line = plan.map(|p| format!("플랜: {p}\n")).unwrap_or_default(),
                credits_line = credits.map(|c| format!("크레딧: {c}\n")).unwrap_or_default(),
            ),
            Lang::En => format!(
                "**{name}** ({email})\n\n\
                 User ID: `{user_id}`\n\
                 {plan_line}\
                 {credits_line}\
                 Granted scopes: {scopes}\n\n\
                 `/account logout` ‒ log out on this device. The next launch asks for approval again.",
                plan_line = plan.map(|p| format!("Plan: {p}\n")).unwrap_or_default(),
                credits_line = credits.map(|c| format!("Credits: {c}\n")).unwrap_or_default(),
            ),
        }
    }
    /// What to say when `/account` couldn't reach the server.
    pub fn account_error(self, why: &str) -> String {
        match self {
            Lang::Ko => format!("계정 정보를 가져오지 못했습니다: {why}"),
            Lang::En => format!("Could not fetch the account info: {why}"),
        }
    }
    /// What `/account logout` says when the credentials were discarded.
    ///
    /// **The connection currently running is left alone** (`enroll::Reauth::discard_once`) —
    /// the credentials are empty, so the next launch asks cleanly.
    pub fn account_logged_out(self) -> &'static str {
        self.pick(
            "로그아웃했습니다. 자격을 지우고 연결을 끊었습니다 ‒ 잠시 뒤 등록 코드 창이 뜹니다.",
            "Logged out. The credentials are cleared and the connection dropped ‒ the enrolment \
             code appears in a moment.",
        )
    }
    /// What `/account logout` says when there is nothing to discard — a token given directly
    /// (env/file) is not ours to drop, and there is no one to ask again.
    pub fn account_logout_nothing(self) -> &'static str {
        self.pick(
            "로그아웃할 자격이 없습니다 ‒ 토큰을 직접 줘서 로그인했습니다.",
            "Nothing to log out ‒ the token was given directly, not enrolled.",
        )
    }
    /// What `/account logout` says when the credentials could not be cleared — already
    /// discarded this process, or the file refused to be removed.
    pub fn account_logout_failed(self) -> &'static str {
        self.pick(
            "자격을 지우지 못했습니다 ‒ 이미 처리했거나 파일을 지울 수 없었습니다.",
            "Could not clear the credentials ‒ already done, or the file could not be removed.",
        )
    }
    /// What `/status` shows — the current session's picture. **Every line earns its place:**
    /// the thread id is how the server names this conversation, the project decides where new
    /// work lands, the agent answers who is listening, and the usage numbers tell how much
    /// this thread has cost so far.
    pub fn status_text(self, info: &StatusInfo) -> String {
        let StatusInfo { session_id, project, agent, mode, cwd, usage, pending } = info;
        let mut s = String::new();

        let thread = match session_id {
            Some(id) => format!("**thread** `{id}`"),
            None => match self {
                Lang::Ko => "**thread** 아직 없음 ‒ 첫 메시지에서 만들어집니다".to_string(),
                Lang::En => "**thread** none yet ‒ your first message creates it".to_string(),
            },
        };
        let project = match project {
            Some(p) => match self {
                Lang::Ko => format!("프로젝트 **{p}**"),
                Lang::En => format!("project **{p}**"),
            },
            None => match self {
                Lang::Ko => "프로젝트 기본 (안 고름)".to_string(),
                Lang::En => "project default (not picked)".to_string(),
            },
        };
        s.push_str(&format!("{thread} ∙ {project}\n"));
        s.push_str(&match self {
            Lang::Ko => format!("에이전트 **{agent}** ∙ 모드 **{mode}**\n"),
            Lang::En => format!("agent **{agent}** ∙ mode **{mode}**\n"),
        });

        if let Some(model) = &usage.model {
            s.push_str(&match self {
                Lang::Ko => format!("모델 {model}\n"),
                Lang::En => format!("model {model}\n"),
            });
        }

        // The same numbers the bottom bar shows — one picture, two places to read it.
        let mut segs: Vec<String> = Vec::new();
        if let Some(credits) = &usage.credits_used {
            segs.push(format!("{} {credits}", self.credits()));
        }
        if let Some(used) = usage.context_tokens {
            let text = match crate::usage::context_limit(usage.model.as_deref()) {
                Some(max) => {
                    let pct = if max > 0 { used.saturating_mul(100) / max } else { 0 };
                    format!(
                        "{}% ({}/{})",
                        pct,
                        crate::usage::compact(used),
                        crate::usage::compact(max)
                    )
                }
                None => crate::usage::compact(used),
            };
            segs.push(format!("{} {text}", self.context()));
        }
        if let Some(tokens) = usage.total_tokens {
            segs.push(format!("{} {}", self.total_tokens(), crate::usage::compact(tokens)));
        }
        if !segs.is_empty() {
            s.push_str(&format!("\n{}\n", segs.join(" ∙ ")));
        }

        s.push_str(&match self {
            Lang::Ko => format!("도구는 `{}`에서 돕니다.\n", cwd.display()),
            Lang::En => format!("Tools run in `{}`.\n", cwd.display()),
        });

        // Say in advance where the next message goes — work·job open something new.
        match pending {
            Some(Route::Work) => s.push_str(match self {
                Lang::Ko => "다음 메시지가 **새 work**를 엽니다.\n",
                Lang::En => "Your next message opens a **new work**.\n",
            }),
            Some(Route::Job) => s.push_str(match self {
                Lang::Ko => "다음 메시지가 **새 job**을 엽니다.\n",
                Lang::En => "Your next message opens a **new job**.\n",
            }),
            _ => {}
        }
        s
    }
    pub fn agent_staged(self, name: &str) -> String {
        match self {
            Lang::Ko => format!(
                "에이전트: **{name}** ∙ 다음 메시지에서 새 thread가 열립니다. \
                 앞 thread는 ←의 목록에 그대로 있습니다."
            ),
            Lang::En => format!(
                "Agent: **{name}** ∙ a new thread opens with your next message. \
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
        self.pick("에이전트를 찾지 못해 보낼 수 없습니다.", "No agent ‒ can't send.")
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
            // That a file was created comes before +N — undoing it means deleting it,
            // so it carries a different weight.
            let note = if c.created {
                self.pick(" ∙ 새로 만든 것", " ∙ created").to_string()
            } else if c.edits > 1 {
                match self {
                    Lang::Ko => format!(" ∙ {}번 고침", c.edits),
                    Lang::En => format!(" ∙ edited {} times", c.edits),
                }
            } else {
                String::new()
            };
            s.push_str(&format!("\n- `{}`  +{} −{}{note}", shown.display(), c.added, c.removed));
        }
        // **Say it matches the range that can be undone.** People press `/undo` after
        // seeing this list, so a mismatch is only discovered by pressing it.
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
                    Lang::Ko => format!("\n- `{name}` ‒ 도구 {n}개"),
                    Lang::En => format!("\n- `{name}` ‒ {n} tools"),
                },
                Err(why) => match self {
                    Lang::Ko => format!("\n- `{name}` ‒ 못 띄웠습니다: {why}"),
                    Lang::En => format!("\n- `{name}` ‒ couldn't start it: {why}"),
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
                Lang::Ko => format!("\n- `{}` ‒ {count}자", f.path.display()),
                Lang::En => format!("\n- `{}` ‒ {count} chars", f.path.display()),
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
            s.push_str(&format!("\n- **{}** ‒ {}", skill.name, skill.description));
        }
        s
    }

    // ── Plugin (`/plugin`)
    pub fn plugin_added(self, p: &Plugin, contents: &str) -> String {
        match self {
            Lang::Ko => format!(
                "**{}**을 받았습니다.{}\n\n{contents}\n\n\
                 다음에 zyris-code를 다시 띄우면 붙습니다 ‒ 도구도 스킬도 시작할 때 읽습니다.",
                p.name,
                if p.description.is_empty() { String::new() } else { format!(" {}", p.description) },
            ),
            Lang::En => format!(
                "Installed **{}**.{}\n\n{contents}\n\n\
                 It attaches on the next launch of zyris-code ‒ tools and skills are read at startup.",
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
                    Lang::Ko => format!("\n- `{name}` ‒ 이미 최신입니다"),
                    Lang::En => format!("\n- `{name}` ‒ already up to date"),
                },
                Ok(_) => match self {
                    Lang::Ko => format!("\n- `{name}` ‒ 새로 받았습니다"),
                    Lang::En => format!("\n- `{name}` ‒ fetched the update"),
                },
                Err(why) => format!("\n- `{name}` ‒ {why}"),
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
                "`/plugin {why}`\n\n             - `/plugin` ‒ 받아 둔 것 보기\n\
                 \x20            - `/plugin add owner/repo` ‒ 받기\n\
                 \x20            - `/plugin remove 이름` ‒ 지우기\n\
                 \x20            - `/plugin update [이름]` ‒ 갱신\n\
                 \x20            - `/plugin on|off 이름` ‒ 프로젝트 플러그인 켜기·끄기"
            ),
            Lang::En => format!(
                "`/plugin {why}`\n\n             - `/plugin` ‒ list what's fetched\n\
                 \x20            - `/plugin add owner/repo` ‒ install\n\
                 \x20            - `/plugin remove name` ‒ remove\n\
                 \x20            - `/plugin update [name]` ‒ update\n\
                 \x20            - `/plugin on|off name` ‒ switch a project plugin"
            ),
        }
    }
    pub fn plugin_project_state(self, on: bool) -> &'static str {
        match (self, on) {
            (Lang::Ko, true) => " (프로젝트 ∙ 다음 실행에 켬)",
            (Lang::Ko, false) => " (프로젝트 ∙ 꺼짐)",
            (Lang::En, true) => " (project ∙ on next launch)",
            (Lang::En, false) => " (project ∙ off)",
        }
    }
    pub fn plugin_switched(self, name: &str, on: bool) -> String {
        match (self, on) {
            (Lang::Ko, true) => format!("`{name}`을 켰습니다. 다시 띄우면 적용됩니다."),
            (Lang::Ko, false) => format!("`{name}`을 껐습니다. 다시 띄우면 빠집니다."),
            (Lang::En, true) => format!("`{name}` is on. Restart to apply it."),
            (Lang::En, false) => format!("`{name}` is off. Restart and it goes."),
        }
    }
    pub fn plugin_already(self, name: &str, on: bool) -> String {
        match (self, on) {
            (Lang::Ko, true) => format!("`{name}`은 이미 켜져 있습니다."),
            (Lang::Ko, false) => format!("`{name}`은 이미 꺼져 있습니다."),
            (Lang::En, true) => format!("`{name}` is already on."),
            (Lang::En, false) => format!("`{name}` is already off."),
        }
    }
    pub fn plugin_switch_not_found(self, name: &str) -> String {
        match self {
            Lang::Ko => format!(
                "`{name}` 프로젝트 플러그인을 찾지 못했습니다. `/plugin`으로 확인해 주세요."
            ),
            Lang::En => format!("Project plugin `{name}` was not found. Check `/plugin`."),
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
    /// A plugin command whose file had no prompt in it. **Said rather than sending nothing** — an
    /// Enter that produces no message reads as the app having missed the key.
    pub fn plugin_command_empty(self, name: &str) -> String {
        match self {
            Lang::Ko => {
                format!("`/{name}`에는 보낼 내용이 없습니다. 플러그인의 명령 파일이 비어 있습니다.")
            }
            Lang::En => {
                format!("`/{name}` has nothing to send ‒ the plugin's command file is empty.")
            }
        }
    }
    // ── GitHub screen (`/github`)
    pub fn github_form_title(self) -> &'static str {
        self.pick("GitHub", "GitHub")
    }
    pub fn github_row_user(self) -> &'static str {
        self.pick("내 계정", "My account")
    }
    /// **The reviewer row is named for what it does, not for what it holds.** "Token" would say
    /// how it is filled in; this says why it exists.
    pub fn github_row_reviewer(self) -> &'static str {
        self.pick("리뷰 계정", "Reviews as")
    }
    pub fn github_not_connected(self) -> &'static str {
        self.pick("이어져 있지 않음", "not connected")
    }
    pub fn github_paste_token(self) -> &'static str {
        self.pick("토큰을 붙여넣으세요", "paste a token")
    }
    pub fn github_enter_browser(self) -> &'static str {
        self.pick("Enter ‒ 브라우저로 잇습니다", "Enter ‒ connect through the browser")
    }
    pub fn github_enter_disconnect(self) -> &'static str {
        self.pick("Enter ‒ 끊습니다", "Enter ‒ disconnect")
    }
    /// **Says which kind of token and why.** A fine-grained token can be limited to one repository
    /// and to pull requests alone; the browser route hands out the same broad access the person has.
    pub fn github_reviewer_help(self) -> &'static str {
        self.pick(
            "fine-grained 토큰을 붙여넣고 Enter. 리포 하나 ∙ Pull requests 쓰기만 주면 됩니다.",
            "Paste a fine-grained token and press Enter. One repository, Pull requests: write, is enough.",
        )
    }
    pub fn github_row_signing(self) -> &'static str {
        self.pick("커밋 서명", "Signing")
    }
    pub fn github_signing_off(self) -> &'static str {
        self.pick("꺼져 있음", "off")
    }
    /// **Says what it costs before it happens.** Approving this hands over the right to add keys to
    /// the account, and the address commits carry changes — both are worth knowing beforehand.
    pub fn github_enter_sign(self) -> &'static str {
        self.pick(
            "Enter로 서명 키를 만듭니다. 권한을 하나 더 승인해야 하고, 이 앱이 만드는 커밋은 GitHub noreply 주소로 남습니다.",
            "Enter makes a signing key. It asks for one more permission, and commits made here will carry your GitHub noreply address.",
        )
    }
    pub fn github_enter_stop_signing(self) -> &'static str {
        self.pick(
            "Enter로 서명을 끕니다. 키는 GitHub 계정에 그대로 남습니다.",
            "Enter stops signing. The key stays on your GitHub account.",
        )
    }
    pub fn github_sign_needs_an_account(self) -> &'static str {
        self.pick("먼저 위에서 GitHub에 이어야 합니다.", "Connect a GitHub account above first.")
    }
    /// What the signing set-up is doing right now. **Each step is named** — making a key can take
    /// a while on a machine with little entropy, and silence there reads as a hang.
    pub fn github_signing_step(self, step: &str) -> String {
        match self {
            Lang::Ko => format!("서명 설정: {step}"),
            Lang::En => format!("Signing: {step}"),
        }
    }
    pub fn github_signing_no_gpg(self) -> &'static str {
        self.pick(
            "이 컴퓨터에 GnuPG가 없습니다. 설치한 뒤 다시 시도해 주세요 (Windows는 gpg4win.org).",
            "GnuPG is not installed on this machine. Install it and try again (Windows: gpg4win.org).",
        )
    }
    pub fn github_signing_on(self, email: &str) -> String {
        match self {
            Lang::Ko => format!("이제 커밋에 서명합니다. 작성자 주소는 {email} 입니다."),
            Lang::En => format!("Commits are signed from now on, authored as {email}."),
        }
    }
    pub fn github_signing_stopped(self) -> &'static str {
        self.pick(
            "서명을 껐습니다. 키는 GitHub 계정에 남아 있으니 필요하면 직접 지워 주세요.",
            "Signing is off. The key is still on your GitHub account ‒ remove it there if you want it gone.",
        )
    }
    pub fn github_signing_failed(self, why: &str) -> String {
        match self {
            Lang::Ko => format!("서명 설정에 실패했습니다: {why}"),
            Lang::En => format!("Could not set signing up: {why}"),
        }
    }
    /// While a code is waiting. **Says to open it, not just that something is happening** — the
    /// address right above is the only thing that moves this along.
    pub fn github_approve_it(self) -> &'static str {
        self.pick(
            "위 주소를 열고(Ctrl+클릭) 이 코드를 넣어 승인해 주세요. 승인하면 저절로 이어집니다.",
            "Open the address above (Ctrl+click), enter this code and approve. It connects by itself.",
        )
    }
    pub fn github_working(self) -> &'static str {
        self.pick("GitHub에 물어보는 중…", "asking GitHub…")
    }
    pub fn github_form_keys(self) -> &'static str {
        self.pick(
            "↑↓ 이동 ∙ Enter 실행 ∙ Ctrl+U 지우기 ∙ Esc 닫기",
            "↑↓ move ∙ Enter ∙ Ctrl+U clear ∙ Esc close",
        )
    }

    // ── GitHub (`/github`)
    /// Both slots. **The reviewer is named even when there isn't one** — which account a review
    /// goes out under is the whole point of having two, and silence there reads as "the same one".
    pub fn github_signed_in(self, login: &str, reviewer: Option<&str>) -> String {
        match (self, reviewer) {
            (Lang::Ko, Some(r)) => format!(
                "GitHub에 `{login}`으로 이어져 있습니다.\n리뷰는 `{r}` 이름으로 나갑니다.\n\n\
                 `/github logout`∙`/github logout reviewer`로 끊습니다."
            ),
            (Lang::Ko, None) => format!(
                "GitHub에 `{login}`으로 이어져 있습니다.\n리뷰어를 따로 잇지 않아 리뷰도 \
                 `{login}` 이름으로 나갑니다 ‒ 자기 PR은 승인할 수 없습니다.\n\n\
                 `/github login reviewer`로 리뷰 전용 계정을 잇습니다."
            ),
            (Lang::En, Some(r)) => format!(
                "Connected to GitHub as `{login}`.\nReviews go out as `{r}`.\n\n\
                 `/github logout` and `/github logout reviewer` disconnect."
            ),
            (Lang::En, None) => format!(
                "Connected to GitHub as `{login}`.\nNo separate reviewer is connected, so reviews \
                 go out as `{login}` too ‒ and nobody can approve their own pull request.\n\n\
                 `/github login reviewer` connects an account just for reviews."
            ),
        }
    }
    pub fn github_signed_out(self) -> &'static str {
        self.pick(
            "GitHub에 이어져 있지 않습니다. `/github login`으로 잇습니다.",
            "Not connected to GitHub. `/github login` connects it.",
        )
    }
    /// **A build with no OAuth app registered.** Saying "not signed in" here would send someone
    /// looking for a browser page that is never going to appear.
    pub fn github_no_app(self) -> &'static str {
        self.pick(
            "이 빌드에는 GitHub 앱이 등록돼 있지 않아 로그인할 수 없습니다. \
             GitHub에서 OAuth App을 만들고 `ZYRIS_CODE_GITHUB_CLIENT_ID`에 client id를 주세요.",
            "This build has no GitHub app registered, so there is nothing to log in to. Create an \
             OAuth App on GitHub and give its client id as `ZYRIS_CODE_GITHUB_CLIENT_ID`.",
        )
    }
    /// The code to type, and where. **Both, together** — a code with nowhere to put it is no use.
    pub fn github_code(self, code: &str, url: &str, role: crate::github::auth::Role) -> String {
        use crate::github::auth::Role;
        // **The reviewer line says to use a logged-out window.** Approving in the everyday browser
        // signs in as yourself, which is the one mistake this arrangement exists to prevent — and
        // it looks exactly like it worked.
        let head = match (self, role) {
            (Lang::Ko, Role::User) => "이 창을 GitHub 계정에 잇습니다.",
            (Lang::Ko, Role::Reviewer) => {
                "리뷰 전용 계정을 잇습니다. **시크릿 창에서 그 계정으로 로그인한 뒤** 승인해 주세요 \
                 ‒ 평소 브라우저로 하면 본인 계정으로 이어집니다."
            }
            (Lang::En, Role::User) => "Connecting this window to a GitHub account.",
            (Lang::En, Role::Reviewer) => {
                "Connecting the account reviews go out under. **Approve from a private window \
                 signed in as that account** ‒ your everyday browser would connect you instead."
            }
        };
        match self {
            Lang::Ko => {
                format!("{head}\n\n[{url}]({url}) 을 열고 이 코드를 넣어 주세요:\n\n**{code}**")
            }
            Lang::En => format!("{head}\n\nOpen [{url}]({url}) and enter this code:\n\n**{code}**"),
        }
    }
    pub fn github_logged_in(self, login: &str, role: crate::github::auth::Role) -> String {
        use crate::github::auth::Role;
        let who = if login.is_empty() { "GitHub" } else { login };
        match (self, role) {
            (Lang::Ko, Role::User) => format!("GitHub에 `{who}`으로 이었습니다."),
            (Lang::Ko, Role::Reviewer) => format!("리뷰는 이제 `{who}` 이름으로 나갑니다."),
            (Lang::En, Role::User) => format!("Connected to GitHub as `{who}`."),
            (Lang::En, Role::Reviewer) => format!("Reviews now go out as `{who}`."),
        }
    }
    /// **Says the token is not revoked at GitHub.** Device flow has no secret to revoke with, and
    /// leaving that unsaid would let someone think a live token had been destroyed.
    pub fn github_logged_out(self, role: crate::github::auth::Role) -> String {
        use crate::github::auth::Role;
        let revoke = self.pick(
            "GitHub 쪽 권한은 남아 있으니 [github.com/settings/applications](https://github.com/settings/applications) 에서 직접 해제해 주세요.",
            "The authorisation still stands on GitHub ‒ revoke it at [github.com/settings/applications](https://github.com/settings/applications).",
        );
        let head = match (self, role) {
            (Lang::Ko, Role::User) => "GitHub 자격을 지웠습니다.",
            (Lang::Ko, Role::Reviewer) => {
                "리뷰어 자격을 지웠습니다. 이제 리뷰도 본인 계정으로 나갑니다."
            }
            (Lang::En, Role::User) => "The GitHub credential is gone from this machine.",
            (Lang::En, Role::Reviewer) => {
                "The reviewer credential is gone; reviews go out as you again."
            }
        };
        format!("{head} {revoke}")
    }
    pub fn github_nothing_to_log_out(self) -> &'static str {
        self.pick("이어져 있는 GitHub 계정이 없습니다.", "No GitHub account is connected.")
    }
    /// A pasted token GitHub would not accept. **Checked before it is kept** — a bad token has to
    /// fail here, where it can be pasted again.
    pub fn github_token_refused(self, why: &str) -> String {
        match self {
            Lang::Ko => format!("이 토큰은 GitHub이 받지 않습니다: {why}"),
            Lang::En => format!("GitHub would not take that token: {why}"),
        }
    }
    pub fn github_login_failed(self, why: &str) -> String {
        match self {
            Lang::Ko => format!("GitHub에 잇지 못했습니다: {why}"),
            Lang::En => format!("Could not connect to GitHub: {why}"),
        }
    }
    pub fn plugin_where_title(self) -> &'static str {
        self.pick("어디에 받을까요?", "Where should it go?")
    }
    pub fn plugin_where_machine(self) -> &'static str {
        self.pick("이 컴퓨터", "This machine")
    }
    pub fn plugin_where_machine_note(self) -> &'static str {
        self.pick("모든 프로젝트에서 씁니다", "Used from every project")
    }
    pub fn plugin_where_project(self) -> &'static str {
        self.pick("이 프로젝트", "This project")
    }
    /// **Says it lands in the repo.** A fetched plugin inside the working directory shows up in
    /// `git status`, and finding out at commit time is the wrong time.
    pub fn plugin_where_project_note(self) -> &'static str {
        self.pick(
            "`.zyris-code/plugins/`에 받습니다 ‒ git에 잡힙니다",
            "Into `.zyris-code/plugins/` ‒ it will show up in git",
        )
    }
    pub fn plugin_contents_text(self, p: &Plugin) -> String {
        let mut s = String::new();
        for spec in &p.mcp {
            s.push_str(&match self {
                Lang::Ko => {
                    format!(
                        "- MCP `{}` ‒ 다음 실행 때 `{}`을 씁니다\n",
                        spec.slug,
                        spec.transport.summary()
                    )
                }
                Lang::En => {
                    format!(
                        "- MCP `{}` ‒ uses `{}` on the next launch\n",
                        spec.slug,
                        spec.transport.summary()
                    )
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
                    format!(" ‒ {}", p.description)
                },
            ));
        }
        s
    }

    // ── Popup panels (/mode · /mcp · /skills · /plugin · /account · /status)
    /// The hint line at the bottom of every panel.
    pub fn panel_keys(self) -> String {
        match self {
            Lang::Ko => "↑↓ 스크롤 ∙ Esc 닫기".into(),
            Lang::En => "↑↓ scroll ∙ Esc close".into(),
        }
    }
    /// A missing value in a panel — a dash, never an invented number.
    pub fn panel_dash(self) -> &'static str {
        self.pick("‒", "‒")
    }
    /// Panel titles.
    pub fn title_mode(self) -> &'static str {
        self.pick("모드", "Mode")
    }
    pub fn title_mcp(self) -> &'static str {
        self.pick("MCP 서버", "MCP servers")
    }
    pub fn title_skills(self) -> &'static str {
        self.pick("스킬", "Skills")
    }
    pub fn title_plugins(self) -> &'static str {
        self.pick("플러그인", "Plugins")
    }
    pub fn title_account(self) -> &'static str {
        self.pick("계정", "Account")
    }
    pub fn title_status(self) -> &'static str {
        self.pick("상태", "Status")
    }
    pub fn title_config(self) -> &'static str {
        self.pick("설정", "Settings")
    }

    // ── Config panel
    /// The row label for the directory-access policy.
    pub fn cfg_dir_access(self) -> &'static str {
        self.pick("다른 디렉토리 접근", "Directory access")
    }
    pub fn cfg_dir_allow(self) -> &'static str {
        self.pick("허용", "allow")
    }
    pub fn cfg_dir_deny(self) -> &'static str {
        self.pick("거부", "deny")
    }
    /// The row label for the default-mode setting.
    pub fn cfg_default_mode(self) -> &'static str {
        self.pick("기본 모드", "Default mode")
    }
    /// The "unset" value of the default-mode row — the app then opens in normal.
    pub fn cfg_off(self) -> &'static str {
        self.pick("안 씀", "off")
    }
    /// The row label for the screen language. The setting itself lives with `lang.rs`.
    pub fn cfg_language(self) -> &'static str {
        self.pick("언어", "Language")
    }
    /// The row label for the palette.
    pub fn cfg_update(self) -> &'static str {
        self.pick("업데이트", "Updates")
    }
    /// The name of an update policy.
    pub fn cfg_update_name(self, p: crate::update::Policy) -> &'static str {
        use crate::update::Policy::{Auto, Notify, Off};
        match (self, p) {
            (Lang::Ko, Auto) => "자동",
            (Lang::Ko, Notify) => "알림",
            (Lang::Ko, Off) => "끔",
            (Lang::En, Auto) => "auto",
            (Lang::En, Notify) => "notify",
            (Lang::En, Off) => "off",
        }
    }
    pub fn cfg_update_desc(self, p: crate::update::Policy) -> &'static str {
        use crate::update::Policy::{Auto, Notify, Off};
        match (self, p) {
            (Lang::Ko, Auto) => "새 버전이 나오면 받아서 설치하고 그 버전으로 다시 시작합니다.",
            (Lang::Ko, Notify) => "새 버전이 나오면 알리기만 합니다 ‒ `/update`로 설치합니다.",
            (Lang::Ko, Off) => "새 버전이 있는지 확인하지 않습니다.",
            (Lang::En, Auto) => "Install a newer release and come back on it.",
            (Lang::En, Notify) => "Say a newer release exists ‒ `/update` installs it.",
            (Lang::En, Off) => "Never look for one.",
        }
    }
    /// Said when a newer release is found and the policy is `notify`.
    pub fn update_available(self, tag: &str) -> String {
        match self {
            Lang::Ko => format!("새 버전 {tag}이(가) 나왔습니다. `/update`로 설치합니다."),
            Lang::En => format!("{tag} is out. `/update` installs it."),
        }
    }
    /// The first line of an update, printed on the terminal before the screen exists.
    ///
    /// **It names both versions.** This appears in front of somebody who typed a command and
    /// expected an app, so what is happening and why has to be readable in one line.
    pub fn update_installing_from(self, current: &str, tag: &str) -> String {
        match self {
            Lang::Ko => format!("새 버전을 설치합니다: {current} → {tag}"),
            Lang::En => format!("Updating: {current} → {tag}"),
        }
    }
    /// The last line of an update, just before the new version takes this terminal.
    pub fn update_restarting(self, tag: &str) -> String {
        match self {
            Lang::Ko => format!("{tag}으로 다시 시작합니다."),
            Lang::En => format!("Restarting on {tag}."),
        }
    }
    /// Installed, but this process could not become the new one.
    ///
    /// **Worth saying loudly.** Silence here reads as an update that did not happen, and the
    /// version number stays where it was until somebody runs the command again.
    pub fn update_installed_not_started(self, why: &str) -> String {
        match self {
            Lang::Ko => {
                format!("설치는 됐지만 다시 시작하지 못했습니다 ({why}). 다시 실행해 주세요.")
            }
            Lang::En => format!("Installed, but could not restart ({why}). Run it again."),
        }
    }
    /// Said by the process an update started, when it was asked to update again.
    ///
    /// **The version, not "already the newest".** If the install landed somewhere PATH does not
    /// reach, this is still the old one, and saying it is current would be a lie told by the very
    /// thing that failed.
    pub fn update_now_on(self, version: &str) -> String {
        match self {
            Lang::Ko => format!("지금 버전: {version}"),
            Lang::En => format!("Now on {version}."),
        }
    }
    /// Why an update asked for by name could not even begin.
    pub fn update_no_answer(self) -> &'static str {
        self.pick("GitHub에 닿지 못했습니다", "could not reach GitHub")
    }
    /// Said when `/update` was asked for and there is nothing to do.
    pub fn update_current(self) -> &'static str {
        self.pick("이미 최신입니다.", "Already the newest release.")
    }
    /// While `/update` is finding out whether there is a newer release. **Something has to be said
    /// meanwhile**: the answer can take a few seconds, and a command that appears to do nothing
    /// reads as one that did not go in.
    pub fn update_checking(self) -> &'static str {
        self.pick("새 버전을 확인하는 중…", "Looking for a newer release…")
    }
    /// Said when the release could not be looked up, or the handover could not be started.
    pub fn update_failed(self, why: &str) -> String {
        match self {
            Lang::Ko => format!("업데이트하지 못했습니다: {why}"),
            Lang::En => format!("Could not update: {why}"),
        }
    }
    pub fn cfg_theme(self) -> &'static str {
        self.pick("화면 색", "Colours")
    }
    /// The name of a palette choice.
    pub fn cfg_theme_name(self, choice: crate::config::ThemeChoice) -> &'static str {
        use crate::config::ThemeChoice::{Auto, Dark, Light};
        match (self, choice) {
            (Lang::Ko, Auto) => "자동",
            (Lang::Ko, Dark) => "어둡게",
            (Lang::Ko, Light) => "밝게",
            (Lang::En, Auto) => "auto",
            (Lang::En, Dark) => "dark",
            (Lang::En, Light) => "light",
        }
    }
    /// What picking that palette means.
    ///
    /// **This app paints no background of its own** — the terminal's own shows through — so the
    /// palette has to suit it. The dark text on a light terminal measures 1.19:1, which is words
    /// the colour of the paper.
    pub fn cfg_theme_desc(self, choice: crate::config::ThemeChoice) -> &'static str {
        use crate::config::ThemeChoice::{Auto, Dark, Light};
        match (self, choice) {
            (Lang::Ko, Auto) => "터미널에 맞춰 고릅니다 ‒ 알 수 없으면 어두운 쪽입니다.",
            (Lang::Ko, Dark) => "어두운 터미널에 맞춘 색입니다.",
            (Lang::Ko, Light) => "밝은 터미널에 맞춘 색입니다.",
            (Lang::En, Auto) => "Follows the terminal, and falls back to dark when it won't say.",
            (Lang::En, Dark) => "Colours for a dark terminal.",
            (Lang::En, Light) => "Colours for a light terminal.",
        }
    }
    /// What the directory-access value under the cursor means.
    ///
    /// **The description describes the value, not the setting.** A row label already says
    /// which setting it is; what a person cannot guess is what `allow` will actually do.
    pub fn cfg_dir_desc(self, access: crate::config::DirAccess) -> &'static str {
        use crate::config::DirAccess::{Allow, Deny};
        match (self, access) {
            (Lang::Ko, Allow) => "작업 디렉터리 밖도 묻지 않고 통과합니다.",
            (Lang::Ko, Deny) => "작업 디렉터리 밖은 묻지 않고 거부합니다.",
            (Lang::En, Allow) => "Paths outside the working directory run without asking.",
            (Lang::En, Deny) => "Paths outside the working directory are refused outright.",
        }
    }
    /// What picking this screen language means.
    pub fn cfg_lang_desc(self, choice: Lang) -> &'static str {
        match (self, choice) {
            (Lang::Ko, Lang::Ko) => "화면을 한국어로 씁니다.",
            (Lang::Ko, Lang::En) => "화면을 영어로 씁니다.",
            (Lang::En, Lang::Ko) => "Draws the screen in Korean.",
            (Lang::En, Lang::En) => "Draws the screen in English.",
        }
    }
    /// What the default-mode value means. `None` is the unset state.
    ///
    /// **This does not reuse `mode_desc`.** Those sentences carry markdown (`**일**`), and the
    /// panel draws its lines as-is — the asterisks would show up literally.
    pub fn cfg_mode_desc(self, mode: Option<Mode>) -> String {
        match (self, mode) {
            (Lang::Ko, None) => "따로 정하지 않습니다 ‒ 일반 모드로 시작합니다.".into(),
            (Lang::En, None) => "No override ‒ starts in normal.".into(),
            (Lang::Ko, Some(m)) => format!("켤 때 {} 모드로 시작합니다.", m.label(self)),
            (Lang::En, Some(m)) => format!("Starts in {} mode.", m.label(self)),
        }
    }
    /// The hint line of the settings form. **It names every key the form answers to** —
    /// a form whose keys you have to guess is a form nobody finishes.
    pub fn form_keys(self) -> String {
        match self {
            Lang::Ko => "↑↓ 항목 ∙ ←→ 값 ∙ Enter 저장 ∙ Esc 취소".into(),
            Lang::En => "↑↓ row ∙ ←→ value ∙ Enter save ∙ Esc cancel".into(),
        }
    }
    /// The hint line of the `/config` panel — everything here is settable by command too.
    pub fn config_keys(self) -> String {
        match self {
            Lang::Ko => "`/config dir allow∙deny` ∙ `/config lang ko∙en` ∙ \
                 `/config mode 일반∙계획∙일∙작업∙off`"
                .into(),
            Lang::En => "`/config dir allow|deny` ∙ `/config lang ko|en` ∙ \
                 `/config mode normal|plan|work|job|off`"
                .into(),
        }
    }
    /// Said while `/reconnect` is reattaching.
    /// Said **once** when a selection could not reach the system clipboard, because this terminal
    /// was not found to read OSC 52. Copying looks like it worked otherwise — the text is held in
    /// the app and pastes back into it — so without a word the paste into another window is where
    /// it is discovered, and by then the reason is nowhere in sight.
    pub fn copy_stayed_here(self) -> &'static str {
        self.pick(
            "복사한 글이 이 앱 안에만 있습니다 ‒ 이 터미널이 시스템 클립보드 쓰기를 안 받습니다. \
             ZYRIS_CODE_OSC52=1로 켜 보거나, ZYRIS_CODE_MOUSE=0으로 터미널이 직접 긁게 하세요.",
            "The copy stayed inside this app ‒ this terminal was not found to accept clipboard \
             writes. Try ZYRIS_CODE_OSC52=1, or ZYRIS_CODE_MOUSE=0 to let the terminal select \
             text itself.",
        )
    }
    pub fn reconnecting(self) -> &'static str {
        self.pick("다시 붙는 중…", "attaching again…")
    }
    /// `/reconnect` before there is anything to drop.
    pub fn reconnect_not_attached(self) -> &'static str {
        self.pick("아직 붙지 않았습니다.", "not attached yet.")
    }

    /// What `/config theme …` says it changed.
    pub fn config_theme_changed(self, choice: crate::config::ThemeChoice) -> String {
        match self {
            Lang::Ko => format!("화면 색을 **{}**로 바꿨습니다.", self.cfg_theme_name(choice)),
            Lang::En => format!("Colours set to **{}**.", self.cfg_theme_name(choice)),
        }
    }

    /// What `/config dir …` says it changed.
    pub fn config_dir_changed(self, access: crate::config::DirAccess) -> String {
        match (self, access) {
            (Lang::Ko, crate::config::DirAccess::Allow) => {
                "다른 디렉토리 접근을 **허용**으로 바꿨습니다. 작업 디렉터리 밖도 묻지 않고 \
                 만집니다. 다음에 켤 때도 이대로입니다."
                    .into()
            }
            (Lang::En, crate::config::DirAccess::Allow) => {
                "Directory access is now **allow** ‒ tools may touch outside the working \
                 directory without asking. It stays this way next time."
                    .into()
            }
            (Lang::Ko, crate::config::DirAccess::Deny) => {
                "다른 디렉토리 접근을 **거부**로 바꿨습니다. 작업 디렉터리 밖은 만질 수 \
                 없습니다. 다음에 켤 때도 이대로입니다."
                    .into()
            }
            (Lang::En, crate::config::DirAccess::Deny) => {
                "Directory access is now **deny** ‒ tools may not touch outside the working \
                 directory. It stays this way next time."
                    .into()
            }
        }
    }
    /// What `/config mode …` says it changed. `None` is the off state.
    pub fn config_mode_changed(self, mode: Option<Mode>) -> String {
        match self {
            Lang::Ko => match mode {
                Some(m) => {
                    format!("기본 모드: **{}**. 다음에 켜면 이 모드로 시작합니다.", m.label(self))
                }
                None => "기본 모드를 껐습니다. 다음에 켜면 **일반** 모드로 시작합니다.".into(),
            },
            Lang::En => match mode {
                Some(m) => {
                    format!(
                        "Default mode is now **{}** ‒ the app opens in it next time.",
                        m.label(self)
                    )
                }
                None => "Default mode is off ‒ the app opens in **normal** next time.".into(),
            },
        }
    }

    // ── Mode panel
    pub fn current_mode(self) -> &'static str {
        self.pick("지금 모드", "Current mode")
    }
    /// The hint line of the `/mode` panel.
    ///
    /// **It takes `↑↓` now.** The panel used to be read-only — you changed the mode with
    /// `Shift+Tab` and the panel only told you what the four were — but it draws a cursor beside
    /// one of them, so the arrows are what anybody presses.
    pub fn mode_pick_keys(self) -> &'static str {
        self.pick(
            "↑↓ 고르기 ∙ Enter 적용 ∙ Esc 닫기 ∙ Shift+Tab은 그대로 돕니다",
            "↑↓ choose ∙ Enter apply ∙ Esc close ∙ Shift+Tab still cycles",
        )
    }
    /// One line on what each mode does, for the `/mode` panel. The words match the
    /// tool-gate table in `mode.rs`.
    pub fn mode_desc(self, mode: Mode) -> &'static str {
        match mode {
            Mode::Normal => self.pick(
                "물어보지 않고 도구를 돌립니다. 기본값 ‒ 평범한 대화에 이어붙습니다.",
                "Runs tools without asking. The default ‒ carries on one plain conversation.",
            ),
            Mode::Plan => self.pick(
                "도구를 돌리지 않고, 먼저 할 일을 세웁니다.",
                "Does not run tools ‒ lays out what to do first.",
            ),
            Mode::Work => self.pick(
                "다음 말이 attacca의 **일**(work) 목표가 됩니다. 계획을 태스크로 쪼갭니다.",
                "The next message becomes a work goal. Attacca plans it into tasks.",
            ),
            Mode::Job => self.pick(
                "다음 말을 하나의 **작업**(job)으로 넘깁니다. 되묻는 것이 있어도 끝까지 해냅니다.",
                "Hands the next message off as one job that runs to the end.",
            ),
        }
    }

    // ── MCP panel
    pub fn mcp_empty(self) -> &'static str {
        self.pick(
            "붙은 MCP 서버가 없습니다. `.mcp.json`이나 `~/.config/zyris-code/mcp.json`에 적습니다.",
            "No MCP servers are attached. Write one in `.mcp.json` or `~/.config/zyris-code/mcp.json`.",
        )
    }
    pub fn mcp_tools(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("도구 {n}개"),
            Lang::En => format!("{n} tools"),
        }
    }
    pub fn mcp_failed(self, why: &str) -> String {
        match self {
            Lang::Ko => format!("못 띄웠습니다: {why}"),
            Lang::En => format!("couldn't start it: {why}"),
        }
    }
    pub fn mcp_config_hint(self) -> &'static str {
        self.pick(
            "`.mcp.json` ∙ `~/.config/zyris-code/mcp.json`에 적습니다.",
            "Write in `.mcp.json` or `~/.config/zyris-code/mcp.json`.",
        )
    }
    /// Heading over repository and external-client servers that need approval.
    pub fn mcp_found_heading(self) -> &'static str {
        self.pick("승인이 필요한 서버", "Servers requiring approval")
    }
    /// Where one was found, and whether this machine said yes to it.
    pub fn mcp_found_from(self, source: &str, on: bool) -> String {
        match (self, on) {
            (Lang::Ko, true) => format!("{source} ∙ 다음 실행에 켭니다"),
            (Lang::Ko, false) => format!("{source} ∙ 꺼져 있습니다"),
            (Lang::En, true) => format!("{source} ∙ on from the next launch"),
            (Lang::En, false) => format!("{source} ∙ off"),
        }
    }
    pub fn mcp_switch_hint(self) -> &'static str {
        self.pick(
            "`/mcp on <이름>`으로 켜고 `/mcp off <이름>`으로 끕니다. 찾은 것은 켜기 전에는 돌지 않습니다.",
            "`/mcp on <name>` turns one on, `/mcp off <name>` turns it off. Nothing found here runs until you do.",
        )
    }
    /// What `/mcp on|off` answers. **It says to restart** — servers are started once, at launch.
    pub fn mcp_switched(self, slug: &str, on: bool) -> String {
        match (self, on) {
            (Lang::Ko, true) => format!("`{slug}`을 켰습니다. 다시 띄우면 도구가 붙습니다."),
            (Lang::Ko, false) => format!("`{slug}`을 껐습니다. 다시 띄우면 빠집니다."),
            (Lang::En, true) => format!("`{slug}` is on. Restart and its tools attach."),
            (Lang::En, false) => format!("`{slug}` is off. Restart and it goes."),
        }
    }
    /// Asked to switch something that is not in the found list.
    pub fn mcp_not_found(self, slug: &str) -> String {
        match self {
            Lang::Ko => format!(
                "`{slug}`은 켜고 끌 수 있는 목록에 없습니다. `/mcp`로 이름을 확인해 주세요."
            ),
            Lang::En => {
                format!("`{slug}` is not in the switchable list. Check the name with `/mcp`.")
            }
        }
    }
    /// Nothing changed, because it already was that way.
    pub fn mcp_already(self, slug: &str, on: bool) -> String {
        match (self, on) {
            (Lang::Ko, true) => format!("`{slug}`은 이미 켜져 있습니다."),
            (Lang::Ko, false) => format!("`{slug}`은 이미 꺼져 있습니다."),
            (Lang::En, true) => format!("`{slug}` is already on."),
            (Lang::En, false) => format!("`{slug}` is already off."),
        }
    }

    // ── The managers (`/mcp` · `/plugin`)

    /// The question asked before something is taken away. **It names the row** — the cursor can
    /// move between the two presses, and "press again" with no name would be a question about
    /// whatever happens to be under the cursor when it is answered.
    pub fn manager_confirm(self, id: &str) -> String {
        match self {
            Lang::Ko => format!("`{id}`을 지우려면 한 번 더 누르세요."),
            Lang::En => format!("Press again to remove `{id}`."),
        }
    }

    /// A key that does nothing on this row, and why. **Said rather than swallowed** — a key that
    /// quietly does nothing reads as the panel being broken.
    ///
    /// One sentence for all three keys (`Enter`, `d`, `u`), because what it says is the same
    /// thing: this row does not take that act. **One sentence also keeps the box one size** —
    /// it is measured against every question the panel can ask (`panel::room_for`).
    pub fn manager_cannot(self, id: &str) -> String {
        match self {
            Lang::Ko => format!("`{id}`에는 이 키가 하는 일이 없습니다."),
            Lang::En => format!("`{id}`: that key does nothing here."),
        }
    }

    /// A server taken out of a config file.
    pub fn mcp_removed(self, slug: &str, at: &str) -> String {
        match self {
            Lang::Ko => format!("`{slug}`을 `{at}`에서 지웠습니다."),
            Lang::En => format!("Removed `{slug}` from `{at}`."),
        }
    }

    /// An approval forgotten. **The entry in the other program's file is untouched** — that file
    /// is not ours to write, and saying so is the difference between a person understanding this
    /// and believing the server was deleted.
    pub fn mcp_forgotten(self, slug: &str) -> String {
        match self {
            Lang::Ko => {
                format!(
                    "`{slug}`을 승인 목록에서 뺐습니다. 그 프로그램의 설정 파일은 그대로입니다."
                )
            }
            Lang::En => format!(
                "`{slug}` is off our list. The file that wrote it belongs to another program and \
                 was left alone."
            ),
        }
    }

    /// The key hint along the bottom of a manager. **It is the whole discovery surface for the
    /// keys** — nothing else says `d` removes and `u` updates.
    pub fn manager_keys(self, kind: crate::panel::ManagerKind) -> String {
        use crate::panel::ManagerKind;
        match (self, kind) {
            (Lang::Ko, ManagerKind::Mcp) => {
                "↑↓ 고르기 ‒ Enter 켜기/끄기 ‒ a 추가 ‒ d 지우기 ‒ r 다시 읽기 ‒ Esc 닫기"
                    .to_string()
            }
            (Lang::En, ManagerKind::Mcp) => {
                "↑↓ pick ‒ Enter on/off ‒ a add ‒ d remove ‒ r re-read ‒ Esc close".to_string()
            }
            (Lang::Ko, ManagerKind::Plugins) => {
                "↑↓ 고르기 ‒ Enter 켜기/끄기 ‒ a 받기 ‒ u 갱신 ‒ d 지우기 ‒ r 다시 읽기 ‒ Esc 닫기"
                    .to_string()
            }
            (Lang::En, ManagerKind::Plugins) => {
                "↑↓ pick ‒ Enter on/off ‒ u update ‒ d remove ‒ r re-read ‒ Esc close".to_string()
            }
        }
    }

    /// The detail block's labels. **Short, because they are a column** — the value is the sentence.
    pub fn d_state(self) -> &'static str {
        self.pick("상태", "state")
    }
    pub fn d_source(self) -> &'static str {
        self.pick("출처", "from")
    }
    pub fn d_runs(self) -> &'static str {
        self.pick("실행", "runs")
    }
    pub fn d_env(self) -> &'static str {
        self.pick("환경변수", "env")
    }
    pub fn d_tools(self) -> &'static str {
        self.pick("도구", "tools")
    }
    pub fn d_agent(self) -> &'static str {
        self.pick("에이전트가 부르는 이름", "called as")
    }
    pub fn d_path(self) -> &'static str {
        self.pick("자리", "path")
    }
    pub fn d_adds(self) -> &'static str {
        self.pick("주는 것", "adds")
    }
    pub fn d_about(self) -> &'static str {
        self.pick("설명", "about")
    }
    pub fn d_version(self) -> &'static str {
        self.pick("판", "version")
    }
    pub fn d_author(self) -> &'static str {
        self.pick("만든이", "author")
    }
    pub fn d_home(self) -> &'static str {
        self.pick("홈", "home")
    }
    pub fn d_repo(self) -> &'static str {
        self.pick("저장소", "repo")
    }
    pub fn d_license(self) -> &'static str {
        self.pick("라이선스", "license")
    }
    pub fn d_keywords(self) -> &'static str {
        self.pick("낱말", "keywords")
    }

    pub fn on_off(self, on: bool) -> &'static str {
        match (self, on) {
            (Lang::Ko, true) => "켜짐",
            (Lang::Ko, false) => "꺼짐",
            (Lang::En, true) => "on",
            (Lang::En, false) => "off",
        }
    }

    /// Where a server written in one of our own files came from.
    pub fn mcp_from_user(self) -> &'static str {
        self.pick("이 앱의 설정", "this app's settings")
    }
    pub fn mcp_from_project(self) -> &'static str {
        self.pick("이 저장소", "this repository")
    }
    pub fn mcp_from_plugin(self, name: &str) -> String {
        match self {
            Lang::Ko => format!("플러그인 `{name}`"),
            Lang::En => format!("the plugin `{name}`"),
        }
    }
    /// A server that is running, and what it brought.
    pub fn mcp_row_running(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("돌고 있습니다 ‒ 도구 {n}개"),
            Lang::En => format!("running ‒ {n} tools"),
        }
    }
    /// Nothing was switched off: it is written down here, so it starts itself.
    pub fn mcp_row_always_on(self) -> &'static str {
        self.pick("적혀 있어서 스스로 뜹니다", "written down, so it starts itself")
    }

    /// A plugin row's sentence about where it came from.
    pub fn plugin_row_fetched(self) -> &'static str {
        self.pick("받아 둔 것", "fetched")
    }
    pub fn plugin_row_project(self, on: bool) -> String {
        match (self, on) {
            (Lang::Ko, true) => "이 저장소 ‒ 켜짐".to_string(),
            (Lang::Ko, false) => "이 저장소 ‒ 꺼짐 (승인 필요)".to_string(),
            (Lang::En, true) => "this repository ‒ on".to_string(),
            (Lang::En, false) => "this repository ‒ off (needs approval)".to_string(),
        }
    }
    /// What a plugin contributes, as one line of counts.
    pub fn plugin_adds_line(
        self,
        commands: usize,
        skills: usize,
        hooks: usize,
        mcp: usize,
    ) -> String {
        let mut parts: Vec<String> = Vec::new();
        if commands > 0 {
            parts.push(match self {
                Lang::Ko => format!("명령 {commands}개"),
                Lang::En => format!("{commands} commands"),
            });
        }
        if skills > 0 {
            parts.push(match self {
                Lang::Ko => format!("스킬 {skills}개"),
                Lang::En => format!("{skills} skills"),
            });
        }
        if mcp > 0 {
            parts.push(match self {
                Lang::Ko => format!("MCP {mcp}개"),
                Lang::En => format!("{mcp} MCP servers"),
            });
        }
        if hooks > 0 {
            parts.push(match self {
                Lang::Ko => format!("훅 {hooks}개"),
                Lang::En => format!("{hooks} hooks"),
            });
        }
        if parts.is_empty() {
            return self.pick("얹는 것이 없습니다", "adds nothing").to_string();
        }
        parts.join(" ‒ ")
    }

    // ── The add forms (`/mcp` a · `/plugin` a)

    /// The key hint under an open form.
    pub fn add_keys(self) -> String {
        match self {
            Lang::Ko => {
                "글자 입력 ‒ ↑↓∙Tab 칸 이동 ‒ ←→ 값 고르기 ‒ Enter 다음∙추가 ‒ Esc 취소".to_string()
            }
            Lang::En => {
                "type ‒ ↑↓∙Tab move ‒ ←→ pick a value ‒ Enter next/add ‒ Esc cancel".to_string()
            }
        }
    }

    // The field labels. **Short, because they are a column** — the value is the answer.
    pub fn f_name(self) -> &'static str {
        self.pick("이름", "name")
    }
    pub fn f_kind(self) -> &'static str {
        self.pick("방식", "kind")
    }
    pub fn f_command(self) -> &'static str {
        self.pick("명령", "command")
    }
    pub fn f_url(self) -> &'static str {
        self.pick("주소", "url")
    }
    pub fn f_args(self) -> &'static str {
        self.pick("인자", "args")
    }
    pub fn f_env(self) -> &'static str {
        self.pick("환경변수", "env")
    }
    pub fn f_where(self) -> &'static str {
        self.pick("어디에", "where")
    }
    pub fn f_source(self) -> &'static str {
        self.pick("받아 올 곳", "source")
    }
    /// The two answers the `where` row walks. **`stdio` and `http` are protocol names**, not words
    /// to translate, so they are not here.
    pub fn f_machine(self) -> &'static str {
        self.pick("이 컴퓨터", "this machine")
    }
    pub fn f_project(self) -> &'static str {
        self.pick("이 저장소", "this repository")
    }

    /// What a field is for, under the form. **One per field**, so the box is one height on every
    /// row of it.
    pub fn f_hint(self, field: &str) -> String {
        let (ko, en) = match field {
            "name" => (
                "서버 이름 ‒ 띄어쓰기 없이. 에이전트는 `mcp_<이름>`으로 봅니다",
                "the server's name, no spaces ‒ the agent sees it as `mcp_<name>`",
            ),
            "kind" => (
                "이 컴퓨터에서 프로그램으로 돌릴지(stdio), 원격 서버에 붙을지(http)",
                "run a program here (stdio), or talk to a remote server (http)",
            ),
            "command" => (
                "실행할 프로그램. `npx`처럼 이름만 적으면 PATH에서 찾습니다",
                "the program to run; a bare name like `npx` is looked up on PATH",
            ),
            "args" => (
                "프로그램에 넘길 인자 ‒ 띄어쓰기로 나눕니다 (따옴표는 못 씁니다)",
                "arguments, split on spaces (this form cannot carry quotes)",
            ),
            "env" => (
                "`키=값` 꼴로, 띄어쓰기로 구분. 적은 값은 설정 파일에 그대로 남습니다",
                "`KEY=value` pairs, space-separated. What you type is written to the file",
            ),
            "where" => (
                "이 컴퓨터에 두면 모든 프로젝트에서 쓰고, 저장소에 두면 git에 잡힙니다",
                "this machine: every project ∙ this repository: it shows up in git",
            ),
            "url" => (
                "`http://` 또는 `https://`로 시작하는 주소",
                "an address starting with `http://` or `https://`",
            ),
            "source" => (
                "`owner/repo`∙`https://…`∙`git@…`∙로컬 경로 ‒ git으로 받습니다",
                "`owner/repo`, `https://…`, `git@…` or a local path ‒ fetched with git",
            ),
            _ => ("", ""),
        };
        match self {
            Lang::Ko => ko.to_string(),
            Lang::En => en.to_string(),
        }
    }

    /// The label a field key is shown by. **The key is what the code says, the label is what the
    /// screen says** — a refusal has to name the field in the reader's language.
    fn f_label(self, key: &str) -> &'static str {
        match key {
            "name" => self.f_name(),
            "kind" => self.f_kind(),
            "command" => self.f_command(),
            "url" => self.f_url(),
            "args" => self.f_args(),
            "env" => self.f_env(),
            "where" => self.f_where(),
            "source" => self.f_source(),
            _ => self.pick("칸", "field"),
        }
    }

    /// The sentence under an open form when Enter was refused, and which field it is about.
    pub fn f_needs(self, key: &str) -> String {
        let label = self.f_label(key);
        match self {
            Lang::Ko => format!("`{label}`을(를) 채워 주세요."),
            Lang::En => format!("`{label}` is needed."),
        }
    }
    /// A value that cannot hold a space: a server's name becomes part of the tool name the agent
    /// calls (`mcp_<name>`), where a space would be washed out and the two would stop matching.
    pub fn f_no_space(self, key: &str) -> String {
        let label = self.f_label(key);
        match self {
            Lang::Ko => format!("`{label}`에는 띄어쓰기를 넣을 수 없습니다."),
            Lang::En => format!("`{label}` cannot have a space in it."),
        }
    }
    pub fn f_bad_url(self) -> &'static str {
        self.pick(
            "주소는 http:// 나 https://로 시작해야 합니다.",
            "the address has to start with http:// or https://",
        )
    }
    pub fn f_bad_pair(self, token: &str) -> String {
        match self {
            Lang::Ko => format!("`{token}`은(는) `키=값` 꼴이 아닙니다."),
            Lang::En => format!("`{token}` is not a `KEY=value` pair."),
        }
    }
    pub fn f_name_taken(self, name: &str) -> String {
        match self {
            Lang::Ko => format!("`{name}`은(는) 이미 있습니다. `r`로 다시 읽어 보세요."),
            Lang::En => format!("`{name}` already exists. Press `r` to read the list again."),
        }
    }
    /// A server written into one of our files. **Says that it takes effect on the next launch** —
    /// servers are started once, at announce time.
    pub fn f_mcp_added(self, name: &str, at: &str) -> String {
        match self {
            Lang::Ko => {
                format!("`{name}`을 `{at}`에 적었습니다. 다시 띄우면 도구가 붙습니다.")
            }
            Lang::En => format!("`{name}` is written into `{at}`. Restart and its tools attach."),
        }
    }
    pub fn f_plugin_added(self, name: &str, contents: &str) -> String {
        match self {
            Lang::Ko => format!("**{name}**을 받았습니다.\n\n{contents}\n\n다시 띄우면 붙습니다."),
            Lang::En => {
                format!("Installed **{name}**.\n\n{contents}\n\nIt attaches on the next launch.")
            }
        }
    }

    // ── Skills panel
    pub fn skills_empty(self) -> &'static str {
        self.pick(
            "쓸 수 있는 스킬이 없습니다. `.zyris-code/skills/`나 `~/.config/zyris-code/skills/`에 둡니다.",
            "No skills available. Put them in `.zyris-code/skills/` or `~/.config/zyris-code/skills/`.",
        )
    }

    // ── Plugins panel
    pub fn plugins_empty(self) -> &'static str {
        self.pick(
            "플러그인이 없습니다. `/plugin add owner/repo`로 받습니다.",
            "No plugins. Install one with `/plugin add owner/repo`.",
        )
    }
    pub fn plugin_hand_placed(self) -> &'static str {
        self.pick(" (직접 둔 것)", " (hand-placed)")
    }
    pub fn plugin_mcp_line(self, slug: &str, command: &str) -> String {
        match self {
            Lang::Ko => format!("MCP `{slug}` ‒ `{command}`"),
            Lang::En => format!("MCP `{slug}` ‒ runs `{command}`"),
        }
    }
    pub fn plugin_skills_line(self) -> &'static str {
        self.pick("스킬이 딸려 있습니다", "ships a skill")
    }

    // ── Account panel
    pub fn acc_id(self) -> &'static str {
        self.pick("아이디", "User ID")
    }
    pub fn acc_plan(self) -> &'static str {
        self.pick("플랜", "Plan")
    }
    pub fn acc_scopes(self) -> &'static str {
        self.pick("부여된 권한", "Granted scopes")
    }
    pub fn acc_none(self) -> &'static str {
        self.pick("없음", "none")
    }
    pub fn acc_logout_note(self) -> &'static str {
        self.pick(
            "로그아웃하면 저장된 자격이 지워지고, 다음 실행 때 다시 승인을 받습니다.",
            "Logging out clears the stored credentials ‒ the next launch asks for approval again.",
        )
    }
    /// The label of the account panel's logout button.
    pub fn acc_logout_button(self) -> &'static str {
        self.pick("로그아웃", "Log out")
    }
    /// The hint line when the panel carries a button — Tab moves focus onto it.
    pub fn panel_keys_button(self) -> String {
        match self {
            Lang::Ko => "↑↓ 스크롤 ∙ Tab 버튼 ∙ Esc 닫기".into(),
            Lang::En => "↑↓ scroll ∙ Tab button ∙ Esc close".into(),
        }
    }
    /// The hint line when the panel's button is focused — Enter activates it.
    pub fn panel_keys_button_focused(self) -> String {
        match self {
            Lang::Ko => "Enter 실행 ∙ Esc 닫기".into(),
            Lang::En => "Enter activate ∙ Esc close".into(),
        }
    }

    // ── Status panel
    pub fn st_thread(self) -> &'static str {
        // Deliberately not translated — sessions are called `thread` on screen in both languages.
        self.pick("thread", "thread")
    }
    pub fn st_project(self) -> &'static str {
        self.pick("프로젝트", "Project")
    }
    pub fn st_agent(self) -> &'static str {
        self.pick("에이전트", "Agent")
    }
    pub fn st_mode(self) -> &'static str {
        self.pick("모드", "Mode")
    }
    pub fn st_model(self) -> &'static str {
        self.pick("모델", "Model")
    }
    pub fn st_cwd(self) -> &'static str {
        self.pick("작업 위치", "Working dir")
    }
    pub fn st_thread_none(self) -> &'static str {
        self.pick(
            "아직 없음 ‒ 첫 메시지에서 만들어집니다",
            "none yet ‒ your first message creates it",
        )
    }
    pub fn st_project_default(self) -> &'static str {
        self.pick("기본 (안 고름)", "default (not picked)")
    }
    pub fn st_pending_work(self) -> &'static str {
        self.pick("다음 메시지가 새 work를 엽니다.", "Your next message opens a new work.")
    }
    pub fn st_pending_job(self) -> &'static str {
        self.pick("다음 메시지가 새 job을 엽니다.", "Your next message opens a new job.")
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
    /// The answer sent back when a question arrives in print mode, where no screen exists to put
    /// it on.
    ///
    /// **Addressed to the agent, because it is delivered as the answer** — the server's
    /// `question_waiter` takes the next ordinary message as the reply, so this text is what the
    /// turn reads. A bare refusal would leave it free to ask again and block a second time, so it
    /// says what to do instead.
    pub fn question_unattended(self) -> &'static str {
        self.pick(
            "이 턴은 화면 없이 돌고 있어 답할 사람이 없습니다. 다시 묻지 마세요. 안전한 기본값으로 \
             진행하거나, 그럴 수 없다면 무엇이 필요했는지 적고 멈추세요.",
            "This turn is running with no screen, so there is nobody to answer. Do not ask again: \
             either carry on with the safe default, or stop and say what you needed.",
        )
    }
    /// Said on stderr when that happens.
    ///
    /// **Not on stdout**, which is the agent's answer and nothing else — `-p` exists to be piped.
    /// A reader of the script's log needs to know the answer they are holding was produced without
    /// the decision they were asked for.
    pub fn question_unattended_notice(self) -> &'static str {
        self.pick(
            "에이전트가 물었지만 -p 에는 답할 화면이 없습니다:",
            "The agent asked something, and `-p` has no screen to answer it on:",
        )
    }
    pub fn all_skipped(self) -> &'static str {
        self.pick("모두 건너뛰었습니다.", "Skipped them all.")
    }
    /// The marker that flags a typed-in answer. **Must match between the composed answer
    /// (`question::answer_text`) and the reading of that answer back (`question::answer_picks`)**
    /// — both use the same lang.
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
    /// A command that finished cleanly. **A quiet success still has to say so** — an empty detail
    /// reads as a broken tool.
    pub fn detail_ok(self) -> &'static str {
        self.pick("완료", "Done")
    }
    /// The headline of a `grep` detail: how many matches, across how many files scanned.
    pub fn detail_hits(self, hits: usize, scanned: u32) -> String {
        match self {
            Lang::Ko => format!("{hits}처 ∙ {scanned}개 파일을 살펴봄"),
            Lang::En => format!("{hits} matches ∙ {scanned} files scanned"),
        }
    }
    /// The headline of a `glob`/`list` detail.
    pub fn detail_found(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("{n}개"),
            Lang::En => format!("{n} entries"),
        }
    }
    /// Says the result was cut short. **Without it, "nothing more matched" and "we stopped
    /// looking" are indistinguishable.**
    pub fn detail_truncated(self) -> &'static str {
        self.pick("… 여기까지만 가져왔습니다", "… cut short here")
    }
    /// A reasoning chip with no title and no words yet.
    pub fn thinking(self) -> &'static str {
        self.pick("생각하는 중…", "Thinking…")
    }
    /// The head of a stretch of working that is over. **Not the last title it carried** — that one
    /// described a step in the middle, and left standing it reads as still going.
    pub fn run_done(self) -> &'static str {
        self.pick("완료", "Done")
    }
    /// The head of a stretch of working **a person stopped with `Esc`**.
    ///
    /// **Not `run_done`.** The run was cut short, and the head over the reasoning it was cut in is
    /// the one line on screen that would tell the person it finished — the opposite of what they
    /// just did. `rows::make` draws it for the one card the stop landed on; the server cannot say
    /// which, because a stopped turn and a finished one end the same way on the wire
    /// (`Timeline::mark_stopped`).
    pub fn run_stopped(self) -> &'static str {
        self.pick("중단됨", "Stopped")
    }
    pub fn detail_no_output(self) -> &'static str {
        self.pick("(출력 없음)", "(no output)")
    }
    pub fn detail_clipped(self) -> &'static str {
        self.pick("… (잘렸습니다)", "… (clipped)")
    }
    /// The byte offset a ranged `file_io.read` started at. It keeps repeated reads of one file apart.
    pub fn action_from_byte(self, offset: i64) -> String {
        match self {
            Lang::Ko => format!("{offset}바이트부터"),
            Lang::En => format!("from byte {offset}"),
        }
    }
    pub fn default_shell(self) -> &'static str {
        self.pick("기본 셸", "default shell")
    }
    /// **English needs the singular.** A card that used one tool said "1 tools", which reads as a
    /// bug in the very line that is supposed to summarize the run.
    pub fn tool_count(self, n: usize) -> String {
        match (self, n) {
            (Lang::Ko, _) => format!("도구 {n}개"),
            (Lang::En, 1) => "1 tool".to_string(),
            (Lang::En, _) => format!("{n} tools"),
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
    /// The keys a list answers to. **`Tab` is named only where it does something** — a list whose
    /// rows carry no note draws no note area at all, and promising a key that does nothing is the
    /// same lie as an overflow mark on a list that was never cut.
    pub fn picker_keys(self, back: &str, opens_note: bool) -> String {
        match (self, opens_note) {
            (Lang::Ko, true) => format!("↑↓ 이동 ∙ Enter 고르기 ∙ Tab 설명 ∙ {back}"),
            (Lang::Ko, false) => format!("↑↓ 이동 ∙ Enter 고르기 ∙ {back}"),
            (Lang::En, true) => format!("↑↓ move ∙ Enter choose ∙ Tab detail ∙ {back}"),
            (Lang::En, false) => format!("↑↓ move ∙ Enter choose ∙ {back}"),
        }
    }
    pub fn cannot_choose(self) -> &'static str {
        self.pick("지금은 고를 수 없습니다", "Can't choose right now")
    }
    /// The footer while a deletion waits for a yes. **It names what would go**, because the
    /// cursor is the only other thing saying which row this is about, and a list is exactly
    /// where the wrong row gets deleted.
    pub fn picker_delete_ask(self, name: &str) -> String {
        match self {
            Lang::Ko => format!("{name} 지울까요?  Enter 지우기 ∙ Esc 그만두기"),
            Lang::En => format!("Delete {name}?  Enter delete ∙ Esc keep"),
        }
    }
    pub fn picker_deleted(self, name: &str) -> String {
        match self {
            Lang::Ko => format!("{name} 지웠습니다"),
            Lang::En => format!("Deleted {name}"),
        }
    }
    /// Del on a thread row. **The server has no way to delete a session** — there is no
    /// `delete_session` on the API and no archived flag on `ZSession`. Saying nothing would read
    /// as the key being broken, which is worse than saying it cannot be done.
    pub fn threads_stay(self) -> &'static str {
        self.pick(
            "쓰레드는 아직 지울 수 없습니다. 서버에 지우는 길이 없습니다",
            "Threads can't be deleted yet: the server offers no way to",
        )
    }
    /// Del on the "new" row, or on a list where nothing can go.
    pub fn nothing_to_delete(self) -> &'static str {
        self.pick("여기서는 지울 것이 없습니다", "Nothing to delete here")
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
                 never widen.\n\nA fresh enrollment-code window appears on reconnect ‒ check \
                 **every** scope there."
            ),
        }
    }
    pub fn scopes_asked_again(self, missing: &str) -> String {
        match self {
            Lang::Ko => format!(
                "**권한이 모자랍니다: {missing}**. 다시 승인받을 수 있도록 이 컴퓨터의 자격을 비웠습니다.\n\n\
                 잠시 뒤 등록 코드 창이 뜹니다. 승인 화면에서 권한을 **모두** 체크해 주세요."
            ),
            Lang::En => format!(
                "**Not enough permissions: {missing}**. Dropped this machine's credentials so \
                 you can be approved again.\n\nThe enrolment-code window appears in a moment ‒ \
                 check **every** scope there."
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
}

/// How wide the run's own words may get on the activity line. Long enough for a sentence the
/// server wrote, short enough to leave the line's hint room — the point of the line is that a
/// person takes it in at a glance.
const ACTIVITY_WIDTH: usize = 48;

/// Cuts text to a column budget, **counting a wide character as the two columns it takes up**.
///
/// `chars().take(n)` is wrong here for the same reason it is wrong everywhere in this app: a
/// Hangul syllable is one `char` and two columns, so counting characters buys twice the line.
fn clip_columns(text: &str, width: usize) -> String {
    if crate::markdown::display_width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    // One column is kept for the ellipsis, so the result never reads as if it ended there.
    for ch in text.chars() {
        let w = crate::markdown::display_width(&ch.to_string()).max(1);
        if used + w > width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {

    /// **The run's own words are cut to fit, by columns and not by characters.** A Hangul syllable
    /// is one `char` and two columns, so a character count would hand back twice the line — and this
    /// is the line whose whole reason for changing was that it got too long.
    #[test]
    fn the_activity_line_clips_the_runs_words_by_columns() {
        let long = "가".repeat(80);
        let text = Lang::Ko.running_tool("exec", &long, 0);
        let label = text.trim_start_matches("▶ exec  ∙  ").trim_end_matches("  ∙  0초");
        assert_ne!(label, long, "the words were not clipped: {text}");
        assert_eq!(label.chars().last(), Some('…'), "{text}");
        assert!(
            crate::markdown::display_width(label) <= ACTIVITY_WIDTH,
            "{} columns: {label}",
            crate::markdown::display_width(label)
        );
        // Nothing to clip is left exactly as it was, ellipsis and all.
        assert_eq!(
            Lang::En.running_tool("exec", "running the tests", 0),
            "▶ exec  ∙  running the tests  ∙  0s"
        );
        // And no words at all is the tool alone, not a dangling separator.
        assert_eq!(Lang::Ko.running_tool("exec", "", 12), "▶ exec  ∙  12초");
    }

    /// Found against a real session: a card that used one tool said "1 tools".
    #[test]
    fn one_tool_is_singular_in_english() {
        assert_eq!(Lang::En.tool_count(1), "1 tool");
        assert_eq!(Lang::En.tool_count(2), "2 tools");
        assert_eq!(Lang::Ko.tool_count(1), "도구 1개");
    }

    /// **Seconds stop being readable past a minute.** `110s` has to be divided in somebody's head;
    /// `1분 50초` does not. At most two units, and once there are hours the seconds go.
    #[test]
    fn a_span_of_seconds_is_split_into_minutes_and_hours() {
        for (secs, want) in [
            (0, "0초"),
            (59, "59초"),
            (60, "1분 0초"),
            (110, "1분 50초"),
            (252, "4분 12초"),
            (3599, "59분 59초"),
            (3600, "1시간 0분"),
            (7325, "2시간 2분"),
        ] {
            assert_eq!(Lang::Ko.duration(secs), want, "{secs}초");
        }
        for (secs, want) in [
            (0, "0s"),
            (59, "59s"),
            (60, "1m 0s"),
            (110, "1m 50s"),
            (252, "4m 12s"),
            (3599, "59m 59s"),
            (3600, "1h 0m"),
            (7325, "2h 2m"),
        ] {
            assert_eq!(Lang::En.duration(secs), want, "{secs}s");
        }
    }

    /// **The four places that show a span end up with the same words as each other**, because they
    /// all go through `duration` — one of them rendering `초` on an English screen is exactly the
    /// kind of drift a shared helper prevents.
    #[test]
    fn every_span_shown_uses_the_same_units() {
        assert!(Lang::Ko.running_tool("exec", "", 110).contains("1분 50초"));
        assert!(Lang::En.running_tool("exec", "", 110).contains("1m 50s"));
        assert!(Lang::Ko.job_ended("b1", true, 3600).contains("1시간 0분"));
        assert!(Lang::En.job_ended("b1", true, 3600).contains("1h 0m"));
        assert!(Lang::Ko.background_job(1, "b1", "build", 110).contains("1분 50초"));
        assert!(Lang::En.background_job(1, "b1", "build", 110).contains("1m 50s"));
        assert!(Lang::Ko.jobs_row("b1", "build", 3660).contains("1시간 1분"));
        assert!(Lang::En.jobs_row("b1", "build", 3660).contains("1h 1m"));
    }
    use super::*;

    /// **Both languages' names are accepted.** Typing `/config lang` with a Korean word on an English screen
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
        assert_eq!(from_locale(None), Lang::En, "unknown means English");
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
            (ko.run_done(), en.run_done()),
            (ko.run_stopped(), en.run_stopped()),
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
            (ko.esc_stops(), en.esc_stops()),
            (ko.enroll_title(), en.enroll_title()),
            (ko.enroll_steps(), en.enroll_steps()),
            (ko.enroll_warning(), en.enroll_warning()),
            (ko.copy_stayed_here(), en.copy_stayed_here()),
            (ko.enroll_lapsed(), en.enroll_lapsed()),
            (ko.enroll_denied(), en.enroll_denied()),
            (ko.enroll_keys(), en.enroll_keys()),
            (ko.clear_done(), en.clear_done()),
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
            (ko.question_unattended(), en.question_unattended()),
            (ko.question_unattended_notice(), en.question_unattended_notice()),
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
            (ko.stood_by_notice(), en.stood_by_notice()),
            (ko.free_mark(), en.free_mark()),
        ];
        for (k, e) in pairs {
            assert_ne!(k, e, "a phrase was left untranslated: {k}");
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
            en.esc_stops(),
            en.run_stopped(),
            en.quit_armed(),
            en.lang_changed(),
            en.enroll_title(),
            en.enroll_steps(),
            en.enroll_warning(),
            en.copy_stayed_here(),
            en.enroll_lapsed(),
            en.enroll_denied(),
            en.enroll_keys(),
            en.connected(),
            en.waiting_answer(),
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
            en.stood_by_notice(),
            en.clear_done(),
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
            en.question_unattended(),
            en.question_unattended_notice(),
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
                "Hangul left in an English phrase: {text}"
            );
        }
        // **Arguments-filled phrases.** These take data, so they can't sit in the array above —
        // the call itself is the check.
        let with_args = [
            en.agent_list_error("x"),
            en.connect_failed("x"),
            en.previous_error("x"),
            en.screen_failed("x"),
            en.log_location("/tmp/zyris-code.log"),
            en.server_unreachable(5, "x"),
            en.cwd_text(std::path::Path::new("/home/ruma"), "node", "slug", "cred"),
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
            en.plugin_removed("x"),
            en.plugin_unknown("x"),
            en.detail_exit_code(1),
            en.action_from_byte(120),
            en.detail_hits(3, 42),
            en.detail_found(3),
            en.tool_count(3),
            en.diff_skip(2),
            en.pick_more(true, 3),
            en.picker_keys("← close", true),
            en.picker_keys("← close", false),
            en.changes_text(&[], std::path::Path::new("/")),
            en.mcp_report_text(&[]),
            en.rules_text(&[]),
            en.skills_text(&[]),
            en.plugin_list_text(&[]),
            en.plugin_update_text(&[]),
            en.plugin_contents_text(&Plugin {
                name: "x".into(),
                description: String::new(),
                mcp: vec![crate::mcp::bridge::ServerSpec {
                    slug: "m".into(),
                    transport: crate::mcp::bridge::Transport::Stdio {
                        command: "cmd".into(),
                        args: vec![],
                        env: Default::default(),
                    },
                }],
                skills: None,
                agents: None,
                commands: Vec::new(),
                hooks: Vec::new(),
                about: crate::plugin::About::default(),
                root: "/tmp".into(),
            }),
            en.plugin_added(
                &Plugin {
                    name: "x".into(),
                    description: String::new(),
                    mcp: vec![],
                    skills: None,
                    agents: None,
                    commands: Vec::new(),
                    hooks: Vec::new(),
                    about: crate::plugin::About::default(),
                    root: "/tmp".into(),
                },
                "contents",
            ),
        ];
        for text in with_args {
            assert!(
                !text.chars().any(|c| ('가'..='힣').contains(&c)),
                "Hangul left in an English phrase: {text}"
            );
        }
        assert!(!en.queued(3).chars().any(|c| ('가'..='힣').contains(&c)));
        // **The duration words are units, not sentences.** `1m 50s`, never `1분 50초`.
        for secs in [0, 59, 60, 110, 3599, 3600, 7325] {
            let text = en.duration(secs);
            assert!(!text.chars().any(|c| ('가'..='힣').contains(&c)), "Hangul in {text:?}");
        }
        for text in [
            en.running_tool("exec", "", 110),
            en.job_ended("b1", true, 110),
            en.job_ended("b1", false, 3660),
            en.background_job(2, "b1", "build", 110),
            en.jobs_row("b1", "build", 7325),
            en.report_head(true).to_string(),
            en.report_head(false).to_string(),
        ] {
            assert!(!text.chars().any(|c| ('가'..='힣').contains(&c)), "Hangul in {text:?}");
        }
        assert!(!en.threads_in("proj").chars().any(|c| ('가'..='힣').contains(&c)));
    }

    /// In Korean, thread is **sseuredeu**. Keeping the English word would make it stick out in the list.
    #[test]
    fn thread_reads_as_sseurede_in_korean() {
        assert!(Lang::Ko.new_thread().contains("쓰레드"), "{}", Lang::Ko.new_thread());
        assert!(Lang::Ko.threads_in("proj").contains("쓰레드"));
    }
}
