//! 내주는 캐퍼빌리티를 감싸 **모든 호출에 판정을 끼운다.**
//!
//! announce하는 순간 그 계정의 모든 세션이 이 노드를 보고, 경로 해석은 감옥이 아니라서
//! 절대경로는 작업 디렉터리를 벗어난다.
//!
//! **묻는 자리는 하나뿐이다: 작업 디렉터리 밖.** 안쪽 일에는 아무것도 묻지 않고,
//! 밖으로 나가면 모드와 무관하게 사람에게 묻는다(`gate::escaping_path`).

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use zyris::{
    CapabilityDescriptor, IncomingCall, Outgoing, Payload, Result, ServeCapability, WireError,
};

use crate::app::Frame;
use crate::app::Verdict;
use crate::tools::bridge::Bridge;
use crate::tools::gate::{escaping_path, target_of, Call, Decision};

pub struct Gate<C> {
    inner: C,
    /// `descriptor()`는 도구 스키마를 통째로 만든다. 호출마다 부르지 않으려고 한 번만 뽑는다.
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
        // **도구 정의를 토큰 예산에 맞춘다.** 상류(zyris-caps)의 doc 주석이 그대로
        // 에이전트에게 실리는데, 매 세션·매 턴마다 반복되면 컨텍스트를 잡아먹는다.
        // 설명만 자르고 이름·스키마의 값 해석 부분은 그대로 — dispatch는 설명을 읽지
        // 않으므로 여기서 자르는 것이 announce되는 것에만 닿는다.
        let mut descriptor = self.inner.descriptor();
        crate::tools::trim::trim_descriptor(&mut descriptor);
        descriptor
    }

    async fn dispatch(&self, call: IncomingCall) -> Result<Outgoing> {
        // 인자를 못 읽으면 대상을 알 수 없고, 대상을 모르면 무엇을 승인하는지도 모른다.
        // 그럴 때는 빈 값으로 두고 판정을 받는다 — 통과시키지 않는다.
        let args = call.params.to_json().unwrap_or(Value::Null);
        let target = target_of(&self.capability, &call.tool, &args);
        let outside = escaping_path(&self.bridge.root(), &self.capability, &call.tool, &args);
        let gated = Call::new(&self.capability, &call.tool, target).leaving(outside);

        match self.bridge.decide(&gated) {
            Decision::Run => {}
            Decision::Refuse(why) => return Err(WireError::invalid_params(why)),
            Decision::Ask => self.ask_and_wait(&gated).await?,
        }

        let (call, cut) = self.clamp_exec(call, &args, wire_deadline());
        // **명령이 도는 동안 무엇이 도는지 보여 준다.** `exec`은 완료될 때 한 번만
        // 결과를 주므로(프로토콜 §terminal), 이것이 없으면 사람은 최대 55초를
        // 아무것도 모른 채 기다린다.
        // **어느 창이 이 호출을 받았는가.** 창을 여럿 띄우면 서버가 고른 창으로만 가는데,
        // 화면만 봐서는 안 온 창인지 안 부른 것인지 알 수 없다.
        tracing::info!(capability = %gated.capability, tool = %gated.tool, "도구 호출을 받았다");

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
    /// 사람에게 묻고 답을 기다린다. **기다리는 시간에 제한이 없다** — 창은 사람이
    /// 답할 때까지 화면에 남는다. 마감이 걸리는 것은 와이어뿐이다.
    async fn ask_and_wait(&self, call: &Call) -> Result<()> {
        // **왜 물었는지 남긴다.** 승인 창을 못 본 사람에게는 "승인이 없어서 못 했다"는
        // 에이전트의 말만 남는데, 그것만으로는 무엇이 울타리를 넘었는지 알 수 없다.
        let outside = call.outside.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        tracing::info!(
            capability = %call.capability,
            tool = %call.tool,
            path = %outside,
            "작업 디렉터리 밖이라 사람에게 묻는다"
        );

        let Some((id, wait)) = self.bridge.ask(call.clone(), summarize(call)) else {
            // 화면이 없으면 물을 곳이 없다. **통과시키면 몰래 밖으로 나간다.**
            return Err(WireError::invalid_params(format!(
                "`{outside}`은 작업 디렉터리 밖이라 사람의 승인이 필요한데, 이 노드에 화면이 \
                 붙어 있지 않아 물을 곳이 없습니다. 작업 디렉터리 안에서 되는 길을 찾아 주세요."
            )));
        };

        let answer = match wire_deadline() {
            Some(d) => match tokio::time::timeout(d, wait).await {
                Ok(v) => v.ok(),
                // **와이어에만 답하고 창은 남긴다.** attacca가 노드 호출을 60초에
                // 끊으므로 그 전에 우리가 먼저 말한다.
                Err(_) => {
                    self.bridge.expire(id);
                    return Err(WireError::invalid_params(format!(
                        "`{outside}`은 작업 디렉터리 밖이라 사람에게 물었는데 아직 답이 \
                         없습니다. 승인 창은 화면에 그대로 있으니, 같은 인자로 다시 호출하면 \
                         그때의 답이 반영됩니다. 창이 여럿이면 다른 창에 떠 있을 수 있습니다.",
                    )));
                }
            },
            None => wait.await.ok(),
        };

        match answer {
            Some(Verdict::Allow | Verdict::AllowAlways) => Ok(()),
            Some(Verdict::Deny) => Err(WireError::invalid_params(format!(
                "사용자가 `{outside}`을 거부했습니다. 작업 디렉터리 안에서 할 수 있는 길을 \
                 찾거나, 무엇이 필요한지 물어보세요."
            ))),
            None => Err(WireError::invalid_params("승인을 받지 못했습니다.")),
        }
    }

    /// `terminal.exec`이면 무엇이 도는지 화면에 알린다. 그 밖에는 아무 일도 안 한다.
    ///
    /// 돌려주는 것은 끝났을 때 지울 번호다. **`exec`만이다** — 나머지 도구는 금방
    /// 끝나서, 하나하나 알리면 활동 줄이 깜박이기만 한다.
    fn tell_the_screen_it_started(&self, call: &Call, args: &Value) -> Option<u64> {
        if (call.capability.as_str(), call.tool.as_str()) != ("terminal", "exec") {
            return None;
        }
        let command = args.get("command").and_then(Value::as_str)?.to_string();
        let id = self.bridge.next_id();
        self.bridge.frame(crate::app::Frame::ExecStart { id, command });
        Some(id)
    }

    /// **와이어 마감이 켜져 있으면 `exec`의 `timeout_ms`를 그 안쪽으로 자른다.**
    ///
    /// 이것은 도구의 마감이 아니라 저쪽 사정이다. attacca가 노드 호출을 60초
    /// (`ZYRIS_CALL_TIMEOUT_SECS`)에 끊는데, 그보다 오래 도는 명령은 저쪽에서 Timeout
    /// **오류**가 되어 에이전트는 무슨 일이 있었는지 모른 채 남는다. 우리가 먼저 끝내고
    /// 잘랐다고 적어 주면 에이전트가 `terminal.open`으로 갈아탈 수 있다.
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
        // 에이전트가 이미 짧게 잡았으면 그쪽이 이긴다. 마감은 상한이지 기본값이 아니다.
        if args.get("timeout_ms").and_then(Value::as_u64).is_some_and(|ms| ms <= room) {
            return (call, None);
        }
        let mut patched = args.clone();
        let Some(obj) = patched.as_object_mut() else { return (call, None) };
        obj.insert("timeout_ms".into(), Value::from(room));
        (IncomingCall { params: Payload::from_json(patched), ..call }, Some(deadline))
    }

    /// 열리고 닫히는 셸을 화면에 알린다.
    ///
    /// **모든 `terminal` 호출이 여기를 지나므로 아는 곳이 여기뿐이다.** 안 알리면
    /// 에이전트가 열어 둔 셸이 사람 모르게 돈다.
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

