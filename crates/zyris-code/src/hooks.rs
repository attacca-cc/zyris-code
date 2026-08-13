//! Commands a plugin asks to have run around a tool call.
//!
//! **This is the one part of a plugin that executes.** Everything else a plugin brings is text —
//! prompts, procedures, a server to talk to. A hook is somebody else's shell command running on
//! this machine, unattended, every time a tool fires. So the rules here are narrower than the rest
//! of the file would suggest:
//!
//! - **`PreToolUse` and `PostToolUse` only.** They sit on the one choke point every tool call
//!   already passes through (`tools::guard`), so there is nothing to thread through the app and no
//!   second path to keep in step. The rest of Claude Code's events describe a harness this app does
//!   not have.
//! - **A hook may refuse a call, and that is all it may change.** Exit code 2 blocks, with stderr as
//!   the reason. Anything else — a rewritten argument, an injected message — would mean a plugin
//!   silently steering the agent, and there would be no way to read back what happened.
//! - **A hook that hangs must not hang the turn.** attacca cuts a node call at 60s, so a hook gets
//!   a small budget of its own and a timeout counts as "said nothing".
//!
//! The matcher is a plain substring against the tool name, which is what the manifests in the wild
//! actually use (`"Bash"`, `"Edit|Write"`). A `|` means any of them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

/// How long one hook may take. **Small on purpose** — this is spent inside the tool call's own
/// budget, and a slow hook is indistinguishable to the person from a slow tool.
const BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// When a hook runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Before the tool runs. **This one can refuse.**
    Before,
    /// After it ran. Its verdict is not read — the call already happened.
    After,
}

impl When {
    fn from(name: &str) -> Option<When> {
        match name {
            "PreToolUse" => Some(When::Before),
            "PostToolUse" => Some(When::After),
            // Everything else in the format describes events this app does not have. **Skipped
            // quietly**: a plugin written for another harness is not a broken plugin.
            _ => None,
        }
    }
}

/// One command to run, and what it runs for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub when: When,
    /// Tool names this fires for. Empty means every tool.
    pub matches: Vec<String>,
    pub command: String,
    /// The plugin it came from, so a refusal can say who refused.
    pub plugin: String,
    /// Where to run it. The plugin's own directory, so `${CLAUDE_PLUGIN_ROOT}/x.sh` resolves.
    pub root: PathBuf,
}

/// What the tool names in these files mean here.
///
/// **The matchers were written against another harness's tool names**, and none of them is a
/// substring of ours — `Bash` appears nowhere in `terminal.exec`. Without this table every hook in
/// every plugin would match nothing at all and quietly do nothing, which is the worst of the
/// possible outcomes: the plugin looks installed and is not.
///
/// Left-hand side is theirs, lowercased; right-hand side is a fragment of ours.
const ALIASES: &[(&str, &[&str])] = &[
    ("bash", &["terminal.", "wait."]),
    ("edit", &["code_edit"]),
    ("multiedit", &["code_edit"]),
    ("write", &["code_edit"]),
    ("read", &["file_io", "skill."]),
    ("glob", &["search."]),
    ("grep", &["search."]),
    ("task", &["work."]),
    ("todowrite", &["todo"]),
];

impl Hook {
    /// Does this hook fire for `tool`?
    ///
    /// Three ways to match, in order: the name is one this app knows under another harness's name
    /// (`ALIASES`), the matcher is a substring of ours, or the matcher is `*`.
    ///
    /// **Erring towards firing is the right side to err on.** A hook that runs for one tool too
    /// many is visible and can be narrowed; one that never runs is indistinguishable from a plugin
    /// that was never installed.
    pub fn covers(&self, tool: &str) -> bool {
        if self.matches.is_empty() {
            return true;
        }
        let tool = tool.to_ascii_lowercase();
        self.matches.iter().any(|m| {
            let m = m.trim().to_ascii_lowercase();
            if m.is_empty() {
                return false;
            }
            if m == "*" {
                return true;
            }
            if let Some((_, ours)) = ALIASES.iter().find(|(theirs, _)| *theirs == m) {
                if ours.iter().any(|o| tool.contains(o)) {
                    return true;
                }
            }
            tool.contains(&m)
        })
    }
}

// ── The file shape ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct HooksFile {
    #[serde(default)]
    hooks: HashMap<String, Vec<MatcherFile>>,
}

#[derive(Debug, Deserialize)]
struct MatcherFile {
    #[serde(default)]
    matcher: String,
    #[serde(default)]
    hooks: Vec<CommandFile>,
}

#[derive(Debug, Deserialize)]
struct CommandFile {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    command: String,
}

