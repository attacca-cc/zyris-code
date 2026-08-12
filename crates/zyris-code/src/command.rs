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
    /// `/mcp` on its own lists; `/mcp on|off <name>` decides whether a **discovered** server may
    /// start (`mcp::discovery`). What was written for this app directly is not switchable — it was
    /// written for this app.
    Mcp(Option<McpSwitch>),
    Skills,
    /// Which `CLAUDE.md`·`AGENTS.md` are loaded into the session.
    Rules,
    Cwd,
    /// Without an argument, opens the list; with one, goes straight to that name.
    Agent(Option<String>),
    Plugin(Plugin),
    Undo,
    Clear,
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
    /// GitHub — who this node is signed in as, signing in, and signing out.
    Github(Option<AccountAction>),
    /// The current session's picture — thread, project, agent, mode, and usage.
    Status,
    /// The settings — with no argument the panel; with `option value`, sets that one.
    Config(Option<ConfigAction>),
    /// Drops the connection so the runner redials.
    ///
    /// **The way back from a connection the server no longer routes to.** attacca's registry is
    /// `insert(node_id, connection)`, so a second window with the same credentials displaces the
    /// first — and if that window then closes, the registry points at a dead connection and every
    /// tool call sits pending forever. Nothing detects it: zyris discards the heartbeat the server
    /// advertises, has no ping/pong, and `conn.closed()` never fires for a merely unrouted socket.
    /// Redialling re-announces, which puts this connection back in the registry.
    Reconnect,
    /// Something unknown. **Not sent to the server; tells what's available instead.**
    Unknown(String),
}

/// What `/account` can do after the command word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountAction {
    /// Forgets the stored credentials. The next launch asks for approval again.
    ///
    /// The role is GitHub's — `/account logout` passes `Role::User` and ignores it, since the
    /// node has only ever had one identity.
    Logout(crate::github::auth::Role),
    /// Signs in. **Only GitHub uses this** — the node's own credentials are made on first launch,
    /// so there is nothing for `/account login` to do.
    Login(crate::github::auth::Role),
}