/// 사람이 읽을 한 줄. **무엇이 밖으로 나가는지**가 이 승인의 전부다.
fn summarize(call: &Call) -> String {
    match &call.outside {
        Some(path) => path.to_string_lossy().into_owned(),
        None => call.target.clone(),
    }
}

/// 열린 PTY의 식별자. 단항 응답이든 스트림 머리든 같은 자리에 있다.
fn pty_id_of(out: &Outgoing) -> Option<String> {
    let payload = match out {
        Outgoing::Response(p) => p,
        Outgoing::Stream { head, .. } => head,
    };
    let v = payload.to_json().ok()?;
    v.get("pty")?.as_str().map(str::to_string)
}

/// 잘린 채로 시간이 다 됐으면 **결과에 그렇게 적는다.** 조용히 끊으면 에이전트는
/// 명령이 실패한 줄 알고 같은 것을 다시 시도한다.
fn note_the_cut(out: Outgoing, deadline: Duration) -> Outgoing {
    let Outgoing::Response(payload) = out else { return out };
    let Ok(mut v) = payload.to_json() else { return Outgoing::Response(payload) };
    if v.get("timed_out").and_then(Value::as_bool) != Some(true) {
        return Outgoing::Response(payload);
    }
    let Some(obj) = v.as_object_mut() else { return Outgoing::Response(payload) };
    let mut stderr = obj.get("stderr").and_then(Value::as_str).unwrap_or_default().to_string();
    stderr.push_str(&format!(
        "\n\n이 배포는 노드 호출을 {}초에 끊습니다. 더 긴 명령은 terminal.open으로 열고 \
         terminal.read로 나눠 받으세요.",
        deadline.as_secs()
    ));
    obj.insert("stderr".into(), Value::from(stderr));
    Outgoing::Response(Payload::from_json(v))
}

