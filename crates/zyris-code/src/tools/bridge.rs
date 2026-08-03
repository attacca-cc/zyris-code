//! 도구를 돌리는 쪽과 화면 쪽을 잇는다.
//!
//! 둘은 다른 태스크에서 돈다 — 도구는 zyris `Runner` 위에서, 화면은 `app::run`에서.
//! 판정에 필요한 것(모드·허용 목록)은 화면이 쥐고 있고 물어볼 곳도 화면뿐이라, 그 사이를
//! 오가는 것을 여기 한 곳에 모았다.
//!
//! **`apply`가 순수해야 한다는 제약이 이 파일의 모양을 정했다.** 화면 상태를 여기로
//! 옮기는 것은 I/O 자리(`run_inner`)가 하고, 여기서 화면으로 가는 것은 `Action`뿐이다.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

use crate::app::{Action, Frame, ToolAsk, Verdict};
use crate::mode::Mode;
use crate::tools::gate::{decide, Call, Decision, Grants};

#[derive(Clone, Default)]
pub struct Bridge(Arc<Inner>);

#[derive(Default)]
struct Inner {
    /// 지금 모드. 화면이 바꾸면 I/O 자리가 옮겨 준다.
    mode: Mutex<Mode>,
    /// 사람이 열어 둔 바깥 디렉터리. 화면 쪽 목록의 사본이다.
    granted: Mutex<Grants>,
    /// 작업 디렉터리. **밖으로 나가는지 재는 기준이다.**
    root: Mutex<PathBuf>,
    /// 화면으로 보낼 것. 화면이 뜨기 전에는 비어 있다.
    to_app: Mutex<Option<mpsc::UnboundedSender<Action>>>,
    /// 답을 기다리는 물음들.
    waiting: Mutex<HashMap<u64, oneshot::Sender<Verdict>>>,
    /// 세션을 만들 때 실을 스킬 목록. `tools::announce`가 정하고 화면이 집어 간다.
    ///
    /// **세션 수명 동안 고정이다** — attacca의 `preamble`은 세션을 만들 때 한 번 정해지고
    /// 뒤에 바꿀 수 없다. 그래서 나중에 붙는 MCP 도구는 여기 실리지 않는다.
    preamble: Mutex<Option<String>>,
    /// 붙었거나 못 붙은 MCP 서버. `/mcp`가 읽는다.
    ///
    /// **실패도 남긴다.** 지금은 `Frame::Notice`로 6초 뜨고 사라져, 나중에 "그 서버
    /// 왜 없지"를 물어볼 방법이 없다.
    mcp: Mutex<Vec<(String, Result<usize, String>)>>,
    /// 편집 도구가 쓰는 되돌림 기록. `/undo`가 **같은 손잡이**를 써야 잠금이 하나다.
    undo: Mutex<Option<crate::undo::Undo>>,
    /// 자격을 버리고 다시 승인받게 하는 손잡이.
    ///
    /// 모자란 권한을 알아채는 것은 **붙은 뒤의 화면 쪽**(`me()`의 스코프)이고, 버릴 자격을
    /// 쥐고 있는 것은 시작할 때의 `main.rs`다. 그 둘을 잇는 자리가 여기다.
    reauth: Mutex<Option<crate::enroll::Reauth>>,
    next_id: AtomicU64,
}

impl Bridge {
    pub fn new() -> Bridge {
        Bridge::default()
    }

    /// 화면이 뜨면 자기 손잡이를 꽂는다. 이 전에 오는 물음은 갈 곳이 없어 거부가 된다.
    pub fn attach(&self, to_app: mpsc::UnboundedSender<Action>) {
        *self.0.to_app.lock().unwrap() = Some(to_app);
    }

    /// 화면 쪽 판정 재료를 옮겨 둔다. I/O 자리가 상태를 만질 때마다 부른다.
    pub fn sync(&self, mode: Mode, granted: &Grants) {
        *self.0.mode.lock().unwrap() = mode;
        *self.0.granted.lock().unwrap() = granted.clone();
    }

    /// 작업 디렉터리를 알려 둔다. `tools::announce`가 한 번 부른다.
    pub fn set_root(&self, root: PathBuf) {
        *self.0.root.lock().unwrap() = root;
    }

    pub fn root(&self) -> PathBuf {
        self.0.root.lock().unwrap().clone()
    }

