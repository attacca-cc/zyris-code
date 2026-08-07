//! Whether to run a tool or refuse. **The decision is a single pure function.**
//!
//! Since the executing side is our node, we decide whether to run it — attacca doesn't
//! know about this decision, so the server doesn't need to be fixed. `mode.rs`'s two get their meaning here.
//!
//! **There is one policy for the outside: `/config`'s directory access.**
//!
//! capkit's path resolution is not a jail (`path.rs`: "the root is a default, not a jail"),
//! so absolute paths simply escape the working directory. If launched from `~/zyris-code`,
//! only that and below should be touched; anything outside follows the setting — `deny`
//! (the default) refuses it, `allow` runs it. The approval window that used to ask a human
//! per directory is gone (2026-08-07 user decision) — asking broke the flow, and the
//! setting says the same thing without the interruption.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::{Config, DirAccess};
use crate::mode::Mode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub capability: String,
    pub tool: String,
    /// What it targets. Used to tell the screen what is running.
    pub target: String,
    /// Path leading outside the working directory. The policy decides what happens to it.
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
    /// **Must be a sentence the agent can read and change its behavior by.** Silently returning an empty result
    /// makes it think the tool is broken and try the same thing another way.
    Refuse(String),
}

/// The target that marks a `wait.until` call as a probe.
///
/// **The plan-mode decision reads this.** `until` splits on its arguments — asking about something
/// already running is a read, but a probe runs a command. `decide` never looks at arguments, so
/// `target_of` carries that fact in the target instead.
pub const PROBE_TARGET: &str = "probe";

/// Read-only capabilities. They pass even in plan mode — **you must see before you can plan.**
fn only_reads(call: &Call) -> bool {
    let tool = call.tool.as_str();
    match call.capability.as_str() {
        "file_io" | "skill" | "search" => true,
        // Peeking at a PTY is reading. Opening and writing are not.
        "terminal" => matches!(tool, "read" | "screen"),
        // work creates work on the **server**, not this computer. Only the two that look are reads —
        // waking a sub-agent is exactly what plan mode is meant to stop.
        "work" => matches!(tool, "status" | "list"),
        // Only looking passes. `start` runs a command, and `stop` kills a running build — an
        // **irreversible write**. `until` runs a command only when it is a probe.
        "wait" => match tool {
            "list" | "logs" => true,
            "until" => call.target != PROBE_TARGET,
            _ => false,
        },
        _ => false,
    }
}

pub fn decide(mode: Mode, config: &Config, call: &Call) -> Decision {
    // Plan mode comes first. Refusing over something immutable is wasted effort.
    if mode == Mode::Plan && !only_reads(call) {
        return Decision::Refuse(
            "계획 모드입니다. 지금은 파일을 바꾸거나 명령을 돌릴 수 없습니다. \
             무엇을 할지 먼저 말해 주세요."
                .into(),
        );
    }
    // **Outside the working directory, the setting decides.** `deny` (the default) refuses;
    // `allow` runs it. Nothing inside is ever refused — refusing every time only broke the
    // flow (2026-08-02).
    if let Some(path) = &call.outside {
        if config.dir_access == DirAccess::Deny {
            return Decision::Refuse(format!(
                "`{}`은(는) 작업 디렉터리 밖이라 만질 수 없습니다 — 설정의 \
                 '다른 디렉토리 접근'이 거부로 되어 있습니다. `/config dir allow`로 \
                 허용하거나, 작업 디렉터리 안에서 할 수 있는 길을 찾아 주세요.",
                path.display()
            ));
        }
    }
    Decision::Run
}

/// Argument names that may hold a path.
///
/// **Free-form strings like `command` don't go here** — reading `/tmp` in `ls /tmp` as a path
/// would trip on every command. The shell is handled separately (`escaping_path`).
const PATH_KEYS: &[&str] =
    &["path", "cwd", "file", "dir", "directory", "root", "source", "destination", "target_path"];