/// What `/config` can set after the command word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigAction {
    /// 다른 디렉토리 접근 — whether tools may touch outside the working directory.
    Dir(crate::config::DirAccess),
    /// 화면 색 — which palette the screen is drawn in.
    Theme(crate::config::ThemeChoice),
    /// The UI language.
    Lang(crate::lang::Lang),
    /// 기본 모드 — the mode the app opens in. `None` turns the setting off.
    Mode(Option<crate::mode::Mode>),
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
    // **The command word is recognized from `COMMANDS`** — the same table `/help` and the `/`-list
    // are built from, so recognition and the help can't drift apart. Only argument handling lives here.
    let Some(spec) = COMMANDS.iter().find(|c| c.matches(name)) else {
        return Some(Command::Unknown(name.to_string()));
    };
    Some(match spec.name {
        "/help" => Command::Help,
        "/mode" => Command::Mode(match arg {
            "" => None,
            other => match mode_named(other) {
                Some(m) => Some(m),
                // An unknown mode name isn't silently ignored — otherwise the user wouldn't know it didn't change.
                None => return Some(Command::Unknown(format!("mode {other}"))),
            },
        }),
        "/mcp" => Command::Mcp(mcp_switch(arg)),
        "/skills" => Command::Skills,
        "/rules" => Command::Rules,
        "/cwd" => Command::Cwd,
        "/agent" => Command::Agent(some(arg)),
        "/plugin" => Command::Plugin(plugin_action(arg)),
        "/undo" => Command::Undo,
        "/clear" => Command::Clear,
        "/reconnect" => Command::Reconnect,
        "/config" => match arg {
            "" => Command::Config(None),
            given => match given.split_once(char::is_whitespace) {
                Some((option, value)) => match option {
                    "dir" | "directory" | "디렉토리" | "디렉터리" => {
                        match crate::config::DirAccess::parse(value) {
                            Some(access) => Command::Config(Some(ConfigAction::Dir(access))),
                            None => return Some(Command::Unknown(format!("config dir {value}"))),
                        }
                    }
                    "theme" | "colour" | "color" | "색" | "테마" => {
                        match crate::config::ThemeChoice::parse(value) {
                            Some(theme) => Command::Config(Some(ConfigAction::Theme(theme))),
                            None => return Some(Command::Unknown(format!("config theme {value}"))),
                        }
                    }
                    "lang" | "language" | "언어" => match crate::lang::Lang::parse(value) {
                        Some(lang) => Command::Config(Some(ConfigAction::Lang(lang))),
                        None => return Some(Command::Unknown(format!("config lang {value}"))),
                    },
                    "mode" | "모드" => match mode_named(value) {
                        Some(mode) => Command::Config(Some(ConfigAction::Mode(Some(mode)))),
                        // **`off` turns the setting off** — without it there is no way
                        // back to the launch default.
                        None if matches!(value, "off" | "끔" | "없음") => {
                            Command::Config(Some(ConfigAction::Mode(None)))
                        }
                        None => return Some(Command::Unknown(format!("config mode {value}"))),
                    },
                    other => return Some(Command::Unknown(format!("config {other}"))),
                },
                None => return Some(Command::Unknown(format!("config {arg}"))),
            },
        },
        "/changes" => Command::Changes,
        // Looking and stopping only. Starting one stays the agent's `wait.start` alone.
        "/jobs" => match arg.split_once(' ') {
            None if arg.is_empty() || arg == "list" => Command::Jobs(None),
            Some(("stop" | "kill", id)) if !id.trim().is_empty() => {
                Command::Jobs(Some(id.trim().to_string()))
            }
            _ => Command::Unknown(format!("jobs {arg}")),
        },
        // `/github login [reviewer]` · `/github logout [reviewer]`. **The role rides on the verb**
        // rather than being a separate command, because it is the same act either way — the only
        // difference is which slot the token lands in.
        "/github" => {
            let (verb, who) = match arg.split_once(char::is_whitespace) {
                Some((v, r)) => (v, r.trim()),
                None => (arg, ""),
            };
            let role = crate::github::auth::Role::parse(who);
            match (verb, role) {
                ("" | "status", _) => Command::Github(None),
                (_, None) => return Some(Command::Unknown(format!("github {verb} {who}"))),
                ("login" | "signin" | "로그인", Some(role)) => {
                    Command::Github(Some(AccountAction::Login(role)))
                }
                ("logout" | "로그아웃", Some(role)) => {
                    Command::Github(Some(AccountAction::Logout(role)))
                }
                (other, _) => return Some(Command::Unknown(format!("github {other}"))),
            }
        }
        "/account" => match arg {
            "" => Command::Account(None),
            "logout" | "log out" | "로그아웃" => {
                Command::Account(Some(AccountAction::Logout(crate::github::auth::Role::User)))
            }
            other => return Some(Command::Unknown(format!("account {other}"))),
        },
        "/status" => Command::Status,
        "/quit" => Command::Quit,
        // **Only when a `COMMANDS` entry is missing its dispatch arm** — the coverage test catches it.
        _ => unreachable!("no dispatch arm for {}", spec.name),
    })
}

/// What comes after `/plugin`. **Arguments can contain spaces** (addresses don't, but names can).
/// `/mcp on <name>` · `/mcp off <name>`. Anything else lists.
///
/// **An unknown word is not silently taken as a list.** `/mcp of playwright` would then look like
/// it worked and change nothing.
fn mcp_switch(arg: &str) -> Option<McpSwitch> {
    let (verb, name) = match arg.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim()),
        None => (arg, ""),
    };
    match (verb, name) {
        ("" | "list", _) => None,
        ("on" | "off", "") => Some(McpSwitch::Unknown(format!("mcp {verb}"))),
        ("on", name) => Some(McpSwitch::On(name.to_string())),
        ("off", name) => Some(McpSwitch::Off(name.to_string())),
        (other, _) => Some(McpSwitch::Unknown(format!("mcp {other}"))),
    }
}

