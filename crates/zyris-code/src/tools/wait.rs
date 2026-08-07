//! The waiting tool. **Not a background-run tool** — a local build, a remote build and an
//! attacca work all wait through this same tool.
//!
//! **This whole file is `until`'s return contract.** Finished or not it answers with success,
//! it comes back inside the wire deadline, and when it is not finished it says to call again.
//!
//! This is what it fixes: when `terminal.exec` is cut off at 50s, kills the process tree with
//! it and hands back `timed_out: true, exit_code: -1`, the agent **reads that as a failure and
//! stops.** The agent is right to — what needs fixing is the side that makes that shape.

use std::path::PathBuf;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use zyris::WireError;
// `AttaccaApi` is a trait. Import only the client and every method is "method not found".
use zyris_attacca::{AttaccaApi, AttaccaApiClient};

use crate::tools::bridge::Bridge;
use crate::tools::jobs::{Chunk, Jobs, Snapshot, Spec};

/// The most one `logs` hands back. The rest is announced with `more`.
const LOGS_BYTES: usize = 16_000;
/// The output tail carried in a result. **Carrying the whole thing every time leaves no
/// context** — a 20-minute build takes some 25 checks. The whole thing comes via `wait.logs`.
const TAIL_BYTES: usize = 2000;
/// Room for us to build an answer and send it. The deadline minus this is one call's budget.
const HEADROOM: std::time::Duration = std::time::Duration::from_secs(5);
/// The cap when the deadline is off. That means the other side does not cut us off, but we
/// cannot hold on forever either.
const NO_DEADLINE_CAP: std::time::Duration = std::time::Duration::from_secs(600);
/// The default gap between probes.
const PROBE_EVERY_MS: u64 = 5_000;
/// Its floor. **A probe command has to be cheap** — it stops the mistake of putting
/// `cargo build` on a probe.
const PROBE_FLOOR_MS: u64 = 2_000;
/// The limit on one probe command. Past it that round counts as false.
const PROBE_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The gap between probes of an attacca work. **That side has no notice, so polling is all.**
const WORK_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

fn probe_gap(every_ms: Option<u64>) -> std::time::Duration {
    std::time::Duration::from_millis(every_ms.unwrap_or(PROBE_EVERY_MS).max(PROBE_FLOOR_MS))
}