/// Whether this call touches outside the working directory. Returns the first path leaving it.
///
/// **The shell cannot be fully blocked.** `terminal.exec`'s command is an arbitrary program, and
/// there is no way to read the text and know what it touches — one `sh -c` line does anything. What
/// this does is filter visible absolute paths — a net to catch **accidentally leaving**, not a wall.
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
    // Visible paths inside a shell command. Looks after stripping quotes and common separators.
    //
    // **`wait` takes the same command text.** `wait.start` only puts it in the background — what
    // runs is the very same shell, and so is `wait.until`'s probe. Leaving those two out here makes
    // them a back door around the whole fence — wrapping a capability in `Gate` is not enough,
    // because **the gate only sees the tools it knows about**.
    let runs_a_shell =
        matches!((capability, tool), ("terminal", "exec") | ("wait", "start") | ("wait", "until"));
    if runs_a_shell {
        let command = args.get("command").and_then(Value::as_str).unwrap_or_default();
        for word in command.split_whitespace() {
            let bare = word.trim_matches(|c| matches!(c, '\'' | '"' | '(' | ')' | ';' | ',' | '`'));
            if !(bare.starts_with('/') || bare.starts_with("~/") || bare.contains("../")) {
                continue;
            }
            // Asking about things pointing at executables like `/bin/ls` would trip on every command.
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

/// Pulls from the arguments the target used to tell the screen what is running.
pub fn target_of(capability: &str, tool: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match (capability, tool) {
        // The command's first word.
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
        // Things that continue an already-open PTY target that PTY.
        ("terminal", _) => args.get("pty").and_then(Value::as_str).unwrap_or_default().to_string(),
        // Backgrounding it also goes by the command's first word — `cargo` reads better on screen.
        ("wait", "start") => {
            let first = s("command").split_whitespace().next().unwrap_or_default().to_string();
            if first.is_empty() {
                s("label")
            } else {
                first
            }
        }
        // **Whether it is a probe splits the plan-mode decision** (`only_reads`).
        ("wait", "until") => {
            if args.get("command").and_then(Value::as_str).is_some_and(|c| !c.is_empty()) {
                PROBE_TARGET.to_string()
            } else {
                format!("{}{}", s("job"), s("work"))
            }
        }
        ("wait", _) => s("job"),
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

    /// **Plan mode can't wake sub-agents.** `work.start` creates work on the server and opens
    /// a worktree per task — exactly what plan mode is meant to stop.
    #[test]
    fn planning_mode_may_look_at_works_but_not_start_one() {
        let config = Config::default();
        for read in ["status", "list"] {
            let seen = decide(Mode::Plan, &config, &call("work", read, ""));
            assert_eq!(seen, Decision::Run, "looking is allowed in plan mode too: {read}");
        }
        for write in ["start", "say", "stop", "resume"] {
            let seen = decide(Mode::Plan, &config, &call("work", write, ""));
            assert!(matches!(seen, Decision::Refuse(_)), "it passed in plan mode: {write}");
        }
    }

    /// **`wait.start`'s command text is the very same thing `terminal.exec` takes.**
    /// Leaving it out here makes a back door around the whole fence — wrapping the capability in
    /// `Gate` is not enough. **The gate only sees the tools it knows about.**
    #[test]
    fn starting_a_job_outside_the_working_dir_is_caught() {
        let args = json!({ "command": "cat /etc/shadow" });
        assert!(escaping_path(root(), "wait", "start", &args).is_some());
        let inside = json!({ "command": "cargo build" });
        assert_eq!(escaping_path(root(), "wait", "start", &inside), None);
        // Leaving through `cwd` is the same.
        assert!(escaping_path(root(), "wait", "start", &json!({ "cwd": "/etc" })).is_some());
    }

    /// A probe runs a command too. Waiting on a background job does not.
    #[test]
    fn a_probe_command_gets_the_same_fence() {
        let probe = json!({ "command": "ls /etc/ssh" });
        assert!(escaping_path(root(), "wait", "until", &probe).is_some());
        assert_eq!(escaping_path(root(), "wait", "until", &json!({ "job": "b1" })), None);
        assert_eq!(escaping_path(root(), "wait", "until", &json!({ "work": "w_1" })), None);
    }

    /// Plan mode means "do nothing yet". **Only looking passes.**
    #[test]
    fn planning_mode_refuses_start_but_answers_list() {
        let config = Config::default();
        let seen = |tool: &str, args: Value| {
            decide(Mode::Plan, &config, &call("wait", tool, &target_of("wait", tool, &args)))
        };
        assert_eq!(seen("list", json!({})), Decision::Run);
        assert_eq!(seen("logs", json!({ "job": "b1" })), Decision::Run);
        assert_eq!(seen("until", json!({ "job": "b1" })), Decision::Run);
        assert_eq!(seen("until", json!({ "work": "w_1" })), Decision::Run);
        for refused in [
            seen("start", json!({ "command": "ls" })),
            seen("until", json!({ "command": "gh run view" })),
            seen("stop", json!({ "job": "b1" })),
        ] {
            assert!(matches!(refused, Decision::Refuse(_)), "passed in plan mode: {refused:?}");
        }
    }

    /// The target that says on screen what is running.
    #[test]
    fn the_target_of_a_job_is_its_command_or_its_id() {
        assert_eq!(target_of("wait", "start", &json!({ "command": "cargo build" })), "cargo");
        assert_eq!(target_of("wait", "logs", &json!({ "job": "b1" })), "b1");
        assert_eq!(target_of("wait", "stop", &json!({ "job": "b2" })), "b2");
        assert_eq!(target_of("wait", "until", &json!({ "job": "b1" })), "b1");
    }

    /// **Plan is the only blocking mode.** work·job modes only decide where my words go, and
    /// tool decisions must match the normal mode — if they blocked too, a job left running would
    /// be unable to do anything the moment it called this node's tools.
    #[test]
    fn only_planning_mode_holds_tools_back() {
        let config = Config::default();
        let calls = [
            call("terminal", "exec", "ls"),
            call("code_edit", "write", "a.rs"),
            call("work", "start", ""),
            call("file_io", "read", "a.rs"),
        ];
        for mode in [Mode::Job, Mode::Work, Mode::Job] {
            for c in &calls {
                assert_eq!(decide(mode, &config, c), Decision::Run, "{mode:?} blocked it: {c:?}");
            }
        }
        // The comparison spot — plan mode blocks the same call.
        assert!(matches!(
            decide(Mode::Plan, &config, &call("code_edit", "write", "a.rs")),
            Decision::Refuse(_)
        ));
    }

    /// **The fence is independent of mode.** All four refuse outside when the setting is deny —
    /// if work·job modes were the hole, just switching modes would open the whole computer.
    #[test]
    fn every_mode_refuses_outside_when_denied() {
        let outside = out("file_io", "read", "/etc/passwd");
        for mode in Mode::ALL {
            let seen = decide(mode, &Config::default(), &outside);
            // Plan mode already refuses before that (writes) or here (reads). Not passing is enough.
            assert_ne!(seen, Decision::Run, "{mode:?} just walked outside");
        }
    }

    /// In normal mode it **just runs without asking.** With no path there is nothing for the fence to catch —
    /// this capability has only one decision: the mode.
    #[test]
    fn a_work_call_has_no_path_to_escape_from() {
        let seen = decide(Mode::Job, &Config::default(), &call("work", "start", ""));
        assert_eq!(seen, Decision::Run);
    }

    /// Nothing inside is ever refused. Refusing every time only broke the flow.
    #[test]
    fn nothing_inside_the_working_directory_is_ever_refused() {
        let c = Config::default();
        for call in [
            out("code_edit", "edit", "src/app.rs"),
            out("code_edit", "write", &format!("{ROOT}/깊은/곳/새것.rs")),
            out("file_io", "read", "Cargo.toml"),
            out("terminal", "exec", "."),
        ] {
            assert_eq!(decide(Mode::Job, &c, &call), Decision::Run, "{call:?}");
        }
    }

    /// **Leaving refuses when the setting is deny.** That is the default, and the point of
    /// setting a working directory.
    #[test]
    fn leaving_the_working_directory_refuses_when_denied() {
        let c = Config::default();
        for path in ["/home/ruma/attacca/Cargo.toml", "../attacca/x.rs", "/etc/passwd"] {
            let call = out("code_edit", "edit", path);
            assert!(call.outside.is_some(), "{path} was not caught as leaving");
            assert!(matches!(decide(Mode::Job, &c, &call), Decision::Refuse(_)), "{path}");
        }
    }

    /// **Even reading refuses.** What the user wanted to stop was "read everything then touch".
    #[test]
    fn even_reading_outside_refuses_when_denied() {
        let call = out("file_io", "read", "/home/ruma/attacca/.env");
        assert!(matches!(decide(Mode::Job, &Config::default(), &call), Decision::Refuse(_)));
    }

    /// **`allow` runs it** — no per-directory grants, no approval window. That is the whole
    /// point of the setting.
    #[test]
    fn allowing_makes_outside_run() {
        let c = Config { dir_access: DirAccess::Allow, ..Config::default() };
        for path in ["/home/ruma/attacca/Cargo.toml", "/etc/passwd"] {
            let call = out("file_io", "read", path);
            assert!(call.outside.is_some(), "{path} was not caught as leaving");
            assert_eq!(decide(Mode::Job, &c, &call), Decision::Run, "{path}");
        }
    }

    /// Plan mode blocks changes. **Refusing over something immutable is wasted effort.**
    #[test]
    fn planning_refuses_before_the_directory_policy() {
        let call = out("code_edit", "edit", "/home/ruma/attacca/x.rs");
        let Decision::Refuse(why) = decide(Mode::Plan, &Config::default(), &call) else {
            panic!("it passed in plan mode");
        };
        assert!(why.contains("계획"), "{why}");
    }

    /// Reading passes in plan mode too — but the directory policy still applies outside.
    #[test]
    fn reading_works_while_planning_but_still_respects_the_policy() {
        let c = Config::default();
        assert_eq!(decide(Mode::Plan, &c, &out("search", "grep", ".")), Decision::Run);
        assert!(matches!(
            decide(Mode::Plan, &c, &out("file_io", "read", "/etc/passwd")),
            Decision::Refuse(_)
        ));
        let allow = Config { dir_access: DirAccess::Allow, ..Config::default() };
        assert_eq!(
            decide(Mode::Plan, &allow, &out("file_io", "read", "/etc/passwd")),
            Decision::Run
        );
    }

    /// Absolute paths inside a shell command are caught too. **A net, not a wall** — catches accidental leaving.
    #[test]
    fn an_absolute_path_inside_a_shell_command_is_caught() {
        let args = json!({"command": "cat /home/ruma/attacca/.env"});
        let got = escaping_path(root(), "terminal", "exec", &args);
        assert_eq!(got, Some(PathBuf::from("/home/ruma/attacca/.env")));
    }

    /// Climbing out with `..` is caught too.
    #[test]
    fn climbing_out_with_dot_dot_is_caught() {
        let args = json!({"command": "ls ../attacca/crates"});
        assert!(escaping_path(root(), "terminal", "exec", &args).is_some());
    }

    /// **Asking about executable paths trips on every command.** `/usr/bin/env` isn't leaving.
    #[test]
    fn a_program_path_is_not_treated_as_leaving() {
        for command in ["/usr/bin/env cargo test", "/bin/sh -c 'ls'", "cargo build -j2"] {
            let args = json!({ "command": command });
            assert_eq!(escaping_path(root(), "terminal", "exec", &args), None, "{command}");
        }
    }

    /// Giving an outside `cwd` is leaving outright.
    #[test]
    fn running_with_an_outside_cwd_is_caught() {
        let args = json!({"command": "ls", "cwd": "/home/ruma/attacca"});
        assert!(escaping_path(root(), "terminal", "exec", &args).is_some());
    }

    /// Inside commands must just run — if caught here, nothing could run.
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