/// What `/mcp on|off` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpSwitch {
    /// Let this discovered server start from the next launch.
    On(String),
    Off(String),
    /// Neither, and **said rather than swallowed** — a typo that changes nothing silently reads
    /// as the setting having been taken.
    Unknown(String),
}

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

/// Accepts both the Korean names shown on screen and the English ones — the screen is
/// Korean, but which one is familiar varies from person to person.
fn mode_named(s: &str) -> Option<Mode> {
    match s {
        // **Both Korean names are accepted** — `일반` is what the screen shows now,
        // `기본` is what it used to show and still feels natural to type.
        "일반" | "기본" | "normal" | "default" => Some(Mode::Normal),
        "계획" | "plan" => Some(Mode::Plan),
        "일" | "work" => Some(Mode::Work),
        "작업" | "잡" | "job" => Some(Mode::Job),
        _ => None,
    }
}

/// One slash command: what it's called, what else it answers to, and what it does.
///
/// **The single source of truth** for a command's name, aliases, and both-language descriptions.
/// `parse` recognizes a command through it, and `/help` and the `/`-list are built from it — so
/// adding a command here (plus its dispatch arm in `parse`) is all it takes, and the two can't drift.
pub struct CommandSpec {
    /// How it's shown in the list, with the leading `/` — `/mode`.
    pub name: &'static str,
    /// The tokens (without the `/`) the parser also accepts — e.g. `skill` for `/skills`.
    pub aliases: &'static [&'static str],
    pub note_ko: &'static str,
    pub note_en: &'static str,
}

impl CommandSpec {
    /// The token (without the leading `/`) that names this command.
    pub fn token(&self) -> &'static str {
        self.name.strip_prefix('/').unwrap_or(self.name)
    }
    /// Whether `token` names this command — its own name or one of its aliases.
    pub fn matches(&self, token: &str) -> bool {
        self.token() == token || self.aliases.contains(&token)
    }
}

