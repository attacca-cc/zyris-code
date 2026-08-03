//! 큰 일을 attacca에 넘긴다. **여기서 도는 것이 아니라 저쪽에서 돈다.**
//!
//! 대화 하나(thread)로 하기에 큰 일이 있다. attacca에는 그것을 위한 **work**가 있다 —
//! 목표를 받아 계획을 세우고, 태스크 그래프로 쪼개, 태스크마다 **제 git 워크트리에서
//! 서브에이전트가 돌린 뒤** 통합 브랜치로 합친다. 이 노드가 하는 일은 그 일을 **시작하고
//! 들여다보는 것**뿐이다. 실행은 서버가 한다.
//!
//! ```text
//! Draft → CheckingRequirements → [관문 1] → Planning → [관문 2] → Executing → Verifying → Done
//!                                 목표 승인              계획 승인
//! ```
//!
//! ## 관문 둘은 사람의 것이다
//!
//! `approve_work_goal`·`approve_work_plan`은 **일부러 안 내준다.** 그 둘은 "이대로 진행해도
//! 좋다"고 사람이 말하는 자리인데, 계획을 세운 쪽이 스스로 통과시키면 관문이 있을 이유가
//! 없어진다. 에이전트는 `status`로 지금 어느 관문에서 멈춰 있는지 읽고 **사람에게 말하면
//! 된다.** 승인은 attacca 화면에서 한다.
//!
//! ## 이 캐퍼빌리티만 밖으로 나간다
//!
//! 다른 도구는 이 컴퓨터의 파일과 셸을 만지지만 이것은 **서버에 일을 만든다.** 그래서
//! 게이트에서 갈리는 자리도 다르다 — 경로 판정에 걸릴 것이 없는 대신, `start`·`say`·
//! `stop`·`resume`은 **계획 모드에서 막힌다**(`gate::only_reads`). 계획 모드는 "아직
//! 아무것도 하지 마라"이고, 서브에이전트 열둘을 깨우는 것은 그 반대다.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use zyris::WireError;
use zyris_attacca::{AttaccaApi, AttaccaApiClient, ZNewWork, ZWork, ZWorkFilter, ZWorkState};

/// 한 번에 돌려주는 최대 work 수. 목록은 고르라고 주는 것이지 읽으라고 주는 것이 아니다.
const LIST_LIMIT: u32 = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkView {
    pub id: String,
    pub title: String,
    /// `draft`, `awaiting_goal_approval`, `planning`, `executing`, `done`, …
    pub state: String,
    /// What has to happen next, in one line. When a work is sitting at a gate this says who
    /// has to act — usually the person, not you.
    pub next: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// The measurable success criterion, once it is settled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_goal: Option<String>,
    /// Blockers found up front — a missing toolchain, a login nobody can perform for you.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskView {
    pub id: String,
    pub title: String,
    /// `pending`, `creating`, `verifying`, `done`, `failed`, …
    pub state: String,
    /// The task's own git branch, once it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkStatus {
    pub work: WorkView,
    /// The task graph, once planning has produced one. Empty before that.
    pub tasks: Vec<TaskView>,
}

#[zyris::capability(name = "work", version = 1)]
pub trait Work {
    /// Hand a goal to attacca's work runner, which plans it into tasks and runs each one in
    /// its own git worktree with a subagent. Use this for work too large for one thread —
    /// several independent changes, or anything you would otherwise do in a long sequence.
    ///
    /// Creating a work does **not** start it running. It stops at two gates for a person to
    /// approve: the goal, then the plan. Say so when you report back, and poll with status.
    ///
    /// `goal` is prose; its first line becomes the title. `project_id` decides which checkout
    /// the tasks may change — omit it for the account's default project. `agent_id` picks who
    /// plans the work; omit it for the Main Agent.
    async fn start(
        &self,
        goal: String,
        project_id: Option<String>,
        agent_id: Option<String>,
    ) -> zyris::Result<WorkView>;

    /// Where a work has got to, with its task graph. Poll this instead of guessing — a work
    /// that looks stalled is usually waiting for a person at one of the two gates.
    async fn status(&self, work_id: String) -> zyris::Result<WorkStatus>;

