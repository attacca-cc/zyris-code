//! **A `terminal.exec` answer held to a byte budget**, its head and tail kept and the middle left
//! where `wait.logs` can page it.
//!
//! Nothing upstream bounds `exec` below its 1 MiB per stream, and every byte of an answer rides in
//! the agent's context on every later turn. Measured sessions put the cost in a few large answers —
//! `git diff`, `grep -rn`, `cat` — while the median answer was about 1.4 KB, so a cap touches only
//! the answers that were costing the most.
//!
//! **Head and tail get equal shares.** The large answers are diffs and search hits, where the start
//! matters as much as the end. Lines in the omitted middle that look like errors are carried after
//! the marker, so a failure in the middle of a long log is still seen.

/// The default budget for one answer, stdout and stderr together.
pub const BUDGET: usize = 8_000;
/// How many error-looking lines from the omitted middle are carried over.
const ERROR_LINES: usize = 20;
/// Each carried line is clipped to this many characters.
const ERROR_LINE_CHARS: usize = 200;
/// What separates stdout from stderr in the recorded full output.
pub const STDERR_SEPARATOR: &str = "\n--- stderr ---\n";

/// The budget in bytes: `ZYRIS_CODE_EXEC_BUDGET`, or [`BUDGET`]. **`0` turns shaping off.**
pub fn budget() -> usize {
    std::env::var("ZYRIS_CODE_EXEC_BUDGET").ok().and_then(|v| v.parse().ok()).unwrap_or(BUDGET)
}

/// Whether an answer has to be cut at all.
pub fn over(stdout: &str, stderr: &str, budget: usize) -> bool {
    budget > 0 && stdout.len() + stderr.len() > budget
}

/// Both streams as one buffer, the way `wait.logs` pages it.
pub fn full_output(stdout: &str, stderr: &str) -> String {
    if stderr.is_empty() {
        stdout.to_string()
    } else {
        format!("{stdout}{STDERR_SEPARATOR}{stderr}")
    }
}

/// Both streams cut to `budget` between them, each in proportion to its size. `job` is where
/// [`full_output`] of the same two was recorded.
pub fn shape(
    stdout: &str,
    stderr: &str,
    budget: usize,
    job: &str,
    command: &str,
) -> (String, String) {
    let total = stdout.len() + stderr.len();
    let stdout_room = budget * stdout.len() / total.max(1);
    let hints = hints(command);
    let stderr_base = (stdout.len() + STDERR_SEPARATOR.len()) as u64;
    (
        fit(stdout, stdout_room, 0, job, &hints),
        fit(stderr, budget - stdout_room, stderr_base, job, &hints),
    )
}

/// One stream cut to `room` bytes: its head, a marker saying what was left out and where to read
/// it, and its tail. `base` is where this stream starts in the recorded full output.
fn fit(text: &str, room: usize, base: u64, job: &str, hints: &str) -> String {
    if text.len() <= room {
        return text.to_string();
    }
    let half = room / 2;
    let head_end = line_end_before(text, half);
    let tail_start = line_start_after(text, text.len() - half).max(head_end);
    let middle = &text[head_end..tail_start];
    if middle.is_empty() {
        return text.to_string();
    }

    let mut marker = format!(
        "… {} bytes ({} lines) omitted. wait.logs job=\"{job}\" offset={} reads them.\n{hints}",
        middle.len(),
        middle.lines().count(),
        base + head_end as u64,
    );
    let errors: Vec<String> = middle
        .lines()
        .filter(|l| looks_like_an_error(l))
        .take(ERROR_LINES)
        .map(|l| l.chars().take(ERROR_LINE_CHARS).collect())
        .collect();
    if !errors.is_empty() {
        marker.push_str("Error lines from the omitted part:\n");
        for line in errors {
            marker.push_str(&line);
            marker.push('\n');
        }
    }
    format!("{}{marker}…\n{}", &text[..head_end], &text[tail_start..])
}

/// The end of the last whole line within the first `at` bytes, or `at` itself (on a character
/// boundary) when the first line is longer than that.
fn line_end_before(text: &str, at: usize) -> usize {
    let at = text.floor_char_boundary(at);
    text[..at].rfind('\n').map_or(at, |i| i + 1)
}

/// The start of the first whole line at or after byte `at`, or `at` itself (on a character
/// boundary) when the last line is longer than what is left.
fn line_start_after(text: &str, at: usize) -> usize {
    let at = text.ceil_char_boundary(at);
    if at == 0 || text.as_bytes()[at - 1] == b'\n' {
        return at;
    }
    text[at..].find('\n').map_or(at, |i| at + i + 1)
}