/// `exec`에 주는 여유. 우리가 결과를 만들어 돌려보낼 시간이다.
const EXEC_HEADROOM: Duration = Duration::from_secs(5);

/// 와이어에만 걸리는 마감. **도구의 마감이 아니다.**
///
/// attacca가 노드 호출을 `ZYRIS_CALL_TIMEOUT_SECS`(기본 60)에 끊는데 그 값을 우리가 읽을
/// 수 없다. 그래서 그 안에서 제때 답하고 창은 화면에 남긴다.
///
/// **저쪽이 고쳐지면 `ZYRIS_CODE_WIRE_DEADLINE_SECS=0`으로 끄면 된다** — 그러면 우리가
/// 먼저 답하지 않고 사람을 그냥 기다린다. 코드를 고칠 일이 없다.
fn wire_deadline() -> Option<Duration> {
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

    /// 감싸이는 쪽. **불렸는지 아닌지가 이 파일의 전부다.**
    struct Fake {
        name: &'static str,
        ran: Arc<AtomicBool>,
        /// 돌려줄 결과. 없으면 빈 맵이다.
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

    /// 감싸는 순간 **설명이 예산에 맞아 나간다.** dispatch는 설명을 읽지 않으므로
    /// announce되는 것에만 닿는다 — 토큰 예산(`tools::trim`)의 Gate 쪽 반쪽이다.
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

    /// **안쪽 일에는 화면이 없어도 그냥 돈다.** 묻는 자리는 밖뿐이다.
    #[tokio::test]
    async fn a_tool_inside_the_tree_runs_with_no_screen_attached() {
        let (fake, ran) = Fake::new("code_edit");
        let bridge = Bridge::new();
        bridge.set_root(std::path::PathBuf::from("/tmp/여기"));
        let gate = Gate::new(fake, bridge);
        assert!(gate.dispatch(incoming("edit", json!({"path": "a"}))).await.is_ok());
        assert!(ran.load(Ordering::SeqCst));
    }

    /// **밖으로 나가는데 물을 곳이 없으면 안 돌아야 한다.** 통과시키면 몰래 나간다.
    #[tokio::test]
    async fn leaving_the_tree_with_no_screen_refuses_instead_of_passing() {
        let (fake, ran) = Fake::new("code_edit");
        let bridge = Bridge::new();
        bridge.set_root(std::path::PathBuf::from("/tmp/여기"));
        let gate = Gate::new(fake, bridge);

        let Err(e) = gate.dispatch(incoming("edit", json!({"path": "/etc/passwd"}))).await else {
            panic!("승인 없이 밖으로 나갔다")
        };
        assert!(!ran.load(Ordering::SeqCst), "도구가 돌아 버렸다");
        assert!(e.message.contains("작업 디렉터리 밖"), "{}", e.message);
    }

    /// 밖으로 나가면 **화면에 물음이 간다.**
    #[tokio::test]
    async fn leaving_the_tree_asks_the_screen() {
        let (fake, ran) = Fake::new("code_edit");
        let bridge = Bridge::new();
        bridge.set_root(std::path::PathBuf::from("/tmp/여기"));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        bridge.attach(tx);
        let gate = Gate::new(fake, bridge.clone());

        // 답하지 않으므로 마감에 걸려 돌아온다. 그 전에 물음이 갔는지만 본다.
        std::env::set_var("ZYRIS_CODE_WIRE_DEADLINE_SECS", "1");
        let _ = gate.dispatch(incoming("edit", json!({"path": "/etc/passwd"}))).await;
        std::env::remove_var("ZYRIS_CODE_WIRE_DEADLINE_SECS");

        match rx.try_recv().expect("화면으로 물어야 한다") {
            (_, crate::app::Action::Frame(Frame::Ask(ask))) => {
                assert_eq!(ask.summary, "/etc/passwd");
                assert_eq!(ask.call.outside, Some(std::path::PathBuf::from("/etc/passwd")));
            }
            other => panic!("승인 물음이어야 한다: {other:?}"),
        }
        assert!(!ran.load(Ordering::SeqCst), "답도 안 했는데 돌았다");
    }

    /// 기본 모드는 묻지 않고 돌린다.
    #[tokio::test]
    async fn the_normal_mode_runs_without_asking() {
        let (fake, ran) = Fake::new("code_edit");
        let bridge = Bridge::new();
        bridge.sync(Mode::Job, &Default::default());
        let gate = Gate::new(fake, bridge);

        assert!(gate.dispatch(incoming("edit", json!({"path": "a"}))).await.is_ok());
        assert!(ran.load(Ordering::SeqCst));
    }

    /// 거부는 **에이전트가 읽고 행동을 바꿀 수 있는 문장**이어야 한다.
    #[tokio::test]
    async fn a_refusal_says_what_to_do_instead() {
        let (fake, ran) = Fake::new("code_edit");
        let bridge = Bridge::new();
        bridge.sync(Mode::Plan, &Default::default());
        let gate = Gate::new(fake, bridge);

        let Err(e) = gate.dispatch(incoming("edit", json!({"path": "a"}))).await else {
            panic!("계획 모드에서 통과했다")
        };
        assert!(!ran.load(Ordering::SeqCst));
        assert!(e.message.contains("계획"), "{}", e.message);
    }

    /// 읽기는 어느 모드에서도 막히지 않는다 — 읽지 못하면 아무것도 시작할 수 없다.
    #[tokio::test]
    async fn reading_is_never_gated() {
        let (fake, ran) = Fake::new("file_io");
        let bridge = Bridge::new();
        bridge.sync(Mode::Job, &Default::default());
        let gate = Gate::new(fake, bridge);

        assert!(gate.dispatch(incoming("read", json!({"path": "a"}))).await.is_ok());
        assert!(ran.load(Ordering::SeqCst));
    }

    /// 셸을 열면 화면에 알려야 한다. **안 알리면 유령 셸이 돈다.**
    #[tokio::test]
    async fn opening_a_shell_tells_the_screen() {
        let (mut fake, _) = Fake::new("terminal");
        fake.reply = Some(json!({"pty": "p1"}));
        let bridge = Bridge::new();
        bridge.sync(Mode::Job, &Default::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        bridge.attach(tx);
        let gate = Gate::new(fake, bridge.clone());

        assert!(gate.dispatch(incoming("open", json!({"shell": "zsh"}))).await.is_ok());
        match rx.try_recv().expect("화면으로 알려야 한다") {
            (_, crate::app::Action::Frame(Frame::ShellOpened { id, name })) => {
                assert_eq!((id.as_str(), name.as_str()), ("p1", "zsh"));
            }
            other => panic!("셸이 열렸다고 알려야 한다: {other:?}"),
        }
    }

    /// **긴 명령은 와이어 마감 안쪽으로 자른다.** 안 자르면 저쪽이 먼저 끊고,
    /// 에이전트는 무슨 일이 있었는지 모른 채로 남는다.
    #[tokio::test]
    async fn a_long_exec_is_cut_to_fit_the_wire_deadline() {
        let (fake, _) = Fake::new("terminal");
        let bridge = Bridge::new();
        bridge.sync(Mode::Job, &Default::default());
        let gate = Gate::new(fake, bridge);

        let args = json!({"command": "cargo build", "timeout_ms": 600_000u64});
        let (call, cut) =
            gate.clamp_exec(incoming("exec", args.clone()), &args, Some(Duration::from_secs(55)));
        assert_eq!(cut, Some(Duration::from_secs(55)), "잘랐다고 표시해야 한다");
        let sent = call.params.to_json().unwrap();
        assert_eq!(sent["timeout_ms"], json!(50_000u64), "마감 안쪽으로 들어와야 한다");
        assert_eq!(sent["command"], json!("cargo build"), "다른 인자를 건드리면 안 된다");
    }

    /// 에이전트가 이미 짧게 잡았으면 그쪽이 이긴다. 마감은 상한이지 기본값이 아니다.
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

    /// 마감을 끄면 아무것도 자르지 않는다 — attacca가 고쳐지면 이 길로 간다.
    #[tokio::test]
    async fn turning_the_deadline_off_stops_the_cutting() {
        let (fake, _) = Fake::new("terminal");
        let gate = Gate::new(fake, Bridge::new());
        let args = json!({"command": "cargo build"});
        let (call, cut) = gate.clamp_exec(incoming("exec", args.clone()), &args, None);
        assert_eq!(cut, None);
        assert_eq!(call.params.to_json().unwrap().get("timeout_ms"), None);
    }

    /// 시간이 다 돼서 잘렸으면 **결과에 그렇게 적는다.** 조용히 끊으면 에이전트는
    /// 명령이 실패한 줄 알고 같은 것을 다시 시도한다.
    #[test]
    fn a_cut_run_says_so_in_its_output() {
        let out = Outgoing::Response(Payload::from_json(
            json!({"exit_code": -1, "stdout": "", "stderr": "앞선 오류", "timed_out": true}),
        ));
        let Outgoing::Response(p) = note_the_cut(out, Duration::from_secs(55)) else {
            panic!("단항 응답이어야 한다")
        };
        let v = p.to_json().unwrap();
        let stderr = v["stderr"].as_str().unwrap();
        assert!(stderr.starts_with("앞선 오류"), "원래 있던 것을 지우면 안 된다: {stderr}");
        assert!(stderr.contains("terminal.open"), "무엇을 하라는지 말해야 한다: {stderr}");
    }

    /// 제때 끝난 명령에는 아무 말도 붙이지 않는다.
    #[test]
    fn a_run_that_finished_in_time_is_left_alone() {
        let out = Outgoing::Response(Payload::from_json(
            json!({"exit_code": 0, "stdout": "됐다", "stderr": "", "timed_out": false}),
        ));
        let Outgoing::Response(p) = note_the_cut(out, Duration::from_secs(55)) else {
            panic!("단항 응답이어야 한다")
        };
        assert_eq!(p.to_json().unwrap()["stderr"], json!(""));
    }
}
