//! Commands put in the background. **Spawn them, collect them, kill them by group.**
//!
//! This file knows only processes and buffers. The wire surface (the deadline contract,
//! `until`'s branches) is `wait.rs`. They are split apart for the tests — in one file even
//! the tests that measure the deadline would have to spawn a real process.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use regex::Regex;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;

/// Output cap for one job. On overflow the front is lost and `dropped` grows to match.
const RING_CAP: usize = 1024 * 1024;
/// How many jobs we hold. Over that, the oldest **finished** ones go first.
const KEEP: usize = 20;
/// Grace between `stop` and SIGKILL. Room for a deploy script to clean up.
const GRACE: Duration = Duration::from_secs(2);
/// Grace when quitting the app. **It has to be short here** — hold the way out for two
/// seconds and it reads as a keypress that did not land.
const QUIT_GRACE: Duration = Duration::from_millis(300);

/// ANSI escapes. CSI (`ESC [ … final byte`), OSC (`ESC ] … BEL|ST`), and the other
/// two-character ones.
///
/// `\x1b[` is **deliberately left out** of the two-character branch (`[` = 0x5B is not in
/// `[@-Z\\-_]`) — otherwise the head of an unfinished CSI gets eaten as two characters.
static ESCAPES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]")
        .expect("static regex")
});

/// Turns terminal output into **text an agent can read**.
///
/// The comment in zyris-caps records why: *"a tool result carrying a raw `U+001B` is
/// rejected outright by at least one agent runtime."* cargo output comes coloured, and
/// upstream's `strip_controls` is `pub(crate)`, so it cannot be used from here.
///
/// It does three things: strips escapes, folds the lines a carriage return rewrites, and
/// removes C0 other than `\n` and `\t`. **Anything cut at a chunk boundary is held until
/// the next chunk** — characters, escapes, and a trailing `\r` alike.
#[derive(Debug, Default)]
pub struct Stripper {
    /// Bytes not yet readable as characters, or an unfinished escape.
    carry: Vec<u8>,
    /// The line that has not met a `\n` yet. A carriage return throws this away.
    line: String,
    /// The previous chunk ended with `\r`. **If the next character is `\n` it is CRLF, so
    /// the line must not be thrown away.**
    pending_cr: bool,
}

impl Stripper {
    /// Feeds in new bytes and gives back the text settled so far. Text settles per line.
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.carry.extend_from_slice(bytes);
        // A tail that is not readable as characters is held until the next chunk.
        let valid = match std::str::from_utf8(&self.carry) {
            Ok(_) => self.carry.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            // Truly broken bytes go. Holding them only means they are never read.
            Err(e) => e.valid_up_to() + e.error_len().unwrap_or(1),
        };
        let text = String::from_utf8_lossy(&self.carry[..valid]).into_owned();
        self.carry.drain(..valid);

        // An unfinished escape goes back — it is stripped only once joined to the next chunk.
        let (ready, held) = split_incomplete_escape(&text);
        if !held.is_empty() {
            let mut back = held.as_bytes().to_vec();
            back.extend_from_slice(&self.carry);
            self.carry = back;
        }

