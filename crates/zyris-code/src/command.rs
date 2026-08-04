//! 슬래시 명령. **여기는 순수하다** — 글자를 읽어 무엇을 할지만 정한다.
//!
//! 실행은 `app.rs`가 한다. 어떤 것은 상태만 만지고(`/mode`), 어떤 것은 서버나 디스크가
//! 필요하다(`/agent`·`/undo`) — 그 갈래도 저쪽에서 진다.
//!
//! **명령은 서버로 가지 않는다.** 오타 하나가 크레딧을 쓰면 안 되고, `/mode`처럼
//! 이 노드만 아는 것을 서버에 물어봐야 알 이유도 없다.

use crate::mode::Mode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    /// 인자가 없으면 지금 모드를 말하고, 있으면 바꾼다.
    Mode(Option<Mode>),
    /// 화면 말. 인자가 없으면 지금 것을 말하고, 있으면 바꾼다.
    /// **모르는 말은 `Unknown`으로 떨어뜨리지 않는다** — 무엇이 잘못됐는지 그 자리에서
    /// 말해 줘야 다시 칠 수 있다(`Lang(None)`은 "지금 것"이라는 뜻이라 자리가 없다).
    Lang(Option<crate::lang::Lang>),
    /// `/lang 일본어`처럼 못 알아들은 말.
    LangUnknown(String),
    Mcp,
    Skills,
    /// 세션에 실린 `CLAUDE.md`·`AGENTS.md`가 무엇인지.
    Rules,
    Cwd,
    /// 인자가 없으면 목록을 열고, 있으면 그 이름으로 바로 간다.
    Agent(Option<String>),
    Plugin(Plugin),
    Undo,
    Clear,
    /// 작업 디렉터리 밖으로 열어 둔 곳을 보여준다.
    Grants,
    /// 그것을 전부 닫는다.
    GrantsClose,
    /// 이 디렉터리에서 무엇을 바꿨는가.
    Changes,
    /// 끈다. **도는 턴이 있으면 서버에서도 멈춘다**(`app.rs`의 `turn_to_stop`).
    Quit,
    /// 모르는 것. **서버로 보내지 않고 무엇이 있는지 알려 준다.**
    Unknown(String),
}

/// `/plugin`이 무엇을 하는가.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plugin {
    /// 받아 둔 것을 보여준다.
    List,
    /// `owner/repo`나 clone할 수 있는 주소에서 받는다.
    Add(String),
    Remove(String),
    /// 이름이 없으면 받아 둔 것 전부.
    Update(Option<String>),
    /// 무엇을 하라는 건지 모르겠다. **조용히 삼키지 않는다.**
    Unknown(String),
}

/// 이 글이 명령인가.
///
/// **경로가 명령으로 먹히면 안 된다.** `/home/ruma/...`를 그대로 치는 일이 실제로 있고,
/// 그걸 명령으로 삼키면 메시지가 서버에 안 간다. 그래서 셋을 요구한다 —
/// `/`로 시작하고, 다음 글자가 ASCII 알파벳이고, 첫 낱말에 `/`가 더 없을 것.
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
    // **이름에 공백이 있는 에이전트가 있다**("Main Agent"). 첫 낱말에서 자르면 못 고른다.
    let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
    Some(match name {
        "help" | "h" => Command::Help,
        "mode" => Command::Mode(match arg {
            "" => None,
            other => match mode_named(other) {
                Some(m) => Some(m),
                // 모르는 모드 이름은 조용히 무시하지 않는다 — 안 바뀐 줄 모른다.
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
        // 여는 것은 승인 화면의 `a` 하나뿐이라, 여기서는 보고 닫기만 한다.
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
        "quit" | "exit" | "q" => Command::Quit,
        other => Command::Unknown(other.to_string()),
    })
}

/// `/plugin` 뒤에 오는 것. **인자에 공백이 있을 수 있다**(주소는 없지만 이름은 있을 수 있다).
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
        // **`/plugin owner/repo`도 받는다.** `add`를 빼먹는 것이 제일 흔한 실수다.
        (what, "") if what.contains('/') || what.contains("://") => Plugin::Add(what.to_string()),
        (what, _) => Plugin::Unknown(format!("plugin {what}")),
    }
}