/// The one command list. **Order is the order the `/`-list and `/help` are shown in.**
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "/help",
        aliases: &["h"],
        note_ko: "쓸 수 있는 명령",
        note_en: "What you can type",
    },
    CommandSpec {
        name: "/mode",
        aliases: &[],
        note_ko: "모드를 보거나 바꿉니다 (일반·계획·일·작업)",
        note_en: "Show or change the mode (normal / plan / work / job)",
    },
    CommandSpec {
        name: "/config",
        aliases: &[],
        note_ko: "설정 — 다른 디렉토리 접근(allow·deny)·언어·기본 모드",
        note_en: "Settings — directory access (allow / deny), language, default mode",
    },
    CommandSpec {
        name: "/agent",
        aliases: &[],
        note_ko: "에이전트를 고릅니다. 다음 메시지에서 새 쓰레드가 열립니다",
        note_en: "Pick an agent. A new thread opens with your next message",
    },
    CommandSpec {
        name: "/mcp",
        aliases: &[],
        note_ko: "붙은 MCP 서버와 도구 수",
        note_en: "MCP servers that connected, and how many tools each brought",
    },
    CommandSpec {
        name: "/skills",
        aliases: &["skill"],
        note_ko: "쓸 수 있는 스킬",
        note_en: "Skills available here",
    },
    CommandSpec {
        name: "/plugin",
        aliases: &["plugins"],
        note_ko: "플러그인을 받고 지웁니다 (add·remove·update)",
        note_en: "Install and remove plugins (add / remove / update)",
    },
    CommandSpec {
        name: "/rules",
        aliases: &["claude", "agents"],
        note_ko: "이 쓰레드에 실린 CLAUDE.md·AGENTS.md",
        note_en: "The CLAUDE.md and AGENTS.md loaded into this thread",
    },
    CommandSpec {
        name: "/cwd",
        aliases: &["pwd"],
        note_ko: "도구가 상대경로를 푸는 자리",
        note_en: "Where tools resolve relative paths",
    },
    CommandSpec {
        name: "/reconnect",
        aliases: &[],
        note_ko: "다시 붙습니다. 도구 호출이 응답 없이 멈춰 있을 때",
        note_en: "Attach again — when tool calls sit there with no answer",
    },
    CommandSpec {
        name: "/github",
        aliases: &["gh"],
        note_ko: "GitHub 계정을 잇고 끊습니다 (login · login reviewer · logout)",
        note_en: "Connect or disconnect GitHub (login · login reviewer · logout)",
    },
    CommandSpec {
        name: "/account",
        aliases: &[],
        note_ko: "계정 정보를 보고, 로그아웃합니다 (logout)",
        note_en: "Show account info, or log out (logout)",
    },
    CommandSpec {
        name: "/status",
        aliases: &["info"],
        note_ko: "지금 세션·에이전트·모드·사용량을 한눈에",
        note_en: "Session, agent, mode and usage at a glance",
    },
    CommandSpec {
        name: "/jobs",
        aliases: &["job"],
        note_ko: "배경에서 도는 작업 (stop <id>로 멈춥니다)",
        note_en: "Background jobs (stop <id> kills one)",
    },
    CommandSpec {
        name: "/changes",
        aliases: &["changed", "diff"],
        note_ko: "이 디렉터리에서 바꾼 파일",
        note_en: "Files changed in this directory",
    },
    CommandSpec {
        name: "/undo",
        aliases: &[],
        note_ko: "마지막 편집을 되돌립니다",
        note_en: "Undo the last edit",
    },
    CommandSpec {
        name: "/clear",
        aliases: &[],
        note_ko: "화면의 대화를 지웁니다 (쓰레드는 그대로입니다)",
        note_en: "Clear the screen (the thread itself is untouched)",
    },
    CommandSpec {
        name: "/quit",
        aliases: &["exit", "q"],
        note_ko: "끝냅니다. 도는 턴이 있으면 서버에서도 멈춥니다",
        note_en: "Quit. A running turn is stopped on the server too",
    },
];

/// The list that appears when `/` is typed. **`/help` prints the same thing.**
///
/// **Not hand-written** — built from `COMMANDS`, the one place a command's name and description
/// live, so the list and the parser can't drift apart.
pub fn catalogue(lang: crate::lang::Lang) -> Vec<(&'static str, &'static str)> {
    use crate::lang::Lang;
    COMMANDS
        .iter()
        .map(|c| {
            let note = match lang {
                Lang::Ko => c.note_ko,
                Lang::En => c.note_en,
            };
            (c.name, note)
        })
        .collect()
}

/// The names this app answers to itself, without the slash.
///
/// **A plugin may not take one of these.** `/help` has to stay `/help`, whatever a plugin calls a
/// file of its own — so a colliding plugin command is namespaced instead (`plugin::commands`).
pub fn builtin_names() -> Vec<&'static str> {
    COMMANDS.iter().map(|c| c.name.trim_start_matches('/')).collect()
}

