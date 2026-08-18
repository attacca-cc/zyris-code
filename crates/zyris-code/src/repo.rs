//! The context strip on the divider above the input — working directory and git state.

use std::path::Path;

use ratatui::style::Style;
use ratatui::text::Span;

use crate::markdown::display_width;
use crate::theme;

/// What one `git status --porcelain=v2 --branch` call said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Repo {
    /// Branch name, or a short oid when HEAD is detached.
    pub branch: String,
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicts: usize,
    pub ahead: usize,
    pub behind: usize,
    /// How much code this branch changed against the branch it came off.
    ///
    /// **`None` means there is nothing to compare against**, not "no change" — the default branch
    /// itself, a checkout with no remote, or a git that could not answer. Zero is a real answer
    /// and reads as `+0 -0`; absence has to stay distinguishable from it or a branch that failed
    /// to measure would claim it changed nothing.
    pub diverged: Option<Diff>,
}

/// The pull request this branch is on, as GitHub describes it.
///
/// **Kept apart from [`Repo`].** One is read from git every few seconds and never leaves this
/// machine; the other costs three network calls and is polled far more slowly. Folding them into
/// one struct would mean the git poll wiped the pull request every three seconds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pull {
    pub number: u64,
    /// GitHub's own counts for the whole pull request. **Not the same as [`Repo::diverged`]** —
    /// that is measured here and includes commits that have not been pushed yet.
    pub added: usize,
    pub removed: usize,
    pub checks: Checks,
    pub merged: bool,
}

/// What the checks on the head commit add up to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Checks {
    /// Nothing is configured, or nothing has reported yet. **Drawn as nothing** — a repository
    /// with no CI must not grow a mark saying so on every frame.
    #[default]
    Quiet,
    Running,
    Passed,
    Failed,
}

impl Checks {
    /// The word on the strip, and the colour it wears.
    ///
    /// **Letters, not symbols.** `●`, `✓` and `✗` are East Asian Ambiguous: `unicode-width` calls
    /// them one column and a terminal set to the wide reading draws them as two, which pushes
    /// everything after them one column right and makes the next positioned write eat a
    /// character. That is exactly how `main` once came out as `mai`.
    pub fn mark(self) -> Option<(&'static str, ratatui::style::Color)> {
        match self {
            Checks::Quiet => None,
            Checks::Running => Some(("ci..", theme::warning())),
            Checks::Passed => Some(("ci ok", theme::diff_add())),
            Checks::Failed => Some(("ci x", theme::danger())),
        }
    }
}

/// Lines added and removed, as `git diff --shortstat` counts them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub added: usize,
    pub removed: usize,
}

impl Diff {
    /// Whether there is anything here worth a reader's eye.
    ///
    /// **`+0 -0` is not worth drawing.** It was measured and absence was not, which is why the
    /// two stay apart in [`Repo::diverged`] — but on the row they say the same thing to the person
    /// reading it, and the shorter way to say it is not to. Reported on Windows 2026-08-18, where
    /// a branch level with its base sat there showing zeros.
    pub fn is_nothing(self) -> bool {
        self.added == 0 && self.removed == 0
    }
}

impl Repo {
    /// Nothing to commit and nothing to push.
    pub fn is_clean(&self) -> bool {
        self.staged == 0
            && self.unstaged == 0
            && self.untracked == 0
            && self.conflicts == 0
            && self.ahead == 0
            && self.behind == 0
    }
}

/// Reads what `git status --porcelain=v2 --branch` printed.
///
/// `None` means **this is not a repository we can describe** — no branch header, or a HEAD we
/// cannot name. Every failure funnels here so the caller has exactly one case to handle.
///
/// ```text
/// # branch.oid <sha>          short 7 characters, used only when detached
/// # branch.head <name>        the literal "(detached)" sends us to the oid
/// # branch.ab +N -M           ahead N, behind M. Absent without an upstream.
/// 1 XY ... / 2 XY ...         X is the index column, Y the worktree column; '.' is unchanged
/// u ...                       unmerged
/// ? ...                       untracked
/// ```
///
/// **A path staged and then edited again counts in both columns.** That is what every shell
/// prompt does, and collapsing it would hide the edit that is not staged yet.
pub fn parse(out: &str) -> Option<Repo> {
    let mut repo = Repo::default();
    let mut oid = None;
    let mut saw_header = false;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("# branch.") {
            saw_header = true;
            if let Some(v) = rest.strip_prefix("oid ") {
                oid = Some(v.trim().to_string());
            } else if let Some(v) = rest.strip_prefix("head ") {
                repo.branch = v.trim().to_string();
            } else if let Some(v) = rest.strip_prefix("ab ") {
                (repo.ahead, repo.behind) = ahead_behind(v);
            }
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            // Field two is the two-letter status. Anything shorter is not a line git wrote.
            let mut xy = line.split(' ').nth(1).unwrap_or("..").chars();
            repo.staged += usize::from(xy.next().is_some_and(|c| c != '.'));
            repo.unstaged += usize::from(xy.next().is_some_and(|c| c != '.'));
        } else if line.starts_with("u ") {
            repo.conflicts += 1;
        } else if line.starts_with("? ") {
            repo.untracked += 1;
        }
    }
    if !saw_header {
        return None;
    }
    // A detached HEAD has no name, so the short oid stands in — otherwise the strip would say
    // "(detached)", which is longer and says less.
    if repo.branch.is_empty() || repo.branch == "(detached)" {
        repo.branch = oid.map(|o| o.chars().take(7).collect()).unwrap_or_default();
    }
    (!repo.branch.is_empty()).then_some(repo)
}

