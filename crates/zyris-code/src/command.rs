//! Slash commands. **This module is pure** — it reads text and only decides what to do.
//!
//! `app.rs` executes them. Some only touch state (`/mode`), some need the server or disk
//! (`/agent`·`/undo`) — that branch is also borne over there.
//!
//! **Commands don't go to the server.** A typo must not burn credits, and there's no reason to ask the server
//! about something only this node knows, like `/mode`.

use crate::mode::Mode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    /// Without an argument, reports the current mode; with one, changes it.
    Mode(Option<Mode>),
    /// The screen language. Without an argument, reports the current one; with one, changes it.
    /// **An unknown language isn't dropped into `Unknown`** — it must say what's wrong on the spot
    /// so the user can retype (`Lang(None)` means "the current one", so there's no room).
    Lang(Option<crate::lang::Lang>),
    /// A language we couldn't understand, like `/lang Japanese`.
    LangUnknown(String),
    Mcp,
    Skills,
    /// Which `CLAUDE.md`·`AGENTS.md` are loaded into the session.
    Rules,
    Cwd,
    /// Without an argument, opens the list; with one, goes straight to that name.
    Agent(Option<String>),
    Plugin(Plugin),
    Undo,
    Clear,
    /// Shows what's been opened outside the working directory.
    Grants,
    /// Closes all of them.
    GrantsClose,
    /// What was changed in this directory.
    Changes,
    /// Jobs running in the background. With no argument, the list; with `stop <id>`, stops that one.
    ///
    /// **There are no logs here** — those are what the agent reads with `wait.logs`, and covering
    /// the transcript hides the conversation itself.
    Jobs(Option<String>),
    /// Quits. **If a turn is running, it stops on the server too** (`turn_to_stop` in `app.rs`).
    Quit,
    /// Shows who this node is attached as (`/account`), or logs out (`/account logout`).
    Account(Option<AccountAction>),
    /// Something unknown. **Not sent to the server; tells what's available instead.**
    Unknown(String),
}

/// What `/account` can do after the command word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountAction {
    /// Forgets the stored credentials. The next launch asks for approval again.
    Logout,
}

/// What `/plugin` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plugin {
    /// Shows what was fetched.
    List,
    /// Fetches from an `owner/repo` or a clonable address.
    Add(String),
    Remove(String),
    /// Without a name, everything fetched.
    Update(Option<String>),
    /// It's unclear what's being asked. **Not swallowed silently.**
    Unknown(String),
}

/// Whether this text is a command.
///
/// **A path must not be eaten as a command.** People really do type `/home/ruma/...` verbatim, and
/// swallowing it as a command would keep the message from reaching the server. So three things are required —
/// it starts with `/`, the next character is an ASCII letter, and the first word has no further `/`.
pub fn is_command(text: &str) -> bool {
    let t = text.trim_start();
    let Some(rest) = t.strip_prefix('/') else { return false };
    let Some(first) = rest.chars().next() else { return false };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    !rest.split_whitespace().next().unwrap_or_default().contains('/')
}

pub fn parse(text: &str) -> Option<Command> {
    if !is_command(text) {
        return None;
    }
    let t = text.trim();
    let rest = t.strip_prefix('/')?;
    let (name, arg) = match rest.split_once(char::is_whitespace) {
        Some((n, a)) => (n, a.trim()),
        None => (rest, ""),
    };
    // **Some agents have spaces in their names** ("Main Agent"). Cutting at the first word would make them unpickable.
    let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
    Some(match name {
        "help" | "h" => Command::Help,
        "mode" => Command::Mode(match arg {
            "" => None,
            other => match mode_named(other) {
                Some(m) => Some(m),
                // An unknown mode name isn't silently ignored — otherwise the user wouldn't know it didn't change.
                None => return Some(Command::Unknown(format!("mode {other}"))),
            },
        }),
        "mcp" => Command::Mcp,
        "skills" | "skill" => Command::Skills,
        "rules" | "claude" | "agents" => Command::Rules,
        "cwd" | "pwd" => Command::Cwd,
        "agent" => Command::Agent(some(arg)),
        "plugin" | "plugins" => Command::Plugin(plugin_action(arg)),
        "undo" => Command::Undo,
        "clear" => Command::Clear,
        // Only the approval screen's `a` opens them, so here we only view and close.
        "grants" | "grant" => match arg {
            "" | "list" => Command::Grants,
            "close" | "clear" | "닫기" => Command::GrantsClose,
            other => Command::Unknown(format!("grants {other}")),
        },
        "lang" | "language" | "언어" => match arg {
            "" => Command::Lang(None),
            given => match crate::lang::Lang::parse(given) {
                Some(lang) => Command::Lang(Some(lang)),
                None => Command::LangUnknown(given.to_string()),
            },
        },
        "changes" | "changed" | "diff" => Command::Changes,
        // Looking and stopping only. Starting one stays the agent's `wait.start` alone.
        "jobs" | "job" => match arg.split_once(' ') {
            None if arg.is_empty() || arg == "list" => Command::Jobs(None),
            Some(("stop" | "kill", id)) if !id.trim().is_empty() => {
                Command::Jobs(Some(id.trim().to_string()))
            }
            _ => Command::Unknown(format!("jobs {arg}")),
        },
        "account" => match arg {
            "" => Command::Account(None),
            "logout" | "log out" | "로그아웃" => Command::Account(Some(AccountAction::Logout)),
            other => return Some(Command::Unknown(format!("account {other}"))),
        },
        "quit" | "exit" | "q" => Command::Quit,
        other => Command::Unknown(other.to_string()),
    })
}

