//! 입력란의 편집 상태. 화면 맨 아래에 고정되고 내용에 따라 높이가 자란다.
//!
//! **커서는 문자(char) 인덱스다.** 바이트 인덱스로 다루면 한글에서 문자 경계를
//! 벗어나 패닉한다.

use crate::markdown::display_width;

#[derive(Debug, Default, Clone)]
pub struct Input {
    pub text: String,
    /// 문자 단위 위치.
    pub cursor: usize,
}

impl Input {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, c: char) {
        let byte = self.byte_at(self.cursor);
        self.text.insert(byte, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let from = self.byte_at(self.cursor - 1);
        let to = self.byte_at(self.cursor);
        self.text.replace_range(from..to, "");
        self.cursor -= 1;
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// 커서 앞의 글자 수.
    pub fn len_chars(&self) -> usize {
        self.text.chars().count()
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len_chars());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.len_chars();
    }

    /// 커서 자리의 글자를 지운다(`Delete`). 끝에서는 아무 일도 없다.
    pub fn delete(&mut self) {
        if self.cursor >= self.len_chars() {
            return;
        }
        let from = self.byte_at(self.cursor);
        let to = self.byte_at(self.cursor + 1);
        self.text.replace_range(from..to, "");
    }

    /// 앞 단어를 지운다(`Ctrl+W`). readline과 같이 **삭제**다.
    pub fn delete_word(&mut self) {
        // 커서 바로 앞의 공백을 먼저 먹고, 그다음 공백이 아닌 덩어리를 먹는다.
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        let from = self.byte_at(i);
        let to = self.byte_at(self.cursor);
        self.text.replace_range(from..to, "");
        self.cursor = i;
    }

    /// 붙여넣기. 여러 줄이 와도 그대로 넣는다.
    pub fn insert_str(&mut self, s: &str) {
        let byte = self.byte_at(self.cursor);
        self.text.insert_str(byte, s);
        self.cursor += s.chars().count();
    }

    /// 커서 왼쪽 텍스트가 차지하는 칸 수. 커서를 **전각 기준 제자리**에 그리기 위한 값이다.
    ///
    /// 글자 수로 그리면 한글 앞에서 커서가 왼쪽으로 밀린다.
    pub fn cursor_col(&self) -> usize {
        display_width(&self.text.chars().take(self.cursor).collect::<String>())
    }

    /// 이 폭에서 입력란이 차지하는 줄 수. 최소 한 줄이다.
    pub fn height(&self, width: u16) -> u16 {
        self.wrapped(width).0.len() as u16
    }

    /// 이 폭에서 줄로 접은 결과와 **커서가 앉을 (줄, 칸)**.
    ///
    /// 접는 쪽과 커서를 놓는 쪽이 갈라지면 긴 글에서 커서가 엉뚱한 자리에 선다. 그래서
    /// 한 번에 같이 돌려준다 — `height`도 이걸 쓴다.
    ///
    /// **낱글자 단위로 접는다.** 단어 단위로 접으면 긴 URL 하나가 줄을 통째로 밀어내고,
    /// 무엇보다 커서 자리 계산이 훨씬 까다로워진다. 터미널이 하는 방식과도 같다.
    /// 넘겨받는 폭은 프롬프트(`"> "`)를 뺀 **안쪽 폭**이다.
    pub fn wrapped(&self, width: u16) -> (Vec<String>, (u16, u16)) {
        let limit = width.max(1) as usize;
        let mut lines = vec![String::new()];
        let (mut row, mut col) = (0u16, 0usize);
        let mut at = (0u16, 0u16);

        for (i, ch) in self.text.chars().enumerate() {
            // 붙여넣기로 줄바꿈이 들어올 수 있다. 그 자리는 그대로 끊는다.
            if ch == '\n' {
                if i == self.cursor {
                    at = (row, col as u16);
                }
                lines.push(String::new());
                row += 1;
                col = 0;
                continue;
            }
            let w = display_width(&ch.to_string()).max(1);
            if col + w > limit {
                lines.push(String::new());
                row += 1;
                col = 0;
            }
            if i == self.cursor {
                at = (row, col as u16);
            }
            lines.last_mut().expect("줄은 하나 이상이다").push(ch);
            col += w;
        }
        // 커서가 맨 끝이면 마지막 줄의 끝이다.
        if self.cursor >= self.text.chars().count() {
            at = (row, col as u16);
        }
        (lines, at)
    }