/// `+2 -3` from the `branch.ab` header. Unreadable counts as zero — a wrong number here would be
/// worse than none.
fn ahead_behind(text: &str) -> (usize, usize) {
    let mut out = (0, 0);
    for part in text.split_whitespace() {
        match part.split_at_checked(1) {
            Some(("+", n)) => out.0 = n.parse().unwrap_or(0),
            Some(("-", n)) => out.1 = n.parse().unwrap_or(0),
            _ => {}
        }
    }
    out
}

/// The lead-in before the first piece, and the gap before the rule resumes.
const LEAD: &str = "─ ";
/// Between two pieces that are **both** there.
const SEP: &str = " ∙ ";
/// Below this many trailing rule columns the row stops reading as a divider, so the strip is
/// dropped entirely and the plain rule comes back.
const MIN_RULE: usize = 4;

/// How much of the git piece survives at a given width. **Least urgent first** — walking this
/// list in order is the drop order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Full,
    NoUntracked,
    NoAhead,
    NoChecks,
    NoDiverged,
    NoPullSize,
    NoCounts,
    NoGit,
}

const LEVELS: [Level; 8] = [
    Level::Full,
    Level::NoUntracked,
    Level::NoAhead,
    Level::NoChecks,
    Level::NoDiverged,
    Level::NoPullSize,
    Level::NoCounts,
    Level::NoGit,
];

/// The whole divider row: `─ ~/zyris-code · * main +2 ~1 ────────`.
///
/// **Pure**, and it always returns exactly `width` columns, so the caller hands it straight to a
/// `Line` without measuring anything.
///
/// A piece that is not there contributes **nothing — not even a separator.** Building this by
/// concatenating optional fragments is how a machine without git ends up showing
/// `~/zyris-code ·` with a dangling join. Pieces are collected, empties are dropped, and only
/// the survivors are joined — which is also what lets a piece be added later without leaving a
/// hole on the days its neighbours are missing.
pub fn spans(
    width: u16,
    cwd: &Path,
    home: Option<&Path>,
    repo: Option<&Repo>,
    pull: Option<&Pull>,
) -> Vec<Span<'static>> {
    let width = width as usize;
    let bare = || vec![Span::styled("─".repeat(width), Style::default().fg(theme::border()))];
    // Lead-in, one space, and a rule long enough to still look like a rule.
    let overhead = display_width(LEAD) + 1 + MIN_RULE;
    if width <= overhead {
        return bare();
    }
    let budget = width - overhead;
    let path = path_text(cwd, home);
    for level in LEVELS {
        if let Some(out) = assemble(width, &pieces(&path, repo, pull, level), budget) {
            return out;
        }
    }
    // Even the path alone did not fit — take it from the head, never the tail.
    match shorten(&path, budget) {
        Some(short) => {
            assemble(width, &pieces(&short, None, None, Level::NoGit), budget).unwrap_or_else(bare)
        }
        None => bare(),
    }
}