/// What comes after `/plugin`. **Arguments can contain spaces** (addresses don't, but names can).
fn plugin_action(arg: &str) -> Plugin {
    let (verb, rest) = match arg.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim()),
        None => (arg, ""),
    };
    match (verb, rest) {
        ("", _) | ("list", _) => Plugin::List,
        ("add" | "install", "") => Plugin::Unknown("add — 받아 올 곳을 같이 적어 주세요".into()),
        ("add" | "install", what) => Plugin::Add(what.to_string()),
        ("remove" | "rm" | "uninstall", "") => {
            Plugin::Unknown("remove — 지울 이름을 같이 적어 주세요".into())
        }
        ("remove" | "rm" | "uninstall", what) => Plugin::Remove(what.to_string()),
        ("update" | "upgrade", "") => Plugin::Update(None),
        ("update" | "upgrade", what) => Plugin::Update(Some(what.to_string())),
        // **`/plugin owner/repo` is accepted too.** Forgetting `add` is the most common mistake.
        (what, "") if what.contains('/') || what.contains("://") => Plugin::Add(what.to_string()),
        (what, _) => Plugin::Unknown(format!("plugin {what}")),
    }
}

/// Accepts both the Korean names shown on screen and the English ones — the screen is Korean, but which one is familiar
/// varies from person to person.
///
/// `work`·`job` are **not translated on screen** (`lang::mode_work`) — they must stay exactly as attacca calls them on its own screen
/// so they can be found in that list. Still, the Korean words for them are also accepted here: **widening the input side is free,
/// but having two names floating around on screen is not.**
fn mode_named(s: &str) -> Option<Mode> {
    match s {
        "기본" | "normal" | "default" => Some(Mode::Normal),
        "계획" | "plan" => Some(Mode::Plan),
        "작업" | "work" => Some(Mode::Work),
        "잡" | "job" => Some(Mode::Job),
        _ => None,
    }
}

