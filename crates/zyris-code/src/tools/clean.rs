//! **The terminal's own noise, taken back out of an `exec` result before an agent reads it.**
//!
//! `terminal.exec` is the one tool whose output nothing else bounds: whatever the command wrote
//! comes back whole, and every byte of it rides in the agent's context. A `cargo build` answers
//! with the same progress bar a hundred times over; a coloured tool pays for its colour again once
//! it is JSON-escaped. What comes off here is what a terminal drew and then erased — nothing an
//! agent could have read.
//!
//! **Nothing a terminal would have shown is touched.** `exec` is how the agent reads files and
//! diffs (`cat`, `sed -n`, `git diff`), so trailing spaces and runs of blank lines are content:
//! trimming them turned a whitespace-only diff line `+    ` into `+`, and a file read with its
//! blank lines folded no longer matches what `code_edit` is then asked to find in it.
//!
//! **Quiet is asked for first, and this is what catches whoever colours anyway.** `Gate::dispatch`
//! writes `NO_COLOR` and `CARGO_TERM_COLOR` into an `exec` call before it runs
//! (`guard::quiet_env`), and only then does this run over the answer.
//!
//! `zyris-terminal`'s own stripper (`sanitize.rs`) is deliberately **not** reused. It drops a bare
//! `\r` along with every other control character, which is right for a screen somebody re-renders
//! and wrong for text read once: the `\r` is exactly what says a progress line was overwritten, and
//! losing it leaves every redraw behind.

/// A string as the terminal would have ended up showing it.
///
/// - ANSI escape sequences — CSI, OSC and the rest — are removed.
/// - A line keeps only what follows its last bare `\r`, the way an overwriting progress bar leaves
///   it. `\r\n` is a line ending, not an overwrite.
///
/// **Text with nothing to clean comes back byte for byte.** A step that rewrote output merely for
/// having read it would be a worse fault than the noise it removes.
pub fn clean(text: &str) -> String {
    resolve_returns(&strip_escapes(text))
}

