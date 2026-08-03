//! 도구를 돌릴지 물을지 거부할지. **판정은 순수 함수 하나다.**
//!
//! 실행하는 쪽이 우리 노드이므로 돌릴지 말지는 우리가 정한다 — attacca는 이 결정을
//! 모르고, 그래서 서버를 고칠 필요가 없다. `mode.rs`의 둘이 여기서 뜻을 얻는다.
//!
//! **묻는 자리는 하나뿐이다: 작업 디렉터리 밖.**
//!
//! capkit의 경로 해석은 감옥이 아니라서(`path.rs`: "the root is a default, not a jail")
//! 절대경로는 작업 디렉터리를 그냥 벗어난다. `~/zyris-code`에서 띄웠으면 거기와 그 아래만
//! 만지는 것이 맞고, 그 밖은 **모드와 무관하게** 사람에게 묻는다. 안쪽 일에는 아무것도
//! 묻지 않는다 — 매번 묻는 것은 흐름을 끊기만 했다(2026-08-02).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::mode::Mode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub capability: String,
    pub tool: String,
    /// 무엇을 대상으로 하는가. 화면에 무엇이 도는지 말할 때 쓴다.
    pub target: String,
    /// 작업 디렉터리 밖으로 나가는 경로. 있으면 승인이 필요하다.
    pub outside: Option<PathBuf>,
}

impl Call {
    pub fn new(capability: &str, tool: &str, target: String) -> Call {
        Call { capability: capability.to_string(), tool: tool.to_string(), target, outside: None }
    }

    pub fn leaving(mut self, outside: Option<PathBuf>) -> Call {
        self.outside = outside;
        self
    }

    pub fn tool_key(&self) -> String {
        format!("{}.{}", self.capability, self.tool)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Run,
    /// 사람에게 묻는다. 답이 올 때까지 그 호출은 막혀 있다.
    Ask,
    /// **에이전트가 읽고 행동을 바꿀 수 있는 문장이어야 한다.** 조용히 빈 결과를 주면
    /// 도구가 고장 난 줄 알고 다른 길로 같은 일을 시도한다.
    Refuse(String),
}

/// 이번 세션에서 열어 둔 바깥 디렉터리.
///
/// **디스크에 남기지 않는다.** 앞으로 뭐든 하겠다는 백지수표가 되면 안 되고, 무엇을
/// 허용해 뒀는지 모르는 상태가 제일 나쁘다. 앱을 끄면 잊는다.
#[derive(Debug, Clone, Default)]
pub struct Grants {
    /// **파일 하나가 아니라 디렉터리를 연다.** 남의 리포를 한 번 허락하면 그 안의 파일
    /// 하나하나를 다시 묻는 것은 쓸 수 없다.
    roots: HashSet<PathBuf>,
}

impl Grants {
    /// 이 경로가 든 디렉터리를 통째로 연다. 경로가 디렉터리면 그것 자체를 연다.
    pub fn allow_under(&mut self, path: &Path) {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        self.roots.insert(dir.to_path_buf());
    }

    pub fn covers(&self, path: &Path) -> bool {
        self.roots.iter().any(|r| path.starts_with(r))
    }

