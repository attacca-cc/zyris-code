//! 뷰포트. 줄 단위로 움직인다.
//!
//! "부드럽게"는 픽셀 단위 스크롤이 아니라 **지연 없음**이다 — 노치가 온 만큼 즉시
//! 반영하고, 그리기는 프레임 단위로 합쳐 한 번만 한다.

pub const LINES_PER_NOTCH: usize = 3;

#[derive(Debug, Clone, Copy)]
pub struct Scroll {
    /// 화면 맨 위에 보이는 줄의 인덱스.
    pub top: usize,
    /// 바닥에 붙어 있는가. 붙어 있으면 새 줄이 와도 계속 바닥이다.
    pub stick: bool,
}

impl Default for Scroll {
    fn default() -> Self {
        Self { top: 0, stick: true }
    }
}

impl Scroll {
    pub fn new() -> Self {
        Self::default()
    }

    /// 휠. 양수가 위로 간다.
    pub fn wheel(&mut self, notches: i32, total: usize, height: usize) {
        let max_top = total.saturating_sub(height);
        let delta = notches.unsigned_abs() as usize * LINES_PER_NOTCH;
        self.top = if notches > 0 {
            self.top.saturating_sub(delta)
        } else {
            (self.top + delta).min(max_top)
        };
        // 바닥에 닿으면 고정이 되살아난다.
        self.stick = self.top >= max_top;
    }

    /// 내용이 바뀌었을 때 부른다.
    pub fn on_content(&mut self, total: usize, height: usize) {
        if self.stick {
            self.top = total.saturating_sub(height);
        } else {
            self.top = self.top.min(total.saturating_sub(height));
        }
    }

    /// 지금 그려야 할 줄 범위 `[start, end)`.
    pub fn window(&self, total: usize, height: usize) -> (usize, usize) {
        let start = self.top.min(total.saturating_sub(height));
        (start, (start + height).min(total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_scroll_sticks_to_the_bottom() {
        let mut s = Scroll::new();
        s.on_content(100, 10);
        assert_eq!(s.window(100, 10), (90, 100));
    }

    /// 바닥에 붙어 있으면 새 줄이 와도 계속 바닥이다.
    #[test]
    fn new_content_keeps_a_stuck_view_at_the_bottom() {
        let mut s = Scroll::new();
        s.on_content(100, 10);
        s.on_content(120, 10);
        assert_eq!(s.window(120, 10), (110, 120));
    }

    /// 위로 올려 뒀으면 새 줄이 와도 그 자리를 지킨다 — 읽는 중에 화면이 튀면 안 된다.
    #[test]
    fn new_content_does_not_move_a_scrolled_up_view() {
        let mut s = Scroll::new();
        s.on_content(100, 10);
        s.wheel(3, 100, 10); // 위로 3노치 = 9줄
        let before = s.window(100, 10);
        s.on_content(120, 10);
        assert_eq!(s.window(120, 10), before, "위로 올려 둔 자리를 지켜야 한다");
    }

    #[test]
    fn one_notch_moves_three_lines() {
        let mut s = Scroll::new();
        s.on_content(100, 10);
        s.wheel(1, 100, 10);
        assert_eq!(s.top, 87);
    }

    #[test]
    fn scrolling_past_the_top_stops_at_zero() {
        let mut s = Scroll::new();
        s.on_content(100, 10);
        s.wheel(1000, 100, 10);
        assert_eq!(s.top, 0);
    }

    /// 바닥까지 다시 내려가면 고정이 되살아난다.
    #[test]
    fn scrolling_back_to_the_bottom_restores_stickiness() {
        let mut s = Scroll::new();
        s.on_content(100, 10);
        s.wheel(3, 100, 10);
        assert!(!s.stick);
        s.wheel(-3, 100, 10);
        assert!(s.stick, "바닥에 닿으면 다시 붙어야 한다");
    }

    #[test]
    fn content_shorter_than_the_view_starts_at_zero() {
        let mut s = Scroll::new();
        s.on_content(4, 10);
        assert_eq!(s.window(4, 10), (0, 4));
    }
}