/// 화면에 보이는 한글 이름과 영문 이름을 모두 받는다 — 화면은 한글이지만 손에 익은 쪽이
/// 사람마다 다르다.
///
/// `work`·`job`은 **화면에서는 번역하지 않는다**(`lang::mode_work`) — attacca가 제 화면에서
/// 부르는 이름 그대로여야 저쪽 목록에서 찾을 수 있다. 그래도 여기서는 `작업`·`잡`도
/// 받아 준다: **받는 쪽을 넓히는 것은 공짜지만, 화면에 두 이름이 돌아다니는 것은 그렇지 않다.**
fn mode_named(s: &str) -> Option<Mode> {
    match s {
        "기본" | "normal" | "default" => Some(Mode::Normal),
        "계획" | "plan" => Some(Mode::Plan),
        "작업" | "work" => Some(Mode::Work),
        "잡" | "job" => Some(Mode::Job),
        _ => None,
    }
}

/// `/`를 쳤을 때 뜨는 목록. **`/help`가 찍는 것도 이것이다** — 둘이 갈라지면 하나가 낡는다.
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
            ("/grants", "밖으로 열어 둔 디렉터리 (close로 전부 닫습니다)"),
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
            ("/grants", "Directories opened outside the working directory (close shuts them all)"),
            ("/changes", "Files changed in this directory"),
            ("/undo", "Undo the last edit"),
            ("/clear", "Clear the screen (the thread itself is untouched)"),
            ("/quit", "Quit. A running turn is stopped on the server too"),
        ],
    }
}

/// 알아 둘 키. **화면 안에 없으면 없는 것이다** — README는 필요한 순간에 안 열린다.
///
/// 정본은 `app.rs`의 `on_key`이고 여기는 그중 사람이 외워야 할 것만 옮겨 적은 것이다.
/// 키를 고쳤으면 여기도 고칠 것.
pub fn keys(lang: crate::lang::Lang) -> Vec<(&'static str, &'static str)> {
    use crate::lang::Lang;
    match lang {
        Lang::Ko => vec![
            ("Shift+Tab", "모드 바꾸기 (기본 → 계획 → work → job)"),
            ("←", "프로젝트·쓰레드 목록 (입력란이 비었을 때)"),
            ("↑ ↓", "보낸 말 되살리기"),
            ("Ctrl+O", "작업 카드 접기·펴기"),
            ("Ctrl+B", "사이드바"),
            ("Ctrl+U", "친 것 모두 지우기"),
            ("Esc", "도는 턴 멈추기"),
            ("Ctrl+C", "멈추기 → 한 번 더 누르면 끝내기"),
            ("y n a", "승인 화면에서 허락·거절·이 디렉터리 열기"),
            ("드래그", "고른 글이 놓는 순간 클립보드로"),
        ],
        Lang::En => vec![
            ("Shift+Tab", "Switch mode (normal → plan → work → job)"),
            ("←", "Project and thread list (when the input box is empty)"),
            ("↑ ↓", "Bring back something you sent"),
            ("Ctrl+O", "Fold or unfold a work card"),
            ("Ctrl+B", "Sidebar"),
            ("Ctrl+U", "Clear what you typed"),
            ("Esc", "Stop the running turn"),
            ("Ctrl+C", "Stop, then press again to quit"),
            ("y n a", "On the approval screen: allow, deny, open this directory"),
            ("drag", "Selected text goes to the clipboard when you let go"),
        ],
    }
}