        let stripped = ESCAPES.replace_all(ready, "");
        self.feed(&stripped)
    }

    /// Emits whatever is left once the process has finished.
    ///
    /// **A trailing `\r` does not erase the line.** In a terminal the last progress line
    /// stays on screen too, and that is what the reader last saw.
    pub fn flush(&mut self) -> String {
        let rest = String::from_utf8_lossy(&std::mem::take(&mut self.carry)).into_owned();
        self.pending_cr = false;
        let stripped = ESCAPES.replace_all(&rest, "").into_owned();
        let mut out = self.feed(&stripped);
        self.pending_cr = false;
        out.push_str(&std::mem::take(&mut self.line));
        out
    }

    /// Settles text line by line. **`\r` rewrites that line**, so what came before is gone.
    fn feed(&mut self, text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars().peekable();
        // The previous chunk ended with `\r`. If this chunk starts with `\n` it is CRLF, so
        // the line lives.
        if std::mem::take(&mut self.pending_cr) && chars.peek() != Some(&'\n') {
            self.line.clear();
        }
        while let Some(ch) = chars.next() {
            match ch {
                '\n' => {
                    out.push_str(&std::mem::take(&mut self.line));
                    out.push('\n');
                }
                // CRLF is just a newline. Throwing the line away here loses Windows output
                // entirely.
                '\r' => match chars.peek() {
                    Some('\n') => {}
                    Some(_) => self.line.clear(),
                    None => self.pending_cr = true,
                },
                '\t' => self.line.push('\t'),
                c if (c as u32) < 0x20 || c as u32 == 0x7f => {}
                c => self.line.push(c),
            }
        }
        out
    }
}

/// What to put in the background.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    /// One shell line. Give **exactly one** of this and `argv`.
    pub command: Option<String>,
    /// Program and arguments to spawn as they are, without a shell.
    pub argv: Option<Vec<String>>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub label: Option<String>,
}

/// The one line a person and an agent see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub id: String,
    pub label: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
}

struct Job {
    label: String,
    started: Instant,
    pid: Option<u32>,
    exit_code: Option<i32>,
    running: bool,
    out: Ring,
    /// Tells whoever is waiting that it ended. `wait.until` waits on this.
    ended: watch::Sender<bool>,
}

/// Everything this app has put in the background.
///
/// **There is one registry.** The tools, the screen and `/jobs` must all see the same one —
/// with two copies, fixing one side silently forks them. Same wiring as `Bridge::set_undo`.
#[derive(Clone)]
pub struct Jobs {
    root: PathBuf,
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    jobs: Vec<(String, Job)>,
    next: u64,
}

impl Jobs {
    pub fn new(root: PathBuf) -> Jobs {
        Jobs { root, inner: Arc::new(Mutex::new(Inner::default())) }
    }

    /// Where relative paths resolve. The probe runs here too — **a job and a probe have to
    /// look at the same place.** Otherwise `ls` answers differently from tool to tool.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Puts it in the background and returns at once. **It does not wait for the run.**
    pub fn start(&self, spec: Spec) -> Result<String, String> {
        let mut cmd = build(&spec, &self.root)?;
        let label = spec
            .label
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| spec.command.clone())
            .or_else(|| spec.argv.as_ref().map(|a| a.join(" ")))
            .unwrap_or_default();

        let mut child = cmd.spawn().map_err(|e| format!("띄우지 못했습니다: {e}"))?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (ended, _) = watch::channel(false);

        let id = {
            let mut inner = self.inner.lock().unwrap();
            inner.next += 1;
            let id = format!("b{}", inner.next);
            inner.jobs.push((
                id.clone(),
                Job {
                    label,
                    started: Instant::now(),
                    pid,
                    exit_code: None,
                    running: true,
                    out: Ring::new(RING_CAP),
                    ended,
                },
            ));
            // **Finished ones go first.** Dropping a running one loses the handle to kill it.
            while inner.jobs.len() > KEEP {
                let Some(at) = inner.jobs.iter().position(|(_, j)| !j.running) else { break };
                inner.jobs.remove(at);
            }
            id
        };

