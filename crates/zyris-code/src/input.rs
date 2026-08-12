//! The input field's editing state. Fixed at the bottom of the screen; its height grows with the content.
//!
//! **The cursor is a character (char) index.** Handled as a byte index, it would leave character
//! boundaries in Korean and panic.

use crate::markdown::display_width;

#[derive(Debug, Default, Clone)]
pub struct Input {
    pub text: String,
    /// Position in characters.
    pub cursor: usize,
    /// What the last kill took out, for `yank` to put back (`Ctrl+Y`).
    ///
    /// **This is the only way back from a kill.** A shell has an undo (`Ctrl+_`); this input does
    /// not, so without somewhere to put the text `Ctrl+U` on a long draft loses it outright. One
    /// slot, not readline's ring — the ring needs `Alt+Y` to walk it, and a second key for a case
    /// that hardly arises here.
    killed: String,
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

    /// Number of characters before the cursor.
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

    /// Deletes the character under the cursor (`Delete`). At the end, does nothing.
    pub fn delete(&mut self) {
        if self.cursor >= self.len_chars() {
            return;
        }
        let from = self.byte_at(self.cursor);
        let to = self.byte_at(self.cursor + 1);
        self.text.replace_range(from..to, "");
    }

    /// Deletes the previous word (`Ctrl+W`). Like readline, it's a **delete**.
    ///
    /// **Whitespace is the only boundary**, which is what `bash` and `zsh` both do here
    /// (`unix-word-rubout`): one press takes a whole `src/widgets/picker.rs`. The narrower,
    /// punctuation-aware word is [`Input::delete_word_before`] on `Alt+Backspace`, so both
    /// granularities are reachable — that is why readline has the two keys.
    pub fn delete_word(&mut self) {
        // First swallow the whitespace right before the cursor, then the run of non-whitespace.
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.kill(i, self.cursor);
    }

    /// Everything from the start up to the cursor (`Ctrl+U`).
    ///
    /// **This is `bash`'s `unix-line-discard`, not `zsh`'s `kill-whole-line`.** The two shells
    /// really do differ, and they coincide exactly where the cursor usually is — at the end — so
    /// the everyday press still wipes the draft. It is the other case that decides it: mid-draft,
    /// `zsh` also throws away everything *ahead* of the cursor, which was never asked for.
    /// Paired with [`Input::kill_to_end`] it also spells the whole line, in two presses that each
    /// say what they do.
    ///
    /// **The start is the start of the whole draft, not of the visual line**, matching `home`.
    /// A draft here can hold newlines (`Alt+Enter`), and having `Ctrl+A` and `Ctrl+U` disagree
    /// about where a line begins would be worse than either answer.
    pub fn kill_to_start(&mut self) {
        self.kill(0, self.cursor);
    }

    /// Everything from the cursor to the end (`Ctrl+K`).
    pub fn kill_to_end(&mut self) {
        self.kill(self.cursor, self.len_chars());
    }

    /// The word before the cursor, stopping at punctuation (`Alt+Backspace`).
    ///
    /// Where `Ctrl+W` eats `src/widgets/picker.rs` whole, this takes it back one segment at a
    /// time. Both are worth having, and readline binds both for that reason.
    pub fn delete_word_before(&mut self) {
        self.kill(self.word_start(), self.cursor);
    }

    /// The word after the cursor, stopping at punctuation (`Alt+D`).
    pub fn delete_word_after(&mut self) {
        self.kill(self.cursor, self.word_end());
    }

    /// Moves back one word (`Alt+B`).
    pub fn word_left(&mut self) {
        self.cursor = self.word_start();
    }

    /// Moves forward one word (`Alt+F`).
    pub fn word_right(&mut self) {
        self.cursor = self.word_end();
    }

    /// Puts back what the last kill took (`Ctrl+Y`).
    pub fn yank(&mut self) {
        if !self.killed.is_empty() {
            let put = std::mem::take(&mut self.killed);
            self.insert_str(&put);
            self.killed = put;
        }
    }

