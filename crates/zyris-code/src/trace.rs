//! What the terminal actually sent, for the questions this app cannot answer from inside.
//!
//! **`ZYRIS_CODE_TRACE=keys,term,mouse`.** A key that is swallowed, a click that does nothing, and
//! a console that behaves unlike the one this was written against are all one problem: the app
//! cannot see its own input. crossterm reports what it made of the bytes, and nothing upstream of
//! it knows what the bytes were.
//!
//! So the same binary is run twice — once in each terminal — and the two traces are put side by
//! side. **Whatever differs is the terminal; whatever does not differ is this app.** That is the
//! whole point of the switch, and it is why there is no guess here about which emulator does what:
//! the guess is what has been wrong every time this has come up.
//!
//! **Nothing goes on the screen.** The app owns every cell of it, and a line drawn into the
//! transcript would also corrupt the diff the trace exists to produce. Lines go to stderr and to
//! `$TMPDIR/zyris-code-trace.log`.
//!
//! ```text
//! ZYRIS_CODE_TRACE=term,zutty  zyris-code     # an unknown name changes nothing
//! ZYRIS_CODE_TRACE=keys,mouse  zyris-code     # press a few keys, click a tool row, quit
//! ```

use std::io::Write;
use std::path::PathBuf;

/// The three parts, asked for by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    /// One line at startup: what this terminal says it is, and what the app made of that.
    Term,
    /// Every key event, as the event source hands it over.
    Keys,
    /// Every mouse event, and what the hit-test under it makes of the screen.
    Mouse,
}

impl What {
    /// The name this part answers to in `ZYRIS_CODE_TRACE`.
    fn name(self) -> &'static str {
        match self {
            What::Term => "term",
            What::Keys => "keys",
            What::Mouse => "mouse",
        }
    }
}

/// Which parts were asked for, and where the file goes.
///
/// **Off unless the environment says otherwise**, which is the only reason it is safe to put a
/// `note` call on the typing path: when it is off, that call reads one `bool` and returns.
#[derive(Debug, Clone, Default)]
pub struct Trace {
    term: bool,
    keys: bool,
    mouse: bool,
    /// `None` when nothing was asked for.
    path: Option<PathBuf>,
}

impl Trace {
    /// Reads `ZYRIS_CODE_TRACE` once, at startup.
    pub fn detect() -> Trace {
        Trace::from(std::env::var("ZYRIS_CODE_TRACE").ok().as_deref(), std::env::temp_dir())
    }

    /// The whole decision, over a value and a directory a test can hand in.
    ///
    /// A name we do not know is ignored, and it does not turn the others off — a typo in one place
    /// should not silently take away the trace somebody is trying to read.
    pub fn from(value: Option<&str>, dir: PathBuf) -> Trace {
        let named = |what: What| {
            value.is_some_and(|v| {
                v.split(',').any(|part| part.trim().eq_ignore_ascii_case(what.name()))
            })
        };
        let (term, keys, mouse) = (named(What::Term), named(What::Keys), named(What::Mouse));
        Trace {
            term,
            keys,
            mouse,
            path: (term || keys || mouse).then(|| dir.join("zyris-code-trace.log")),
        }
    }

    pub fn wants(&self, what: What) -> bool {
        match what {
            What::Term => self.term,
            What::Keys => self.keys,
            What::Mouse => self.mouse,
        }
    }

    /// Writes one line, if this part was asked for.
    ///
    /// **Never fails loudly and never unwraps.** A trace that breaks the thing it is measuring is
    /// worse than no trace, and stderr can be a closed pipe as easily as anything else.
    pub fn note(&self, what: What, line: &str) {
        if !self.wants(what) {
            return;
        }
        eprintln!("zyris-code[{}] {line}", what.name());
        let Some(path) = &self.path else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "[{}] {line}", what.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(value: &str) -> Trace {
        Trace::from(Some(value), PathBuf::from("/tmp"))
    }

    /// The names it answers to, one or several.
    #[test]
    fn the_parts_are_asked_for_by_name() {
        assert!(trace("keys").wants(What::Keys));
        assert!(!trace("keys").wants(What::Mouse));
        let both = trace("keys,mouse");
        assert!(both.wants(What::Keys) && both.wants(What::Mouse));
        assert!(!both.wants(What::Term));
    }

    /// Spaces and case are what a person types, not what a parser should demand.
    #[test]
    fn the_names_are_read_as_typed() {
        assert!(trace(" TERM , Keys ").wants(What::Term));
        assert!(trace("MOUSE").wants(What::Mouse));
    }

    /// **A name we do not know changes nothing.** A typo must not take the rest of the trace away,
    /// and it must not turn anything on either.
    #[test]
    fn an_unknown_name_is_ignored() {
        let t = trace("keys,zutty");
        assert!(t.wants(What::Keys), "the unknown name took the known one away");
        assert!(!t.wants(What::Mouse));
        assert!(trace("zutty").path.is_none(), "something was turned on by a name we do not know");
    }

    /// Nothing asked for — the ordinary case — means no file is even named.
    #[test]
    fn nothing_asked_for_is_off() {
        for value in ["", " ", ",,"] {
            let t = trace(value);
            assert!(t.path.is_none(), "{value:?} turned the trace on");
            assert!(!t.wants(What::Term) && !t.wants(What::Keys) && !t.wants(What::Mouse));
        }
        assert!(Trace::from(None, PathBuf::from("/tmp")).path.is_none());
    }

    /// The file goes to the temporary directory, and it is named what the docs say.
    #[test]
    fn the_file_lands_where_the_docs_say() {
        let t = trace("mouse");
        assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/tmp/zyris-code-trace.log")));
    }

    /// **A part that was not asked for writes nothing.** This is what makes the guard on the
    /// typing path worth having.
    #[test]
    fn a_part_that_was_not_asked_for_stays_quiet() {
        let t = trace("keys");
        // No assertion on stderr — the point is that this returns without touching it or a file
        // this test would then have to clean up.
        t.note(What::Mouse, "not written");
    }
}