        let this = self.clone();
        let for_task = id.clone();
        tokio::spawn(async move {
            // stdout and stderr go into **one buffer**. The order is what the reader
            // understands by.
            let pump = {
                let (a, b) = (this.clone(), this.clone());
                let (ia, ib) = (for_task.clone(), for_task.clone());
                async move {
                    tokio::join!(drain(a, ia, stdout), drain(b, ib, stderr));
                }
            };
            let (status, ()) = tokio::join!(child.wait(), pump);
            let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
            let mut inner = this.inner.lock().unwrap();
            if let Some((_, job)) = inner.jobs.iter_mut().find(|(k, _)| *k == for_task) {
                job.running = false;
                job.exit_code = Some(code);
                let _ = job.ended.send(true);
            }
        });
        Ok(id)
    }

    pub fn snapshot(&self, id: &str) -> Option<Snapshot> {
        let inner = self.inner.lock().unwrap();
        inner.jobs.iter().find(|(k, _)| k == id).map(|(k, j)| snap(k, j))
    }

    pub fn list(&self) -> Vec<Snapshot> {
        let inner = self.inner.lock().unwrap();
        inner.jobs.iter().map(|(k, j)| snap(k, j)).collect()
    }

    pub fn read(&self, id: &str, offset: u64) -> Option<Chunk> {
        let inner = self.inner.lock().unwrap();
        inner.jobs.iter().find(|(k, _)| k == id).map(|(_, j)| j.out.read(offset))
    }

    pub fn tail(&self, id: &str, bytes: usize) -> String {
        let inner = self.inner.lock().unwrap();
        inner.jobs.iter().find(|(k, _)| k == id).map(|(_, j)| j.out.tail(bytes)).unwrap_or_default()
    }

    /// A handle to wait on for the end. **If it already ended, it is true from the start.**
    pub fn ended(&self, id: &str) -> Option<watch::Receiver<bool>> {
        let inner = self.inner.lock().unwrap();
        inner.jobs.iter().find(|(k, _)| k == id).map(|(_, j)| j.ended.subscribe())
    }

    /// Kills a running job by the whole group. False if it already ended — not an error.
    pub fn stop(&self, id: &str) -> bool {
        let Some(pid) = self.running_pid(id) else { return false };
        if !term_tree(pid) {
            return true;
        }
        // After the grace, finish it for sure. On its own thread **so the way out is not
        // held up**.
        std::thread::spawn(move || {
            std::thread::sleep(GRACE);
            kill_tree(pid);
        });
        true
    }

    /// Called before quitting the app. **Leaves no orphans.**
    ///
    /// The grace is short here (`QUIT_GRACE`). Waiting two seconds for each one to end reads
    /// as an app that will not close. TERM still goes first, to leave room to clean up.
    pub fn stop_all(&self) {
        let pids: Vec<u32> = {
            let inner = self.inner.lock().unwrap();
            inner.jobs.iter().filter(|(_, j)| j.running).filter_map(|(_, j)| j.pid).collect()
        };
        if pids.is_empty() {
            return;
        }
        for pid in &pids {
            term_tree(*pid);
        }
        std::thread::sleep(QUIT_GRACE);
        for pid in &pids {
            kill_tree(*pid);
        }
    }

    fn running_pid(&self, id: &str) -> Option<u32> {
        let inner = self.inner.lock().unwrap();
        match inner.jobs.iter().find(|(k, _)| k == id) {
            Some((_, j)) if j.running => j.pid,
            _ => None,
        }
    }

    fn absorb(&self, id: &str, text: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some((_, job)) = inner.jobs.iter_mut().find(|(k, _)| k == id) {
            job.out.push(text);
        }
    }
}

fn snap(id: &str, j: &Job) -> Snapshot {
    Snapshot {
        id: id.to_string(),
        label: j.label.clone(),
        running: j.running,
        exit_code: j.exit_code,
        elapsed_ms: j.started.elapsed().as_millis() as u64,
    }
}

/// Reads one stream to the end and puts it in the ring.
async fn drain<R: tokio::io::AsyncRead + Unpin>(jobs: Jobs, id: String, stream: Option<R>) {
    let Some(mut stream) = stream else { return };
    let mut strip = Stripper::default();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let text = strip.push(&buf[..n]);
                if !text.is_empty() {
                    jobs.absorb(&id, &text);
                }
            }
        }
    }
    let rest = strip.flush();
    if !rest.is_empty() {
        jobs.absorb(&id, &rest);
    }
}