    pub fn decide(&self, call: &Call) -> Decision {
        let mode = *self.0.mode.lock().unwrap();
        decide(mode, &self.0.granted.lock().unwrap(), call)
    }

    /// 물어보고 답을 기다릴 손잡이를 낸다. 화면이 없으면 `None`이다.
    pub fn ask(&self, call: Call, summary: String) -> Option<(u64, oneshot::Receiver<Verdict>)> {
        let id = self.next_id();
        let (tx, rx) = oneshot::channel();
        self.0.waiting.lock().unwrap().insert(id, tx);
        let ask = ToolAsk { id, call, summary, expired: false };
        match self.send(Action::Frame(Frame::Ask(ask))) {
            Some(()) => Some((id, rx)),
            None => {
                self.0.waiting.lock().unwrap().remove(&id);
                None
            }
        }
    }

    /// 사람이 답했다. I/O 자리가 `State.verdict_out`을 집어 여기로 넘긴다.
    pub fn answer(&self, id: u64, verdict: Verdict) {
        if let Some(tx) = self.0.waiting.lock().unwrap().remove(&id) {
            let _ = tx.send(verdict);
        }
    }

    /// 화면이 붙어 있는가.
    ///
    /// 등록 코드를 **어디에** 그릴지가 여기서 갈린다: 화면이 있으면 화면이 겹쳐 그리고,
    /// 없으면(첫 실행) stderr에 상자로 찍는다. 둘 다 하면 프레임이 글자를 덮는다.
    pub fn has_screen(&self) -> bool {
        self.0.to_app.lock().unwrap().is_some()
    }

    /// 프레임을 보내고 **닿았는지** 알려 준다. 안 닿았으면 부르는 쪽이 스스로 말해야 한다.
    pub fn reaches_screen(&self, frame: Frame) -> bool {
        self.send(Action::Frame(frame)).is_some()
    }

    /// 와이어 마감이 사람보다 먼저 왔다. **창은 화면에 남기고** 사실만 알린다.
    pub fn expire(&self, id: u64) {
        self.0.waiting.lock().unwrap().remove(&id);
        self.frame(Frame::Expired(id));
    }

    /// MCP 서버 하나의 결말을 적어 둔다. 도구 수를 알면 성공, 사유를 알면 실패다.
    pub fn note_mcp(&self, slug: &str, outcome: Result<usize, String>) {
        let mut all = self.0.mcp.lock().unwrap();
        match all.iter_mut().find(|(s, _)| s == slug) {
            Some(slot) => slot.1 = outcome,
            None => all.push((slug.to_string(), outcome)),
        }
    }