    /// Cuts `[from, to)` out and remembers it for [`Input::yank`].
    ///
    /// **An empty cut leaves the remembered text alone.** `Ctrl+U` at the start of the draft, or
    /// `Ctrl+K` at the end, takes nothing — and if that overwrote the slot it would quietly throw
    /// away the text the person is about to bring back, which is the one thing this slot exists
    /// to prevent.
    fn kill(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }
        let (a, b) = (self.byte_at(from), self.byte_at(to));
        self.killed = self.text[a..b].to_string();
        self.text.replace_range(a..b, "");
        self.cursor = from;
    }

    /// Where the word before the cursor begins, by readline's reckoning: skip whatever is not
    /// alphanumeric, then the run that is.
    fn word_start(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor;
        while i > 0 && !chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        while i > 0 && chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        i
    }

    /// Where the word after the cursor ends. The mirror of [`Input::word_start`].
    fn word_end(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let n = chars.len();
        let mut i = self.cursor;
        while i < n && !chars[i].is_alphanumeric() {
            i += 1;
        }
        while i < n && chars[i].is_alphanumeric() {
            i += 1;
        }
        i
    }

    /// Paste. Even multi-line text goes in as-is.
    pub fn insert_str(&mut self, s: &str) {
        let byte = self.byte_at(self.cursor);
        self.text.insert_str(byte, s);
        self.cursor += s.chars().count();
    }

    /// Columns taken by the text left of the cursor. Used to draw the cursor **in place by wide-character width**.
    ///
    /// Drawn by character count, the cursor shifts left in front of Korean.
    pub fn cursor_col(&self) -> usize {
        display_width(&self.text.chars().take(self.cursor).collect::<String>())
    }

    /// Rows the input occupies at this width. At least one.
    pub fn height(&self, width: u16) -> u16 {
        self.wrapped(width).0.len() as u16
    }

    /// The wrapped result at this width and **the (row, column) where the cursor sits**.
    ///
    /// If the wrapping and the cursor placement disagree, the cursor lands in the wrong spot on long
    /// text. So both come back together — `height` uses this too.
    ///
    /// **Wraps per character.** Wrapping per word would let one long URL push an entire line out, and
    /// cursor placement math would get far trickier. It's also how terminals do it.
    /// The width passed in is the **inner width**, minus the prompt (`"> "`).
    pub fn wrapped(&self, width: u16) -> (Vec<String>, (u16, u16)) {
        let limit = width.max(1) as usize;
        let mut lines = vec![String::new()];
        let (mut row, mut col) = (0u16, 0usize);
        let mut at = (0u16, 0u16);

        for (i, ch) in self.text.chars().enumerate() {
            // Pasting can bring in newlines. Break the line right there.
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
            lines.last_mut().expect("there is always at least one line").push(ch);
            col += w;
        }
        // If the cursor is at the very end, it's the end of the last line.
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

    /// The cursor is in characters (char). Counted in byte indices, it panics on Korean.
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

    /// A long input grows the field taller and shrinks the conversation area by the same amount.
    #[test]
    fn a_long_input_grows_taller() {
        let mut i = Input::new();
        for c in "가나다라마바사아자차".chars() {
            i.insert(c);
        }
        assert!(
            i.height(10) >= 2,
            "ten wide glyphs (20 columns) in a width of 10 take more than one line"
        );
    }

    #[test]
    fn an_empty_input_is_one_line_tall() {
        assert_eq!(Input::new().height(40), 1);
    }

    /// **A long input wraps onto the next line.** Cut off, you can't tell what you're typing.
    #[test]
    fn a_long_input_wraps_onto_the_next_line() {
        let mut i = Input::new();
        for c in "abcdefghij".chars() {
            i.insert(c);
        }
        let (lines, at) = i.wrapped(4);
        assert_eq!(lines, vec!["abcd", "efgh", "ij"]);
        assert_eq!(at, (2, 2), "the cursor is at the end of the last line");
    }

    /// If the wrap point and the cursor spot disagree, the cursor stands in the wrong place on long text.
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
        // Five characters in, at width 4 that's the second column of the second line.
        assert_eq!(i.wrapped(4).1, (1, 1));
    }

    /// A wide character takes two columns, so only half fits inside the width. Wrapping by character count overflows on the right.
    #[test]
    fn wide_characters_wrap_by_columns_not_by_character_count() {
        let mut i = Input::new();
        for c in "가나다라".chars() {
            i.insert(c);
        }
        assert_eq!(i.wrapped(4).0, vec!["가나", "다라"]);
    }

    /// A newline that came in through paste breaks the line right there.
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

    /// You must be able to fix a character in the middle. Without this, fixing a typo means deleting everything after it.
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

    /// Ctrl+W is a **delete**, per the readline standard.
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

    /// The cursor column counts wide characters as two. Drawn by character count, it shifts left in front of Korean.
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