    /// Recent works on this account, newest first.
    async fn list(
        &self,
        project_id: Option<String>,
        limit: Option<u32>,
    ) -> zyris::Result<Vec<WorkView>>;

    /// Say something to the work's planner — argue with the plan, add a constraint, answer a
    /// question it raised. Goes to the planning conversation, not to a running task.
    async fn say(&self, work_id: String, message: String) -> zyris::Result<()>;

    /// Stop a work. Running tasks are cancelled; the branches they made stay.
    async fn stop(&self, work_id: String) -> zyris::Result<()>;

    /// Resume a work that halted for review.
    async fn resume(&self, work_id: String) -> zyris::Result<WorkView>;
}

/// attacca 쪽 손잡이를 들고 있는 구현.
///
/// **손잡이는 붙은 뒤에 온다.** 도구는 붙기 전에 announce되므로(`tools::announce`) 여기서는
/// `watch`로 받아 두고 부를 때 집는다 — 연결이 끊겼다 다시 붙으면 새 손잡이로 저절로
/// 갈아탄다. 아직 안 붙었으면 그렇게 말한다: 조용히 실패하면 에이전트가 원인을 못 찾는다.
#[derive(Clone)]
pub struct Works {
    api: watch::Receiver<Option<Arc<AttaccaApiClient>>>,
}

impl Works {
    pub fn new(api: watch::Receiver<Option<Arc<AttaccaApiClient>>>) -> Works {
        Works { api }
    }

    fn api(&self) -> Result<Arc<AttaccaApiClient>, WireError> {
        self.api.borrow().clone().ok_or_else(|| {
            WireError::internal("아직 attacca에 붙지 않았습니다. 잠시 뒤에 다시 불러 주세요.")
        })
    }
}

#[async_trait::async_trait]
impl Work for Works {
    async fn start(
        &self,
        goal: String,
        project_id: Option<String>,
        agent_id: Option<String>,
    ) -> zyris::Result<WorkView> {
        if goal.trim().is_empty() {
            return Err(WireError::invalid_params("goal이 비어 있습니다."));
        }
        let work =
            self.api()?.create_work(ZNewWork { message: goal, agent_id, project_id }).await?;
        Ok(view(&work))
    }

    async fn status(&self, work_id: String) -> zyris::Result<WorkStatus> {
        let api = self.api()?;
        let work = api.get_work(work_id.clone()).await?;
        // 태스크는 계획이 끝나야 생긴다. 없다고 실패로 만들지 않는다 — 관문 앞에서
        // 기다리는 것이 정상 상태다.
        let tasks = api.work_tasks(work_id).await.map(|t| t.tasks).unwrap_or_default();
        Ok(WorkStatus {
            work: view(&work),
            tasks: tasks
                .iter()
                .map(|t| TaskView {
                    id: t.id.clone(),
                    title: t.title.clone(),
                    state: state_name(format!("{:?}", t.state)),
                    branch: t.branch.clone(),
                })
                .collect(),
        })
    }

    async fn list(
        &self,
        project_id: Option<String>,
        limit: Option<u32>,
    ) -> zyris::Result<Vec<WorkView>> {
        let works = self
            .api()?
            .list_works(ZWorkFilter { project_id, limit: Some(limit.unwrap_or(LIST_LIMIT)) })
            .await?;
        Ok(works.iter().map(view).collect())
    }

    async fn say(&self, work_id: String, message: String) -> zyris::Result<()> {
        self.api()?.work_message(work_id, message, Vec::new()).await
    }

    async fn stop(&self, work_id: String) -> zyris::Result<()> {
        self.api()?.stop_work(work_id).await
    }

    async fn resume(&self, work_id: String) -> zyris::Result<WorkView> {
        let work = self.api()?.continue_work(work_id).await?;
        Ok(view(&work))
    }
}

/// `ZWork`에서 에이전트가 실제로 쓸 것만 뽑는다.
///
/// **통째로 넘기지 않는다.** `ZWork`에는 옵션 필드가 열 개 넘게 붙어 있고 대부분은 서버
/// 내부 사정이라, 그대로 주면 정작 "지금 무엇을 해야 하는가"가 묻힌다.
fn view(work: &ZWork) -> WorkView {
    WorkView {
        id: work.id.clone(),
        title: work.title.clone(),
        state: state_name(format!("{:?}", work.state)),
        next: next_step(work.state),
        project_id: work.project_id.clone(),
        final_goal: work.final_goal.clone(),
        blocked_by: work.requirements_report.clone(),
        failure_reason: work.failure_reason.clone(),
    }
}