/// Reads a plugin's `hooks/hooks.json`. **A missing or unreadable file is not an error** — almost
/// no plugin has one.
pub fn read(root: &Path, plugin: &str) -> Vec<Hook> {
    let at = root.join("hooks/hooks.json");
    let Ok(text) = std::fs::read_to_string(&at) else { return Vec::new() };
    let parsed: HooksFile = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("could not read {}: {e}", at.display());
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for (event, matchers) in parsed.hooks {
        let Some(when) = When::from(&event) else { continue };
        for matcher in matchers {
            let matches: Vec<String> = matcher
                .matcher
                .split('|')
                .map(str::trim)
                .filter(|m| !m.is_empty() && *m != "*")
                .map(str::to_string)
                .collect();
            for command in matcher.hooks {
                // Only `command` hooks exist in the format, but say so rather than running
                // something whose shape we did not check.
                if !command.kind.is_empty() && command.kind != "command" {
                    tracing::warn!("skipping a '{}' hook in {plugin}", command.kind);
                    continue;
                }
                if command.command.trim().is_empty() {
                    continue;
                }
                out.push(Hook {
                    when,
                    matches: matches.clone(),
                    command: command.command.clone(),
                    plugin: plugin.to_string(),
                    root: root.to_path_buf(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.command.cmp(&b.command));
    out
}

// ── Running them ─────────────────────────────────────────────────────────────────────────────

/// What the hooks had to say about a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to report. **The overwhelmingly common answer.**
    Fine,
    /// A hook refused, and this is the reason to hand back.
    Refused(String),
}

/// Runs every hook that covers `tool`, and reports the first refusal.
///
/// **The first refusal wins and the rest are not run.** Once the call is not happening there is
/// nothing left for the others to inspect, and running them anyway would mean side effects for a
/// call that never occurred.
pub async fn run(hooks: &[Hook], when: When, tool: &str, input: &Value) -> Verdict {
    for hook in hooks.iter().filter(|h| h.when == when && h.covers(tool)) {
        match one(hook, tool, input).await {
            Verdict::Fine => {}
            // **Only `Before` can refuse.** After the fact there is nothing to refuse; the call
            // already ran, and pretending otherwise would report a failure that did not happen.
            refused if when == When::Before => return refused,
            _ => {}
        }
    }
    Verdict::Fine
}

/// One hook. **Anything that goes wrong reads as "said nothing"** — a plugin whose hook is broken
/// must not be able to stop this app from working.
async fn one(hook: &Hook, tool: &str, input: &Value) -> Verdict {
    use tokio::io::AsyncWriteExt;

    let payload = json!({
        "hook_event_name": match hook.when { When::Before => "PreToolUse", When::After => "PostToolUse" },
        "tool_name": tool,
        "tool_input": input,
        "cwd": hook.root.to_string_lossy(),
    })
    .to_string();

    let mut command = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(&hook.command);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(&hook.command);
        c
    };
    // The variable every manifest in the wild uses to find its own scripts.
    command.env("CLAUDE_PLUGIN_ROOT", &hook.root);
    command.env("ZYRIS_PLUGIN_ROOT", &hook.root);
    command.current_dir(&hook.root);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command.kill_on_drop(true);

    let Ok(mut child) = command.spawn() else {
        tracing::warn!("could not run a hook from {}", hook.plugin);
        return Verdict::Fine;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes()).await;
        drop(stdin);
    }
    let finished = tokio::time::timeout(BUDGET, child.wait_with_output()).await;
    let Ok(Ok(output)) = finished else {
        // **A timeout says nothing rather than refusing.** A hook that hangs would otherwise take
        // every tool call down with it, and the person would have no idea why.
        tracing::warn!("a hook from {} did not finish in time", hook.plugin);
        return Verdict::Fine;
    };
    // Exit code 2 is the format's "block this". Every other code is the hook's own business.
    if output.status.code() == Some(2) {
        let why = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let why = if why.is_empty() { hook.command.clone() } else { why };
        return Verdict::Refused(format!("{} : {why}", hook.plugin));
    }
    Verdict::Fine
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, body: &str) {
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        std::fs::write(root.join("hooks/hooks.json"), body).unwrap();
    }

    const FILE: &str = r#"{"hooks":{
        "PreToolUse":[{"matcher":"Bash|Edit","hooks":[{"type":"command","command":"echo hi"}]}],
        "PostToolUse":[{"matcher":"","hooks":[{"type":"command","command":"echo bye"}]}],
        "SessionStart":[{"matcher":"","hooks":[{"type":"command","command":"echo no"}]}]
    }}"#;

    #[test]
    fn the_two_events_this_app_has_are_read_and_the_rest_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), FILE);
        let got = read(dir.path(), "p");
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().any(|h| h.when == When::Before && h.command == "echo hi"));
        assert!(got.iter().any(|h| h.when == When::After));
        // **A plugin written for another harness is not a broken plugin.**
        assert!(!got.iter().any(|h| h.command == "echo no"), "{got:?}");
    }

    /// **The names in these files were written for another harness.** Matching them exactly would
    /// mean no hook ever fired, which is worse than one firing a little eagerly.
    #[test]
    fn a_matcher_written_for_another_harness_still_finds_our_tool() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), FILE);
        let pre = read(dir.path(), "p").into_iter().find(|h| h.when == When::Before).unwrap();
        assert!(pre.covers("terminal.exec"), "a Bash matcher must reach the shell tool");
        assert!(pre.covers("code_edit.edit"), "an Edit matcher must reach the edit tool");
        assert!(!pre.covers("search.grep"), "it must not cover everything: {pre:?}");
    }

    #[test]
    fn an_empty_matcher_covers_every_tool() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), FILE);
        let post = read(dir.path(), "p").into_iter().find(|h| h.when == When::After).unwrap();
        assert!(post.covers("anything.at.all"));
    }

    #[test]
    fn a_plugin_with_no_hooks_file_is_ordinary() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path(), "p").is_empty());
        write(dir.path(), "not json");
        assert!(read(dir.path(), "p").is_empty(), "a broken file must not take the app down");
    }

    fn hook(command: &str, when: When, root: &Path) -> Hook {
        Hook {
            when,
            matches: Vec::new(),
            command: command.into(),
            plugin: "p".into(),
            root: root.to_path_buf(),
        }
    }

    /// **Exit code 2 is the one thing a hook can decide.** Everything else it might print is its
    /// own business.
    ///
    /// The command is a shell snippet, so it must be written per platform — `cmd /C` runs the
    /// hook on Windows and does not speak bash (`;`, `exit 2`, single quotes are all different).
    #[tokio::test]
    async fn a_hook_that_exits_two_refuses_the_call_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let command = if cfg!(windows) {
            "echo not on my watch 1>&2 & exit /b 2"
        } else {
            "echo 'not on my watch' >&2; exit 2"
        };
        let hooks = vec![hook(command, When::Before, dir.path())];
        let verdict = run(&hooks, When::Before, "terminal.exec", &json!({})).await;
        match verdict {
            Verdict::Refused(why) => assert!(why.contains("not on my watch"), "{why}"),
            other => panic!("it must refuse: {other:?}"),
        }
    }

    #[tokio::test]
    async fn every_other_exit_code_lets_the_call_through() {
        let dir = tempfile::tempdir().unwrap();
        for command in ["exit 0", "exit 1", "exit 7", "no-such-program-here"] {
            let hooks = vec![hook(command, When::Before, dir.path())];
            assert_eq!(
                run(&hooks, When::Before, "t", &json!({})).await,
                Verdict::Fine,
                "`{command}` must not block"
            );
        }
    }

    /// **After the fact there is nothing to refuse.** The call already ran, and reporting a failure
    /// would describe something that did not happen.
    #[tokio::test]
    async fn a_hook_that_runs_after_the_call_cannot_refuse_it() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![hook("exit 2", When::After, dir.path())];
        assert_eq!(run(&hooks, When::After, "t", &json!({})).await, Verdict::Fine);
    }

    /// **A hook that hangs must not take the turn with it.** attacca cuts a node call at 60s, and
    /// a tool that never answers is the worst shape a failure can take.
    #[tokio::test]
    async fn a_hook_that_never_finishes_is_given_up_on() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![hook("sleep 30; exit 2", When::Before, dir.path())];
        let started = std::time::Instant::now();
        assert_eq!(run(&hooks, When::Before, "t", &json!({})).await, Verdict::Fine);
        assert!(started.elapsed() < BUDGET * 2, "it waited too long: {:?}", started.elapsed());
    }

    /// The tool call is handed over on stdin, the way the format says.
    #[tokio::test]
    async fn the_call_is_handed_to_the_hook_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("seen.json");
        // `cat >` is bash; on Windows the hook runs under `cmd /C`, where `more` is the
        // stdin-to-stdout copy that works non-interactively.
        let command = if cfg!(windows) {
            format!("more > {}", out.display())
        } else {
            format!("cat > {}", out.display())
        };
        let hooks = vec![hook(&command, When::Before, dir.path())];
        run(&hooks, When::Before, "terminal.exec", &json!({"command": "ls"})).await;
        let seen: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(seen["tool_name"], json!("terminal.exec"));
        assert_eq!(seen["tool_input"]["command"], json!("ls"));
        assert_eq!(seen["hook_event_name"], json!("PreToolUse"));
    }
}
