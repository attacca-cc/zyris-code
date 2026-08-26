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
    CallLimit, CapabilityDescriptor, IncomingCall, Outgoing, Payload, Result, ServeCapability,
    WireError,
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
        // **And says how long a caller should be willing to wait.** Every announced capability
        // passes here, upstream's and this crate's alike, so this is the one place that can say it
        // for tools whose descriptor is generated somewhere else (`terminal.exec` is exactly that).
        declare_limits(&mut descriptor, exec_ceiling());
        descriptor
    }

    async fn dispatch(&self, call: IncomingCall) -> Result<Outgoing> {
        // If the args can't be read, the target is unknown, and without the target it's unknown
        // what is running. Then leave it as the empty value and take the decision.
        let args = call.params.to_json().unwrap_or(Value::Null);
        let target = target_of(&self.capability, &call.tool, &args);
        let root = self.bridge.root();
        let outside = escaping_path(&root, &self.capability, &call.tool, &args);
        // **Where this app's credentials live.** Read per call rather than held, because it is a
        // pure function of the environment and holding it would be one more thing to keep in step.
        let secret = crate::conn::app_dir().and_then(|dir| {
            crate::tools::gate::secret_path(&dir, &root, &self.capability, &call.tool, &args)
        });
        let gated =
            Call::new(&self.capability, &call.tool, target).leaving(outside).reaching_for(secret);

        match self.bridge.decide(&gated) {
            Decision::Run => {}
            Decision::Refuse(why) => return Err(WireError::invalid_params(why)),
        }

        // **A plugin's hooks run here and nowhere else.** This is the one point every tool call
        // already passes, so there is no second path to keep in step — and a hook can only refuse,
        // never rewrite, so nothing downstream has to re-read what it did (`hooks.rs`).
        let hooks = self.bridge.hooks();
        let named = format!("{}.{}", gated.capability, gated.tool);
        if let crate::hooks::Verdict::Refused(why) =
            crate::hooks::run(&hooks, crate::hooks::When::Before, &named, &args).await
        {
            return Err(WireError::invalid_params(why));
        }

        let (call, cut) = self.clamp_exec(call, &args, exec_ceiling());
        // **Shows what's running while it runs.** `exec` gives its result only once at completion
        // (protocol §terminal), so without this a human waits out the whole command knowing nothing.
        // **Which window took this call.** With several windows up, it only goes to the one the server picked, and
        // looking at the screen alone can't tell whether a window missed it or wasn't asked.
        tracing::info!(capability = %gated.capability, tool = %gated.tool, "took a tool call");

        let running = self.tell_the_screen_it_started(&gated, &args, asking_session(&call));
        let out = self.inner.dispatch(call).await;
        if let Some(id) = running {
            self.bridge.frame(crate::app::Frame::ExecDone { id });
        }
        // **After the call, whatever it did.** The verdict is not read — the call already happened,
        // and reporting a refusal now would describe something that did not occur.
        if !hooks.is_empty() {
            crate::hooks::run(&hooks, crate::hooks::When::After, &named, &args).await;
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
    fn tell_the_screen_it_started(
        &self,
        call: &Call,
        args: &Value,
        session: Option<String>,
    ) -> Option<u64> {
        if (call.capability.as_str(), call.tool.as_str()) != ("terminal", "exec") {
            return None;
        }
        let command = args.get("command").and_then(Value::as_str)?.to_string();
        let id = self.bridge.next_id();
        self.bridge.frame(crate::app::Frame::ExecStart { id, command, session });
        Some(id)
    }

    /// **`exec`'s `timeout_ms` is held inside the ceiling this node enforces.**
    ///
    /// Two reasons, and the second is the one that is easy to miss.
    ///
    /// Left out, `timeout_ms` means *no* clock at all in capkit — `child.wait()` with nothing
    /// bounding it — so a command that never returns never returns, and neither end has a way to
    /// take it back. Something has to fill that in, and this is the only place every call passes.
    ///
    /// And **what this node waits for is what it asked the caller to wait for**
    /// (`declare_limits`). A run allowed past the ceiling would outlive the declaration, and the
    /// caller would give up on an answer that was still coming — the failure this whole thing
    /// exists to stop, arrived at from the other direction.
    ///
    /// This used to clamp to the *wire* deadline instead, because attacca cut every node call at
    /// 60 seconds no matter what the tool was. It does not any more: the limit declared above is
    /// what it waits for (attacca#122).
    fn clamp_exec(
        &self,
        call: IncomingCall,
        args: &Value,
        ceiling: Option<Duration>,
    ) -> (IncomingCall, Option<Duration>) {
        let Some(ceiling) = ceiling else { return (call, None) };
        if self.capability != "terminal" || call.tool != "exec" {
            return (call, None);
        }
        let room = u64::try_from(ceiling.as_millis()).unwrap_or(u64::MAX);
        if room == 0 {
            return (call, None);
        }
        // If the agent already set it shorter, that side wins. The ceiling is a cap, not a default.
        if args.get("timeout_ms").and_then(Value::as_u64).is_some_and(|ms| ms <= room) {
            return (call, None);
        }
        let mut patched = args.clone();
        let Some(obj) = patched.as_object_mut() else { return (call, None) };
        obj.insert("timeout_ms".into(), Value::from(room));
        (IncomingCall { params: Payload::from_json(patched), ..call }, Some(ceiling))
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

/// **Which conversation asked for this call**, when the server said so.
///
/// This node runs one account's tools, and attacca hands them to *every* session on that account —
/// including ones open in another window on this machine. Nothing in the arguments distinguishes
/// them, by design: which session asked is the server's business, not the tool's, so it rides
/// beside the arguments in `meta` (`route::call_meta` on the attacca side).
///
/// `None` is the ordinary answer, not a fault. A server built before it says nothing, and so does
/// anything else that reaches these capabilities directly.
fn asking_session(call: &IncomingCall) -> Option<String> {
    let meta = call.meta.to_json().ok()?;
    let id = meta.get("session_id")?.as_str()?.trim();
    (!id.is_empty()).then(|| id.to_string())
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
fn note_the_cut(out: Outgoing, ceiling: Duration) -> Outgoing {
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
        "\n\n이 노드는 명령을 {}초에 끊습니다. **명령은 실패한 것이 아니라 시간에 \
         잘린 것입니다.** 더 오래 걸리는 것은 wait.start로 배경에 걸고 wait.until로 \
         기다리세요 ‒ 그쪽은 끝날 때까지 나눠서 기다릴 수 있습니다.",
        ceiling.as_secs()
    ));
    obj.insert("stderr".into(), Value::from(stderr));
    Outgoing::Response(Payload::from_json(v))
}

/// Added to the ceiling when it is declared, never when it is enforced. A run stopped exactly at
/// the ceiling still has to have its output gathered and sent, and a caller that gave up one
/// instant before that answer arrived would be the very failure this is here to prevent.
const REPLY_HEADROOM: Duration = Duration::from_secs(10);

/// The longest `terminal.exec` may run on this node before the process tree is killed.
///
/// **This is the node's number, not the wire's.** It used to be neither — `exec` was cut to fit
/// inside whatever attacca would wait, which was 60 seconds for every tool alike, and so a build
/// could not be run at all. Now the wait is what the tool asks for (`declare_limits`), and this is
/// what it asks for.
///
/// `ZYRIS_CODE_EXEC_MAX_SECS`, default half an hour. **`0` lifts the ceiling**, and then the
/// declaration says `Unlimited` — the agent's own `timeout_ms` is the only bound left, and a call
/// that omits it can hang until the connection dies. That is the point of it being off by default.
pub(crate) fn exec_ceiling() -> Option<Duration> {
    let secs: u64 =
        std::env::var("ZYRIS_CODE_EXEC_MAX_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(1800);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// **What this node asks of a caller's clock, tool by tool.**
///
/// A caller cannot tell from a schema whether a tool answers in a millisecond or builds a
/// workspace, so the node holding the tool says which (`zyris::CallLimit`, attacca#122). Saying
/// nothing asks for the caller's own default, and that is right for everything here but one:
/// every other tool answers well inside a minute, and `terminal.exec` is the one that cannot.
///
/// **What is declared is what is enforced.** The number is `clamp_exec`'s ceiling and the time to
/// send an answer back, so the two cannot drift: declare longer and a caller waits for something
/// this node already killed, declare shorter and it gives up on an answer that is on its way.
pub(crate) fn declare_limits(descriptor: &mut CapabilityDescriptor, ceiling: Option<Duration>) {
    if descriptor.name != "terminal" {
        return;
    }
    let limit = match ceiling {
        // Saturating rather than adding: `Duration + Duration` panics on overflow, and the number
        // comes from the environment, where anything at all can be written.
        Some(ceiling) => {
            let secs = ceiling.as_secs().saturating_add(REPLY_HEADROOM.as_secs());
            CallLimit::Secs(secs.min(u64::from(u32::MAX)) as u32)
        }
        None => CallLimit::Unlimited,
    };
    for tool in &mut descriptor.tools {
        if tool.name == "exec" {
            tool.call_limit = Some(limit);
        }
    }
}

/// A deadline that only applies to the wire. **Not the tool's deadline.**
///
/// A tool that declares nothing gets the caller's default, which on attacca is
/// `ZYRIS_CALL_TIMEOUT_SECS` (60) and cannot be read from here. So `wait.until` answers in time
/// within it — with success, saying to call again — rather than being cut off mid-wait.
///
/// **`terminal.exec` no longer reads this.** It declares a limit of its own (`declare_limits`)
/// and is held to that instead; this is what is left for the tools that declare nothing.
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
                    call_limit: None,
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
                        call_limit: None,
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
            meta: Payload::default(),
        }
    }

    /// **A call that says which session asked is the only one that can be attributed.** Anything
    /// else — an older server, or something calling these capabilities directly — says nothing,
    /// and that is the ordinary answer rather than a fault.
    #[test]
    fn who_asked_is_read_only_when_the_caller_actually_said() {
        let with = |meta: Value| IncomingCall {
            meta: Payload::from_json(meta),
            ..incoming("exec", serde_json::json!({}))
        };
        assert_eq!(
            asking_session(&with(serde_json::json!({"session_id": "s-1"}))),
            Some("s-1".to_string())
        );
        assert_eq!(asking_session(&incoming("exec", serde_json::json!({}))), None);
        assert_eq!(asking_session(&with(serde_json::json!({}))), None);
        // A blank id is not an id. Kept as one it would be compared against the session on screen
        // and never match, silently hiding this conversation's own commands.
        assert_eq!(asking_session(&with(serde_json::json!({"session_id": "  "}))), None);
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
            false,
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
        bridge.sync(Mode::Job, &crate::config::Config::default(), false);
        let gate = Gate::new(fake, bridge);

        assert!(gate.dispatch(incoming("edit", json!({"path": "a"}))).await.is_ok());
        assert!(ran.load(Ordering::SeqCst));
    }

    /// A refusal must be **a sentence the agent can read and change its behavior by.**
    #[tokio::test]
    async fn a_refusal_says_what_to_do_instead() {
        let (fake, ran) = Fake::new("code_edit");
        let bridge = Bridge::new();
        bridge.sync(Mode::Plan, &crate::config::Config::default(), false);
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
        bridge.sync(Mode::Job, &crate::config::Config::default(), false);
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
        bridge.sync(Mode::Job, &crate::config::Config::default(), false);
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

    /// **A long command is held to the ceiling this node enforces**, and no further. It used to be
    /// cut to fit inside what attacca would wait for — 60 seconds for every tool alike — which is
    /// why a build could not be run at all.
    #[tokio::test]
    async fn a_long_exec_is_cut_to_the_ceiling_this_node_enforces() {
        let (fake, _) = Fake::new("terminal");
        let bridge = Bridge::new();
        bridge.sync(Mode::Job, &crate::config::Config::default(), false);
        let gate = Gate::new(fake, bridge);

        let ceiling = Duration::from_secs(1800);
        let args = json!({"command": "cargo build", "timeout_ms": 7_200_000u64});
        let (call, cut) = gate.clamp_exec(incoming("exec", args.clone()), &args, Some(ceiling));
        assert_eq!(cut, Some(ceiling), "it must mark that it was cut");
        let sent = call.params.to_json().unwrap();
        assert_eq!(sent["timeout_ms"], json!(1_800_000u64), "it must come inside the ceiling");
        assert_eq!(sent["command"], json!("cargo build"), "no other argument may be touched");
    }

    /// **Naming no timeout is not asking for no timeout.** Left out, `timeout_ms` means capkit
    /// waits on the child with no clock at all, and neither end can take that call back — so the
    /// ceiling is written in rather than left absent.
    #[tokio::test]
    async fn an_exec_that_named_no_timeout_is_given_the_ceiling() {
        let (fake, _) = Fake::new("terminal");
        let gate = Gate::new(fake, Bridge::new());
        let args = json!({"command": "cargo build"});
        let (call, cut) =
            gate.clamp_exec(incoming("exec", args.clone()), &args, Some(Duration::from_secs(1800)));
        assert_eq!(cut, Some(Duration::from_secs(1800)));
        assert_eq!(call.params.to_json().unwrap()["timeout_ms"], json!(1_800_000u64));
    }

    /// If the agent already set it shorter, that side wins. The ceiling is a cap, not a default.
    #[tokio::test]
    async fn a_short_exec_is_left_alone() {
        let (fake, _) = Fake::new("terminal");
        let bridge = Bridge::new();
        let gate = Gate::new(fake, bridge);

        let args = json!({"command": "ls", "timeout_ms": 1_000u64});
        let (call, cut) =
            gate.clamp_exec(incoming("exec", args.clone()), &args, Some(Duration::from_secs(1800)));
        assert_eq!(cut, None);
        assert_eq!(call.params.to_json().unwrap()["timeout_ms"], json!(1_000u64));
    }

    /// Ceiling lifted means nothing is cut, and the declaration says as much — the agent's own
    /// `timeout_ms` becomes the only bound there is.
    #[tokio::test]
    async fn lifting_the_ceiling_leaves_the_agents_own_clock_alone() {
        let (fake, _) = Fake::new("terminal");
        let gate = Gate::new(fake, Bridge::new());
        let args = json!({"command": "cargo build"});
        let (call, cut) = gate.clamp_exec(incoming("exec", args.clone()), &args, None);
        assert_eq!(cut, None);
        assert_eq!(call.params.to_json().unwrap().get("timeout_ms"), None);
    }

    /// **What is declared is what is enforced.** A caller is asked to wait for exactly as long as
    /// this node will let the command run, plus the time to send an answer back. Drift either way
    /// is a bug with a face: declared short, the caller gives up on an answer that is coming;
    /// declared long, it waits on one this node already killed.
    #[test]
    fn what_exec_declares_is_what_this_node_enforces() {
        let terminal = |ceiling| {
            let mut d = zyris::ServeCapability::descriptor(&zyris_caps::TerminalServer(
                zyris_capkit::PtyTerminal::default(),
            ));
            declare_limits(&mut d, ceiling);
            d
        };
        let limit = |d: &CapabilityDescriptor, tool: &str| {
            d.tools.iter().find(|t| t.name == tool).expect("the tool is announced").call_limit
        };

        let d = terminal(Some(Duration::from_secs(1800)));
        assert_eq!(
            limit(&d, "exec"),
            Some(CallLimit::Secs(1810)),
            "the declaration must cover the ceiling and the answer",
        );
        // **Only `exec`.** Everything else here answers well inside a caller's own default, and
        // saying so for them would be asking a caller to wait on tools that never need it.
        assert_eq!(limit(&d, "read"), None, "only exec has anything to declare");

        // Lifted, it says so rather than naming a number nothing holds it to.
        let lifted = terminal(None);
        assert_eq!(limit(&lifted, "exec"), Some(CallLimit::Unlimited));

        // And nothing outside `terminal` is touched.
        let (fake, _) = Fake::new("file_io");
        let mut other = zyris::ServeCapability::descriptor(&fake);
        declare_limits(&mut other, Some(Duration::from_secs(1800)));
        assert_eq!(other.tools[0].call_limit, None, "another capability is not exec's business");
    }

    /// Cut because time ran out, **the result says so.** Cut silently, the agent
    /// thinks the command failed and retries the same thing.
    #[test]
    fn a_cut_run_says_so_in_its_output() {
        let out = Outgoing::Response(Payload::from_json(
            json!({"exit_code": -1, "stdout": "", "stderr": "앞선 오류", "timed_out": true}),
        ));
        let Outgoing::Response(p) = note_the_cut(out, Duration::from_secs(1800)) else {
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
        let Outgoing::Response(p) = note_the_cut(out, Duration::from_secs(1800)) else {
            panic!("it must be a unary response")
        };
        assert_eq!(p.to_json().unwrap()["stderr"], json!(""));
    }
}