/// `AwaitingGoalApproval` → `awaiting_goal_approval`. 와이어에 나가는 이름과 맞춘다.
fn state_name(debug: String) -> String {
    let mut out = String::with_capacity(debug.len() + 4);
    for (i, ch) in debug.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// 지금 무엇을 기다리는가. **누가 움직여야 하는지까지 말한다.**
///
/// 이것이 없으면 에이전트는 관문 앞에 선 work를 "멈췄다"로 읽고 다시 시작하려 든다 —
/// 실제로 필요한 것은 사람의 승인 한 번이다.
fn next_step(state: ZWorkState) -> String {
    match state {
        ZWorkState::Draft | ZWorkState::CheckingRequirements => {
            "Checking what this needs. Nothing to do yet.".into()
        }
        ZWorkState::AwaitingGoalApproval => {
            "Gate 1: a person has to approve the goal in attacca before planning starts. \
             Tell them, and do not wait on it."
                .into()
        }
        ZWorkState::Planning => "Being planned into tasks.".into(),
        ZWorkState::AwaitingPlanApproval => {
            "Gate 2: a person has to approve the plan in attacca before anything runs. \
             Tell them, and do not wait on it."
                .into()
        }
        ZWorkState::Executing => "Tasks are running, each in its own worktree.".into(),
        ZWorkState::Halted => "Paused after a phase. Call resume to carry on.".into(),
        ZWorkState::Verifying => "Checking the result against the goal.".into(),
        ZWorkState::Done => "Finished.".into(),
        ZWorkState::Failed => "Failed. Read failure_reason.".into(),
        ZWorkState::Cancelled => "Cancelled.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 상태 이름은 **와이어에 나가는 이름과 같아야 한다.** 다르면 에이전트가 문서에서 본
    /// 이름으로 견주다 아무것도 못 맞춘다.
    #[test]
    fn a_state_reads_as_its_wire_name() {
        assert_eq!(state_name("AwaitingGoalApproval".into()), "awaiting_goal_approval");
        assert_eq!(state_name("Done".into()), "done");
        assert_eq!(state_name("CheckingRequirements".into()), "checking_requirements");
    }

    /// **관문에서는 사람이 움직여야 한다고 말해야 한다.** 안 그러면 에이전트가 멈춘 것으로
    /// 읽고 work를 하나 더 만든다.
    #[test]
    fn a_gate_says_who_has_to_act() {
        for state in [ZWorkState::AwaitingGoalApproval, ZWorkState::AwaitingPlanApproval] {
            let said = next_step(state);
            assert!(said.contains("a person"), "{state:?}: {said}");
            assert!(said.contains("approve"), "{state:?}: {said}");
        }
        // 도는 중인 것을 승인 기다리는 것으로 말하면 안 된다.
        assert!(!next_step(ZWorkState::Executing).contains("person"));
    }

    /// **와이어 이름이 정확히 넷으로 갈라져야 한다.** attacca가 `zyris__{노드}__{캐퍼빌리티}
    /// __{도구}`를 `__`로 쪼개 되읽으므로, 이름 안에 `__`가 있거나 끝이 `_`이면 그 자리에서
    /// 어긋난다. 판정은 언제나 **이어 붙여 쪼개 보는 것**이다.
    #[test]
    fn the_wire_name_splits_into_exactly_four() {
        for tool in ["start", "status", "list", "say", "stop", "resume"] {
            let wire = format!("zyris__arch-zyris-code__work__{tool}");
            let parts: Vec<&str> = wire.split("__").collect();
            assert_eq!(parts.len(), 4, "{wire}");
            assert_eq!(parts[2], "work");
            assert_eq!(parts[3], tool);
        }
    }

    /// 실패는 **왜인지 읽을 자리를 가리켜야 한다.** "실패했습니다"만으로는 다음 수가 없다.
    #[test]
    fn a_failure_points_at_the_reason() {
        assert!(next_step(ZWorkState::Failed).contains("failure_reason"));
    }
}