fn looks_like_an_error(line: &str) -> bool {
    let lower = line.to_lowercase();
    ["error", "panicked", "failed", "fatal"].iter().any(|w| lower.contains(w))
}

/// What to do instead, for the commands measured to produce the largest answers. **Said at the cut,
/// because that is when the agent has just paid for the expensive way.**
fn hints(command: &str) -> String {
    let words: Vec<&str> =
        command.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')).collect();
    let has = |w: &str| words.contains(&w);
    let mut out = String::new();
    if command.contains("git diff") || command.contains("git show") {
        out.push_str("For a diff, run `git diff --stat` first and then diff one file at a time.\n");
    }
    if has("grep") || has("rg") {
        out.push_str("To search code, use search.grep; it pages its results.\n");
    }
    if has("cat") || has("sed") || has("head") || has("tail") {
        out.push_str("To read a file, use file_io.read; it pages the file.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize, width: usize) -> String {
        (0..n).map(|i| format!("{i:0width$}\n")).collect()
    }

    /// **Under budget, nothing changes** — and a budget of 0 means no budget at all.
    #[test]
    fn an_answer_within_the_budget_is_left_alone() {
        assert!(!over("abc", "de", 5));
        assert!(over("abc", "def", 5));
        assert!(!over(&"x".repeat(100_000), "", 0));
        assert_eq!(fit("short\n", 100, 0, "b1", ""), "short\n");
    }

    /// Head and tail on line boundaries, equal in size, with a marker whose numbers are right and
    /// whose offset points at the first omitted byte.
    #[test]
    fn a_long_answer_keeps_its_head_and_tail_and_says_where_the_rest_is() {
        let text = lines(1000, 9); // 10 bytes a line
        let out = fit(&text, 1000, 0, "b7", "");
        let (head, rest) = out.split_once('…').unwrap();
        assert_eq!(head, &text[..500]);
        assert!(out.ends_with(&text[text.len() - 500..]));
        assert!(
            rest.starts_with(" 9000 bytes (900 lines) omitted. wait.logs job=\"b7\" offset=500 ")
        );
        // The offset is where the omitted part starts in the recorded output.
        assert!(text[500..].starts_with("000000050\n"));
    }

    /// An error in the middle is carried after the marker; a clean middle adds nothing.
    #[test]
    fn an_error_in_the_omitted_middle_is_still_seen() {
        let mut text = lines(500, 9);
        text.insert_str(2500, "error[E0308]: mismatched types\n");
        let out = fit(&text, 1000, 0, "b1", "");
        assert!(
            out.contains("Error lines from the omitted part:\nerror[E0308]: mismatched types\n")
        );
        assert!(!fit(&lines(500, 9), 1000, 0, "b1", "").contains("Error lines"));
    }

    /// Each stream gets its share, and stderr's offset counts stdout and the separator before it.
    #[test]
    fn both_streams_share_the_budget_and_stderr_points_past_stdout() {
        let stdout = lines(300, 9);
        let stderr = lines(100, 9);
        let (out, err) = shape(&stdout, &stderr, 800, "b2", "cargo build");
        assert!(out.len() < stdout.len() && err.len() < stderr.len());
        let base = stdout.len() + STDERR_SEPARATOR.len();
        let full = full_output(&stdout, &stderr);
        let offset: usize =
            err.split("offset=").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
        assert!(offset > base && full[offset..].starts_with(&stderr[offset - base..]));
        assert_eq!(full_output("a", ""), "a");
    }

    /// A single line longer than the budget, full of multi-byte characters, is cut on character
    /// boundaries rather than panicking.
    #[test]
    fn one_long_line_of_multibyte_text_does_not_panic() {
        let text = "가".repeat(5000);
        let out = fit(&text, 1001, 0, "b1", "");
        assert!(out.contains("omitted"));
    }

    /// The hints follow the command, including one run after `cd … &&`.
    #[test]
    fn the_hints_name_the_tool_for_the_command_that_was_run() {
        assert!(hints("cd /x && git diff -- src").contains("git diff --stat"));
        assert!(hints("grep -rn foo src").contains("search.grep"));
        assert!(hints("rg foo").contains("search.grep"));
        assert!(hints("cat a.rs; sed -n 1,9p b.rs").contains("file_io.read"));
        assert_eq!(hints("cargo test"), "");
        // A word that merely contains one is not it.
        assert_eq!(hints("./categorize --regrep"), "");
    }
}