/// Has the work reached **a place where a person or the agent has to move**?
///
/// The two gates, halted, and done. `Planning` and `Executing` mean that side is still
/// running, and waking the agent there brings it back with nothing to do but check again.
///
/// **Deliberately exhaustive** — when upstream grows a state, compilation has to break here
/// so that someone decides which side the new state belongs on.
fn work_needs_someone(state: zyris_attacca::ZWorkState) -> bool {
    use zyris_attacca::ZWorkState::*;
    match state {
        CheckingRequirements | Planning | Executing | Verifying => false,
        Draft | AwaitingGoalApproval | AwaitingPlanApproval | Halted | Done | Failed
        | Cancelled => true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Started {
    /// Pass this to `wait.until`, `wait.logs` and `wait.stop`.
    pub id: String,
    pub label: String,
    /// What to do next. Follow it.
    pub next: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobRef {
    pub id: String,
    pub label: String,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Logs {
    pub text: String,
    /// Pass this back as `offset` to read on from here.
    pub next_offset: u64,
    /// More is buffered. **Call again and you get it.**
    pub more: bool,
    /// Bytes lost for good to overflow. Calling again will not bring them back.
    pub dropped: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[zyris::capability(name = "wait", version = 1)]
pub trait Wait {
    /// Run a command in the background and return at once. Use this for anything that might
    /// take more than a minute — builds, test suites, deploys — then wait with `until`.
    ///
    /// Give exactly one of `command` (a shell one-liner) or `argv` (a program and its
    /// arguments, run without a shell). `cwd` resolves against the working directory unless
    /// it starts with `/`. `env` takes `KEY=VALUE` lines. `label` is what a person sees on
    /// screen; it defaults to the command.
    async fn start(
        &self,
        command: Option<String>,
        argv: Option<Vec<String>>,
        cwd: Option<String>,
        env: Option<Vec<String>>,
        label: Option<String>,
    ) -> zyris::Result<Started>;

    /// Background jobs from this session, oldest first.
    async fn list(&self) -> zyris::Result<Vec<JobRef>>;

    /// A job's output from `offset` (0 for the beginning). Output stays readable after the
    /// job has ended, so you can read the whole thing once `until` says it is done.
    async fn logs(&self, job: String, offset: Option<u64>) -> zyris::Result<Logs>;

    /// Wait until something finishes, answering within the call deadline. **Not finishing is
    /// not an error** — when `done` is false, call again with the same arguments.
    ///
    /// Give exactly one of: `job` (a background job from `start`), `command` (re-run it until
    /// it exits 0, or until its output matches `matches`), or `work` (an attacca work id).
    /// `every_ms` is the gap between re-runs of `command`, at least 2000. `timeout_ms` caps
    /// this one call; it is trimmed to the deadline, never past it.
    async fn until(
        &self,
        job: Option<String>,
        command: Option<String>,
        work: Option<String>,
        matches: Option<String>,
        every_ms: Option<u64>,
        timeout_ms: Option<u64>,
    ) -> zyris::Result<Outcome>;

    /// Kill a background job and everything it started. A job that already ended is fine.
    async fn stop(&self, job: String) -> zyris::Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Outcome {
    /// True when the thing you waited for has finished. **False is not a failure** — it
    /// means not yet.
    pub done: bool,
    /// What happened, in one line.
    pub why: String,
    /// What to do next. When `done` is false this tells you to call again.
    pub next: String,
    pub elapsed_ms: u64,
    /// The last few lines of output, when there are any. Read it all with `logs`.
    pub tail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// The time this one call may spend.
///
/// **attacca cuts a node call off at 60s with an error.** We have to answer with success
/// before that or the agent reads it as a failure. The deadline is a ceiling, not a default —
/// when the agent asks for less, that wins.
fn budget(deadline: Option<std::time::Duration>, timeout_ms: Option<u64>) -> std::time::Duration {
    let asked = timeout_ms.map(std::time::Duration::from_millis);
    match deadline {
        Some(d) => {
            let room = d.saturating_sub(HEADROOM).max(std::time::Duration::from_secs(1));
            asked.map(|a| a.min(room)).unwrap_or(room)
        }
        None => asked.unwrap_or(NO_DEADLINE_CAP),
    }
}

/// The implementation, holding `Jobs` and the attacca handle.
///
/// **The handle arrives after we connect** — tools are announced before that, so we take it
/// over a `watch` and pick it up when called. The same trick as `Works` in `work.rs`.
#[derive(Clone)]
pub struct Waits {
    pub(crate) jobs: Jobs,
    api: watch::Receiver<Option<Arc<AttaccaApiClient>>>,
    /// The way to the screen. **This is the only place that knows the screen** — `jobs.rs`
    /// must know only processes and buffers so its tests run without one.
    bridge: Bridge,
}

impl Waits {
    pub fn new(
        jobs: Jobs,
        api: watch::Receiver<Option<Arc<AttaccaApiClient>>>,
        bridge: Bridge,
    ) -> Waits {
        Waits { jobs, api, bridge }
    }

    /// Put a started job on the screen, and clear it once it ends.
    ///
    /// Only the reaping task in `Jobs` knows when it ends, so we wait on that signal here.
    /// **To keep one single way to the screen** we did not plant a callback in `jobs.rs`.
    fn tell_the_screen(&self, id: &str, label: &str) {
        self.bridge
            .frame(crate::app::Frame::JobStart { id: id.to_string(), label: label.to_string() });
        let (Some(mut ended), bridge, jobs, id) =
            (self.jobs.ended(id), self.bridge.clone(), self.jobs.clone(), id.to_string())
        else {
            return;
        };
        tokio::spawn(async move {
            while !*ended.borrow() {
                if ended.changed().await.is_err() {
                    return;
                }
            }
            let snap = jobs.snapshot(&id);
            let ok = snap.as_ref().is_some_and(|s| s.exit_code == Some(0));
            let secs = snap.map(|s| s.elapsed_ms / 1000).unwrap_or(0);
            bridge.frame(crate::app::Frame::JobEnded { id, ok, secs });
        });
    }

    pub(crate) fn api(&self) -> Result<Arc<AttaccaApiClient>, WireError> {
        self.api.borrow().clone().ok_or_else(|| {
            WireError::internal("아직 attacca에 붙지 않았습니다. 잠시 뒤에 다시 불러 주세요.")
        })
    }

    /// An unknown job is **an error.** Hand back a quietly empty result and the agent takes
    /// the tool for broken and tries the same thing another way.
    fn known(&self, id: &str) -> Result<Snapshot, WireError> {
        self.jobs.snapshot(id).ok_or_else(|| {
            WireError::invalid_params(format!(
                "`{id}`이라는 배경 작업이 없습니다. wait.list로 확인해 주세요."
            ))
        })
    }
}

#[async_trait::async_trait]
impl Wait for Waits {
    async fn start(
        &self,
        command: Option<String>,
        argv: Option<Vec<String>>,
        cwd: Option<String>,
        env: Option<Vec<String>>,
        label: Option<String>,
    ) -> zyris::Result<Started> {
        let spec = Spec {
            command,
            argv,
            cwd: cwd.filter(|s| !s.is_empty()).map(PathBuf::from),
            // Taken as `KEY=VALUE` lines. Models get a map in the schema wrong far too often.
            env: env
                .unwrap_or_default()
                .iter()
                .filter_map(|line| line.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            label,
        };
        let id = self.jobs.start(spec).map_err(WireError::invalid_params)?;
        let snap = self.known(&id)?;
        self.tell_the_screen(&id, &snap.label);
        Ok(Started {
            id: id.clone(),
            label: snap.label,
            next: format!("걸었습니다. `wait.until`을 `job: \"{id}\"`로 불러 끝나기를 기다리세요."),
        })
    }

    async fn list(&self) -> zyris::Result<Vec<JobRef>> {
        Ok(self.jobs.list().into_iter().map(job_ref).collect())
    }

    async fn logs(&self, job: String, offset: Option<u64>) -> zyris::Result<Logs> {
        let snap = self.known(&job)?;
        let offset = offset.unwrap_or(0);
        let chunk = self.jobs.read(&job, offset).unwrap_or_else(empty_chunk);
        // Where this chunk actually starts, absolute. Asking for dropped bytes pushes it on.
        let start = offset + chunk.dropped;
        // Not all of it at once. **The rest is announced with `more`** — cut without saying
        // so and the agent takes it for the whole output.
        let (text, more, next_offset) = if chunk.text.len() > LOGS_BYTES {
            let cut = chunk.text.floor_char_boundary(LOGS_BYTES);
            (chunk.text[..cut].to_string(), true, start + cut as u64)
        } else {
            (chunk.text, false, chunk.next_offset)
        };
        Ok(Logs { text, next_offset, more, dropped: chunk.dropped, exit_code: snap.exit_code })
    }

    async fn until(
        &self,
        job: Option<String>,
        command: Option<String>,
        work: Option<String>,
        matches: Option<String>,
        every_ms: Option<u64>,
        timeout_ms: Option<u64>,
    ) -> zyris::Result<Outcome> {
        let chosen = [job.is_some(), command.is_some(), work.is_some()];
        if chosen.iter().filter(|c| **c).count() != 1 {
            return Err(WireError::invalid_params("job·command·work 중 정확히 하나를 주세요."));
        }
        let budget = budget(crate::tools::guard::wire_deadline(), timeout_ms);
        let at = std::time::Instant::now();

        if let Some(id) = job {
            return self.until_job(&id, budget, at).await;
        }
        if let Some(line) = command {
            let gap = probe_gap(every_ms);
            return self.until_probe(&line, matches.as_deref(), gap, budget, at).await;
        }
        let work = work.expect("exactly one of the three is set");
        self.until_work(&work, budget, at).await
    }

    async fn stop(&self, job: String) -> zyris::Result<()> {
        self.known(&job)?;
        self.jobs.stop(&job);
        Ok(())
    }
}

impl Waits {
    /// Wait for a background job to end. **The moment it ends, this returns.**
    async fn until_job(
        &self,
        id: &str,
        budget: std::time::Duration,
        at: std::time::Instant,
    ) -> zyris::Result<Outcome> {
        let snap = self.known(id)?;
        if !snap.running {
            return Ok(self.finished(id, snap, at));
        }
        let Some(mut ended) = self.jobs.ended(id) else {
            return Ok(self.finished(id, self.known(id)?, at));
        };
        let waited = tokio::time::timeout(budget, async {
            while !*ended.borrow() {
                if ended.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;

        let snap = self.known(id)?;
        if waited.is_ok() && !snap.running {
            return Ok(self.finished(id, snap, at));
        }
        // **This is the point of the whole file.** Not finished is still not an error — hand
        // back an error and the agent takes the build for failed and stops.
        Ok(Outcome {
            done: false,
            why: format!(
                "`{id}`({})이 아직 돌고 있습니다. {}초째입니다.",
                snap.label,
                snap.elapsed_ms / 1000
            ),
            next: format!("같은 인자로 `wait.until`을 다시 부르세요 — `job: \"{id}\"`."),
            elapsed_ms: at.elapsed().as_millis() as u64,
            tail: self.jobs.tail(id, TAIL_BYTES),
            exit_code: None,
        })
    }

    /// Re-run a command until it is true. **Remote builds, CI, a file appearing, a port
    /// opening — all of it takes this branch.**
    ///
    /// True is exit code 0 by default; give `matches` and it is true when the output hits
    /// that regex.
    async fn until_probe(
        &self,
        command: &str,
        matches: Option<&str>,
        gap: std::time::Duration,
        budget: std::time::Duration,
        at: std::time::Instant,
    ) -> zyris::Result<Outcome> {
        let re = matches
            .map(|p| {
                regex::Regex::new(p).map_err(|e| {
                    WireError::invalid_params(format!("matches가 정규식이 아닙니다: {e}"))
                })
            })
            .transpose()?;
        let root = self.jobs.root();
        let mut last;
        let mut rounds = 0u32;
        loop {
            // **At least one round always runs.** Returning without a single look because
            // the budget is short leaves the caller with nothing.
            rounds += 1;
            let left = budget.saturating_sub(at.elapsed());
            let limit = PROBE_LIMIT.min(left.max(std::time::Duration::from_secs(1)));
            let (ok, out) = probe_once(command, root, limit).await;
            last = out;
            let hit = match &re {
                Some(re) => re.is_match(&last),
                None => ok,
            };
            if hit {
                return Ok(Outcome {
                    done: true,
                    why: format!("{rounds}번째 확인에서 조건이 참이 되었습니다."),
                    next: "끝났습니다. 다음 일을 하세요.".into(),
                    elapsed_ms: at.elapsed().as_millis() as u64,
                    tail: tail_of(&last),
                    exit_code: None,
                });
            }
            // No time left for another round, so answer here.
            if budget.saturating_sub(at.elapsed()) <= gap {
                break;
            }
            tokio::time::sleep(gap).await;
        }
        Ok(Outcome {
            done: false,
            why: format!("{rounds}번 확인했지만 아직 조건이 참이 아닙니다."),
            next: "같은 인자로 `wait.until`을 다시 부르세요.".into(),
            elapsed_ms: at.elapsed().as_millis() as u64,
            tail: tail_of(&last),
            exit_code: None,
        })
    }

    /// Wait for an attacca work to reach **a place where a person or the agent has to move**.
    ///
    /// That side has no notice, so polling is all. There is still one reason this branch has
    /// to live here — **upstream zyris does not know attacca.** This repo is the only place
    /// the three can be tied into one tool.
    async fn until_work(
        &self,
        work_id: &str,
        budget: std::time::Duration,
        at: std::time::Instant,
    ) -> zyris::Result<Outcome> {
        let api = self.api()?;
        let mut state;
        loop {
            let work = api.get_work(work_id.to_string()).await?;
            state = work.state;
            if work_needs_someone(state) {
                let name = crate::tools::work::state_name(format!("{state:?}"));
                return Ok(Outcome {
                    done: true,
                    why: format!("work `{work_id}`이 `{name}`에 닿았습니다."),
                    next: format!(
                        "`work.status`로 `work_id: \"{work_id}\"`를 읽고 다음에 무엇이 \
                         필요한지 사람에게 말하세요."
                    ),
                    elapsed_ms: at.elapsed().as_millis() as u64,
                    tail: String::new(),
                    exit_code: None,
                });
            }
            if budget.saturating_sub(at.elapsed()) <= WORK_EVERY {
                break;
            }
            tokio::time::sleep(WORK_EVERY).await;
        }
        let name = crate::tools::work::state_name(format!("{state:?}"));
        Ok(Outcome {
            done: false,
            why: format!("work `{work_id}`이 아직 `{name}`입니다."),
            next: format!("같은 인자로 `wait.until`을 다시 부르세요 — `work: \"{work_id}\"`."),
            elapsed_ms: at.elapsed().as_millis() as u64,
            tail: String::new(),
            exit_code: None,
        })
    }

    fn finished(&self, id: &str, snap: Snapshot, at: std::time::Instant) -> Outcome {
        let ok = snap.exit_code == Some(0);
        Outcome {
            done: true,
            why: format!(
                "`{id}`({})이 {}초 만에 끝났습니다. 종료 코드 {}.",
                snap.label,
                snap.elapsed_ms / 1000,
                snap.exit_code.unwrap_or(-1)
            ),
            next: if ok {
                "끝났습니다. 전문이 필요하면 `wait.logs`로 가져오세요.".into()
            } else {
                format!("실패했습니다. `wait.logs`로 `job: \"{id}\"`의 출력을 읽어 원인을 보세요.")
            },
            elapsed_ms: at.elapsed().as_millis() as u64,
            tail: self.jobs.tail(id, TAIL_BYTES),
            exit_code: snap.exit_code,
        }
    }
}

fn job_ref(s: Snapshot) -> JobRef {
    JobRef {
        id: s.id,
        label: s.label,
        running: s.running,
        exit_code: s.exit_code,
        elapsed_ms: s.elapsed_ms,
    }
}

/// One probe round. **Not left in the job list** — a line piling up per probe makes `/jobs`
/// unusable, and a probe is a question, not a job.
async fn probe_once(
    command: &str,
    root: &std::path::Path,
    limit: std::time::Duration,
) -> (bool, String) {
    // The same shell `wait.start` uses — and on Windows that is `cmd /C`, not `/bin/sh`.
    let mut cmd = crate::tools::jobs::shell_running(command);
    cmd.current_dir(root);
    cmd.env("TERM", "dumb").env("NO_COLOR", "1");
    cmd.stdin(std::process::Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);

    match tokio::time::timeout(limit, cmd.output()).await {
        Ok(Ok(out)) => {
            let mut strip = crate::tools::jobs::Stripper::default();
            let mut text = strip.push(&out.stdout);
            text.push_str(&strip.push(&out.stderr));
            text.push_str(&strip.flush());
            (out.status.success(), text)
        }
        // **Over the limit or unable to spawn means that round is false.** Not an error —
        // waiting on a server that is not up yet is a normal use of this tool.
        Ok(Err(e)) => (false, format!("되묻기를 띄우지 못했습니다: {e}")),
        Err(_) => (false, format!("되묻기가 {}초를 넘겨 끊었습니다.", limit.as_secs())),
    }
}

fn tail_of(text: &str) -> String {
    let from = text.floor_char_boundary(text.len().saturating_sub(TAIL_BYTES));
    text[from..].to_string()
}

fn empty_chunk() -> Chunk {
    Chunk { text: String::new(), next_offset: 0, more: false, dropped: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waits() -> Waits {
        let (tx, rx) = watch::channel(None);
        // Keep the sender alive — dropping it still lets `borrow` see the last value, but it
        // blurs what the test is measuring.
        std::mem::forget(tx);
        Waits::new(Jobs::new(std::env::temp_dir()), rx, Bridge::new())
    }

    /// The budget is **not read from the environment.** `guard`'s tests set and clear
    /// `ZYRIS_CODE_WIRE_DEADLINE_SECS` while these run, so measuring the budget through the
    /// public path gives a different value every run. The calculation itself is locked purely
    /// by `the_budget_never_outlives_the_wire_deadline`.
    fn ms(n: u64) -> std::time::Duration {
        std::time::Duration::from_millis(n)
    }

    async fn wait_for(w: &Waits, id: &str) {
        let mut ended = w.jobs.ended(id).expect("the job must exist");
        while !*ended.borrow() {
            ended.changed().await.expect("the sender must stay alive");
        }
    }

    #[tokio::test]
    async fn starting_a_job_says_what_to_do_next() {
        let w = waits();
        let started = w.start(Some("echo hi".into()), None, None, None, None).await.unwrap();
        assert_eq!(started.id, "b1");
        assert_eq!(started.label, "echo hi");
        // **It says what to do next.** Without it the agent starts a job and forgets it.
        assert!(started.next.contains("wait.until"), "{}", started.next);
    }

    #[tokio::test]
    async fn a_job_shows_up_in_the_list_and_its_logs_are_readable() {
        let w = waits();
        let id = w.start(Some("echo 안녕".into()), None, None, None, None).await.unwrap().id;
        wait_for(&w, &id).await;

        let list = w.list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].running);

        let logs = w.logs(id.clone(), None).await.unwrap();
        assert!(logs.text.contains("안녕"), "{}", logs.text);
        assert_eq!(logs.exit_code, Some(0));
        assert!(!logs.more);
        // Reading on gives back nothing — the same bytes must never come twice.
        assert_eq!(w.logs(id, Some(logs.next_offset)).await.unwrap().text, "");
    }

    /// When it is cut, **it says so** and gives the place to read on from.
    #[tokio::test]
    async fn a_long_log_is_cut_but_says_there_is_more() {
        let w = waits();
        let id = w
            .start(Some("head -c 40000 /dev/zero | tr '\\0' 'a'".into()), None, None, None, None)
            .await
            .unwrap()
            .id;
        wait_for(&w, &id).await;
        let first = w.logs(id.clone(), None).await.unwrap();
        assert!(first.more);
        assert_eq!(first.text.len(), LOGS_BYTES);
        let second = w.logs(id, Some(first.next_offset)).await.unwrap();
        assert!(!second.text.is_empty());
    }

    #[tokio::test]
    async fn stopping_a_job_that_already_ended_is_not_an_error() {
        let w = waits();
        let id = w.start(Some("true".into()), None, None, None, None).await.unwrap().id;
        wait_for(&w, &id).await;
        assert!(w.stop(id).await.is_ok());
    }

    /// Hand back a quietly empty result and the agent takes the tool for broken and tries the
    /// same thing another way.
    #[tokio::test]
    async fn an_unknown_job_is_an_error_not_an_empty_answer() {
        let w = waits();
        assert!(w.logs("b9".into(), None).await.is_err());
        assert!(w.stop("b9".into()).await.is_err());
    }

    /// Wrong arguments are an error. **That is the only error.**
    #[tokio::test]
    async fn starting_with_neither_command_nor_argv_is_an_error() {
        let w = waits();
        assert!(w.start(None, None, None, None, None).await.is_err());
    }

    /// **The budget never outlives the deadline.** attacca cuts off at 60s with an error, so
    /// we have to answer with success before that. It is measured through the pure function
    /// because shaking an environment variable treads on other tests running alongside.
    #[test]
    fn the_budget_never_outlives_the_wire_deadline() {
        let deadline = Some(std::time::Duration::from_secs(55));
        // Asking for 10 minutes still gets trimmed inside the deadline.
        assert_eq!(budget(deadline, Some(600_000)), std::time::Duration::from_secs(50));
        // Asking for less wins — **the deadline is a ceiling, not a default.**
        assert_eq!(budget(deadline, Some(3_000)), std::time::Duration::from_millis(3_000));
        assert_eq!(budget(deadline, None), std::time::Duration::from_secs(50));
        // With the deadline off (once that side is fixed) only the agent's value counts.
        assert_eq!(budget(None, Some(600_000)), std::time::Duration::from_secs(600));
    }

    /// **The canonical statement of the bug.** Not finished is not a failure — the agent
    /// reading `exec`'s `timed_out: true, exit_code: -1` as a failure is why this work exists.
    #[tokio::test]
    async fn an_unfinished_wait_answers_success_not_an_error() {
        let w = waits();
        let id = w.start(Some("sleep 30".into()), None, None, None, None).await.unwrap().id;
        let out = w
            .until_job(&id, ms(1500), std::time::Instant::now())
            .await
            .expect("not finishing must not raise an error");
        assert!(!out.done);
        assert_eq!(out.exit_code, None);
        // It has to say to call again. Otherwise the agent reads it as "stuck" and gives up.
        assert!(out.next.contains("wait.until"), "{}", out.next);
        assert!(out.why.contains(&id), "{}", out.why);
        w.stop(id).await.unwrap();
    }

    /// It comes back inside its budget.
    #[tokio::test]
    async fn a_wait_answers_before_its_budget_runs_out() {
        let w = waits();
        let id = w.start(Some("sleep 30".into()), None, None, None, None).await.unwrap().id;
        let at = std::time::Instant::now();
        let _ = w.until_job(&id, ms(1000), at).await.unwrap();
        assert!(at.elapsed() < std::time::Duration::from_secs(3), "{:?}", at.elapsed());
        w.stop(id).await.unwrap();
    }

    /// Once it ends, it returns on the spot instead of waiting out the budget.
    #[tokio::test]
    async fn a_wait_returns_the_moment_the_job_ends() {
        let w = waits();
        let id = w.start(Some("sleep 1; echo 끝".into()), None, None, None, None).await.unwrap().id;
        let at = std::time::Instant::now();
        let out = w.until_job(&id, ms(20_000), at).await.unwrap();
        assert!(out.done, "{}", out.why);
        assert_eq!(out.exit_code, Some(0));
        assert!(out.tail.contains("끝"), "{}", out.tail);
        assert!(at.elapsed() < std::time::Duration::from_secs(5), "{:?}", at.elapsed());
    }

    /// Waiting on something already finished answers done at once. **Budget plays no part**,
    /// so this one goes through the public path — it locks the branch wiring too.
    #[tokio::test]
    async fn waiting_on_an_already_finished_job_answers_at_once() {
        let w = waits();
        let id = w.start(Some("exit 7".into()), None, None, None, None).await.unwrap().id;
        wait_for(&w, &id).await;
        let out = w.until(Some(id), None, None, None, None, None).await.unwrap();
        assert!(out.done);
        assert_eq!(out.exit_code, Some(7));
        // A failed job says **where to look**.
        assert!(out.next.contains("wait.logs"), "{}", out.next);
    }

    /// Exactly one thing to wait for, no more and no fewer.
    #[tokio::test]
    async fn until_needs_exactly_one_thing_to_wait_for() {
        let w = waits();
        assert!(w.until(None, None, None, None, None, None).await.is_err());
        assert!(w
            .until(Some("b1".into()), Some("true".into()), None, None, None, None)
            .await
            .is_err());
    }

    /// Turning true ends the wait on the spot. **Budget plays no part**, so this one goes
    /// through the public path.
    #[tokio::test]
    async fn a_probe_that_succeeds_ends_the_wait() {
        let w = waits();
        let out = w.until(None, Some("true".into()), None, None, None, None).await.unwrap();
        assert!(out.done, "{}", out.why);
    }

    /// Output hitting the pattern is true — **this is how a remote build's state is read.**
    /// The pattern wins even when the exit code is 0.
    #[tokio::test]
    async fn a_probe_can_match_on_output_instead_of_exit_code() {
        let w = waits();
        let out = w
            .until_probe(
                "echo status=queued",
                Some("completed"),
                probe_gap(None),
                ms(1500),
                std::time::Instant::now(),
            )
            .await
            .unwrap();
        assert!(!out.done, "{}", out.why);
        assert!(out.tail.contains("queued"), "{}", out.tail);
    }

    /// **A probe command has to be cheap.** Asking for a gap under the floor does not go under.
    #[test]
    fn a_probe_command_cannot_run_more_often_than_the_floor() {
        assert_eq!(probe_gap(Some(10)), std::time::Duration::from_millis(PROBE_FLOOR_MS));
        assert_eq!(probe_gap(None), std::time::Duration::from_millis(PROBE_EVERY_MS));
        assert_eq!(probe_gap(Some(30_000)), std::time::Duration::from_millis(30_000));
    }

    /// **It really does run again.** Measured with a command that only turns true on the
    /// second round.
    #[tokio::test]
    async fn a_probe_actually_runs_again_until_it_is_true() {
        let w = waits();
        let dir = tempfile::tempdir().unwrap();
        let n = dir.path().join("n").display().to_string();
        let out = w
            .until_probe(
                &format!("echo x >> {n}; test $(wc -c < {n}) -ge 3"),
                None,
                probe_gap(Some(0)), // rises to the floor (2s)
                ms(20_000),
                std::time::Instant::now(),
            )
            .await
            .unwrap();
        assert!(out.done, "{}", out.why);
        // Two bytes pile up per round, so four bytes means it ran twice.
        assert_eq!(std::fs::read(&n).unwrap().len(), 4);
    }

    /// Even when it never hits, it comes back **with success** inside the deadline and says
    /// to call again.
    #[tokio::test]
    async fn a_probe_that_never_succeeds_still_answers_success() {
        let w = waits();
        let at = std::time::Instant::now();
        let out = w.until_probe("false", None, probe_gap(None), ms(1500), at).await.unwrap();
        assert!(!out.done);
        assert!(out.next.contains("wait.until"), "{}", out.next);
        assert!(at.elapsed() < std::time::Duration::from_secs(4), "{:?}", at.elapsed());
    }

    /// Not a regex is an argument error. This one is a real error.
    #[tokio::test]
    async fn a_broken_pattern_is_an_argument_error() {
        let w = waits();
        assert!(w
            .until(None, Some("true".into()), None, Some("[".into()), None, Some(1000))
            .await
            .is_err());
    }

    /// **A wire name has to split into exactly four.** This repo got it wrong twice, and both
    /// times the local tests stayed green and it surfaced live.
    #[test]
    fn the_wire_name_splits_into_exactly_four() {
        use zyris::ServeCapability;
        let d = WaitServer(waits()).descriptor();
        assert!(!d.name.contains("__") && !d.name.ends_with('_'), "{}", d.name);
        for tool in ["start", "until", "list", "logs", "stop"] {
            assert!(d.tools.iter().any(|t| t.name == tool), "{tool} is missing");
            let wire = format!("zyris__arch__{}__{tool}", d.name);
            assert_eq!(wire.split("__").count(), 4, "{wire}");
        }
    }

    /// Do the descriptions fit the budget? The twin of the file_io test in `trim.rs`.
    #[test]
    fn the_announced_wait_fits_the_budget() {
        use zyris::ServeCapability;
        let gate = crate::tools::guard::Gate::new(
            WaitServer(waits()),
            crate::tools::bridge::Bridge::new(),
        );
        for tool in gate.descriptor().tools {
            assert!(
                tool.description.len() <= crate::tools::trim::DESCRIPTION_LIMIT,
                "{}: {} bytes\n{}",
                tool.name,
                tool.description.len(),
                tool.description
            );
        }
    }

    /// **Wake only at a gate, at halted, at done.** Sending the agent back while that side is
    /// still running wakes it with nothing to do, and then it only repeats the check.
    #[test]
    fn a_work_only_wakes_the_agent_where_someone_must_move() {
        use zyris_attacca::ZWorkState::*;
        for state in [CheckingRequirements, Planning, Executing, Verifying] {
            assert!(!work_needs_someone(state), "{state:?}");
        }
        for state in
            [Draft, AwaitingGoalApproval, AwaitingPlanApproval, Halted, Done, Failed, Cancelled]
        {
            assert!(work_needs_someone(state), "{state:?}");
        }
    }

    /// Not connected says so. **A quiet failure leaves the cause unfindable.**
    #[tokio::test]
    async fn waiting_on_a_work_before_attacca_is_up_says_so() {
        let w = waits();
        let err = w.until(None, None, Some("w_1".into()), None, None, Some(1000)).await;
        assert!(err.is_err());
    }

    /// `KEY=VALUE` lines become the environment as they are.
    #[tokio::test]
    async fn env_lines_reach_the_command() {
        let w = waits();
        let id = w
            .start(
                Some("echo $FOO".into()),
                None,
                None,
                Some(vec!["FOO=바".into()]),
                Some("환경 시험".into()),
            )
            .await
            .unwrap()
            .id;
        wait_for(&w, &id).await;
        assert_eq!(w.logs(id, None).await.unwrap().text.trim(), "바");
    }
}
