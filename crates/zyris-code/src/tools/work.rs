//! Hands big work to attacca. **It doesn't run here; it runs over there.**
//!
//! Some work is too big for one conversation (thread). attacca has **work** for that — it takes a
//! goal, plans it, splits it into a task graph, runs a **subagent in its own git worktree per
//! task**, then merges onto an integration branch. All this node does is **start and watch** the
//! work. The server does the execution.
//!
//! ```text
//! Draft → CheckingRequirements → [Gate 1] → Planning → [Gate 2] → Executing → Verifying → Done
//!                                 goal approval          plan approval
//! ```
//!
//! ## Both gates belong to the person
//!
//! `approve_work_goal`·`approve_work_plan` are **deliberately not exposed.** Those two are where a
//! person says "go ahead with this" — if the planner could pass itself through, the gates would
//! have no reason to exist. The agent reads which gate a work is stopped at via `status` and
//! **tells the person.** Approval happens on the attacca screen.
//!
//! ## Only this capability goes outside
//!
//! The other tools touch this computer's files and shell, but this one **creates work on the
//! server.** So the place it splits at the gate is different too — nothing to catch on path checks,
//! but instead `start`·`say`·`stop`·`resume` are **blocked in plan mode** (`gate::only_reads`).
//! Plan mode means "don't do anything yet", and waking a dozen subagents is the opposite of that.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use zyris::WireError;
use zyris_attacca::{AttaccaApi, AttaccaApiClient, ZNewWork, ZWork, ZWorkFilter, ZWorkState};

/// Maximum number of works returned at once. The list is for picking, not reading.
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

/// The implementation holding the attacca-side handle.
///
/// **The handle arrives after attaching.** Tools are announced before attach (`tools::announce`),
/// so here it's received via `watch` and grabbed at call time — if the connection drops and
/// reattaches, it switches to the new handle. If not attached yet, it says so: failing silently would leave the agent unable to find the cause.
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
            WireError::internal("Not attached to attacca yet. Call again in a moment.")
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
            return Err(WireError::invalid_params("`goal` is empty"));
        }
        let work =
            self.api()?.create_work(ZNewWork { message: goal, agent_id, project_id }).await?;
        Ok(view(&work))
    }

    async fn status(&self, work_id: String) -> zyris::Result<WorkStatus> {
        let api = self.api()?;
        let work = api.get_work(work_id.clone()).await?;
        // Tasks only appear once planning finishes. Their absence isn't treated as failure —
        // waiting in front of a gate is the normal state.
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

/// Picks out of `ZWork` only what the agent will actually use.
///
/// **Not handed over whole.** `ZWork` carries more than ten option fields, most of them server
/// internals — passed as-is, the actual "what should I do now" would be buried.
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

/// `AwaitingGoalApproval` → `awaiting_goal_approval`. Matches the name that goes on the wire.
pub(crate) fn state_name(debug: String) -> String {
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

/// What it's waiting for now. **Says who has to act too.**
///
/// Without this, the agent would read a work stopped at a gate as "stalled" and try to restart it —
/// what's actually needed is a single approval from a person.
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

    /// State names must **match the names that go on the wire.** Differently, the agent compares
    /// against the name it saw in the docs and matches nothing.
    #[test]
    fn a_state_reads_as_its_wire_name() {
        assert_eq!(state_name("AwaitingGoalApproval".into()), "awaiting_goal_approval");
        assert_eq!(state_name("Done".into()), "done");
        assert_eq!(state_name("CheckingRequirements".into()), "checking_requirements");
    }

    /// **At a gate it must say a person has to act.** Otherwise the agent reads it as stalled and
    /// creates one more work.
    #[test]
    fn a_gate_says_who_has_to_act() {
        for state in [ZWorkState::AwaitingGoalApproval, ZWorkState::AwaitingPlanApproval] {
            let said = next_step(state);
            assert!(said.contains("a person"), "{state:?}: {said}");
            assert!(said.contains("approve"), "{state:?}: {said}");
        }
        // Must not describe something running as waiting for approval.
        assert!(!next_step(ZWorkState::Executing).contains("person"));
    }

    /// **The wire name must split into exactly three.** attacca re-reads
    /// `zyris__{capability}_v{version}__{tool}` by splitting on `__`, so a `__` inside the name or
    /// a trailing `_` misaligns it right there. The test is always **join it back together and
    /// split it.**
    #[test]
    fn the_wire_name_splits_into_exactly_three() {
        for tool in ["start", "status", "list", "say", "stop", "resume"] {
            let wire = format!("zyris__work_v1__{tool}");
            let parts: Vec<&str> = wire.split("__").collect();
            assert_eq!(parts.len(), 3, "{wire}");
            assert_eq!(parts[1], "work_v1");
            assert_eq!(parts[2], tool);
        }
    }

    /// A failure must **point at where to read why.** "It failed" alone leaves no next move.
    #[test]
    fn a_failure_points_at_the_reason() {
        assert!(next_step(ZWorkState::Failed).contains("failure_reason"));
    }
}
