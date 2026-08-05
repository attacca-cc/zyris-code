//! How a file changed. All computation happens here; the screen just draws the result.
//!
//! **The diff rides in a tool result and does a round trip through the server.** That way,
//! reopening a session shows the same thing through history replay. Kept only in local memory,
//! it vanishes the moment the app closes. Hence the round trip of hardening it into text and
//! reading it back, locked in by `a_diff_survives_the_round_trip_through_text` — if it breaks, the diff silently degrades to a JSON dump.

use similar::{ChangeTag, TextDiff};

/// Context lines to keep around a change. `Keep`s farther than this are folded — otherwise
/// fixing one file would fill the screen with that file.
const CONTEXT: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Add(String),
    Del(String),
    Keep(String),
    /// Number of folded context lines.
    Skip(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub path: String,
    pub added: u32,
    pub removed: u32,
    pub lines: Vec<DiffLine>,
}

pub fn diff(old: &str, new: &str, path: &str) -> Diff {
    let text = TextDiff::from_lines(old, new);
    let mut all: Vec<DiffLine> = Vec::new();
    let mut added = 0;
    let mut removed = 0;
    for change in text.iter_all_changes() {
        let value = change.value().trim_end_matches('\n').to_string();
        match change.tag() {
            ChangeTag::Insert => {
                added += 1;
                all.push(DiffLine::Add(value));
            }
            ChangeTag::Delete => {
                removed += 1;
                all.push(DiffLine::Del(value));
            }
            ChangeTag::Equal => all.push(DiffLine::Keep(value)),
        }
    }
    Diff { path: path.to_string(), added, removed, lines: fold_context(all) }
}

/// Folds `Keep`s farther than `CONTEXT` lines from a change into `Skip`.
fn fold_context(lines: Vec<DiffLine>) -> Vec<DiffLine> {
    if lines.is_empty() {
        return lines;
    }
    let last = lines.len() - 1;
    let near: Vec<bool> = (0..lines.len())
        .map(|i| {
            let lo = i.saturating_sub(CONTEXT);
            let hi = (i + CONTEXT).min(last);
            (lo..=hi).any(|j| !matches!(lines[j], DiffLine::Keep(_)))
        })
        .collect();
    let mut out: Vec<DiffLine> = Vec::new();
    let mut skipped = 0u32;
    for (line, keep_it) in lines.into_iter().zip(near) {
        if keep_it {
            if skipped > 0 {
                out.push(DiffLine::Skip(std::mem::take(&mut skipped)));
            }
            out.push(line);
        } else {
            skipped += 1;
        }
    }
    if skipped > 0 {
        out.push(DiffLine::Skip(skipped));
    }
    out
}

impl Diff {
    /// Text shape to carry in a tool result. `@@ N lines omitted @@` marks a folded spot.
    pub fn to_unified(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            match line {
                DiffLine::Add(s) => out.push_str(&format!("+{s}\n")),
                DiffLine::Del(s) => out.push_str(&format!("-{s}\n")),
                DiffLine::Keep(s) => out.push_str(&format!(" {s}\n")),
                DiffLine::Skip(n) => {
                    out.push_str(&crate::lang::current().diff_omitted(*n));
                    out.push('\n');
                }
            }
        }
        out
    }

    /// Reads back what `to_unified` wrote. `None` if it does not look right — then the screen
    /// falls back to a JSON dump as today. **The point is not to die.**
    pub fn parse(text: &str, path: &str, added: u32, removed: u32) -> Option<Diff> {
        let mut lines = Vec::new();
        for raw in text.lines() {
            let line = if let Some(rest) = raw.strip_prefix("@@ ") {
                let n = rest.split('줄').next()?.trim().parse().ok()?;
                DiffLine::Skip(n)
            } else {
                match raw.chars().next() {
                    Some('+') => DiffLine::Add(raw[1..].to_string()),
                    Some('-') => DiffLine::Del(raw[1..].to_string()),
                    Some(' ') => DiffLine::Keep(raw[1..].to_string()),
                    _ => return None,
                }
            };
            lines.push(line);
        }
        (!lines.is_empty()).then(|| Diff { path: path.to_string(), added, removed, lines })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_changed_line_counts_one_each() {
        let d = diff("a\nb\nc\n", "a\nB\nc\n", "x.rs");
        assert_eq!((d.added, d.removed), (1, 1));
    }

    /// Keep at most 3 lines of context and fold the rest. Otherwise fixing one file
    /// would fill the screen with that file.
    #[test]
    fn far_away_context_is_folded() {
        let old: String = (0..40).map(|i| format!("{i}\n")).collect();
        let new = old.replace("20\n", "스물\n");
        let d = diff(&old, &new, "x.rs");
        assert!(d.lines.iter().any(|l| matches!(l, DiffLine::Skip(_))), "먼 문맥은 접혀야 한다");
        assert!(d.lines.len() < 20, "40줄짜리가 그대로 나오면 안 된다: {}", d.lines.len());
    }

    /// Fixing a file without a trailing newline must not count the last line as changed.
    #[test]
    fn a_missing_trailing_newline_is_not_a_change() {
        let d = diff("a\nb", "a\nb", "x.rs");
        assert_eq!((d.added, d.removed), (0, 0));
    }

    #[test]
    fn an_empty_file_gaining_content_is_all_additions() {
        let d = diff("", "한\n글\n", "x.rs");
        assert_eq!((d.added, d.removed), (2, 0));
    }

    /// The screen side must read back the diff sent in the result. If the round trip breaks, the diff is not shown.
    #[test]
    fn a_diff_survives_the_round_trip_through_text() {
        let d = diff("a\nb\nc\n", "a\nB\nc\n", "x.rs");
        let back = Diff::parse(&d.to_unified(), "x.rs", d.added, d.removed).expect("다시 읽힌다");
        assert_eq!(back.lines, d.lines);
    }

    /// The fold markers must survive the round trip too — diffs of long files almost always include folds.
    #[test]
    fn a_folded_diff_also_survives_the_round_trip() {
        let old: String = (0..40).map(|i| format!("{i}\n")).collect();
        let new = old.replace("20\n", "스물\n");
        let d = diff(&old, &new, "x.rs");
        let back = Diff::parse(&d.to_unified(), "x.rs", d.added, d.removed).expect("다시 읽힌다");
        assert_eq!(back.lines, d.lines);
    }

    /// Feeding it non-diff text must yield `None` instead of quietly drawing something wrong.
    #[test]
    fn something_that_is_not_a_diff_is_not_read_as_one() {
        assert_eq!(Diff::parse("그냥 문장입니다", "x.rs", 0, 0), None);
    }
}