/// The list that appears when `/` is typed. **`/help` prints the same thing** — if the two diverge, one goes stale.
pub fn catalogue(lang: crate::lang::Lang) -> Vec<(&'static str, &'static str)> {
    use crate::lang::Lang;
    match lang {
        Lang::Ko => vec![
            ("/help", "쓸 수 있는 명령"),
            ("/mode", "모드를 보거나 바꿉니다 (기본·계획·work·job)"),
            ("/lang", "화면 말을 바꿉니다 (ko·en)"),
            ("/agent", "에이전트를 고릅니다. 다음 메시지에서 새 쓰레드가 열립니다"),
            ("/mcp", "붙은 MCP 서버와 도구 수"),
            ("/skills", "쓸 수 있는 스킬"),
            ("/plugin", "플러그인을 받고 지웁니다 (add·remove·update)"),
            ("/rules", "이 쓰레드에 실린 CLAUDE.md·AGENTS.md"),
            ("/cwd", "도구가 상대경로를 푸는 자리"),
            ("/account", "계정 정보를 보고, 로그아웃합니다 (logout)"),
            ("/grants", "밖으로 열어 둔 디렉터리 (close로 전부 닫습니다)"),
            ("/jobs", "배경에서 도는 작업 (stop <id>로 멈춥니다)"),
            ("/changes", "이 디렉터리에서 바꾼 파일"),
            ("/undo", "마지막 편집을 되돌립니다"),
            ("/clear", "화면의 대화를 지웁니다 (쓰레드는 그대로입니다)"),
            ("/quit", "끝냅니다. 도는 턴이 있으면 서버에서도 멈춥니다"),
        ],
        Lang::En => vec![
            ("/help", "What you can type"),
            ("/mode", "Show or change the mode (normal / plan / work / job)"),
            ("/lang", "Change the interface language (ko / en)"),
            ("/agent", "Pick an agent. A new thread opens with your next message"),
            ("/mcp", "MCP servers that connected, and how many tools each brought"),
            ("/skills", "Skills available here"),
            ("/plugin", "Install and remove plugins (add / remove / update)"),
            ("/rules", "The CLAUDE.md and AGENTS.md loaded into this thread"),
            ("/cwd", "Where tools resolve relative paths"),
            ("/account", "Show account info, or log out (logout)"),
            ("/grants", "Directories opened outside the working directory (close shuts them all)"),
            ("/jobs", "Background jobs (stop <id> kills one)"),
            ("/changes", "Files changed in this directory"),
            ("/undo", "Undo the last edit"),
            ("/clear", "Clear the screen (the thread itself is untouched)"),
            ("/quit", "Quit. A running turn is stopped on the server too"),
        ],
    }
}

/// Keys worth knowing. **If it isn't on the screen, it doesn't exist** — a README isn't opened when you need it.
///
/// The canonical source is `on_key` in `app.rs`; this is only a transcription of what a human should memorize.
/// If a key changes, change it here too.
pub fn keys(lang: crate::lang::Lang) -> Vec<(&'static str, &'static str)> {
    use crate::lang::Lang;
    match lang {
        Lang::Ko => vec![
            ("Shift+Tab", "모드 바꾸기 (기본 → 계획 → work → job)"),
            (
                "Shift+Enter · Alt+Enter",
                "줄바꿈 (Shift+Enter는 키티 키보드 프로토콜 지원 터미널에서만)",
            ),
            ("←", "프로젝트·쓰레드 목록 (입력란이 비었을 때)"),
            ("↑ ↓", "보낸 말 되살리기"),
            ("Ctrl+O", "작업 카드 접기·펴기"),
            ("Ctrl+U", "친 것 모두 지우기"),
            ("Esc", "도는 턴 멈추기"),
            ("Ctrl+C", "멈추기 → 한 번 더 누르면 끝내기"),
            ("y n a", "승인 화면에서 허락·거절·이 디렉터리 열기"),
            ("드래그", "화면 아무 데나 — 놓는 순간 고른 글이 클립보드로"),
        ],
        Lang::En => vec![
            ("Shift+Tab", "Switch mode (normal → plan → work → job)"),
            (
                "Shift+Enter · Alt+Enter",
                "Newline (Shift+Enter needs a kitty-keyboard-protocol terminal)",
            ),
            ("←", "Project and thread list (when the input box is empty)"),
            ("↑ ↓", "Bring back something you sent"),
            ("Ctrl+O", "Fold or unfold a work card"),
            ("Ctrl+U", "Clear what you typed"),
            ("Esc", "Stop the running turn"),
            ("Ctrl+C", "Stop, then press again to quit"),
            ("y n a", "On the approval screen: allow, deny, open this directory"),
            ("drag", "Drag anywhere — the selected text goes to the clipboard when you let go"),
        ],
    }
}

