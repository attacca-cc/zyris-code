//! 대화 영역의 텍스트 선택.
//!
//! 좌표는 **(행, 화면 열)**이다. 열은 글자 수가 아니라 칸 수라 전각이 2를 차지한다 —
//! 마우스가 주는 값이 칸 단위이기 때문이다. 글자 수로 다루면 한글이 섞인 줄에서
//! 선택 범위가 어긋난다.

use crate::markdown::display_width;

/// 드래그 중인 범위. 시작점과 지금 점이며, 순서는 뒤바뀔 수 있다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drag {
    pub from: (usize, usize),
    pub to: (usize, usize),
}

impl Drag {
    pub fn new(at: (usize, usize)) -> Self {
        Self { from: at, to: at }
    }

    /// 위에서 아래로 정렬한 (시작, 끝).
    pub fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.from <= self.to {
            (self.from, self.to)
        } else {
            (self.to, self.from)
        }
    }

    /// 아직 한 칸도 안 움직였는가. 그렇다면 선택이 아니라 클릭이다.
    pub fn is_click(&self) -> bool {
        self.from == self.to
    }
}

/// 고른 텍스트를 뽑는다. 줄 사이는 `\n`으로 잇는다.
pub fn extract(rows: &[String], drag: &Drag) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let ((r0, c0), (r1, c1)) = drag.ordered();
    let last = rows.len() - 1;
    let (r0, r1) = (r0.min(last), r1.min(last));

    if r0 == r1 {
        return slice_cols(&rows[r0], c0.min(c1), c0.max(c1));
    }

    let mut out = vec![slice_cols(&rows[r0], c0, usize::MAX)];
    for row in &rows[r0 + 1..r1] {
        out.push(row.trim_end().to_string());
    }
    out.push(slice_cols(&rows[r1], 0, c1));
    out.join("\n")
}

/// 이 행에서 반전시켜야 할 열 구간 `[from, to)`. 범위 밖이면 `None`.
///
/// 첫 행은 시작 열부터 끝까지, 가운데 행은 통째로, 마지막 행은 0부터 끝 열까지다.
pub fn highlight_span(drag: &Drag, row: usize) -> Option<(usize, usize)> {
    let ((r0, c0), (r1, c1)) = drag.ordered();
    if row < r0 || row > r1 {
        return None;
    }
    if r0 == r1 {
        return Some((c0.min(c1), c0.max(c1)));
    }
    if row == r0 {
        Some((c0, usize::MAX))
    } else if row == r1 {
        Some((0, c1))
    } else {
        Some((0, usize::MAX))
    }
}

/// 화면 열 `[from, to)` 구간의 글자들. 전각은 2칸으로 센다.
fn slice_cols(row: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    for ch in row.chars() {
        let w = display_width(&ch.to_string()).max(1);
        if col >= to {
            break;
        }
        if col >= from {
            out.push(ch);
        }
        col += w;
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<String> {
        vec![
            "안녕하세요 반갑습니다".to_string(),
            "second line".to_string(),
            "세 번째 줄".to_string(),
        ]
    }

    #[test]
    fn a_drag_that_never_moved_is_a_click() {
        assert!(Drag::new((2, 3)).is_click());
        let mut d = Drag::new((2, 3));
        d.to = (2, 4);
        assert!(!d.is_click());
    }

    /// 한 줄 안에서 고르면 그 구간만 나온다. 전각은 2칸이라 열 4는 세 번째 글자다.
    #[test]
    fn selecting_within_one_line_takes_that_span() {
        let d = Drag { from: (0, 0), to: (0, 4) };
        assert_eq!(extract(&rows(), &d), "안녕");
    }

    #[test]
    fn selecting_backwards_gives_the_same_text() {
        let forward = Drag { from: (0, 0), to: (0, 4) };
        let backward = Drag { from: (0, 4), to: (0, 0) };
        assert_eq!(extract(&rows(), &forward), extract(&rows(), &backward));
    }

    /// 여러 줄을 고르면 가운데 줄은 통째로, 양 끝은 잘려서 나온다.
    #[test]
    fn selecting_across_lines_keeps_the_middle_whole() {
        let d = Drag { from: (0, 10), to: (2, 4) };
        let got = extract(&rows(), &d);
        let lines: Vec<&str> = got.lines().collect();
        assert_eq!(lines.len(), 3, "{got:?}");
        assert_eq!(lines[1], "second line", "가운데 줄은 통째로여야 한다");
        // '번'은 열 3~4를 차지하므로 열 4까지 끌면 그 위에 마우스가 있는 것이라 포함된다.
        assert_eq!(lines[2], "세 번", "마지막 줄은 열 4가 걸친 글자까지");
    }

    /// 화면 밖을 가리켜도 죽지 않는다 — 마우스는 어디든 갈 수 있다.
    #[test]
    fn dragging_past_the_end_is_clamped() {
        let d = Drag { from: (0, 0), to: (99, 999) };
        let got = extract(&rows(), &d);
        assert!(got.ends_with("세 번째 줄"), "{got:?}");
    }

    /// 하이라이트 구간이 추출과 같은 규칙이어야 한다. 어긋나면 고른 것과 보이는 것이 다르다.
    #[test]
    fn the_highlight_span_matches_what_gets_extracted() {
        let d = Drag { from: (1, 3), to: (3, 5) };
        assert_eq!(highlight_span(&d, 0), None, "범위 위는 반전하지 않는다");
        assert_eq!(highlight_span(&d, 1), Some((3, usize::MAX)), "첫 행은 시작 열부터 끝까지");
        assert_eq!(highlight_span(&d, 2), Some((0, usize::MAX)), "가운데 행은 통째로");
        assert_eq!(highlight_span(&d, 3), Some((0, 5)), "마지막 행은 끝 열까지");
        assert_eq!(highlight_span(&d, 4), None, "범위 아래도 반전하지 않는다");
    }

    /// 한 줄 안에서 고르면 그 줄의 그 구간만 반전된다 — 줄 통째가 아니다.
    #[test]
    fn a_single_line_selection_highlights_only_that_span() {
        let d = Drag { from: (2, 4), to: (2, 9) };
        assert_eq!(highlight_span(&d, 2), Some((4, 9)));
    }

    #[test]
    fn selecting_nothing_gives_an_empty_string() {
        assert_eq!(extract(&[], &Drag::new((0, 0))), "");
    }
}