/// The pieces, in order, at this level of detail. **Empty inner vectors are the whole point** —
/// `assemble` drops them, and with them their separator.
fn pieces(
    path: &str,
    repo: Option<&Repo>,
    pull: Option<&Pull>,
    level: Level,
) -> Vec<Vec<Span<'static>>> {
    let muted = Style::default().fg(theme::text_muted());
    let mut git: Vec<Span<'static>> = Vec::new();
    if let (Some(r), false) = (repo, level == Level::NoGit) {
        // **`*`, and it has to stay something like it.** The branch used to be marked `⎇`
        // (U+2387), which `unicode-width` calls one column — and which almost no terminal font
        // has a glyph for, so the terminal falls back to a font that draws it two columns wide.
        // Everything after it then sits one column right of where the layout put it, and the
        // next positioned write eats a character: `main` came out as `mai`. `*` is what
        // `git branch` marks the current branch with, it is ASCII, and no font can widen it.
        git.push(Span::styled(format!("* {}", r.branch), muted));
        if level != Level::NoCounts {
            let mut count = |n: usize, mark: char, style: Style| {
                if n > 0 {
                    git.push(Span::styled(format!(" {mark}{n}"), style));
                }
            };
            let warn = Style::default().fg(theme::warning());
            // A conflict is the one thing here that must not be missed, so it comes first and
            // wears the only alarming colour on the row.
            count(r.conflicts, '!', Style::default().fg(theme::danger()));
            count(r.staged, '+', warn);
            count(r.unstaged, '~', warn);
            if level == Level::Full {
                // Files git does not know are usually build litter, so this stays quiet — but
                // quiet in its own tint, not the paint the path is wearing.
                count(r.untracked, '?', Style::default().fg(theme::untracked()));
            }
            if matches!(level, Level::Full | Level::NoUntracked) {
                // Pushing is a later errand than committing, so these keep away from `warning()`
                // — but they point opposite ways and are read together, so they part in colour.
                count(r.ahead, '↑', Style::default().fg(theme::ahead()));
                count(r.behind, '↓', Style::default().fg(theme::behind()));
            }
        }
    }
    // **Its own piece, not more marks on the git one.** `+` already means "staged" two columns
    // to the left, so `* main +2 ~1 +120 -34` asks the reader to know that the third number
    // changed units. A separator says they are different things without a word of explanation.
    let mut diverged: Vec<Span<'static>> = Vec::new();
    if let (Some(r), true) =
        (repo, matches!(level, Level::Full | Level::NoUntracked | Level::NoAhead | Level::NoChecks))
    {
        if let Some(d) = r.diverged.filter(|d| !d.is_nothing()) {
            diverged.push(Span::styled(
                format!("+{}", d.added),
                Style::default().fg(theme::diff_add()),
            ));
            diverged.push(Span::styled(
                format!(" -{}", d.removed),
                Style::default().fg(theme::diff_del()),
            ));
        }
    }
    // The pull request this branch is on. **Its own piece as well**, and for the same reason the
    // branch's counts are: `#53` is an identity, `+118 -30` are GitHub's numbers for the whole
    // request, and neither belongs among marks that describe the working tree.
    let mut pr: Vec<Span<'static>> = Vec::new();
    if let (Some(p), false) = (pull, level == Level::NoGit) {
        // **Yellow while it is somebody else's turn, purple once it has landed.** The number
        // carries the colour because the number is the thing being talked about.
        let state = match p.merged {
            true => Style::default().fg(theme::merged()),
            false => Style::default().fg(theme::warning()),
        };
        pr.push(Span::styled(format!("#{}", p.number), state));
        let size = Diff { added: p.added, removed: p.removed };
        if !matches!(level, Level::NoPullSize | Level::NoCounts) && !size.is_nothing() {
            pr.push(Span::styled(format!(" +{}", p.added), Style::default().fg(theme::diff_add())));
            pr.push(Span::styled(
                format!(" -{}", p.removed),
                Style::default().fg(theme::diff_del()),
            ));
        }
        if matches!(level, Level::Full | Level::NoUntracked | Level::NoAhead) {
            if let Some((word, colour)) = p.checks.mark() {
                pr.push(Span::styled(format!(" {word}"), Style::default().fg(colour)));
            }
        }
    }
    vec![vec![Span::styled(path.to_string(), muted)], git, diverged, pr]
}

/// Joins the non-empty pieces and pads out to exactly `width`. `None` when the body is over
/// budget, which is the signal to try a lower level.
fn assemble(
    width: usize,
    parts: &[Vec<Span<'static>>],
    budget: usize,
) -> Option<Vec<Span<'static>>> {
    let live: Vec<&Vec<Span<'static>>> = parts.iter().filter(|p| !p.is_empty()).collect();
    let body: usize = live.iter().flat_map(|p| p.iter()).map(|s| display_width(&s.content)).sum();
    let joins = live.len().saturating_sub(1) * display_width(SEP);
    if body + joins > budget {
        return None;
    }
    let border = Style::default().fg(theme::border());
    let mut out = vec![Span::styled(LEAD, border)];
    for (i, piece) in live.iter().enumerate() {
        if i > 0 {
            out.push(Span::styled(SEP, Style::default().fg(theme::border_light())));
        }
        out.extend(piece.iter().cloned());
    }
    let rule = width - display_width(LEAD) - body - joins - 1;
    out.push(Span::styled(format!(" {}", "─".repeat(rule)), border));
    Some(out)
}

/// `/home/ruma/zyris-code` under `/home/ruma` becomes `~/zyris-code`.
///
/// **Not under home is shown as it is** — `/etc/nginx` cut down to a name would say less than
/// the path does.
fn path_text(cwd: &Path, home: Option<&Path>) -> String {
    let full = cwd.display().to_string();
    let Some(home) = home.map(|h| h.display().to_string()).filter(|h| !h.is_empty()) else {
        return full;
    };
    if full == home {
        return "~".into();
    }
    match full.strip_prefix(&format!("{home}/")) {
        Some(rest) => format!("~/{rest}"),
        None => full,
    }
}

/// Drops leading components until it fits, marking the cut with `…/`.
///
/// **The tail is what identifies a directory**, so the head is what goes. `None` when even that
/// does not fit, and then the strip is not drawn at all.
fn shorten(path: &str, budget: usize) -> Option<String> {
    if display_width(path) <= budget {
        return Some(path.to_string());
    }
    // **Split on either separator.** Splitting on `/` alone meant a Windows path was one single
    // component, so nothing could ever be cut off the head and this returned `None` — and then
    // `spans` drew no strip at all. The whole "where am I, and what does git say" line was
    // missing on Windows for that one reason.
    let sep = if path.contains('\\') && !path.contains('/') { '\\' } else { '/' };
    let parts: Vec<&str> = path.split(sep).filter(|p| !p.is_empty()).collect();
    (1..parts.len())
        .map(|cut| format!("…{sep}{}", parts[cut..].join(&sep.to_string())))
        .find(|candidate| display_width(candidate) <= budget)
}