    /// 화면에 무언가를 표시하고 나중에 지울 때 쓸 번호. 승인 물음과 같은 통을 쓴다 —
    /// 둘이 겹칠 일이 없어야 지울 것을 헷갈리지 않는다.
    pub fn next_id(&self) -> u64 {
        self.0.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// `/mcp`가 보여줄 것. 적힌 순서 그대로다.
    pub fn mcp_report(&self) -> Vec<(String, Result<usize, String>)> {
        self.0.mcp.lock().unwrap().clone()
    }

    pub fn set_preamble(&self, preamble: Option<String>) {
        *self.0.preamble.lock().unwrap() = preamble;
    }

    pub fn preamble(&self) -> Option<String> {
        self.0.preamble.lock().unwrap().clone()
    }

    /// 편집 도구가 쓰는 되돌림 기록을 화면 쪽에도 알려 준다.
    ///
    /// **같은 손잡이여야 한다.** `Undo::for_dir`로 각자 만들면 잠금이 따로 놀아,
    /// 에이전트가 편집하는 중에 `/undo`를 누르면 로그 두 줄이 겹쳐 쓰인다.
    pub fn set_undo(&self, undo: crate::undo::Undo) {
        *self.0.undo.lock().unwrap() = Some(undo);
    }

    pub fn undo(&self) -> Option<crate::undo::Undo> {
        self.0.undo.lock().unwrap().clone()
    }

    /// 자격을 버릴 수 있는 손잡이를 얹는다. **사람이 토큰을 직접 준 자리에는 없다.**
    pub fn set_reauth(&self, reauth: crate::enroll::Reauth) {
        *self.0.reauth.lock().unwrap() = Some(reauth);
    }

    pub fn reauth(&self) -> Option<crate::enroll::Reauth> {
        self.0.reauth.lock().unwrap().clone()
    }

    pub fn frame(&self, frame: Frame) {
        let _ = self.send(Action::Frame(frame));
    }

    fn send(&self, action: Action) -> Option<()> {
        self.0.to_app.lock().unwrap().as_ref()?.send(action).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit_call() -> Call {
        Call::new("code_edit", "edit", "/tmp/a".into())
    }

    /// 모드는 화면이 정하고 게이트가 그것을 본다. **안 옮기면 화면은 계획 모드인데
    /// 도구는 그대로 돈다.**
    #[test]
    fn the_gate_sees_the_mode_the_screen_is_in() {
        let b = Bridge::new();
        assert_eq!(b.decide(&edit_call()), Decision::Run, "기본값은 그냥 돈다");
        b.sync(Mode::Plan, &Default::default());
        assert!(matches!(b.decide(&edit_call()), Decision::Refuse(_)));
        b.sync(Mode::Job, &Default::default());
        assert_eq!(b.decide(&edit_call()), Decision::Run);
    }

    /// **화면이 없으면 물을 곳이 없다.** 조용히 통과시키면 몰래 밖으로 나간다.
    #[test]
    fn asking_before_the_screen_exists_fails_instead_of_passing() {
        let b = Bridge::new();
        let leaving = edit_call().leaving(Some(PathBuf::from("/etc/passwd")));
        assert!(b.ask(leaving, "/etc/passwd".into()).is_none());
    }

    /// 답이 오면 기다리던 쪽이 깨어난다.
    #[tokio::test]
    async fn an_answer_reaches_the_call_that_is_waiting() {
        let b = Bridge::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        b.attach(tx);
        let (id, wait) = b.ask(edit_call(), "x".into()).expect("화면이 붙어 있다");
        assert!(rx.try_recv().is_ok(), "화면으로 물음이 가야 한다");
        b.answer(id, Verdict::Allow);
        assert_eq!(wait.await.unwrap(), Verdict::Allow);
    }

    /// 마감이 지나면 기다리던 손잡이를 버린다 — 뒤늦은 답이 몰래 실행되면 안 된다.
    #[tokio::test]
    async fn expiring_drops_the_waiter_so_a_late_answer_runs_nothing() {
        let b = Bridge::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        b.attach(tx);
        let (id, wait) = b.ask(edit_call(), "x".into()).unwrap();
        b.expire(id);
        b.answer(id, Verdict::Allow);
        assert!(wait.await.is_err(), "포기한 호출이 답을 받으면 안 된다");
    }

    /// 화면이 아직 없으면 프레임은 갈 곳이 없다. **죽지 않고 버린다.**
    #[test]
    fn a_frame_sent_before_the_screen_exists_is_dropped_quietly() {
        Bridge::new().frame(Frame::Notice("아무도 못 본다".into()));
    }

    /// 화면이 붙으면 프레임이 그리로 간다.
    #[test]
    fn a_frame_reaches_the_screen_once_it_is_attached() {
        let b = Bridge::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        b.attach(tx);
        b.frame(Frame::ShellClosed { id: "p1".into() });
        assert!(rx.try_recv().is_ok(), "화면으로 가야 한다");
    }

    /// 번호는 겹치면 안 된다 — 지울 것을 헷갈린다.
    #[test]
    fn every_id_is_new() {
        let b = Bridge::new();
        assert_ne!(b.next_id(), b.next_id());
    }

    /// **못 뜬 MCP 서버도 남아야 한다.** 상태 줄은 6초 뒤 사라져 나중에 물어볼 수 없다.
    #[test]
    fn a_failed_mcp_server_is_remembered_with_its_reason() {
        let b = Bridge::new();
        b.note_mcp("github", Ok(12));
        b.note_mcp("깨진것", Err("npx를 찾지 못했습니다".into()));
        let report = b.mcp_report();
        assert_eq!(report.len(), 2);
        assert_eq!(report[1].1.as_ref().unwrap_err(), "npx를 찾지 못했습니다");
    }

    /// 같은 서버를 두 번 적으면 덮어쓴다 — 목록에 두 벌로 보이면 안 된다.
    #[test]
    fn noting_the_same_server_twice_replaces_it() {
        let b = Bridge::new();
        b.note_mcp("github", Err("아직".into()));
        b.note_mcp("github", Ok(3));
        assert_eq!(b.mcp_report(), vec![("github".to_string(), Ok(3))]);
    }
}
