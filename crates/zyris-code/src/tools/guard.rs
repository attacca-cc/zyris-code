//! Wraps the served capability to **inject a decision into every call.**
//!
//! The moment it announces, every session of that account sees this node, and path resolution isn't a jail, so
//! absolute paths escape the working directory.
//!
//! **There is no asking anymore.** Outside the working directory, the `/config` directory-access
//! setting decides: `deny` (the default) refuses the call, `allow` runs it (`gate::decide`).

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use zyris::{
    CapabilityDescriptor, IncomingCall, Outgoing, Payload, Result, ServeCapability, WireError,
};

use crate::app::Frame;
use crate::tools::bridge::Bridge;
use crate::tools::gate::{escaping_path, target_of, Call, Decision};

pub struct Gate<C> {
    inner: C,
    /// `descriptor()` builds the whole tool schema. Pulled once so it isn't called per call.
    capability: String,
    bridge: Bridge,
}

impl<C: ServeCapability> Gate<C> {
    pub fn new(inner: C, bridge: Bridge) -> Gate<C> {
        let capability = inner.descriptor().name;
        Gate { inner, capability, bridge }
    }
}

#[async_trait]
impl<C: ServeCapability> ServeCapability for Gate<C> {
    fn descriptor(&self) -> CapabilityDescriptor {
        // **Fits the tool definition into the token budget.** The upstream (zyris-caps) doc comments ride along
        // to the agent as-is, and repeated every session·turn they eat context.
        // Only the description is trimmed; name·schema value-parsing parts stay — dispatch doesn't read
        // the description, so trimming here only touches what gets announced.
        let mut descriptor = self.inner.descriptor();
        crate::tools::trim::trim_descriptor(&mut descriptor);
        descriptor
    }

    async fn dispatch(&self, call: IncomingCall) -> Result<Outgoing> {
        // If the args can't be read, the target is unknown, and without the target it's unknown
        // what is running. Then leave it as the empty value and take the decision.
        let args = call.params.to_json().unwrap_or(Value::Null);
        let target = target_of(&self.capability, &call.tool, &args);
        let outside = escaping_path(&self.bridge.root(), &self.capability, &call.tool, &args);
        let gated = Call::new(&self.capability, &call.tool, target).leaving(outside);

        match self.bridge.decide(&gated) {
            Decision::Run => {}
            Decision::Refuse(why) => return Err(WireError::invalid_params(why)),
        }

        let (call, cut) = self.clamp_exec(call, &args, wire_deadline());
        // **Shows what's running while it runs.** `exec` gives its result only once at completion
        // (protocol §terminal), so without this a human waits up to 55 seconds
        // knowing nothing.
        // **Which window took this call.** With several windows up, it only goes to the one the server picked, and
        // looking at the screen alone can't tell whether a window missed it or wasn't asked.
        tracing::info!(capability = %gated.capability, tool = %gated.tool, "took a tool call");

        let running = self.tell_the_screen_it_started(&gated, &args);
        let out = self.inner.dispatch(call).await;
        if let Some(id) = running {
            self.bridge.frame(crate::app::Frame::ExecDone { id });
        }
        let out = out?;
        self.note_the_shells(&gated, &args, &out);
        Ok(match cut {
            Some(deadline) => note_the_cut(out, deadline),
            None => out,
        })
    }
}

impl<C: ServeCapability> Gate<C> {
    /// Tells the screen what's running for `terminal.exec`. Does nothing otherwise.
    ///
    /// What's returned is the number to clear when done. **Only `exec`** — the other tools finish
    /// quickly, so announcing each one would just make the activity line flicker.
    fn tell_the_screen_it_started(&self, call: &Call, args: &Value) -> Option<u64> {
        if (call.capability.as_str(), call.tool.as_str()) != ("terminal", "exec") {
            return None;
        }
        let command = args.get("command").and_then(Value::as_str)?.to_string();
        let id = self.bridge.next_id();
        self.bridge.frame(crate::app::Frame::ExecStart { id, command });
        Some(id)
    }