/// How long one `git status` may take before we give up on it.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Asks git what it thinks of `cwd`.
///
/// **`--no-optional-locks` is not optional.** A plain `git status` refreshes and relocks the
/// index; polling that every few seconds would collide with a `git commit` the user is running in
/// another terminal.
///
/// `--porcelain=v2 --branch` is one call for all of it — branch, upstream, ahead/behind, and a
/// line per changed path. Measured at 8ms on this repository.
///
/// **Every way this can fail returns `None`**: no git binary, not a repository, a timeout, or
/// output we cannot read. The strip then shows the path alone, with nothing where git was.
/// `kill_on_drop` matters because a `git` wedged on a dead network mount must not outlive the
/// timeout as a leaked process.
pub async fn read(cwd: &Path) -> Option<Repo> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["--no-optional-locks", "-C"])
        .arg(cwd)
        .args(["status", "--porcelain=v2", "--branch"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(READ_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    let mut repo = parse(&String::from_utf8_lossy(&out.stdout))?;
    repo.diverged = measure_against_base(cwd, &repo.branch).await;
    Some(repo)
}

/// How far this branch has moved from the one it came off, or `None` when that question has no
/// answer here.
///
/// **Nothing to say on the base branch itself.** Sitting on `main`, `main...HEAD` is empty by
/// definition, and `+0 -0` on every checkout that never made a branch is a column of noise.
///
/// `A...HEAD` (three dots) is deliberate: it measures from where the two last agreed, so pulling
/// the base does not suddenly credit this branch with everyone else's work.
async fn measure_against_base(cwd: &Path, branch: &str) -> Option<Diff> {
    let base = base_ref(cwd).await?;
    // `origin/main` and `main` both describe the branch named `main`.
    if base.rsplit('/').next() == Some(branch) {
        return None;
    }
    let out = git(cwd, &["diff", "--shortstat", &format!("{base}...HEAD")]).await?;
    Some(parse_shortstat(&out))
}

/// What `git diff --shortstat` said, in lines.
///
/// ```text
///  3 files changed, 120 insertions(+), 34 deletions(-)
///  1 file changed, 2 insertions(+)
/// ```
///
/// **Pure, and it never fails** — a clause that is absent is zero, which is exactly what git means
/// by leaving it out. Anything unparseable also reads as zero rather than as an error, because the
/// caller has already decided there is a branch worth measuring and a strip that silently loses
/// its numbers is better than one that loses the whole row.
pub fn parse_shortstat(text: &str) -> Diff {
    let mut out = Diff::default();
    for part in text.split(',') {
        let part = part.trim();
        let Some((n, rest)) = part.split_once(' ') else { continue };
        let Ok(n) = n.parse::<usize>() else { continue };
        // `insertion`/`insertions` and `deletion`/`deletions` — match the stem, not the plural.
        if rest.starts_with("insertion") {
            out.added = n;
        } else if rest.starts_with("deletion") {
            out.removed = n;
        }
    }
    out
}

/// The branch this checkout's work is measured against.
///
/// **Asked once per process.** It is a property of the remote, not of the moment, and the strip
/// polls every few seconds — re-resolving it each tick would double the git calls to learn
/// something that does not change. The cost of caching is that a repository which gains an
/// `origin/HEAD` mid-session keeps saying nothing until the app restarts, which is the cheaper
/// mistake.
///
/// `origin/HEAD` is the honest answer but is very often unset on a fresh clone, so the usual two
/// names are tried after it. `None` means there is nothing to compare against, and the strip then
/// shows no counts at all rather than comparing against something invented.
async fn base_ref(cwd: &Path) -> Option<&'static str> {
    static BASE: tokio::sync::OnceCell<Option<&'static str>> = tokio::sync::OnceCell::const_new();
    *BASE
        .get_or_init(|| async {
            for name in ["origin/HEAD", "origin/main", "origin/master", "main", "master"] {
                if git(cwd, &["rev-parse", "--verify", "--quiet", name]).await.is_some() {
                    return Some(name);
                }
            }
            None
        })
        .await
}

/// Runs a git command in `cwd` and hands back its stdout, or `None` for any failure at all.
async fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["--no-optional-locks", "-C"])
        .arg(cwd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(READ_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// How often to ask. `ZYRIS_CODE_GIT_MS`, 0 turns git off.
pub fn poll_interval() -> Option<std::time::Duration> {
    interval_from(std::env::var("ZYRIS_CODE_GIT_MS").ok().as_deref())
}

/// How often the pull request on the strip is refreshed.
///
/// **Far slower than git, because it costs three network calls.** Nothing on this row changes in
/// three seconds that a person needs to see in three seconds: a review lands, or CI turns over,
/// and thirty is soon enough for both. `0` turns it off.
pub fn pull_interval() -> Option<std::time::Duration> {
    pull_interval_from(std::env::var("ZYRIS_CODE_PULL_MS").ok().as_deref())
}

/// The decision on its own, for the same reason [`interval_from`] is split out.
pub fn pull_interval_from(given: Option<&str>) -> Option<std::time::Duration> {
    const DEFAULT_MS: u64 = 30_000;
    /// GitHub's own rate limit is per hour; anything under this spends it on nobody's behalf.
    const FLOOR_MS: u64 = 5_000;
    let ms = match given.map(str::trim).filter(|v| !v.is_empty()) {
        None => DEFAULT_MS,
        Some(text) => match text.parse::<u64>() {
            Ok(0) => return None,
            Ok(ms) => ms.max(FLOOR_MS),
            Err(_) => DEFAULT_MS,
        },
    };
    Some(std::time::Duration::from_millis(ms))
}

/// The decision, kept **pure** so tests do not have to shake the environment — the same reason
/// `theme::page_bg_from` is split out.
///
/// An unreadable value falls back to the default rather than turning the feature off: a typo that
/// silently removes the strip is harder to notice than one that changes nothing.
pub fn interval_from(given: Option<&str>) -> Option<std::time::Duration> {
    const DEFAULT_MS: u64 = 3000;
    /// Below this, git would be running more or less continuously for no gain.
    const FLOOR_MS: u64 = 250;
    let ms = match given.map(str::trim).filter(|v| !v.is_empty()) {
        None => DEFAULT_MS,
        Some(v) => match v.parse::<u64>() {
            Ok(0) => return None,
            Ok(ms) => ms.min(3_600_000),
            Err(_) => DEFAULT_MS,
        },
    };
    Some(std::time::Duration::from_millis(ms.max(FLOOR_MS)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A Windows path shortens too.** Splitting on `/` alone made `C:\Users\…` one single
    /// component, so nothing could be cut off the head, `shorten` returned `None`, and `spans`
    /// drew no strip at all — the whole "where am I" line was missing on Windows.
    #[test]
    fn a_path_shortens_on_either_separator() {
        let posix = shorten("/home/ruma/zyris-code/crates/zyris-code", 24);
        assert_eq!(posix.as_deref(), Some("…/crates/zyris-code"), "{posix:?}");

        let windows = shorten("C:\\Users\\ruma\\zyris-code\\crates\\zyris-code", 24);
        assert_eq!(windows.as_deref(), Some("…\\crates\\zyris-code"), "{windows:?}");

        // What already fits is left exactly as it was, on either platform.
        assert_eq!(shorten("C:\\proj", 20).as_deref(), Some("C:\\proj"));
    }

    #[test]
    fn the_poll_interval_is_read_off_the_environment() {
        use std::time::Duration;
        assert_eq!(super::interval_from(None), Some(Duration::from_millis(3000)));
        assert_eq!(super::interval_from(Some("500")), Some(Duration::from_millis(500)));
        // Zero is the way to turn git off entirely — then the strip is just the path.
        assert_eq!(super::interval_from(Some("0")), None);
        // A typo must not silently disable the feature, and must not let git spin either.
        assert_eq!(super::interval_from(Some("nonsense")), Some(Duration::from_millis(3000)));
        assert_eq!(super::interval_from(Some("1")), Some(Duration::from_millis(250)));
    }

    #[tokio::test]
    async fn a_directory_that_is_not_a_repository_reads_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(super::read(dir.path()).await, None);
    }

    #[tokio::test]
    async fn this_repository_reads_back_a_branch() {
        // The repo the tests run in is a git checkout, so this exercises the real call.
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let got = super::read(here).await.expect("zyris-code is a git checkout");
        assert!(!got.branch.is_empty());
    }

    // ── The pull request on the strip ─────────────────────────────────────────────────────

    /// **Its own piece, not more marks on the branch's.** `+` two columns to the left already
    /// means "staged", and `#53` is an identity while `+118 -30` are GitHub's numbers for the
    /// whole request — a separator says they are different things without a word of explanation.
    #[test]
    fn a_pull_request_stands_apart_from_what_the_branch_staged() {
        let text = strip_with(100, "/w", None, Some(&dirty()), Some(&open_pull()));
        assert!(text.contains("#53"), "{text}");
        assert!(text.contains("+118 -30"), "{text}");
        // The branch's own marks are still there, and something sits between them.
        assert!(text.contains("+2"), "{text}");
        let (before, after) = text.split_once("#53").expect("the number is on the row");
        assert!(before.trim_end().ends_with(SEP.trim_end()), "no separator before it: {text}");
        assert!(after.contains("+118"), "{text}");
    }

    /// **A branch with nothing open on it draws nothing.** Most branches never have a pull
    /// request, and a placeholder on every one of them is a row that stops being read.
    #[test]
    fn a_branch_with_no_pull_request_shows_nothing_about_one() {
        let text = strip_with(100, "/w", None, Some(&dirty()), None);
        assert!(!text.contains('#'), "{text}");
    }

    /// **Running, passed and failed each say a different word.** "if CI is going, show it" is the
    /// ask, and a mark that cannot tell the three apart answers none of them.
    #[test]
    fn the_state_of_the_checks_is_written_out() {
        let mut pull = open_pull();
        for (checks, word) in
            [(Checks::Running, "ci.."), (Checks::Passed, "ci ok"), (Checks::Failed, "ci x")]
        {
            pull.checks = checks;
            let text = strip_with(100, "/w", None, Some(&dirty()), Some(&pull));
            assert!(text.contains(word), "{checks:?} did not say {word}: {text}");
        }
        // **Nothing configured draws nothing.** A repository without CI must not grow a mark
        // that says so on every frame.
        pull.checks = Checks::Quiet;
        let text = strip_with(100, "/w", None, Some(&dirty()), Some(&pull));
        assert!(!text.contains("ci"), "{text}");
    }

    /// **The colours say the state, and they are not the ones the row already uses.** Yellow
    /// while it is somebody else's turn, purple once it has landed.
    #[test]
    fn a_merged_pull_request_is_a_different_colour_from_an_open_one() {
        let colour_of = |pull: &Pull| {
            super::spans(100, Path::new("/w"), None, Some(&dirty()), Some(pull))
                .into_iter()
                .find(|s| s.content.contains("#53"))
                .and_then(|s| s.style.fg)
                .expect("the number is drawn")
        };
        let mut pull = open_pull();
        let open = colour_of(&pull);
        pull.merged = true;
        let merged = colour_of(&pull);
        assert_ne!(open, merged, "open and merged look the same");
        assert_eq!(open, theme::warning(), "an open request waits on somebody");
        assert_eq!(merged, theme::merged(), "a landed one is over");
    }

    /// **The number outlives its own counts.** When the row gets tight, what is dropped is the
    /// size of the pull request; which pull request it is stays, because that is the part a
    /// person cannot work out from anything else on the row.
    #[test]
    fn the_pull_request_keeps_its_number_longest_when_the_row_gets_tight() {
        let mut last_with_number = 0;
        for width in (30u16..=100).rev() {
            let text = strip_with(width, "/w", None, Some(&dirty()), Some(&open_pull()));
            if text.contains("#53") {
                last_with_number = width;
            }
            if text.contains("+118") {
                assert!(text.contains("#53"), "the size outlived the number at {width}: {text}");
            }
        }
        assert!(last_with_number < 60, "the number never survived a narrow row");
    }

    /// The strip as plain text, the way a terminal would show it.
    fn strip(width: u16, cwd: &str, home: Option<&str>, repo: Option<&Repo>) -> String {
        strip_with(width, cwd, home, repo, None)
    }

    /// The same, with a pull request on it.
    fn strip_with(
        width: u16,
        cwd: &str,
        home: Option<&str>,
        repo: Option<&Repo>,
        pull: Option<&Pull>,
    ) -> String {
        super::spans(width, Path::new(cwd), home.map(Path::new), repo, pull)
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    /// A branch with an open pull request on it, checks still going.
    fn open_pull() -> Pull {
        Pull { number: 53, added: 118, removed: 30, checks: Checks::Running, merged: false }
    }

    fn dirty() -> Repo {
        Repo {
            branch: "main".into(),
            staged: 2,
            unstaged: 1,
            untracked: 3,
            conflicts: 0,
            ahead: 1,
            behind: 0,
            diverged: None,
        }
    }

    /// **The marks are painted, not merely printed.** `?3 ↑2 ↓1` spelled in one grey says three
    /// different things in a single voice, and the only way to tell them apart is to stop and read
    /// the glyph.
    #[test]
    fn the_marks_carry_the_colours_that_tell_them_apart() {
        let repo = Repo {
            branch: "main".into(),
            staged: 1,
            unstaged: 0,
            untracked: 3,
            conflicts: 0,
            ahead: 2,
            behind: 1,
            diverged: None,
        };
        let spans = super::spans(
            80,
            Path::new("/home/ruma/zyris-code"),
            Some(Path::new("/home/ruma")),
            Some(&repo),
            None,
        );
        let colour_of =
            |mark: char| spans.iter().find(|s| s.content.contains(mark)).and_then(|s| s.style.fg);
        assert_eq!(colour_of('?'), Some(theme::untracked()), "untracked wears its own tint");
        assert_eq!(colour_of('↑'), Some(theme::ahead()), "ahead is yours to push");
        assert_eq!(colour_of('↓'), Some(theme::behind()), "behind came from elsewhere");
        assert_ne!(colour_of('↑'), colour_of('↓'), "the two directions must not look alike");
        assert_ne!(colour_of('?'), colour_of('↑'), "litter must not look like work to send");
    }

    /// **A clause git left out is zero, not a failure.** `--shortstat` prints only the counts
    /// that happened, so a commit that adds and never deletes has no deletion clause at all.
    #[test]
    fn a_shortstat_says_zero_for_the_clause_it_leaves_out() {
        let both = parse_shortstat(" 3 files changed, 120 insertions(+), 34 deletions(-)");
        assert_eq!(both, Diff { added: 120, removed: 34 });
        let added_only = parse_shortstat(" 1 file changed, 2 insertions(+)");
        assert_eq!(added_only, Diff { added: 2, removed: 0 });
        let removed_only = parse_shortstat(" 1 file changed, 7 deletions(-)");
        assert_eq!(removed_only, Diff { added: 0, removed: 7 });
        // Nothing changed, and anything unreadable, both land on zero rather than on a panic.
        assert_eq!(parse_shortstat(""), Diff::default());
        assert_eq!(parse_shortstat("what?"), Diff::default());
    }

    /// **`+120 -34` must not be read as staged files.** `+` two columns to the left already means
    /// "staged", so the diverged counts stand as their own piece behind a separator.
    #[test]
    fn the_lines_a_branch_changed_stand_apart_from_the_files_it_staged() {
        let mut repo = dirty();
        repo.diverged = Some(Diff { added: 120, removed: 34 });
        let out = strip(90, "/home/ruma/zyris-code", Some("/home/ruma"), Some(&repo));
        assert!(out.contains("+120 -34"), "the branch's size is missing: {out:?}");
        assert_eq!(out.matches('∙').count(), 2, "path ∙ git ∙ diverged: {out:?}");
    }

    /// **A branch level with its base draws nothing either.** It was measured and absence was
    /// not, which is why they stay apart in the measurement — but `+0 -0` says the same thing to
    /// a reader as nothing does, in more characters. Reported on Windows 2026-08-18.
    #[test]
    fn a_branch_that_changed_nothing_says_nothing() {
        let level = Repo { diverged: Some(Diff::default()), ..dirty() };
        let text = strip(100, "/w", None, Some(&level));
        assert!(!text.contains("+0"), "{text}");
        assert!(!text.contains("-0"), "{text}");
        // The branch's own marks are untouched — this is about the piece, not the row.
        assert!(text.contains("+2"), "{text}");

        // And a pull request that changed nothing keeps its number and drops its zeros.
        let empty = Pull { number: 53, added: 0, removed: 0, ..open_pull() };
        let text = strip_with(100, "/w", None, Some(&dirty()), Some(&empty));
        assert!(text.contains("#53"), "{text}");
        assert!(!text.contains("+0") && !text.contains("-0"), "{text}");
    }

    /// **A branch with nothing to compare against draws nothing**, and it is a different case
    /// from one measured at zero — the measurement keeps them apart even though the row does not.
    #[test]
    fn a_branch_with_nothing_to_compare_against_shows_no_counts() {
        let repo = dirty();
        assert_eq!(repo.diverged, None, "the fixture is the unmeasured case");
        let out = strip(90, "/home/ruma/zyris-code", Some("/home/ruma"), Some(&repo));
        assert!(!out.contains("+0 -0"), "absence was drawn as zero: {out:?}");
        assert_eq!(out.matches('∙').count(), 1, "no empty piece and no stray join: {out:?}");
    }

    /// The counts go before the ahead/behind arrows do — how much the branch changed is the thing
    /// asked for, and the arrows are a smaller errand.
    #[test]
    fn the_branch_size_outlives_the_arrows_when_the_row_gets_tight() {
        let mut repo = dirty();
        repo.diverged = Some(Diff { added: 120, removed: 34 });
        let wide = strip(90, "/home/ruma/zyris-code", Some("/home/ruma"), Some(&repo));
        assert!(wide.contains('↑') && wide.contains("+120"), "both fit at 90: {wide:?}");
        let tight = strip(46, "/home/ruma/zyris-code", Some("/home/ruma"), Some(&repo));
        assert!(!tight.contains('↑'), "the arrow should have gone first: {tight:?}");
        assert!(tight.contains("+120"), "the size should outlive it: {tight:?}");
    }

    #[test]
    fn a_missing_piece_leaves_no_separator_behind() {
        // **The point of the whole design.** Without git the path must be followed by the rule
        // directly — no separator, no reserved gap, nothing for a later piece to trip over.
        let out = strip(60, "/home/ruma/zyris-code", Some("/home/ruma"), None);
        assert!(out.starts_with("─ ~/zyris-code ─"), "no residue where git would be: {out:?}");
        assert!(!out.contains('∙'), "a separator with nothing on one side: {out:?}");
    }

    #[test]
    fn the_separator_appears_only_between_two_present_pieces() {
        let out = strip(60, "/home/ruma/zyris-code", Some("/home/ruma"), Some(&dirty()));
        assert_eq!(out.matches('∙').count(), 1, "exactly one join: {out:?}");
        assert!(out.contains("~/zyris-code ∙ * main"), "{out:?}");
    }

    #[test]
    fn a_clean_repository_shows_no_counts() {
        let clean = Repo { branch: "main".into(), ..Repo::default() };
        let out = strip(60, "/home/ruma/zyris-code", Some("/home/ruma"), Some(&clean));
        assert!(out.contains("* main"), "{out:?}");
        // Every count carries a digit, so no digit means no count. Checking the markers
        // themselves would trip over the `~` of the home directory.
        assert!(!out.contains(|c: char| c.is_ascii_digit()), "clean must be quiet: {out:?}");
        for noise in ['+', '?', '↑', '↓', '!'] {
            assert!(!out.contains(noise), "clean must be quiet, saw {noise:?}: {out:?}");
        }
    }

    #[test]
    fn every_count_shows_when_there_is_room() {
        let mut r = dirty();
        r.behind = 4;
        r.conflicts = 5;
        let out = strip(80, "/home/ruma/zyris-code", Some("/home/ruma"), Some(&r));
        for want in ["!5", "+2", "~1", "?3", "↑1", "↓4"] {
            assert!(out.contains(want), "missing {want}: {out:?}");
        }
    }

    #[test]
    fn narrowing_drops_the_least_urgent_piece_first() {
        let cwd = "/home/ruma/zyris-code";
        let home = Some("/home/ruma");
        let r = dirty();
        // Untracked goes before ahead, ahead before the counts, the counts before git itself.
        let order = ["?3", "↑1", "+2", "* main", "~/zyris-code"];
        let mut gone: Vec<&str> = Vec::new();
        for width in (8..=60u16).rev() {
            let out = strip(width, cwd, home, Some(&r));
            for want in order {
                if !out.contains(want) && !gone.contains(&want) {
                    gone.push(want);
                }
            }
        }
        assert_eq!(gone, order, "things must fall off least-urgent first");
    }

    #[test]
    fn the_strip_never_exceeds_the_width_it_was_given() {
        let r = dirty();
        for width in 0..=120u16 {
            for cwd in ["/", "/home/ruma/zyris-code", "/home/ruma/a/very/deep/tree/of/directories"]
            {
                for repo in [None, Some(&r)] {
                    let out = strip(width, cwd, Some("/home/ruma"), repo);
                    let w = crate::markdown::display_width(&out);
                    assert_eq!(w, width as usize, "width {width} cwd {cwd} gave {w}: {out:?}");
                }
            }
        }
    }

    #[test]
    fn a_rule_too_short_to_read_as_a_rule_is_not_drawn_at_all() {
        // Under the floor the row goes back to what it draws today: a bare rule.
        let out = strip(7, "/home/ruma/zyris-code", Some("/home/ruma"), None);
        assert_eq!(out, "───────");
    }

    #[test]
    fn home_becomes_a_tilde_and_a_long_path_loses_its_head() {
        assert!(
            strip(60, "/home/ruma/zyris-code", Some("/home/ruma"), None).contains("~/zyris-code")
        );
        // Not under home: shown as it is.
        assert!(strip(60, "/etc/nginx", Some("/home/ruma"), None).contains("/etc/nginx"));
        // Too long for the room: the head goes, never the tail.
        let deep = "/home/ruma/a/very/deep/tree/of/directories/indeed";
        let out = strip(28, deep, Some("/home/ruma"), None);
        assert!(out.contains('…'), "{out:?}");
        assert!(out.contains("indeed"), "the tail is the part that identifies it: {out:?}");
    }

    #[test]
    fn every_span_carries_a_colour() {
        // An unstyled span leaks the terminal's own default foreground (`theme` header rule).
        for s in super::spans(60, Path::new("/home/ruma/zyris-code"), None, Some(&dirty()), None) {
            assert!(s.style.fg.is_some(), "uncoloured span: {:?}", s.content);
        }
    }

    #[test]
    fn a_clean_checkout_parses_to_a_clean_repo() {
        let out = "\
# branch.oid d192a603f6d083519b8bf785bcd41061c97e0cb8
# branch.head main
# branch.upstream origin/main
# branch.ab +0 -0
";
        let r = parse(out).expect("a branch header is enough to be a repository");
        assert_eq!(r.branch, "main");
        assert!(r.is_clean());
    }

    #[test]
    fn a_path_counts_in_both_columns_when_it_is_staged_and_edited_again() {
        // XY = "MM": staged in the index and changed again in the worktree.
        let out = "\
# branch.oid abc1234def5678
# branch.head main
1 MM N... 100644 100644 100644 aaa bbb src/app.rs
1 M. N... 100644 100644 100644 ccc ddd src/lib.rs
1 .M N... 100644 100644 100644 eee fff src/rows.rs
";
        let r = parse(out).unwrap();
        assert_eq!((r.staged, r.unstaged), (2, 2));
    }

    #[test]
    fn untracked_and_unmerged_lines_are_counted_apart() {
        let out = "\
# branch.oid abc1234def5678
# branch.head main
? target/
? notes.txt
u UU N... 100644 100644 100644 100644 aaa bbb ccc src/conflict.rs
";
        let r = parse(out).unwrap();
        assert_eq!((r.untracked, r.conflicts), (2, 1));
        assert_eq!((r.staged, r.unstaged), (0, 0));
    }

    #[test]
    fn a_detached_head_shows_a_short_oid_where_the_branch_would_be() {
        let out = "\
# branch.oid d192a603f6d083519b8bf785bcd41061c97e0cb8
# branch.head (detached)
";
        assert_eq!(parse(out).unwrap().branch, "d192a60");
    }

    #[test]
    fn a_branch_with_no_upstream_has_no_ahead_or_behind() {
        let out = "\
# branch.oid abc1234def5678
# branch.head feat/strip
";
        let r = parse(out).unwrap();
        assert_eq!((r.ahead, r.behind), (0, 0));
    }

    #[test]
    fn ahead_and_behind_come_off_the_ab_header() {
        let out = "\
# branch.oid abc1234def5678
# branch.head main
# branch.upstream origin/main
# branch.ab +2 -3
";
        let r = parse(out).unwrap();
        assert_eq!((r.ahead, r.behind), (2, 3));
    }

    #[test]
    fn output_without_a_branch_header_is_not_a_repository() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("fatal: not a git repository"), None);
    }
}