/// Builds the command. Lays down an environment that **discourages colour**, and what the
/// A command that hands `line` to this platform's shell.
///
/// **There is no `/bin/sh` on Windows.** Hardcoding it meant every `wait.start` given a
/// `command` (rather than an `argv`) failed to spawn there, and `wait.until`'s probe with it —
/// so the one capability built for long builds could not run on the platform at all. Upstream
/// capkit already forks the same way for `terminal.exec`
/// (`zyris-capkit/src/terminal/mod.rs`), which is why `exec` worked and `wait` did not.
pub(crate) fn shell_running(line: &str) -> tokio::process::Command {
    if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(line);
        c
    } else {
        let mut c = tokio::process::Command::new("/bin/sh");
        c.arg("-c").arg(line);
        c
    }
}

/// caller gave wins.
fn build(spec: &Spec, root: &Path) -> Result<tokio::process::Command, String> {
    let mut cmd = match (&spec.command, &spec.argv) {
        (Some(_), Some(_)) | (None, None) => {
            return Err("command와 argv 중 정확히 하나를 주세요.".into())
        }
        (Some(line), None) => {
            if line.trim().is_empty() {
                return Err("command가 비어 있습니다.".into());
            }
            shell_running(line)
        }
        (None, Some(argv)) => {
            let Some((program, rest)) = argv.split_first() else {
                return Err("argv가 비어 있습니다.".into());
            };
            let mut c = tokio::process::Command::new(program);
            c.args(rest);
            c
        }
    };
    cmd.current_dir(spec.cwd.clone().unwrap_or_else(|| root.to_path_buf()));
    cmd.env("TERM", "dumb").env("NO_COLOR", "1").env("CARGO_TERM_COLOR", "never");
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        // **Make it the leader of a new group.** Only then do the grandchildren a shell
        // leaves behind die in one go. Take it out and
        // `stopping_a_job_kills_the_whole_process_tree` bites — that was checked by actually
        // taking it out. tokio's `Command` gives this directly (no `CommandExt` needed).
        cmd.process_group(0);
    }
    Ok(cmd)
}