    /// 지금 열어 둔 곳. **볼 수 없는 허용은 없는 것이나 마찬가지로 위험하다** — 한 번
    /// 누른 `a`가 세션 내내 살아 있는데 무엇을 열어 뒀는지 알 길이 없으면 안 된다.
    ///
    /// 순서를 정해 준다. `HashSet`이 주는 순서는 실행마다 달라서, 같은 목록을 두 번
    /// 봤을 때 줄이 뒤바뀌면 뭔가 바뀐 줄 안다.
    pub fn roots(&self) -> Vec<&Path> {
        let mut out: Vec<&Path> = self.roots.iter().map(PathBuf::as_path).collect();
        out.sort_unstable();
        out
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// 전부 닫는다. 닫은 개수를 준다.
    ///
    /// **하나씩 고르게 하지 않는다.** 열어 둔 곳이 여럿이면 어느 것이 위험한지 고르는
    /// 것보다 다 닫고 필요할 때 다시 허락하는 편이 빠르고 확실하다.
    pub fn close_all(&mut self) -> usize {
        let n = self.roots.len();
        self.roots.clear();
        n
    }
}

/// 읽기만 하는 캐퍼빌리티. 계획 모드에서도 통한다 — **계획을 세우려면 먼저 봐야 한다.**
fn only_reads(capability: &str, tool: &str) -> bool {
    match capability {
        "file_io" | "skill" | "search" => true,
        // PTY를 들여다보는 것은 읽기다. 여는 것과 쓰는 것은 아니다.
        "terminal" => matches!(tool, "read" | "screen"),
        // work는 이 컴퓨터가 아니라 **서버에 일을 만든다.** 들여다보는 둘만 읽기다 —
        // 서브에이전트를 깨우는 것은 계획 모드가 막으려는 바로 그것이다.
        "work" => matches!(tool, "status" | "list"),
        _ => false,
    }
}

pub fn decide(mode: Mode, grants: &Grants, call: &Call) -> Decision {
    // 계획 모드가 먼저다. 바꿀 수 없는 것을 두고 승인을 묻는 것은 헛수고다.
    if mode == Mode::Plan && !only_reads(&call.capability, &call.tool) {
        return Decision::Refuse(
            "계획 모드입니다. 지금은 파일을 바꾸거나 명령을 돌릴 수 없습니다. \
             무엇을 할지 먼저 말해 주세요."
                .into(),
        );
    }
    // **밖으로 나가면 읽기여도 묻는다.** 작업 디렉터리를 정한 뜻이 그것이다.
    if let Some(path) = &call.outside {
        if !grants.covers(path) {
            return Decision::Ask;
        }
    }
    Decision::Run
}

/// 경로가 들어 있을 만한 인자 이름.
///
/// **`command` 같은 자유 문자열은 여기 넣지 않는다** — `ls /tmp`의 `/tmp`를 경로로 읽으면
/// 명령마다 걸린다. 셸은 따로 본다(`escaping_path`).
const PATH_KEYS: &[&str] =
    &["path", "cwd", "file", "dir", "directory", "root", "source", "destination", "target_path"];

/// 이 호출이 작업 디렉터리 밖을 건드리는가. 나가는 첫 경로를 준다.
///
/// **셸은 완전히 막을 수 없다.** `terminal.exec`의 명령문은 임의의 프로그램이고, 그것을
/// 글자로 읽어 어디를 만질지 아는 방법은 없다 — `sh -c` 한 줄이면 무엇이든 한다. 여기서
/// 하는 것은 눈에 보이는 절대경로를 걸러 **실수로 나가는 것**을 잡는 그물이지 벽이 아니다.
pub fn escaping_path(root: &Path, capability: &str, tool: &str, args: &Value) -> Option<PathBuf> {
    let outside = |p: &str| {
        let full = zyris_capkit::resolve_under(root, p);
        (!full.starts_with(root)).then_some(full)
    };

    for key in PATH_KEYS {
        if let Some(p) = args.get(*key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
            if let Some(out) = outside(p) {
                return Some(out);
            }
        }
    }
    // 셸 명령문 안의 눈에 띄는 경로. 따옴표와 흔한 구분자를 떼고 본다.
    if (capability, tool) == ("terminal", "exec") {
        let command = args.get("command").and_then(Value::as_str).unwrap_or_default();
        for word in command.split_whitespace() {
            let bare = word.trim_matches(|c| matches!(c, '\'' | '"' | '(' | ')' | ';' | ',' | '`'));
            if !(bare.starts_with('/') || bare.starts_with("~/") || bare.contains("../")) {
                continue;
            }
            // `/bin/ls`처럼 실행 파일을 가리키는 것까지 물으면 명령마다 걸린다.
            if bare.starts_with("/usr/") || bare.starts_with("/bin/") || bare.starts_with("/sbin/")
            {
                continue;
            }
            let bare = match bare.strip_prefix("~/") {
                Some(rest) => home().join(rest).to_string_lossy().into_owned(),
                None => bare.to_string(),
            };
            if let Some(out) = outside(&bare) {
                return Some(out);
            }
        }
    }
    None
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// 화면에 무엇이 도는지 말할 때 쓸 대상을 인자에서 뽑는다.
pub fn target_of(capability: &str, tool: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match (capability, tool) {
        // 명령의 첫 낱말.
        ("terminal", "exec") => {
            s("command").split_whitespace().next().unwrap_or_default().to_string()
        }
        ("terminal", "open") | ("terminal", "open_stream") => {
            let shell = s("shell");
            if shell.is_empty() {
                "기본 셸".into()
            } else {
                shell
            }
        }
        // 이미 열린 PTY를 이어 쓰는 것들은 그 PTY가 대상이다.
        ("terminal", _) => args.get("pty").and_then(Value::as_str).unwrap_or_default().to_string(),
        _ => s("path"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ROOT: &str = "/home/ruma/zyris-code";

    fn root() -> &'static Path {
        Path::new(ROOT)
    }

    fn call(cap: &str, tool: &str, target: &str) -> Call {
        Call::new(cap, tool, target.to_string())
    }

    fn out(cap: &str, tool: &str, path: &str) -> Call {
        call(cap, tool, path).leaving(escaping_path(root(), cap, tool, &json!({"path": path})))
    }

    /// **계획 모드는 서브에이전트를 못 깨운다.** `work.start`는 서버에 일을 만들고 태스크
    /// 마다 워크트리를 열게 한다 — 계획 모드가 막으려는 것이 정확히 그것이다.
    #[test]
    fn planning_mode_may_look_at_works_but_not_start_one() {
        let grants = Grants::default();
        for read in ["status", "list"] {
            let seen = decide(Mode::Plan, &grants, &call("work", read, ""));
            assert_eq!(seen, Decision::Run, "계획 모드에서도 들여다보는 것은 된다: {read}");
        }
        for write in ["start", "say", "stop", "resume"] {
            let seen = decide(Mode::Plan, &grants, &call("work", write, ""));
            assert!(matches!(seen, Decision::Refuse(_)), "계획 모드에서 통과했다: {write}");
        }
    }

    /// **막는 모드는 계획 하나뿐이다.** work·job 모드는 내 말이 어디로 가는지만 정하고
    /// 도구 판정은 기본 모드와 똑같아야 한다 — 거기서도 막으면 job을 걸어 놓고 그 job이
    /// 이 노드의 도구를 부르는 순간 아무것도 못 하게 된다.
    #[test]
    fn only_planning_mode_holds_tools_back() {
        let grants = Grants::default();
        let calls = [
            call("terminal", "exec", "ls"),
            call("code_edit", "write", "a.rs"),
            call("work", "start", ""),
            call("file_io", "read", "a.rs"),
        ];
        for mode in [Mode::Job, Mode::Work, Mode::Job] {
            for c in &calls {
                assert_eq!(decide(mode, &grants, c), Decision::Run, "{mode:?}가 막았다: {c:?}");
            }
        }
        // 견주는 자리 — 같은 호출을 계획 모드는 막는다.
        assert!(matches!(
            decide(Mode::Plan, &grants, &call("code_edit", "write", "a.rs")),
            Decision::Refuse(_)
        ));
    }

    /// **울타리는 모드와 무관하다.** 작업 디렉터리 밖은 넷 다 묻는다 — work·job 모드가
    /// 그 구멍이 되면 모드를 바꾸는 것만으로 온 컴퓨터가 열린다.
    #[test]
    fn every_mode_still_asks_before_leaving_the_working_directory() {
        let outside = out("file_io", "read", "/etc/passwd");
        for mode in Mode::ALL {
            let seen = decide(mode, &Grants::default(), &outside);
            // 계획 모드는 그 전에 이미 거부하거나(쓰기) 묻는다(읽기). 통과만 아니면 된다.
            assert_ne!(seen, Decision::Run, "{mode:?}가 밖으로 그냥 나갔다");
        }
    }

    /// 기본 모드에서는 **묻지 않고 그냥 돈다.** 경로가 없으니 울타리에 걸릴 것도 없다 —
    /// 이 캐퍼빌리티에 대한 판단은 모드 하나뿐이다.
    #[test]
    fn a_work_call_has_no_path_to_escape_from() {
        let seen = decide(Mode::Job, &Grants::default(), &call("work", "start", ""));
        assert_eq!(seen, Decision::Run);
    }

    /// 안쪽 일에는 아무것도 묻지 않는다. 매번 묻는 것은 흐름을 끊기만 했다.
    #[test]
    fn nothing_inside_the_working_directory_is_ever_asked() {
        let g = Grants::default();
        for c in [
            out("code_edit", "edit", "src/app.rs"),
            out("code_edit", "write", &format!("{ROOT}/깊은/곳/새것.rs")),
            out("file_io", "read", "Cargo.toml"),
            out("terminal", "exec", "."),
        ] {
            assert_eq!(decide(Mode::Job, &g, &c), Decision::Run, "{c:?}");
        }
    }

    /// **밖으로 나가면 묻는다.** 작업 디렉터리를 정한 뜻이 그것이다.
    #[test]
    fn leaving_the_working_directory_asks() {
        let g = Grants::default();
        for path in ["/home/ruma/attacca/Cargo.toml", "../attacca/x.rs", "/etc/passwd"] {
            let c = out("code_edit", "edit", path);
            assert!(c.outside.is_some(), "{path}가 밖으로 안 잡혔다");
            assert_eq!(decide(Mode::Job, &g, &c), Decision::Ask, "{path}");
        }
    }

    /// **읽기여도 묻는다.** 사용자가 막고 싶어한 것은 "다 읽고 건들이는" 쪽이다.
    #[test]
    fn even_reading_outside_asks() {
        let c = out("file_io", "read", "/home/ruma/attacca/.env");
        assert_eq!(decide(Mode::Job, &Grants::default(), &c), Decision::Ask);
    }

    /// **한 번 열면 그 디렉터리째로 열린다.** 파일 하나하나를 다시 묻는 것은 쓸 수 없다.
    #[test]
    fn allowing_once_opens_the_whole_directory() {
        let mut g = Grants::default();
        g.allow_under(Path::new("/home/ruma/attacca/Cargo.toml"));
        for path in ["/home/ruma/attacca/Cargo.toml", "/home/ruma/attacca/README.md"] {
            let c = out("file_io", "read", path);
            assert_eq!(decide(Mode::Job, &g, &c), Decision::Run, "{path}");
        }
        // 옆집까지 열리면 안 된다.
        let c = out("file_io", "read", "/home/ruma/prompts/x.yml");
        assert_eq!(decide(Mode::Job, &g, &c), Decision::Ask);
    }

    /// **무엇을 열어 뒀는지 볼 수 있어야 한다.** 순서까지 정해져 있어야 두 번 봤을 때
    /// 줄이 뒤바뀌어 뭔가 바뀐 줄 아는 일이 없다.
    #[test]
    fn what_is_open_can_be_listed_in_a_settled_order() {
        let mut g = Grants::default();
        assert!(g.is_empty());
        g.allow_under(Path::new("/home/ruma/prompts/x.yml"));
        g.allow_under(Path::new("/home/ruma/attacca/Cargo.toml"));

        assert_eq!(
            g.roots(),
            vec![Path::new("/home/ruma/attacca"), Path::new("/home/ruma/prompts")]
        );
    }

    /// 닫으면 다시 묻는다. 닫았는데 계속 통하면 닫은 것이 아니다.
    #[test]
    fn closing_everything_makes_it_ask_again() {
        let mut g = Grants::default();
        g.allow_under(Path::new("/home/ruma/attacca/Cargo.toml"));
        let c = out("file_io", "read", "/home/ruma/attacca/Cargo.toml");
        assert_eq!(decide(Mode::Job, &g, &c), Decision::Run);

        assert_eq!(g.close_all(), 1);
        assert!(g.is_empty());
        assert_eq!(decide(Mode::Job, &g, &c), Decision::Ask);
    }

    /// 계획 모드는 바꾸는 것을 막는다. **바꿀 수 없는 것을 두고 묻는 것은 헛수고다.**
    #[test]
    fn planning_refuses_before_it_would_ask() {
        let c = out("code_edit", "edit", "/home/ruma/attacca/x.rs");
        let Decision::Refuse(why) = decide(Mode::Plan, &Grants::default(), &c) else {
            panic!("계획 모드에서 통과했다");
        };
        assert!(why.contains("계획"), "{why}");
    }

    /// 계획 모드에서도 읽기는 통한다 — 다만 밖이면 그때는 묻는다.
    #[test]
    fn reading_works_while_planning_but_still_asks_outside() {
        let g = Grants::default();
        assert_eq!(decide(Mode::Plan, &g, &out("search", "grep", ".")), Decision::Run);
        assert_eq!(decide(Mode::Plan, &g, &out("file_io", "read", "/etc/passwd")), Decision::Ask);
    }

    /// 셸 명령문 안의 절대경로도 잡는다. **벽이 아니라 그물이다** — 실수로 나가는 것을 잡는다.
    #[test]
    fn an_absolute_path_inside_a_shell_command_is_caught() {
        let args = json!({"command": "cat /home/ruma/attacca/.env"});
        let got = escaping_path(root(), "terminal", "exec", &args);
        assert_eq!(got, Some(PathBuf::from("/home/ruma/attacca/.env")));
    }

    /// `..`로 거슬러 올라가는 것도 잡는다.
    #[test]
    fn climbing_out_with_dot_dot_is_caught() {
        let args = json!({"command": "ls ../attacca/crates"});
        assert!(escaping_path(root(), "terminal", "exec", &args).is_some());
    }

    /// **실행 파일 경로까지 물으면 명령마다 걸린다.** `/usr/bin/env`는 나가는 것이 아니다.
    #[test]
    fn a_program_path_is_not_treated_as_leaving() {
        for command in ["/usr/bin/env cargo test", "/bin/sh -c 'ls'", "cargo build -j2"] {
            let args = json!({ "command": command });
            assert_eq!(escaping_path(root(), "terminal", "exec", &args), None, "{command}");
        }
    }

    /// `cwd`를 밖으로 주는 것은 대놓고 나가는 것이다.
    #[test]
    fn running_with_an_outside_cwd_is_caught() {
        let args = json!({"command": "ls", "cwd": "/home/ruma/attacca"});
        assert!(escaping_path(root(), "terminal", "exec", &args).is_some());
    }

    /// 안쪽 명령은 그냥 돌아야 한다 — 여기서 걸리면 아무것도 못 한다.
    #[test]
    fn an_ordinary_command_inside_the_tree_is_not_flagged() {
        for command in ["cargo test -j2", "ls src/", "grep -rn draw ./crates"] {
            let args = json!({ "command": command });
            assert_eq!(escaping_path(root(), "terminal", "exec", &args), None, "{command}");
        }
    }

    #[test]
    fn the_target_of_a_command_is_its_first_word() {
        let args = json!({"command": "cargo build -j2"});
        assert_eq!(target_of("terminal", "exec", &args), "cargo");
    }

    #[test]
    fn the_target_of_an_edit_is_its_path() {
        let args = json!({"path": "src/app.rs", "old_string": "a"});
        assert_eq!(target_of("code_edit", "edit", &args), "src/app.rs");
    }
}