/// The text `/help` prints.
pub fn help_text(lang: crate::lang::Lang) -> String {
    use crate::lang::Lang;
    let (head, keys_head) = match lang {
        Lang::Ko => ("쓸 수 있는 명령입니다.\n", "\n\n**키**\n"),
        Lang::En => ("Commands you can type.\n", "\n\n**Keys**\n"),
    };
    let mut s = String::from(head);
    for (name, note) in catalogue(lang) {
        s.push_str(&format!("\n- `{name}` — {note}"));
    }
    s.push_str(keys_head);
    for (key, note) in keys(lang) {
        s.push_str(&format!("\n- `{key}` — {note}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_message_is_not_a_command() {
        assert!(parse("안녕하세요").is_none());
        assert!(parse("").is_none());
        // **A path must not be eaten as a command.** People really do type `/home/...`.
        assert!(parse("/home/ruma/zyris-code를 봐 줘").is_none());
        assert!(parse("/usr/bin/env").is_none());
        // Something starting with a digit isn't a command either — like `/2026-08-02`.
        assert!(parse("/2026-08-02").is_none());
    }

    #[test]
    fn mode_without_an_argument_just_reports() {
        assert_eq!(parse("/mode"), Some(Command::Mode(None)));
    }

    #[test]
    fn mode_takes_the_korean_names_shown_on_screen() {
        assert_eq!(parse("/mode 기본"), Some(Command::Mode(Some(Mode::Normal))));
        assert_eq!(parse("/mode 잡"), Some(Command::Mode(Some(Mode::Job))));
        assert_eq!(parse("/mode 계획"), Some(Command::Mode(Some(Mode::Plan))));
    }

    /// English names are accepted too — the screen is Korean, but what's familiar varies from person to person.
    #[test]
    fn mode_also_takes_the_english_names() {
        assert_eq!(parse("/mode plan"), Some(Command::Mode(Some(Mode::Plan))));
        assert_eq!(parse("/mode job"), Some(Command::Mode(Some(Mode::Job))));
        assert_eq!(parse("/mode normal"), Some(Command::Mode(Some(Mode::Normal))));
    }

    /// **Silently ignoring an unknown mode name would leave the user thinking it didn't change.**
    #[test]
    fn an_unknown_mode_name_is_reported_not_ignored() {
        assert_eq!(parse("/mode 빠름"), Some(Command::Unknown("mode 빠름".into())));
    }

    #[test]
    fn plugin_without_an_argument_lists() {
        assert_eq!(parse("/plugin"), Some(Command::Plugin(Plugin::List)));
        assert_eq!(parse("/plugin list"), Some(Command::Plugin(Plugin::List)));
    }

    #[test]
    fn plugin_add_takes_a_source() {
        assert_eq!(
            parse("/plugin add owner/repo"),
            Some(Command::Plugin(Plugin::Add("owner/repo".into())))
        );
        assert_eq!(
            parse("/plugin install https://github.com/owner/repo"),
            Some(Command::Plugin(Plugin::Add("https://github.com/owner/repo".into())))
        );
    }

    /// **Forgetting `add` is the most common mistake.** If it looks like an address, treat it as a fetch.
    #[test]
    fn a_bare_source_is_taken_as_add() {
        assert_eq!(
            parse("/plugin owner/repo"),
            Some(Command::Plugin(Plugin::Add("owner/repo".into())))
        );
    }

    /// When it's unclear what's being asked, **swallowing it silently** makes it look like nothing happened.
    #[test]
    fn an_incomplete_plugin_command_says_what_is_missing() {
        let Some(Command::Plugin(Plugin::Unknown(why))) = parse("/plugin add") else {
            panic!("it does not fall through as unknown");
        };
        assert!(why.contains("받아 올 곳"), "{why}");
    }

    #[test]
    fn plugin_update_takes_a_name_or_nothing() {
        assert_eq!(parse("/plugin update"), Some(Command::Plugin(Plugin::Update(None))));
        assert_eq!(
            parse("/plugin update 깃허브"),
            Some(Command::Plugin(Plugin::Update(Some("깃허브".into()))))
        );
    }

    /// Some agents have spaces in their names ("Main Agent"). Cutting at the first word would make them unpickable.
    #[test]
    fn agent_keeps_the_whole_name() {
        assert_eq!(parse("/agent Main Agent"), Some(Command::Agent(Some("Main Agent".into()))));
        assert_eq!(parse("/agent"), Some(Command::Agent(None)));
    }

    /// Surrounding whitespace is leftover typing, not meaning.
    #[test]
    fn surrounding_whitespace_does_not_change_the_meaning() {
        assert_eq!(parse("  /cwd  "), Some(Command::Cwd));
        assert_eq!(parse("/agent   Main Agent  "), Some(Command::Agent(Some("Main Agent".into()))));
    }

    /// **An unknown command must not be sent to the server.** One typo burns credits.
    #[test]
    fn an_unknown_command_stays_local() {
        assert_eq!(parse("/nope"), Some(Command::Unknown("nope".into())));
    }

    /// If the list and the parser diverge, what you pick from the list won't work.
    #[test]
    fn the_catalogue_covers_every_command_the_parser_takes() {
        for (name, _) in catalogue(crate::lang::Lang::Ko) {
            let got = parse(name);
            assert!(got.is_some(), "the parser does not know {name}");
            assert!(!matches!(got, Some(Command::Unknown(_))), "{name} falls through as unknown");
        }
    }

    /// **Projects are now made from a form in the list** — no separate command needed.
    #[test]
    fn project_is_not_a_command_anymore() {
        // Unknown commands aren't swallowed silently — it must say what's wrong.
        assert_eq!(parse("/project"), Some(Command::Unknown("project".into())));
        assert_eq!(parse("/project 새 프로젝트"), Some(Command::Unknown("project".into())));
        for (name, _) in
            catalogue(crate::lang::Lang::Ko).iter().chain(catalogue(crate::lang::Lang::En).iter())
        {
            assert_ne!(*name, "/project", "/project is still in the list");
        }
    }

    /// The help text comes from the list — written by hand, one would go stale.
    #[test]
    fn the_help_text_lists_everything_in_the_catalogue() {
        let help = help_text(crate::lang::Lang::Ko);
        for (name, _) in catalogue(crate::lang::Lang::Ko) {
            assert!(help.contains(name), "{name} is missing from the help");
        }
    }

    /// **Keys must be on the screen.** A README isn't opened when you need it.
    #[test]
    fn the_help_text_also_lists_the_keys() {
        let help = help_text(crate::lang::Lang::Ko);
        for (key, _) in keys(crate::lang::Lang::Ko) {
            assert!(help.contains(key), "{key} is missing from the help");
        }
    }

    /// For `/grants`, viewing and closing are different commands — you shouldn't close while trying to view the list.
    #[test]
    fn grants_lists_by_default_and_closes_only_when_asked() {
        assert_eq!(parse("/grants"), Some(Command::Grants));
        assert_eq!(parse("/grants list"), Some(Command::Grants));
        assert_eq!(parse("/grants close"), Some(Command::GrantsClose));
        assert_eq!(parse("/grants clear"), Some(Command::GrantsClose));
    }

    /// `/jobs` only looks and stops. **An argument it doesn't know never falls to the stopping side.**
    #[test]
    fn jobs_lists_and_stops_but_never_guesses() {
        assert_eq!(parse("/jobs"), Some(Command::Jobs(None)));
        assert_eq!(parse("/jobs list"), Some(Command::Jobs(None)));
        assert_eq!(parse("/jobs stop b1"), Some(Command::Jobs(Some("b1".into()))));
        assert_eq!(parse("/jobs kill b2"), Some(Command::Jobs(Some("b2".into()))));
        assert_eq!(parse("/jobs stop"), Some(Command::Unknown("jobs stop".into())));
        assert_eq!(parse("/jobs killall"), Some(Command::Unknown("jobs killall".into())));
    }

    /// When it's unclear what's being asked, **don't pick just anything.** Falling into the closing side would be the worst.
    #[test]
    fn an_unknown_grants_argument_does_nothing() {
        assert_eq!(parse("/grants 다열어"), Some(Command::Unknown("grants 다열어".into())));
    }

    /// `/account` shows by default; only an explicit `logout` forgets the credentials.
    #[test]
    fn account_shows_by_default_and_logs_out_only_when_asked() {
        assert_eq!(parse("/account"), Some(Command::Account(None)));
        assert_eq!(
            parse("/account logout"),
            Some(Command::Account(Some(AccountAction::Logout)))
        );
        assert_eq!(
            parse("/account log out"),
            Some(Command::Account(Some(AccountAction::Logout)))
        );
        assert_eq!(
            parse("/account 로그아웃"),
            Some(Command::Account(Some(AccountAction::Logout)))
        );
        // An unknown argument never falls into the logging-out side.
        assert_eq!(parse("/account stop"), Some(Command::Unknown("account stop".into())));
    }

    /// The familiar name varies from person to person. Not finding how to quit would be a problem.
    #[test]
    fn quitting_answers_to_the_usual_names() {
        for text in ["/quit", "/exit", "/q"] {
            assert_eq!(parse(text), Some(Command::Quit), "{text}");
        }
    }

    #[test]
    fn changes_answers_to_diff_too() {
        assert_eq!(parse("/changes"), Some(Command::Changes));
        assert_eq!(parse("/diff"), Some(Command::Changes));
    }
}
