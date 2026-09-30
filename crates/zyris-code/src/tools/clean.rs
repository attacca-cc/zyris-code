//! **The terminal's own noise, taken back out of tool output before an agent reads it.**
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
//!
//! ## One cleaner, two shapes
//!
//! `terminal.exec` answers with its output whole, so [`clean`] takes it in one call. A background
//! job's output arrives in whatever pieces the pipe hands over (`tools::jobs::drain`), so the same
//! cleaning is done by a [`Stripper`], which holds whatever a chunk broke in half — a character, or
//! **an escape sequence** — until the next chunk brings the rest. `clean` is `Stripper::push` then
//! `Stripper::flush`, one implementation, so the whole-string and streaming paths cannot drift.
//!
//! **A control character other than `\n`, `\t` and `\r` is not text** — a terminal acts on it
//! rather than showing it — so it is dropped.

/// A string as the terminal would have ended up showing it.
///
/// - ANSI escape sequences — CSI, OSC and the rest — are removed. A sequence the output ends in
///   the middle of goes too: the inside of one is not text, and the half that survived carries
///   nothing a reader can use.
/// - A line keeps only what follows its last bare `\r`, the way an overwriting progress bar leaves
///   it. `\r\n` is a line ending, not an overwrite.
/// - A control character other than `\n`, `\t` and `\r` is dropped.
///
/// **Text with nothing to clean comes back byte for byte.** A step that rewrote output merely for
/// having read it would be a worse fault than the noise it removes.
pub fn clean(text: &str) -> String {
    let mut strip = Stripper::default();
    let mut out = strip.push(text.as_bytes());
    out.push_str(&strip.flush());
    out
}

/// How much an unfinished escape may hold back before it is taken for a stray ESC. Real sequences
/// are a few bytes; a terminal title is rarely a hundred.
const HOLD_LIMIT: usize = 4096;

/// The same cleaning, fed a chunk at a time.
///
/// Bytes that cannot be read yet are held rather than guessed at: a multi-byte character cut in
/// the middle, and **an escape sequence cut in the middle** — the two things a chunk boundary
/// lands in. Nothing settled is handed back twice, and no half of either is ever shown.
#[derive(Debug, Default)]
pub struct Stripper {
    /// Bytes not yet readable as characters, or an unfinished escape.
    carry: Vec<u8>,
    /// The line that has not met a `\n` yet. A carriage return makes it overwritable.
    line: String,
    /// A `\r` has been read and nothing has been written over the line since. **It is the next
    /// character written that clears the line, not the `\r` itself** — `done\r\r\n` still
    /// shows `done`.
    returned: bool,
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
        let mut text = String::from_utf8_lossy(&self.carry[..valid]).into_owned();
        self.carry.drain(..valid);

        // An unfinished escape goes back — it is stripped only once joined to the next chunk.
        // **Unless it has run on past any real sequence.** Held for ever, a stray `ESC ]` would
        // keep everything after it from a running job's readers; its ESC is dropped instead, and
        // what followed is shown as the text it evidently is.
        let mut split = split_incomplete_escape(&text).0.len();
        while text.len() - split > HOLD_LIMIT {
            text.remove(split);
            split = split_incomplete_escape(&text).0.len();
        }
        let (ready, held) = text.split_at(split);
        if !held.is_empty() {
            let mut back = held.as_bytes().to_vec();
            back.extend_from_slice(&self.carry);
            self.carry = back;
        }

        let stripped = strip_escapes(ready);
        self.feed(&stripped)
    }

    /// Emits whatever is left once the process has finished.
    ///
    /// **A trailing `\r` does not erase the line.** In a terminal the last progress line stays on
    /// screen too, and that is what the reader last saw. An escape the output was cut off inside is
    /// dropped with the rest of itself.
    pub fn flush(&mut self) -> String {
        let rest = String::from_utf8_lossy(&std::mem::take(&mut self.carry)).into_owned();
        let stripped = strip_escapes(&rest);
        let mut out = self.feed(&stripped);
        // A `\r` the output ended on does not erase the line — it is what the reader last saw.
        self.returned = false;
        out.push_str(&std::mem::take(&mut self.line));
        out
    }

    /// Settles text line by line. **`\r` sends the cursor back to the start of the line, and what
    /// is written after it overwrites what was there** — so the line is dropped when the next
    /// character is written, not when the `\r` is read.
    fn feed(&mut self, text: &str) -> String {
        let mut out = String::new();
        for ch in text.chars() {
            match ch {
                '\n' => {
                    out.push_str(&std::mem::take(&mut self.line));
                    out.push('\n');
                    self.returned = false;
                }
                // A carriage return moves the cursor and erases nothing on its own. `\r\n` is one
                // line ending, and a `\r` followed by another `\r` writes nothing at all.
                '\r' => self.returned = true,
                c if ((c as u32) < 0x20 && c != '\t') || c as u32 == 0x7f => {}
                c => {
                    if std::mem::take(&mut self.returned) {
                        self.line.clear();
                    }
                    self.line.push(c);
                }
            }
        }
        out
    }
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
///
/// `None` is also the streaming answer for \"the sequence is not all here yet\": an ESC with nothing
/// after it, an unterminated CSI, or an OSC whose closing ESC has not arrived. The caller holds from
/// `at` rather than showing half of it.
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
                        // The ESC may be the `\` half of a terminator; wait for the byte after it.
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