    /// **When the wire deadline is on, `exec`'s `timeout_ms` is clamped inside it.**
    ///
    /// This isn't the tool's deadline but the other side's situation. attacca cuts node calls at 60
    /// seconds (`ZYRIS_CALL_TIMEOUT_SECS`), and a command running longer becomes a Timeout
    /// **error** over there, leaving the agent not knowing what happened. We finish first and
    /// note that it was cut, so the agent can switch to `wait.start`.
    fn clamp_exec(
        &self,
        call: IncomingCall,
        args: &Value,
        deadline: Option<Duration>,
    ) -> (IncomingCall, Option<Duration>) {
        let Some(deadline) = deadline else { return (call, None) };
        if self.capability != "terminal" || call.tool != "exec" {
            return (call, None);
        }
        let room = deadline.saturating_sub(EXEC_HEADROOM).as_millis() as u64;
        if room == 0 {
            return (call, None);
        }
        // If the agent already set it shorter, that side wins. The deadline is a cap, not a default.
        if args.get("timeout_ms").and_then(Value::as_u64).is_some_and(|ms| ms <= room) {
            return (call, None);
        }
        let mut patched = args.clone();
        let Some(obj) = patched.as_object_mut() else { return (call, None) };
        obj.insert("timeout_ms".into(), Value::from(room));
        (IncomingCall { params: Payload::from_json(patched), ..call }, Some(deadline))
    }

    /// Tells the screen about shells opening and closing.
    ///
    /// **Every `terminal` call passes here, so this is the only place that knows.** Without it,
    /// shells the agent left open run without the human knowing.
    fn note_the_shells(&self, call: &Call, args: &Value, out: &Outgoing) {
        if self.capability != "terminal" {
            return;
        }
        match call.tool.as_str() {
            "open" | "open_stream" => {
                let Some(id) = pty_id_of(out) else { return };
                let name = args
                    .get("shell")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("기본 셸")
                    .to_string();
                self.bridge.frame(Frame::ShellOpened { id, name });
            }
            "close" => self.bridge.frame(Frame::ShellClosed { id: call.target.clone() }),
            _ => {}
        }
    }
}

/// Identifier of the opened PTY. Same spot in a unary response or a stream head.
fn pty_id_of(out: &Outgoing) -> Option<String> {
    let payload = match out {
        Outgoing::Response(p) => p,
        Outgoing::Stream { head, .. } => head,
    };
    let v = payload.to_json().ok()?;
    v.get("pty")?.as_str().map(str::to_string)
}

/// When time ran out and it was cut, **say so in the result.** Cut silently, the agent
/// thinks the command failed and retries the same thing.
fn note_the_cut(out: Outgoing, deadline: Duration) -> Outgoing {
    let Outgoing::Response(payload) = out else { return out };
    let Ok(mut v) = payload.to_json() else { return Outgoing::Response(payload) };
    if v.get("timed_out").and_then(Value::as_bool) != Some(true) {
        return Outgoing::Response(payload);
    }
    let Some(obj) = v.as_object_mut() else { return Outgoing::Response(payload) };
    let mut stderr = obj.get("stderr").and_then(Value::as_str).unwrap_or_default().to_string();
    // **Says where to go instead.** It used to point at `terminal.open`+`read`, but that path has
    // no signal for "did the command finish", so the agent had to read the prompt by guesswork.
    stderr.push_str(&format!(
        "\n\n이 배포는 노드 호출을 {}초에 끊습니다. **명령은 실패한 것이 아니라 시간에 \
         잘린 것입니다.** 오래 걸리는 것은 wait.start로 배경에 걸고 wait.until로 \
         기다리세요 — 그쪽은 끝날 때까지 나눠서 기다릴 수 있습니다.",
        deadline.as_secs()
    ));
    obj.insert("stderr".into(), Value::from(stderr));
    Outgoing::Response(Payload::from_json(v))
}