    fn byte_at(&self, char_index: usize) -> usize {
        self.text.char_indices().nth(char_index).map(|(b, _)| b).unwrap_or(self.text.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 커서는 문자(char) 단위다. 바이트 인덱스로 세면 한글에서 패닉한다.
    #[test]
    fn backspace_removes_one_korean_character_not_one_byte() {
        let mut i = Input::new();
        for c in "한글".chars() {
            i.insert(c);
        }
        i.backspace();
        assert_eq!(i.text, "한");
        assert_eq!(i.cursor, 1);
    }

    #[test]
    fn backspace_on_an_empty_input_does_nothing() {
        let mut i = Input::new();
        i.backspace();
        assert_eq!(i.text, "");
        assert_eq!(i.cursor, 0);
    }

    #[test]
    fn taking_the_text_clears_the_input() {
        let mut i = Input::new();
        for c in "안녕".chars() {
            i.insert(c);
        }
        assert_eq!(i.take(), "안녕");
        assert_eq!(i.text, "");
        assert_eq!(i.cursor, 0);
    }

    /// 입력이 길면 입력란이 자라고 대화 영역이 그만큼 줄어든다.
    #[test]
    fn a_long_input_grows_taller() {
        let mut i = Input::new();
        for c in "가나다라마바사아자차".chars() {
            i.insert(c);
        }
        assert!(i.height(10) >= 2, "폭 10에 전각 10글자(20칸)면 두 줄 이상이다");
    }

    #[test]
    fn an_empty_input_is_one_line_tall() {
        assert_eq!(Input::new().height(40), 1);
    }

    /// **긴 입력은 다음 줄로 내려간다.** 잘려 나가면 무엇을 치고 있는지 알 수 없다.
    #[test]
    fn a_long_input_wraps_onto_the_next_line() {
        let mut i = Input::new();
        for c in "abcdefghij".chars() {
            i.insert(c);
        }
        let (lines, at) = i.wrapped(4);
        assert_eq!(lines, vec!["abcd", "efgh", "ij"]);
        assert_eq!(at, (2, 2), "커서는 마지막 줄 끝이다");
    }

    /// 접히는 자리와 커서 자리가 갈라지면 긴 글에서 커서가 엉뚱한 곳에 선다.
    #[test]
    fn the_cursor_follows_the_line_it_wrapped_onto() {
        let mut i = Input::new();
        for c in "abcdefgh".chars() {
            i.insert(c);
        }
        i.home();
        for _ in 0..5 {
            i.right();
        }
        // 다섯 글자 앞이면 폭 4에서는 둘째 줄의 둘째 칸이다.
        assert_eq!(i.wrapped(4).1, (1, 1));
    }

    /// 전각은 두 칸이라 폭 안에 반만 들어간다. 글자 수로 접으면 오른쪽이 넘친다.
    #[test]
    fn wide_characters_wrap_by_columns_not_by_character_count() {
        let mut i = Input::new();
        for c in "가나다라".chars() {
            i.insert(c);
        }
        assert_eq!(i.wrapped(4).0, vec!["가나", "다라"]);
    }

    /// 붙여넣기로 들어온 줄바꿈은 그 자리에서 끊긴다.
    #[test]
    fn a_pasted_newline_breaks_the_line_there() {
        let mut i = Input::new();
        i.insert_str("한 줄\n두 줄");
        assert_eq!(i.wrapped(40).0, vec!["한 줄", "두 줄"]);
        assert_eq!(i.height(40), 2);
    }

    fn typed(s: &str) -> Input {
        let mut i = Input::new();
        for c in s.chars() {
            i.insert(c);
        }
        i
    }

    /// 중간 글자를 고칠 수 있어야 한다. 이게 없으면 오타를 지우려고 뒤를 다 지워야 한다.
    #[test]
    fn text_can_be_edited_in_the_middle() {
        let mut i = typed("안녕하세요");
        i.left();
        i.left();
        i.backspace();
        assert_eq!(i.text, "안녕세요");
        assert_eq!(i.cursor, 2);
    }

    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut i = typed("가나");
        i.left();
        i.left();
        i.left();
        assert_eq!(i.cursor, 0);
        i.end();
        i.right();
        assert_eq!(i.cursor, 2);
    }

    #[test]
    fn delete_removes_the_character_under_the_cursor() {
        let mut i = typed("한글");
        i.home();
        i.delete();
        assert_eq!(i.text, "글");
        assert_eq!(i.cursor, 0);
    }

    #[test]
    fn delete_at_the_end_does_nothing() {
        let mut i = typed("한");
        i.delete();
        assert_eq!(i.text, "한");
    }

    /// Ctrl+W는 readline 표준대로 **삭제**다.
    #[test]
    fn ctrl_w_deletes_the_previous_word() {
        let mut i = typed("hello world");
        i.delete_word();
        assert_eq!(i.text, "hello ");
        i.delete_word();
        assert_eq!(i.text, "");
    }

    #[test]
    fn pasting_inserts_at_the_cursor() {
        let mut i = typed("가다");
        i.left();
        i.insert_str("나");
        assert_eq!(i.text, "가나다");
        assert_eq!(i.cursor, 2);
    }

    /// 커서 열은 전각을 2칸으로 센다. 글자 수로 그리면 한글 앞에서 왼쪽으로 밀린다.
    #[test]
    fn the_cursor_column_counts_wide_characters_as_two() {
        let mut i = typed("한a");
        assert_eq!(i.cursor_col(), 3);
        i.home();
        assert_eq!(i.cursor_col(), 0);
        i.right();
        assert_eq!(i.cursor_col(), 2);
    }
}