/// `text` with every escape sequence removed.
///
/// A sequence the output ends in the middle of goes too: the inside of one is not text, and the
/// half that survived carries nothing a reader can use.
fn strip_escapes(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1B {
            i = escape_end(bytes, text, i).unwrap_or(bytes.len());
            continue;
        }
        // A multi-byte character crosses whole or not at all — a `String` will not take a byte of
        // one — and from here the index only ever moves by whole characters.
        let c = text[i..].chars().next().expect("the index is on a character boundary");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// The index just past the escape sequence that starts at `at` (which must be ESC). `None` when the
/// output ran out inside it.
///
/// The shapes are the ones a terminal has: CSI (`ESC [`, ending on a final byte), OSC and the
/// strings like it (`ESC ]`, ending on BEL or `ESC \`), and everything else, which is the ESC and
/// the one character after it.
///
/// **`at + 1` is not a character boundary to guess at.** What follows the ESC is usually an ASCII
/// byte, but the two-byte form's second byte is whatever character came next — take it whole, or
/// the caller is handed an index inside a multi-byte character and slices on it.
fn escape_end(bytes: &[u8], text: &str, at: usize) -> Option<usize> {
    let mut i = at + 1;
    match *bytes.get(i)? {
        b'[' => {
            i += 1;
            loop {
                let c = *bytes.get(i)?;
                i += 1;
                if (0x40..=0x7E).contains(&c) {
                    return Some(i);
                }
            }
        }
        b']' | b'P' | b'X' | b'^' | b'_' => {
            i += 1;
            loop {
                let c = *bytes.get(i)?;
                if c == 0x07 {
                    return Some(i + 1);
                }
                if c == 0x1B {
                    return match bytes.get(i + 1) {
                        Some(b'\\') => Some(i + 2),
                        // Another sequence has begun. Stop before it and let the loop above read
                        // it, rather than swallowing its first byte.
                        Some(_) => Some(i),
                        None => None,
                    };
                }
                i += 1;
            }
        }
        _ => {
            let next = text.get(at + 1..)?.chars().next()?;
            Some(at + 1 + next.len_utf8())
        }
    }
}

/// `text` as the terminal would have ended up showing it.
///
/// `\r\n` is one line ending and becomes `\n`. A `\r` on its own sends the cursor back to the start
/// of the line, and **only what is written after it** replaces what was there: a line that ends on
/// a `\r` (`done\r`) still shows `done`.
///
/// ponytail: a rewrite drops the whole old line, where a terminal overwrites it character by
/// character (`100%\r50%` shows `50%%`). Progress bars pad or clear the line themselves, so this is
/// what they leave; track columns if a tool turns up that relies on the partial overwrite.
fn resolve_returns(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut line = String::new();
    let mut returned = false;
    for c in text.chars() {
        match c {
            '\r' => returned = true,
            '\n' => {
                out.push_str(&line);
                out.push('\n');
                line.clear();
                returned = false;
            }
            _ => {
                if returned {
                    line.clear();
                    returned = false;
                }
                line.push(c);
            }
        }
    }
    out.push_str(&line);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Ordinary text is not touched.** A cleaning step that rewrote output merely for having read
    /// it would be a worse fault than the noise it removes, and every test below leans on this.
    #[test]
    fn ordinary_text_comes_through_byte_for_byte() {
        let text = "running 214 tests\n\ntest result: ok. 214 passed; 0 failed\n";
        assert_eq!(clean(text), text);
        assert_eq!(clean("가나다 🎯\t한글\n"), "가나다 🎯\t한글\n");
        assert_eq!(clean(""), "");
    }

    /// ANSI and OSC — what makes a `cargo` or `git` line cost several tokens a word.
    #[test]
    fn escape_sequences_are_removed() {
        assert_eq!(clean("\u{1b}[0;31mred\u{1b}[0m"), "red");
        // The space a prompt left before the mode-off sequence is text, and stays.
        assert_eq!(clean("\u{1b}[?2004hsh-5.3$ \u{1b}[?2004l"), "sh-5.3$ ");
        assert_eq!(clean("\u{1b}]0;title\u{7}body"), "body");
        assert_eq!(clean("\u{1b}]0;title\u{1b}\\body"), "body");
        // A sequence the output was cut off inside is not text either.
        assert_eq!(clean("abc\u{1b}[0"), "abc");
        assert_eq!(clean("abc\u{1b}"), "abc");
        assert!(!clean("\u{1b}[1m\u{1b}[0m").contains('\u{1b}'));
    }

    /// **A progress bar is one line, not a hundred.** A bare `\r` is the terminal's overwrite; a
    /// `\r\n` is the line ending Windows writes.
    #[test]
    fn a_carriage_return_keeps_only_what_is_left_on_the_line() {
        assert_eq!(clean("50%\r100%"), "100%");
        assert_eq!(clean("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(clean("Compiling x\rCompiling y\rCompiling z\n"), "Compiling z\n");
        // A `\r` with nothing written after it moves the cursor and erases nothing.
        assert_eq!(clean("done\r"), "done");
        assert_eq!(clean("done\r\r\nnext"), "done\nnext");
        // The colour comes off before the overwrite is read, not after.
        assert_eq!(clean("50%\r\u{1b}[32m100%\u{1b}[0m"), "100%");
    }

    /// **Whitespace is content.** A whitespace-only diff line and a file's own blank lines reach the
    /// agent as they are, or what it reads stops matching what `code_edit` finds.
    #[test]
    fn whitespace_and_blank_lines_are_left_alone() {
        let diff = "+    \n-\tx\t\n";
        assert_eq!(clean(diff), diff);
        let blanks = "a\n\n\n\n\nb  \n";
        assert_eq!(clean(blanks), blanks);
    }

    /// **A multi-byte character beside an escape is neither split nor mangled.** The scanner works
    /// in bytes, so every step out of it has to land on a character boundary — the ESC and the
    /// character after it included.
    #[test]
    fn a_multibyte_character_beside_an_escape_survives() {
        assert_eq!(clean("\u{1b}[1m가나다\u{1b}[0m 🎯"), "가나다 🎯");
        // An ESC immediately before a multi-byte character: the two-byte form takes the whole
        // character, and the slice that resumes after it is on a boundary.
        assert_eq!(clean("\u{1b}é"), "");
        assert_eq!(clean("가\u{1b}é나"), "가나");
    }
}
