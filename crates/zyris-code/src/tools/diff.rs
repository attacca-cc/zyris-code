//! 파일이 어떻게 바뀌었는가. 계산은 여기서만 하고 화면은 결과만 그린다.
//!
//! **diff는 도구 결과에 실려 서버를 한 바퀴 돌아온다.** 그래야 세션을 다시 열었을 때
//! 히스토리 재생으로도 같은 것이 보인다. 로컬 기억에만 두면 앱을 끄는 순간 사라진다.
//! 그래서 글자로 굳혔다 다시 읽는 왕복이 있고, `a_diff_survives_the_round_trip_through_text`가
//! 그 왕복을 잠근다 — 깨지면 diff가 조용히 JSON 덤프로 떨어진다.

use similar::{ChangeTag, TextDiff};

/// 변경 주위로 남길 문맥 줄 수. 이보다 멀리 떨어진 `Keep`은 접는다 — 안 그러면 파일
/// 하나 고칠 때 화면이 그 파일로 가득 찬다.
const CONTEXT: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Add(String),
    Del(String),
    Keep(String),
    /// 접은 문맥 줄 수.
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

/// 변경에서 `CONTEXT`줄보다 멀리 떨어진 `Keep`을 `Skip`으로 접는다.
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
    /// 도구 결과에 실을 글자 모양. `@@ N줄 생략 @@`이 접힌 자리를 나타낸다.
    pub fn to_unified(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            match line {
                DiffLine::Add(s) => out.push_str(&format!("+{s}\n")),
                DiffLine::Del(s) => out.push_str(&format!("-{s}\n")),
                DiffLine::Keep(s) => out.push_str(&format!(" {s}\n")),
                DiffLine::Skip(n) => out.push_str(&format!("@@ {n}줄 생략 @@\n")),
            }
        }
        out
    }

    /// `to_unified`가 쓴 것을 다시 읽는다. 모양이 아니면 `None` — 그러면 화면은 지금처럼
    /// JSON 덤프로 떨어진다. **죽지 않는 것이 요점이다.**
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

    /// 문맥은 3줄까지만 남기고 그 사이는 접는다. 안 그러면 파일 하나 고칠 때
    /// 화면이 그 파일로 가득 찬다.
    #[test]
    fn far_away_context_is_folded() {
        let old: String = (0..40).map(|i| format!("{i}\n")).collect();
        let new = old.replace("20\n", "스물\n");
        let d = diff(&old, &new, "x.rs");
        assert!(d.lines.iter().any(|l| matches!(l, DiffLine::Skip(_))), "먼 문맥은 접혀야 한다");
        assert!(d.lines.len() < 20, "40줄짜리가 그대로 나오면 안 된다: {}", d.lines.len());
    }

    /// 끝줄 개행이 없는 파일을 고쳐도 마지막 줄이 바뀐 것으로 잡히면 안 된다.
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

    /// 결과에 실어 보낸 diff를 화면 쪽이 다시 읽어야 한다. 왕복이 깨지면 diff가 안 보인다.
    #[test]
    fn a_diff_survives_the_round_trip_through_text() {
        let d = diff("a\nb\nc\n", "a\nB\nc\n", "x.rs");
        let back = Diff::parse(&d.to_unified(), "x.rs", d.added, d.removed).expect("다시 읽힌다");
        assert_eq!(back.lines, d.lines);
    }

    /// 접힘 표시도 왕복해야 한다 — 긴 파일의 diff는 거의 항상 접힘을 포함한다.
    #[test]
    fn a_folded_diff_also_survives_the_round_trip() {
        let old: String = (0..40).map(|i| format!("{i}\n")).collect();
        let new = old.replace("20\n", "스물\n");
        let d = diff(&old, &new, "x.rs");
        let back = Diff::parse(&d.to_unified(), "x.rs", d.added, d.removed).expect("다시 읽힌다");
        assert_eq!(back.lines, d.lines);
    }

    /// diff가 아닌 글자를 넣으면 조용히 틀린 것을 그리는 대신 None이어야 한다.
    #[test]
    fn something_that_is_not_a_diff_is_not_read_as_one() {
        assert_eq!(Diff::parse("그냥 문장입니다", "x.rs", 0, 0), None);
    }
}