/// The prefix that does not end inside an escape sequence, and the rest.
///
/// **A chunk boundary can land in the middle of one.** The half before the boundary is not text, so
/// it must not be shown; it also must not be thrown away before the other half arrives. The first
/// unfinished escape is where the split goes — everything before it is complete.
fn split_incomplete_escape(text: &str) -> (&str, &str) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1B {
            match escape_end(bytes, text, i) {
                Some(end) => i = end,
                None => return text.split_at(i),
            }
        } else {
            i += text[i..].chars().next().expect("the index is on a character boundary").len_utf8();
        }
    }
    (text, "")
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

    /// **A CSI is whatever lies between `ESC [` and its final byte.** SGR written with colons
    /// (`ESC[38:5:196m`) is as much an escape as the semicolon form, and leaving it behind puts
    /// `[38:5:196m` in the agent's context.
    #[test]
    fn a_csi_with_colon_parameters_is_removed() {
        assert_eq!(clean("a\u{1b}[38:5:196mX\u{1b}[0m"), "aX");
    }

    /// **An OSC the output was cut off inside carries no text.** Its content is not shown, and the
    /// unfinished tail goes with it.
    #[test]
    fn an_unterminated_osc_leaves_nothing() {
        assert_eq!(clean("\u{1b}]0;title"), "");
        assert_eq!(clean("a\u{1b}]t\u{1b}"), "a");
    }

    /// **A control character is acted on, not shown.** Everything below `0x20` except the line and
    /// tab characters is dropped, DEL included.
    #[test]
    fn control_characters_other_than_line_and_tab_are_dropped() {
        assert_eq!(clean("a\u{7}b"), "ab");
        assert_eq!(clean("a\u{c}b"), "ab");
        assert_eq!(clean("a\u{7f}b"), "ab");
        assert_eq!(clean("a\u{0}b"), "ab");
        assert_eq!(clean("a\tb\nc"), "a\tb\nc");
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

    /// An escape cut at a chunk boundary has to be joined to the next chunk — **this is why the
    /// streamer exists**, and it is the one thing a whole-string cleaner cannot do.
    #[test]
    fn an_escape_split_across_chunks_is_still_stripped() {
        let mut s = Stripper::default();
        let a = s.push(b"ok\x1b[3");
        let b = s.push(b"2mgreen\n");
        assert_eq!(format!("{a}{b}"), "okgreen\n");
    }

    /// A CSI in pieces with an OSC in pieces, both across the same boundary style.
    #[test]
    fn a_sequence_split_across_several_chunks_is_still_stripped() {
        let mut s = Stripper::default();
        let mut out = String::new();
        for chunk in [&b"x\x1b]"[..], b"0;ti", b"tle\x1b", b"\\y\n"] {
            out.push_str(&s.push(chunk));
        }
        out.push_str(&s.flush());
        assert_eq!(out, "xy\n");
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

    /// **A sequence that never ends does not hold the rest of the output back.** An OSC waits for
    /// its terminator across chunks; one that never gets it would keep a running job's whole
    /// output out of `wait.until` and `wait.logs` until the process exits.
    #[test]
    fn an_escape_that_never_ends_does_not_hold_the_output_back() {
        let mut s = Stripper::default();
        let mut out = s.push(b"\x1b]stray\n");
        for _ in 0..1000 {
            out.push_str(&s.push(b"line of build output\n"));
        }
        assert!(out.contains("line of build output\n"), "held back: {} bytes out", out.len());
        assert!(!out.contains('\u{1b}'));
    }

    /// A trailing `\r` at the very end does not erase the line the reader last saw.
    #[test]
    fn a_trailing_carriage_return_keeps_the_line() {
        let mut s = Stripper::default();
        let mut out = s.push(b"done\r");
        out.push_str(&s.flush());
        assert_eq!(out, "done");
    }

    /// The unfinished half of an escape is never shown, even if the stream simply stops.
    #[test]
    fn an_unfinished_escape_never_reaches_the_reader() {
        let mut s = Stripper::default();
        let mut out = s.push(b"abc\x1b[0;3");
        out.push_str(&s.flush());
        assert_eq!(out, "abc");
        assert!(!out.contains('['));
    }

    /// Tabs and newlines survive. The rest of C0 is removed.
    #[test]
    fn tabs_and_newlines_survive_but_other_controls_do_not() {
        let mut s = Stripper::default();
        assert_eq!(s.push(b"a\tb\nc\x07d"), "a\tb\n");
        assert_eq!(s.flush(), "cd");
    }
}