/// SIGTERM to the process group. True if it was sent.
///
/// **Rides out the setpgid race** — the signal fails while the child has not made its group
/// yet. Same retry capkit's `kill_tree` does, and without it a job stopped right after it
/// was started silently fails to die.
#[cfg(unix)]
fn term_tree(pid: u32) -> bool {
    let group = -(pid as i32);
    for _ in 0..50 {
        // SAFETY: a negative pid is a process group. We just made this group and are not in it.
        if unsafe { libc::kill(group, libc::SIGTERM) } == 0 {
            return true;
        }
        // If the leader is already dead the group will never come to be.
        if unsafe { libc::kill(pid as i32, 0) } != 0 {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[cfg(unix)]
fn kill_tree(pid: u32) {
    // SAFETY: same as above.
    unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
}

/// Windows: `taskkill /T` kills the tree. There is no gentle signal, so it goes in one shot.
#[cfg(not(unix))]
fn term_tree(pid: u32) -> bool {
    kill_tree(pid);
    false
}

#[cfg(not(unix))]
fn kill_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .output();
}

/// One job's output. **stdout and stderr go in mixed together** — cargo puts progress on
/// stderr, so keeping them apart loses the order, and the order is what the reader
/// understands by.
#[derive(Debug)]
pub struct Ring {
    buf: Vec<u8>,
    cap: usize,
    /// Bytes thrown away off the front. **The origin absolute offsets are measured from.**
    dropped: u64,
}

/// What `Ring::read` gives back.
///
/// `more` and `dropped` are kept apart for the same reason as capkit's `PtyRead` — the
/// **only question that matters to the reader is "can I get it by calling again"**. Mashed
/// into one "truncated" flag, that question has no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    /// Give this offset next time and you carry on from here.
    pub next_offset: u64,
    /// There is still something left in the buffer. **Call again and you get it.**
    pub more: bool,
    /// Bytes lost to overflow. **Calling again does not bring them back.**
    pub dropped: u64,
}

impl Ring {
    pub fn new(cap: usize) -> Ring {
        Ring { buf: Vec::new(), cap, dropped: 0 }
    }

    pub fn push(&mut self, text: &str) {
        self.buf.extend_from_slice(text.as_bytes());
        if self.buf.len() > self.cap {
            let cut = self.buf.len() - self.cap;
            self.buf.drain(..cut);
            self.dropped += cut as u64;
        }
    }

    /// Reads from an absolute offset. Ask for a place already thrown away and it gives
    /// **from the front of what is left**.
    ///
    /// `more` is always false — everything left is handed over. Whether that is too much for
    /// one go is settled by the side that truncates (`wait.logs`).
    pub fn read(&self, offset: u64) -> Chunk {
        let dropped = self.dropped.saturating_sub(offset);
        let from = offset.max(self.dropped) - self.dropped;
        let from = (from as usize).min(self.buf.len());
        Chunk {
            text: String::from_utf8_lossy(&self.buf[from..]).into_owned(),
            next_offset: self.dropped + self.buf.len() as u64,
            more: false,
            dropped,
        }
    }

    /// The last few bytes. **Starts at a line boundary** — starting on half a line leaves
    /// the reader with no idea what is going on. If it all fits, the first line is kept.
    pub fn tail(&self, bytes: usize) -> String {
        let from = self.buf.len().saturating_sub(bytes);
        let slice = &self.buf[from..];
        let start = if from == 0 {
            0
        } else {
            slice.iter().position(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0)
        };
        String::from_utf8_lossy(&slice[start..]).into_owned()
    }
}

/// If the tail is an unfinished escape, settles only up to it and gives the rest back.
fn split_incomplete_escape(text: &str) -> (&str, &str) {
    let Some(at) = text.rfind('\x1b') else { return (text, "") };
    // A finished escape is eaten by the regex right there — nothing to hold on to.
    if ESCAPES.find_at(text, at).is_some_and(|m| m.start() == at) {
        return (text, "");
    }
    text.split_at(at)
}

#[cfg(test)]
mod jobs_tests {
    use super::*;

    fn jobs() -> Jobs {
        Jobs::new(std::env::temp_dir())
    }

    fn shell(command: &str) -> Spec {
        Spec { command: Some(command.into()), ..Default::default() }
    }

    async fn wait_for(j: &Jobs, id: &str) {
        let mut ended = j.ended(id).expect("the job must be there");
        while !*ended.borrow() {
            ended.changed().await.expect("the sender must still be alive");
        }
    }

    #[tokio::test]
    async fn a_finished_job_keeps_its_output_readable() {
        let j = jobs();
        // `cmd /C` does not speak bash: `;` is not a separator and `exit 3` needs `/b`.
        // ASCII only — `cmd /C echo` writes non-ASCII in the console codepage, not UTF-8.
        let cmd = if cfg!(windows) { "echo hi & exit /b 3" } else { "echo hi; exit 3" };
        let id = j.start(shell(cmd)).unwrap();
        wait_for(&j, &id).await;
        let snap = j.snapshot(&id).unwrap();
        assert!(!snap.running);
        assert_eq!(snap.exit_code, Some(3));
        // Still readable after it ends — the agent calls logs after `until` says done.
        assert!(j.read(&id, 0).unwrap().text.contains("hi"));
    }

    /// stdout and stderr arrive in one buffer.
    #[tokio::test]
    async fn both_streams_land_in_one_buffer() {
        let j = jobs();
        // `cmd /C` on Windows uses `&` to separate commands, not `;`. ASCII only — a Korean
        // word would come back in the console codepage rather than UTF-8 and never match.
        let cmd = if cfg!(windows) { "echo hi & echo err 1>&2" } else { "echo hi; echo err 1>&2" };
        let id = j.start(shell(cmd)).unwrap();
        wait_for(&j, &id).await;
        let text = j.read(&id, 0).unwrap().text;
        assert!(text.contains("hi"), "{text}");
        assert!(text.contains("err"), "{text}");
    }

    /// **Kill only the shell and the grandchild is left an orphan.** On this machine that
    /// means a freeze.
    ///
    /// Unix-only: the `( … ) & wait` subshell is bash, and on Windows the tree-kill goes
    /// through `taskkill /T`, which this bash shape cannot exercise.
    #[cfg(unix)]
    #[tokio::test]
    async fn stopping_a_job_kills_the_whole_process_tree() {
        let j = jobs();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild-lived");
        // The grandchild leaves a file after three seconds. Killed by group, no file appears.
        let id = j.start(shell(&format!("(sleep 3; touch {}) & wait", marker.display()))).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(j.stop(&id));
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        assert!(!marker.exists(), "the grandchild survived");
    }

    /// Quitting the app kills the running jobs with it. An orphaned cargo must not be left
    /// on this machine.
    #[tokio::test]
    async fn quitting_the_app_leaves_no_running_job() {
        let j = jobs();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        j.start(shell(&format!("(sleep 3; touch {}) & wait", marker.display()))).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        j.stop_all();
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        assert!(!marker.exists(), "the app quit but a job survived");
    }

    /// Asking to stop something already finished is false. Not an error.
    #[tokio::test]
    async fn stopping_a_finished_job_is_false_not_an_error() {
        let j = jobs();
        let id = j.start(shell("true")).unwrap();
        wait_for(&j, &id).await;
        assert!(!j.stop(&id));
    }

    /// Exactly one of the two arguments is required.
    #[tokio::test]
    async fn a_spec_needs_exactly_one_of_command_or_argv() {
        let j = jobs();
        assert!(j.start(Spec::default()).is_err());
        assert!(j
            .start(Spec {
                command: Some("ls".into()),
                argv: Some(vec!["ls".into()]),
                ..Default::default()
            })
            .is_err());
        assert!(j.start(Spec { command: Some("   ".into()), ..Default::default() }).is_err());
    }

    /// ids have to be short — the agent carries them on every check.
    #[tokio::test]
    async fn ids_are_short_and_ordered() {
        let j = jobs();
        assert_eq!(j.start(shell("true")).unwrap(), "b1");
        assert_eq!(j.start(shell("true")).unwrap(), "b2");
        assert_eq!(j.list().len(), 2);
    }

    /// Spawned so less colour comes out in the first place. The caller's env wins.
    #[tokio::test]
    async fn the_environment_discourages_colour_but_the_caller_wins() {
        let j = jobs();
        let id = j
            .start(Spec {
                command: Some(
                    if cfg!(windows) { "echo %NO_COLOR%-%TERM%" } else { "echo $NO_COLOR-$TERM" }
                        .into(),
                ),
                env: vec![("TERM".into(), "xterm".into())],
                ..Default::default()
            })
            .unwrap();
        wait_for(&j, &id).await;
        assert_eq!(j.read(&id, 0).unwrap().text.trim(), "1-xterm");
    }
}

#[cfg(test)]
mod ring_tests {
    use super::*;

    #[test]
    fn reading_from_the_start_gives_everything_once() {
        let mut r = Ring::new(64);
        r.push("hello\n");
        let c = r.read(0);
        assert_eq!(c.text, "hello\n");
        assert_eq!(c.next_offset, 6);
        assert_eq!(c.dropped, 0);
        // Reading on gives nothing — the same text must not be handed over twice.
        assert_eq!(r.read(c.next_offset).text, "");
    }

    /// Overflow loses the front and **says how much was lost.** Calling again does not bring
    /// it back.
    #[test]
    fn overflow_drops_the_front_and_says_how_much() {
        let mut r = Ring::new(8);
        r.push("0123456789");
        let c = r.read(0);
        assert_eq!(c.text, "23456789");
        assert_eq!(c.dropped, 2);
        assert_eq!(c.next_offset, 10);
    }

    /// Asking on from a spot already read loses nothing.
    #[test]
    fn reading_on_from_a_live_offset_loses_nothing() {
        let mut r = Ring::new(8);
        r.push("01234567");
        let first = r.read(0);
        r.push("89");
        let next = r.read(first.next_offset);
        assert_eq!(next.text, "89");
        assert_eq!(next.dropped, 0);
    }

    /// The tail is the last few bytes. **It does not cut in the middle of a line** — half a
    /// line only confuses whoever reads it.
    #[test]
    fn the_tail_starts_at_a_line_boundary() {
        let mut r = Ring::new(1024);
        r.push("첫 줄\n둘째 줄\n셋째 줄\n");
        let tail = r.tail(20);
        assert!(tail.starts_with("둘째") || tail.starts_with("셋째"), "{tail}");
        assert!(tail.ends_with('\n'));
    }

    /// If it all fits it gives everything from the front — hunting for a line boundary must
    /// not throw the first line away.
    #[test]
    fn a_tail_that_covers_everything_keeps_the_first_line() {
        let mut r = Ring::new(1024);
        r.push("하나\n둘\n");
        assert_eq!(r.tail(1024), "하나\n둘\n");
    }
}

#[cfg(test)]
mod strip_tests {
    use super::*;

    /// Colour codes must not reach the agent — one runtime rejects the whole result.
    #[test]
    fn control_sequences_never_reach_the_agent() {
        let mut s = Stripper::default();
        let out = s.push(b"\x1b[32m   Compiling\x1b[0m zyris-code\n");
        assert_eq!(out, "   Compiling zyris-code\n");
        assert!(!out.contains('\x1b'));
    }

    /// An escape cut at a chunk boundary has to be joined to the next chunk.
    #[test]
    fn an_escape_split_across_chunks_is_still_stripped() {
        let mut s = Stripper::default();
        let a = s.push(b"ok\x1b[3");
        let b = s.push(b"2mgreen\n");
        assert_eq!(format!("{a}{b}"), "okgreen\n");
    }

    /// A multi-byte character cut at a chunk boundary survives intact.
    #[test]
    fn a_character_split_across_chunks_survives() {
        let mut s = Stripper::default();
        let bytes = "한글".as_bytes();
        let a = s.push(&bytes[..4]);
        let b = s.push(&bytes[4..]);
        let c = s.flush();
        assert_eq!(format!("{a}{b}{c}"), "한글");
    }

    /// A carriage return rewrites that line. A progress bar must not become thousands of
    /// lines.
    #[test]
    fn a_progress_line_is_rewritten_not_appended() {
        let mut s = Stripper::default();
        let mut out = String::new();
        out.push_str(&s.push(b"Building [=>   ] 10%\r"));
        out.push_str(&s.push(b"Building [====>] 99%\r"));
        out.push_str(&s.push(b"Building [=====] 100%\n"));
        assert_eq!(out, "Building [=====] 100%\n");
    }

    /// **CRLF is just a newline.** Read `\r` as erase only and Windows output disappears.
    #[test]
    fn a_crlf_is_a_newline_not_an_erase() {
        let mut s = Stripper::default();
        assert_eq!(s.push(b"first\r\nsecond\r\n"), "first\nsecond\n");
        // Same result when the chunk splits in between.
        let mut s = Stripper::default();
        let a = s.push(b"first\r");
        let b = s.push(b"\nsecond\n");
        assert_eq!(format!("{a}{b}"), "first\nsecond\n");
    }

    /// Tabs and newlines survive. The rest of C0 is removed.
    #[test]
    fn tabs_and_newlines_survive_but_other_controls_do_not() {
        let mut s = Stripper::default();
        assert_eq!(s.push(b"a\tb\nc\x07d"), "a\tb\n");
        assert_eq!(s.flush(), "cd");
    }
}