/// Headroom given to `exec`. Time for us to build and send back the result.
const EXEC_HEADROOM: Duration = Duration::from_secs(5);

/// A deadline that only applies to the wire. **Not the tool's deadline.**
///
/// attacca cuts node calls at `ZYRIS_CALL_TIMEOUT_SECS` (default 60) and we can't read that value.
/// So we answer in time within it.
///
/// `wait.until` reads the same value. **Only one place may know the deadline** — with two, fixing
/// one of them leaves one tool answering while the other gets cut off.
pub(crate) fn wire_deadline() -> Option<Duration> {
    let secs: u64 = std::env::var("ZYRIS_CODE_WIRE_DEADLINE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(55);
    (secs > 0).then(|| Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::Mode;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use zyris::{Serialization, ToolDescriptor};

    /// The wrapped side. **Whether it was called is all this file cares about.**
    struct Fake {
        name: &'static str,
        ran: Arc<AtomicBool>,
        /// The result to return. An empty map if none.
        reply: Option<Value>,
    }

    impl Fake {
        fn new(name: &'static str) -> (Fake, Arc<AtomicBool>) {
            let ran = Arc::new(AtomicBool::new(false));
            (Fake { name, ran: Arc::clone(&ran), reply: None }, ran)
        }
    }

    #[async_trait]
    impl ServeCapability for Fake {
        fn descriptor(&self) -> CapabilityDescriptor {
            CapabilityDescriptor {
                name: self.name.to_string(),
                version: 1,
                tools: vec![ToolDescriptor {
                    name: "edit".into(),
                    description: String::new(),
                    transfer: zyris::Transfer::Unary,
                    request_schema: json!({}),
                    response_schema: None,
                    item_schema: None,
                }],
            }
        }

        async fn dispatch(&self, _call: IncomingCall) -> Result<Outgoing> {
            self.ran.store(true, Ordering::SeqCst);
            Ok(Outgoing::Response(Payload::from_json(
                self.reply.clone().unwrap_or_else(|| json!({})),
            )))
        }
    }

    /// At wrap time **the description goes out within budget.** dispatch doesn't read the description, so
    /// it only touches what gets announced — the Gate-side half of the token budget (`tools::trim`).
    #[test]
    fn the_gate_trims_long_descriptions() {
        struct Verbose;
        #[async_trait]
        impl ServeCapability for Verbose {
            fn descriptor(&self) -> CapabilityDescriptor {
                CapabilityDescriptor {
                    name: "verbose".into(),
                    version: 1,
                    tools: vec![ToolDescriptor {
                        name: "talk".into(),
                        description: "Talk about it at great length. ".repeat(40),
                        transfer: zyris::Transfer::Unary,
                        request_schema: json!({}),
                        response_schema: None,
                        item_schema: None,
                    }],
                }
            }
            async fn dispatch(&self, _call: IncomingCall) -> Result<Outgoing> {
                Ok(Outgoing::Response(Payload::from_json(json!({}))))
            }
        }
        let gate = Gate::new(Verbose, Bridge::new());
        let tool = &gate.descriptor().tools[0];
        assert!(tool.description.len() <= crate::tools::trim::DESCRIPTION_LIMIT);
        assert!(tool.description.starts_with("Talk about it"), "{}", tool.description);
    }

    fn incoming(tool: &str, args: Value) -> IncomingCall {
        IncomingCall {
            tool: tool.into(),
            params: Payload::from_json(args),
            serialization: Serialization::Json,
        }
    }

    /// **Inside work just runs with no screen attached** — the policy only looks at paths,
    /// and a path inside the tree is never refused.
    #[tokio::test]
    async fn a_tool_inside_the_tree_runs_with_no_screen_attached() {
        let (fake, ran) = Fake::new("code_edit");
        let bridge = Bridge::new();
        bridge.set_root(std::path::PathBuf::from("/tmp/여기"));
        let gate = Gate::new(fake, bridge);
        assert!(gate.dispatch(incoming("edit", json!({"path": "a"}))).await.is_ok());
        assert!(ran.load(Ordering::SeqCst));
    }

    /// **Leaving with the default (deny) refuses — nothing runs outside.**
    #[tokio::test]
    async fn leaving_the_tree_refuses_when_denied() {
        let (fake, ran) = Fake::new("code_edit");
        let bridge = Bridge::new();
        bridge.set_root(std::path::PathBuf::from("/tmp/여기"));
        let gate = Gate::new(fake, bridge);

        let Err(e) = gate.dispatch(incoming("edit", json!({"path": "/etc/passwd"}))).await else {
            panic!("it left the tree with the policy set to deny")
        };
        assert!(!ran.load(Ordering::SeqCst), "the tool ran anyway");
        assert!(e.message.contains("작업 디렉터리 밖"), "{}", e.message);
    }

    /// **`allow` runs it** — that is the whole point of the setting. No window, no waiting.
    #[tokio::test]
    async fn leaving_runs_when_allowed() {
        let (fake, ran) = Fake::new("code_edit");
        let bridge = Bridge::new();
        bridge.set_root(std::path::PathBuf::from("/tmp/여기"));
        bridge.sync(
            Mode::Normal,
            &crate::config::Config {
                dir_access: crate::config::DirAccess::Allow,
                ..Default::default()
            },
        );
        let gate = Gate::new(fake, bridge);

        assert!(gate.dispatch(incoming("edit", json!({"path": "/etc/passwd"}))).await.is_ok());
        assert!(ran.load(Ordering::SeqCst), "the tool did not run");
    }

    /// The normal mode runs without asking.
    #[tokio::test]
    async fn the_normal_mode_runs_without_asking() {
        let (fake, ran) = Fake::new("code_edit");
        let bridge = Bridge::new();
        bridge.sync(Mode::Job, &crate::config::Config::default());
        let gate = Gate::new(fake, bridge);

        assert!(gate.dispatch(incoming("edit", json!({"path": "a"}))).await.is_ok());
        assert!(ran.load(Ordering::SeqCst));
    }

    /// A refusal must be **a sentence the agent can read and change its behavior by.**
    #[tokio::test]
    async fn a_refusal_says_what_to_do_instead() {
        let (fake, ran) = Fake::new("code_edit");
        let bridge = Bridge::new();
        bridge.sync(Mode::Plan, &crate::config::Config::default());
        let gate = Gate::new(fake, bridge);

        let Err(e) = gate.dispatch(incoming("edit", json!({"path": "a"}))).await else {
            panic!("it passed in plan mode")
        };
        assert!(!ran.load(Ordering::SeqCst));
        assert!(e.message.contains("계획"), "{}", e.message);
    }

    /// Reading is never blocked in any mode — without reading, nothing can start.
    #[tokio::test]
    async fn reading_is_never_gated() {
        let (fake, ran) = Fake::new("file_io");
        let bridge = Bridge::new();
        bridge.sync(Mode::Job, &crate::config::Config::default());
        let gate = Gate::new(fake, bridge);

        assert!(gate.dispatch(incoming("read", json!({"path": "a"}))).await.is_ok());
        assert!(ran.load(Ordering::SeqCst));
    }

    /// Opening a shell must tell the screen. **Without it, ghost shells run.**
    #[tokio::test]
    async fn opening_a_shell_tells_the_screen() {
        let (mut fake, _) = Fake::new("terminal");
        fake.reply = Some(json!({"pty": "p1"}));
        let bridge = Bridge::new();
        bridge.sync(Mode::Job, &crate::config::Config::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        bridge.attach(tx);
        let gate = Gate::new(fake, bridge.clone());

        assert!(gate.dispatch(incoming("open", json!({"shell": "zsh"}))).await.is_ok());
        match rx.try_recv().expect("it must tell the screen") {
            (_, crate::app::Action::Frame(Frame::ShellOpened { id, name })) => {
                assert_eq!((id.as_str(), name.as_str()), ("p1", "zsh"));
            }
            other => panic!("it must say a shell opened: {other:?}"),
        }
    }

    /// **A long command is clamped inside the wire deadline.** Unclamped, the other side cuts first and
    /// the agent is left not knowing what happened.
    #[tokio::test]
    async fn a_long_exec_is_cut_to_fit_the_wire_deadline() {
        let (fake, _) = Fake::new("terminal");
        let bridge = Bridge::new();
        bridge.sync(Mode::Job, &crate::config::Config::default());
        let gate = Gate::new(fake, bridge);

        let args = json!({"command": "cargo build", "timeout_ms": 600_000u64});
        let (call, cut) =
            gate.clamp_exec(incoming("exec", args.clone()), &args, Some(Duration::from_secs(55)));
        assert_eq!(cut, Some(Duration::from_secs(55)), "it must mark that it was cut");
        let sent = call.params.to_json().unwrap();
        assert_eq!(sent["timeout_ms"], json!(50_000u64), "it must come inside the deadline");
        assert_eq!(sent["command"], json!("cargo build"), "no other argument may be touched");
    }

    /// If the agent already set it shorter, that side wins. The deadline is a cap, not a default.
    #[tokio::test]
    async fn a_short_exec_is_left_alone() {
        let (fake, _) = Fake::new("terminal");
        let bridge = Bridge::new();
        let gate = Gate::new(fake, bridge);

        let args = json!({"command": "ls", "timeout_ms": 1_000u64});
        let (call, cut) =
            gate.clamp_exec(incoming("exec", args.clone()), &args, Some(Duration::from_secs(55)));
        assert_eq!(cut, None);
        assert_eq!(call.params.to_json().unwrap()["timeout_ms"], json!(1_000u64));
    }

    /// Deadline off means nothing is cut — once attacca is fixed, this is the way.
    #[tokio::test]
    async fn turning_the_deadline_off_stops_the_cutting() {
        let (fake, _) = Fake::new("terminal");
        let gate = Gate::new(fake, Bridge::new());
        let args = json!({"command": "cargo build"});
        let (call, cut) = gate.clamp_exec(incoming("exec", args.clone()), &args, None);
        assert_eq!(cut, None);
        assert_eq!(call.params.to_json().unwrap().get("timeout_ms"), None);
    }

    /// Cut because time ran out, **the result says so.** Cut silently, the agent
    /// thinks the command failed and retries the same thing.
    #[test]
    fn a_cut_run_says_so_in_its_output() {
        let out = Outgoing::Response(Payload::from_json(
            json!({"exit_code": -1, "stdout": "", "stderr": "앞선 오류", "timed_out": true}),
        ));
        let Outgoing::Response(p) = note_the_cut(out, Duration::from_secs(55)) else {
            panic!("it must be a unary response")
        };
        let v = p.to_json().unwrap();
        let stderr = v["stderr"].as_str().unwrap();
        assert!(stderr.starts_with("앞선 오류"), "what was there must not be erased: {stderr}");
        // **It must say what to do.** And that this isn't a failure — without that, the agent
        // thinks the build broke and stops.
        assert!(stderr.contains("wait.start"), "it must say what to do: {stderr}");
        assert!(stderr.contains("실패한 것이 아니라"), "{stderr}");
    }

    /// A command that finished in time gets nothing appended.
    #[test]
    fn a_run_that_finished_in_time_is_left_alone() {
        let out = Outgoing::Response(Payload::from_json(
            json!({"exit_code": 0, "stdout": "됐다", "stderr": "", "timed_out": false}),
        ));
        let Outgoing::Response(p) = note_the_cut(out, Duration::from_secs(55)) else {
            panic!("it must be a unary response")
        };
        assert_eq!(p.to_json().unwrap()["stderr"], json!(""));
    }
}