/// `/help`가 찍는 글.
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
        // **경로가 명령으로 먹히면 안 된다.** `/home/...`을 치는 일이 실제로 있다.
        assert!(parse("/home/ruma/zyris-code를 봐 줘").is_none());
        assert!(parse("/usr/bin/env").is_none());
        // 숫자로 시작하는 것도 명령이 아니다 — `/2026-08-02` 같은 것.
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

    /// 영문도 받는다 — 화면은 한글이지만 손에 익은 쪽이 사람마다 다르다.
    #[test]
    fn mode_also_takes_the_english_names() {
        assert_eq!(parse("/mode plan"), Some(Command::Mode(Some(Mode::Plan))));
        assert_eq!(parse("/mode job"), Some(Command::Mode(Some(Mode::Job))));
        assert_eq!(parse("/mode normal"), Some(Command::Mode(Some(Mode::Normal))));
    }

    /// **모르는 모드 이름을 조용히 무시하면 안 바뀐 줄 모른다.**
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

    /// **`add`를 빼먹는 것이 제일 흔한 실수다.** 주소처럼 생겼으면 받는 것으로 친다.
    #[test]
    fn a_bare_source_is_taken_as_add() {
        assert_eq!(
            parse("/plugin owner/repo"),
            Some(Command::Plugin(Plugin::Add("owner/repo".into())))
        );
    }

    /// 무엇을 하라는 건지 모를 때 **조용히 삼키면** 아무 일도 안 일어난 줄 안다.
    #[test]
    fn an_incomplete_plugin_command_says_what_is_missing() {
        let Some(Command::Plugin(Plugin::Unknown(why))) = parse("/plugin add") else {
            panic!("모르는 것으로 안 떨어진다");
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

    /// 이름에 공백이 있는 에이전트가 있다("Main Agent"). 첫 낱말에서 자르면 못 고른다.
    #[test]
    fn agent_keeps_the_whole_name() {
        assert_eq!(parse("/agent Main Agent"), Some(Command::Agent(Some("Main Agent".into()))));
        assert_eq!(parse("/agent"), Some(Command::Agent(None)));
    }

    /// 앞뒤 공백은 치다 남은 것이지 뜻이 아니다.
    #[test]
    fn surrounding_whitespace_does_not_change_the_meaning() {
        assert_eq!(parse("  /cwd  "), Some(Command::Cwd));
        assert_eq!(parse("/agent   Main Agent  "), Some(Command::Agent(Some("Main Agent".into()))));
    }

    /// **모르는 명령을 서버로 보내면 안 된다.** 오타 한 번이 크레딧을 쓴다.
    #[test]
    fn an_unknown_command_stays_local() {
        assert_eq!(parse("/nope"), Some(Command::Unknown("nope".into())));
    }

    /// 목록과 파서가 갈라지면 목록에서 고른 것이 안 먹는다.
    #[test]
    fn the_catalogue_covers_every_command_the_parser_takes() {
        for (name, _) in catalogue(crate::lang::Lang::Ko) {
            let got = parse(name);
            assert!(got.is_some(), "{name}을 파서가 모른다");
            assert!(!matches!(got, Some(Command::Unknown(_))), "{name}이 모르는 것으로 떨어진다");
        }
    }

    /// **프로젝트는 이제 목록에서 양식으로 만든다** — 명령이 따로 필요 없다.
    #[test]
    fn project_is_not_a_command_anymore() {
        // 모르는 명령은 조용히 삼키지 않는다 — 무엇이 잘못됐는지 말해 줘야 한다.
        assert_eq!(parse("/project"), Some(Command::Unknown("project".into())));
        assert_eq!(parse("/project 새 프로젝트"), Some(Command::Unknown("project".into())));
        for (name, _) in
            catalogue(crate::lang::Lang::Ko).iter().chain(catalogue(crate::lang::Lang::En).iter())
        {
            assert_ne!(*name, "/project", "목록에 /project가 남아 있다");
        }
    }

    /// 도움말은 목록에서 나온다 — 손으로 적으면 하나가 낡는다.
    #[test]
    fn the_help_text_lists_everything_in_the_catalogue() {
        let help = help_text(crate::lang::Lang::Ko);
        for (name, _) in catalogue(crate::lang::Lang::Ko) {
            assert!(help.contains(name), "{name}이 도움말에 없다");
        }
    }

    /// **키는 화면 안에 있어야 한다.** README는 필요한 순간에 안 열린다.
    #[test]
    fn the_help_text_also_lists_the_keys() {
        let help = help_text(crate::lang::Lang::Ko);
        for (key, _) in keys(crate::lang::Lang::Ko) {
            assert!(help.contains(key), "{key}가 도움말에 없다");
        }
    }

    /// `/grants`는 보는 것과 닫는 것이 다른 명령이다 — 목록을 보려다 닫으면 안 된다.
    #[test]
    fn grants_lists_by_default_and_closes_only_when_asked() {
        assert_eq!(parse("/grants"), Some(Command::Grants));
        assert_eq!(parse("/grants list"), Some(Command::Grants));
        assert_eq!(parse("/grants close"), Some(Command::GrantsClose));
        assert_eq!(parse("/grants clear"), Some(Command::GrantsClose));
    }

    /// 무엇을 하라는 건지 모를 때 **아무거나 고르면 안 된다.** 닫는 쪽으로 떨어지면 최악이다.
    #[test]
    fn an_unknown_grants_argument_does_nothing() {
        assert_eq!(parse("/grants 다열어"), Some(Command::Unknown("grants 다열어".into())));
    }

    /// 손에 익은 이름이 사람마다 다르다. 끝내는 것을 못 찾으면 곤란하다.
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