#[cfg(test)]
mod editing {
    use super::Input;

    fn at(text: &str, cursor: usize) -> Input {
        Input { text: text.into(), cursor, killed: String::new() }
    }

    /// **`Ctrl+U` keeps what is ahead of the cursor.** This is where `bash` and `zsh` part ways,
    /// and the everyday press — cursor at the end — is the case where they agree, so the change
    /// is invisible until the moment it matters.
    #[test]
    fn clearing_the_line_leaves_what_is_ahead_of_the_cursor() {
        let mut i = at("git commit -m \"wip\"", 11);
        i.kill_to_start();
        assert_eq!(i.text, "-m \"wip\"");
        assert_eq!(i.cursor, 0);

        // At the end, which is where it is nearly always pressed, the whole draft goes.
        let mut i = at("a long draft", 12);
        i.kill_to_start();
        assert_eq!(i.text, "");
    }

    /// The two halves spell the whole line between them, and each says what it does.
    #[test]
    fn killing_forward_and_backward_together_take_the_whole_line() {
        let mut i = at("keep this cut that", 10);
        i.kill_to_end();
        assert_eq!(i.text, "keep this ");
        i.kill_to_start();
        assert_eq!(i.text, "");
    }

    /// **Nothing is recoverable unless the kill kept it.** There is no undo for this input, so a
    /// kill that dropped the text on the floor would be the one unrecoverable key in the app.
    #[test]
    fn what_a_kill_took_can_be_put_back() {
        let mut i = at("throw this away", 15);
        i.kill_to_start();
        assert_eq!(i.text, "");
        i.yank();
        assert_eq!(i.text, "throw this away");
        assert_eq!(i.cursor, 15);
        // And again — putting it back does not consume it.
        i.yank();
        assert_eq!(i.text, "throw this awaythrow this away");
    }

    /// **A kill that took nothing must not empty the slot.** `Ctrl+K` at the end of the draft is
    /// an easy accident, and if it cleared what was held, the `Ctrl+Y` that follows would restore
    /// nothing — losing the text at exactly the moment the person reached for it.
    #[test]
    fn a_kill_that_took_nothing_does_not_forget_what_it_held() {
        let mut i = at("precious", 8);
        i.kill_to_start();
        i.kill_to_end(); // empty draft: takes nothing
        i.yank();
        assert_eq!(i.text, "precious");
    }

    /// Two granularities, on purpose. `Ctrl+W` swallows a path; `Alt+Backspace` walks it back a
    /// segment at a time. Having only the wide one makes fixing a typo mid-path a retype.
    #[test]
    fn a_word_means_two_different_things_and_both_are_reachable() {
        let mut wide = at("open src/widgets/picker.rs", 26);
        wide.delete_word();
        assert_eq!(wide.text, "open ");

        let mut narrow = at("open src/widgets/picker.rs", 26);
        narrow.delete_word_before();
        assert_eq!(narrow.text, "open src/widgets/picker.");
    }

    #[test]
    fn moving_and_deleting_by_word_agree_on_where_a_word_is() {
        let mut i = at("alpha beta gamma", 16);
        i.word_left();
        assert_eq!(i.cursor, 11, "left of `gamma`");
        i.word_right();
        assert_eq!(i.cursor, 16);

        let mut d = at("alpha beta gamma", 11);
        d.delete_word_after();
        assert_eq!(d.text, "alpha beta ");
    }

    /// Korean is two columns wide and multiple bytes per character; the cursor is a **char**
    /// index, so every one of these has to slice on character boundaries or it panics.
    #[test]
    fn killing_and_yanking_hold_up_across_wide_characters() {
        let mut i = at("안녕하세요 world", 11);
        i.delete_word();
        assert_eq!(i.text, "안녕하세요 ");
        i.yank();
        assert_eq!(i.text, "안녕하세요 world");

        let mut k = at("안녕 세계", 2);
        k.kill_to_end();
        assert_eq!(k.text, "안녕");
        k.kill_to_start();
        assert_eq!(k.text, "");
    }
}