/// Keys worth knowing. **If it isn't on the screen, it doesn't exist** — a README isn't opened when you need it.
///
/// The canonical source is `on_key` in `app.rs`; this is only a transcription of what a human should memorize.
/// If a key changes, change it here too.
pub fn keys(lang: crate::lang::Lang) -> Vec<(&'static str, &'static str)> {
    use crate::lang::Lang;
    match lang {
        Lang::Ko => vec![
            ("Shift+Tab", "모드 바꾸기 (일반 → 계획 → 일 → 작업)"),
            (
                "Shift+Enter · Alt+Enter",
                "줄바꿈 (Shift+Enter는 키티 키보드 프로토콜 지원 터미널에서만)",
            ),
            ("←", "프로젝트·쓰레드 목록 (입력란이 비었을 때)"),
            ("↑ ↓", "보낸 말 되살리기"),
            ("Ctrl+O", "작업 카드 접기·펴기"),
            ("Ctrl+T", "할 일 목록 펴기·접기 (활동 줄을 눌러도 됩니다)"),
            ("Ctrl+U · Ctrl+K", "커서 앞쪽·뒤쪽 지우기 (Ctrl+U가 친 것 다 지우기입니다)"),
            ("Ctrl+W · Alt+Backspace", "낱말 지우기 (앞은 띄어쓰기까지, 뒤는 한 조각씩)"),
            ("Ctrl+Y", "방금 지운 것 되붙이기"),
            ("Ctrl+A · Ctrl+E", "줄 맨 앞·맨 뒤로"),
            ("Alt+← Alt+→", "낱말 단위로 이동 (Alt+B · Alt+F도 됩니다)"),
            ("Esc", "도는 턴 멈추기"),
            ("Ctrl+C", "멈추기 → 한 번 더 누르면 끝내기"),
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
            ("Ctrl+T", "Unfold or fold the todo list (clicking the activity line does it too)"),
            ("Ctrl+U · Ctrl+K", "Cut back to the start or on to the end (Ctrl+U clears a draft)"),
            ("Ctrl+W · Alt+Backspace", "Delete a word (whole, or one segment at a time)"),
            ("Ctrl+Y", "Put back what you just cut"),
            ("Ctrl+A · Ctrl+E", "Start and end of the line"),
            ("Ctrl+← Ctrl+→", "Move by word (Alt+B and Alt+F work too)"),
            ("Esc", "Stop the running turn"),
            ("Ctrl+C", "Stop, then press again to quit"),
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
        assert_eq!(parse("/mode 일반"), Some(Command::Mode(Some(Mode::Normal))));
        assert_eq!(parse("/mode 기본"), Some(Command::Mode(Some(Mode::Normal))));
        assert_eq!(parse("/mode 계획"), Some(Command::Mode(Some(Mode::Plan))));
        assert_eq!(parse("/mode 일"), Some(Command::Mode(Some(Mode::Work))));
        assert_eq!(parse("/mode 작업"), Some(Command::Mode(Some(Mode::Job))));
        assert_eq!(parse("/mode 잡"), Some(Command::Mode(Some(Mode::Job))));
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
    ///
    /// **Both languages.** Only the Korean list was checked, so an entry added to one and not the
    /// other would sit in the list doing nothing for half the users.
    #[test]
    fn the_catalogue_covers_every_command_the_parser_takes() {
        for lang in [crate::lang::Lang::Ko, crate::lang::Lang::En] {
            for (name, _) in catalogue(lang) {
                let got = parse(name);
                assert!(got.is_some(), "{lang:?}: the parser does not know {name}");
                assert!(
                    !matches!(got, Some(Command::Unknown(_))),
                    "{lang:?}: {name} falls through as unknown"
                );
            }
        }
    }

    /// The registry is the source for the help and the picker, so every name and alias in it
    /// must actually be a command the parser answers to — a token added to the registry but not
    /// to `parse` would sit in the list doing nothing.
    #[test]
    fn every_registry_name_and_alias_is_something_the_parser_knows() {
        for spec in COMMANDS {
            for token in std::iter::once(spec.token()).chain(spec.aliases.iter().copied()) {
                let text = format!("/{token}");
                let got = parse(&text);
                assert!(got.is_some(), "{text} is not a command");
                assert!(
                    !matches!(got, Some(Command::Unknown(_))),
                    "{text} falls through as unknown"
                );
            }
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

    /// **Language lives in `/config` now** — `/lang` is gone, so typing it must say what exists.
    #[test]
    fn lang_is_not_a_command_anymore() {
        assert_eq!(parse("/lang"), Some(Command::Unknown("lang".into())));
        assert_eq!(parse("/lang ko"), Some(Command::Unknown("lang".into())));
        for (name, _) in
            catalogue(crate::lang::Lang::Ko).iter().chain(catalogue(crate::lang::Lang::En).iter())
        {
            assert_ne!(*name, "/lang", "/lang is still in the list");
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

    /// `/config` with no argument opens the panel; `option value` sets that one.
    #[test]
    fn config_without_an_argument_opens_the_panel() {
        assert_eq!(parse("/config"), Some(Command::Config(None)));
    }

    #[test]
    fn config_takes_option_and_value_in_both_languages() {
        assert_eq!(
            parse("/config dir allow"),
            Some(Command::Config(Some(ConfigAction::Dir(crate::config::DirAccess::Allow))))
        );
        assert_eq!(
            parse("/config 디렉토리 거부"),
            Some(Command::Config(Some(ConfigAction::Dir(crate::config::DirAccess::Deny))))
        );
        assert_eq!(
            parse("/config lang ko"),
            Some(Command::Config(Some(ConfigAction::Lang(crate::lang::Lang::Ko))))
        );
        assert_eq!(
            parse("/config mode 계획"),
            Some(Command::Config(Some(ConfigAction::Mode(Some(Mode::Plan)))))
        );
        assert_eq!(
            parse("/config mode off"),
            Some(Command::Config(Some(ConfigAction::Mode(None))))
        );
    }

    /// An unknown option or value must not be swallowed — the user wouldn't know it didn't change.
    #[test]
    fn an_unknown_config_argument_is_reported() {
        assert!(matches!(parse("/config dir 빠르게"), Some(Command::Unknown(_))));
        assert!(matches!(parse("/config 뭔가 값"), Some(Command::Unknown(_))));
        assert!(matches!(parse("/config mode"), Some(Command::Unknown(_))));
        assert!(matches!(parse("/config dir"), Some(Command::Unknown(_))));
    }

    /// For `/jobs`, looking and stopping are different commands — you shouldn't close while trying to view the list.
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
    /// **The two GitHub slots are told apart by the word after the verb.** Getting this wrong
    /// would sign the wrong account in, which looks exactly like it worked.
    #[test]
    fn github_login_takes_a_role_and_refuses_a_word_it_does_not_know() {
        use crate::github::auth::Role;
        assert_eq!(parse("/github"), Some(Command::Github(None)));
        assert_eq!(
            parse("/github login"),
            Some(Command::Github(Some(AccountAction::Login(Role::User))))
        );
        assert_eq!(
            parse("/github login reviewer"),
            Some(Command::Github(Some(AccountAction::Login(Role::Reviewer))))
        );
        assert_eq!(
            parse("/github logout reviewer"),
            Some(Command::Github(Some(AccountAction::Logout(Role::Reviewer))))
        );
        assert_eq!(parse("/gh login 리뷰어"), parse("/github login reviewer"));
        // **A role nobody knows is refused, not taken as the person.** Signing the wrong account
        // in is the one mistake here that looks like success.
        assert_eq!(
            parse("/github login 아무거나"),
            Some(Command::Unknown("github login 아무거나".into()))
        );
    }

    #[test]
    fn account_shows_by_default_and_logs_out_only_when_asked() {
        assert_eq!(parse("/account"), Some(Command::Account(None)));
        assert_eq!(
            parse("/account logout"),
            Some(Command::Account(Some(AccountAction::Logout(crate::github::auth::Role::User))))
        );
        assert_eq!(
            parse("/account log out"),
            Some(Command::Account(Some(AccountAction::Logout(crate::github::auth::Role::User))))
        );
        assert_eq!(
            parse("/account 로그아웃"),
            Some(Command::Account(Some(AccountAction::Logout(crate::github::auth::Role::User))))
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

    /// `/status` is the one command; `/info` is the familiar alias. Arguments are ignored —
    /// there is only one picture to show.
    #[test]
    fn status_answers_to_info_too() {
        assert_eq!(parse("/status"), Some(Command::Status));
        assert_eq!(parse("/info"), Some(Command::Status));
        assert_eq!(parse("/status 지금"), Some(Command::Status));
    }
}
