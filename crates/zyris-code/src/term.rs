//! What this terminal can actually do.
//!
//! **A feature the terminal lacks has to be found out, not assumed.** Sending an escape sequence
//! blind is only free when the terminal ignores what it does not know — and the ones that matter
//! here do not all do that. An emulator that has never heard of OSC 8 prints the bytes, so a
//! hyperlink becomes a line of rubbish across the transcript rather than a link that merely fails
//! to be clickable.
//!
//! There is no query to ask with. OSC 8 and OSC 52 have no "do you support this?" form, and the
//! one thing that would answer — asking the terminal and waiting — is the pattern that froze this
//! app once already (`Terminal::clear()`'s DSR). So this reads the environment, the way every
//! other tool in this space does.
//!
//! **It is a guess and it will be wrong sometimes.** Both ways are recoverable, which is why the
//! defaults lean the way they do:
//!
//! - Wrongly "unsupported": the link is still Ctrl+clickable, because the app opens URLs itself
//!   (`open_url`) rather than leaving it to the emulator. Nothing is lost but the underline.
//! - Wrongly "supported": escape bytes land on screen, or a copy silently goes nowhere.
//!
//! So an unknown terminal is told no, and `$ZYRIS_CODE_HYPERLINKS` / `$ZYRIS_CODE_OSC52` override
//! the guess in either direction for whoever knows better than we do.

/// Terminals known to render OSC 8 hyperlinks.
///
/// Matched against `TERM_PROGRAM` and `LC_TERMINAL`. **`LC_TERMINAL` matters inside tmux**, which
/// overwrites `TERM_PROGRAM` with its own name but passes `LC_*` through — without it every
/// terminal looks like tmux and loses its links.
const HYPERLINK_TERMINALS: &[&str] =
    &["ghostty", "Hyper", "kitty", "alacritty", "iTerm.app", "iTerm2", "WezTerm", "vscode"];

/// Whether a value names one of them, case-insensitively — `TERM_PROGRAM` is not written to one
/// spelling across emulators (`iTerm.app` against `ghostty`).
fn known(value: Option<&str>, list: &[&str]) -> bool {
    value.is_some_and(|v| list.iter().any(|k| k.eq_ignore_ascii_case(v)))
}

/// How the environment answered, or `None` when it said nothing. `1`/`true`/`yes`/`on` and their
/// opposites, so it reads the way the other switches in this app do.
fn override_of(value: Option<&str>) -> Option<bool> {
    let v = value?.trim().to_ascii_lowercase();
    match v.as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// What the app asks about a terminal. Taken from the environment once at startup — reading it per
/// frame would put a `std::env` lookup inside the draw loop for an answer that cannot change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Caps {
    /// Whether to wrap link cells in OSC 8. When false the link is still Ctrl+clickable.
    pub hyperlinks: bool,
    /// Whether a copy is worth pushing to the system clipboard with OSC 52.
    pub osc52: bool,
    /// Whether to take the mouse at all. Off hands selection and copy back to the terminal.
    pub mouse: bool,
}

impl Caps {
    /// Reads the real environment.
    pub fn detect() -> Caps {
        let get = |k: &str| std::env::var(k).ok();
        Caps::from_env(&|k| get(k))
    }

    /// The whole decision, over a lookup the tests can drive.
    pub fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Caps {
        let var = |k: &str| env(k);
        let term = var("TERM");
        let program = var("TERM_PROGRAM");
        let lc = var("LC_TERMINAL");

        // A terminal that says nothing about itself, or says it is a bare tty, gets nothing —
        // `TERM=dumb` is the one case where the answer is certain.
        let dumb = term.as_deref().is_some_and(|t| t == "dumb" || t.is_empty());

        let named = known(program.as_deref(), HYPERLINK_TERMINALS)
            || known(lc.as_deref(), HYPERLINK_TERMINALS)
            // kitty announces itself in TERM rather than TERM_PROGRAM.
            || term.as_deref().is_some_and(|t| t.contains("kitty"))
            // Windows Terminal sets this and nothing else useful. Its absence does not rule
            // Windows out — it only means the old console, which supports neither.
            || var("WT_SESSION").is_some();

        let hyperlinks =
            override_of(var("ZYRIS_CODE_HYPERLINKS").as_deref()).unwrap_or(named && !dumb);
        // **OSC 52 is the same guess but a weaker one.** Several terminals that draw hyperlinks
        // keep clipboard writes switched off by default (xterm, and Alacritty until told
        // otherwise), so a true here means "worth trying", not "will work". Trying costs nothing:
        // the in-app clipboard is filled either way, and a terminal that ignores the sequence
        // ignores it silently.
        let osc52 = override_of(var("ZYRIS_CODE_OSC52").as_deref()).unwrap_or(named && !dumb);
        // **Taking the mouse takes the terminal's own selection with it.** Anyone who would rather
        // keep copy-on-select can say so, and then the drag, the click-to-fold and the Ctrl+click
        // all go back to the terminal.
        let mouse = override_of(var("ZYRIS_CODE_MOUSE").as_deref()).unwrap_or(!dumb);

        Caps { hyperlinks, osc52, mouse }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a lookup from pairs, so a test says only what it is about.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let owned: Vec<(String, String)> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k: &str| owned.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone())
    }

    fn caps(pairs: &[(&str, &str)]) -> Caps {
        Caps::from_env(&env(pairs))
    }

    /// **An unknown terminal is told no.** Being wrong that way costs an underline; being wrong the
    /// other way puts escape bytes on screen, and there is no undoing that from inside the app.
    #[test]
    fn a_terminal_that_says_nothing_about_itself_gets_no_escape_sequences() {
        let c = caps(&[("TERM", "xterm-256color")]);
        assert!(!c.hyperlinks, "OSC 8 went to a terminal that never claimed to read it");
        assert!(!c.osc52);
        assert!(c.mouse, "the mouse is not the same guess — it is asked for and answered");
    }

    #[test]
    fn the_terminals_known_to_draw_links_are_recognised() {
        for (key, value) in [
            ("TERM_PROGRAM", "ghostty"),
            ("TERM_PROGRAM", "iTerm.app"),
            ("TERM_PROGRAM", "WezTerm"),
            ("TERM_PROGRAM", "vscode"),
        ] {
            assert!(caps(&[(key, value)]).hyperlinks, "{value} was not recognised");
        }
        assert!(caps(&[("TERM", "xterm-kitty")]).hyperlinks, "kitty says so in TERM");
        assert!(caps(&[("WT_SESSION", "abc-123")]).hyperlinks, "Windows Terminal");
    }

    /// **`TERM_PROGRAM` is not spelled one way.** `iTerm.app` next to `ghostty` is the whole
    /// problem; matching exactly would drop whichever casing we did not think of.
    #[test]
    fn the_name_is_matched_whatever_its_casing() {
        assert!(caps(&[("TERM_PROGRAM", "Ghostty")]).hyperlinks);
        assert!(caps(&[("TERM_PROGRAM", "ITERM.APP")]).hyperlinks);
    }

    /// **tmux overwrites `TERM_PROGRAM` with its own name but passes `LC_*` through.** Reading only
    /// `TERM_PROGRAM` makes every terminal inside tmux look like tmux, and they all lose their links.
    #[test]
    fn a_terminal_keeps_its_name_through_tmux() {
        let c = caps(&[
            ("TERM_PROGRAM", "tmux"),
            ("LC_TERMINAL", "iTerm2"),
            ("TERM", "screen-256color"),
        ]);
        assert!(c.hyperlinks, "the terminal underneath tmux was not seen");
    }

    /// `TERM=dumb` is the one answer that is certain, and it rules out the mouse as well.
    #[test]
    fn a_dumb_terminal_is_given_nothing_at_all() {
        let c = caps(&[("TERM", "dumb"), ("TERM_PROGRAM", "ghostty")]);
        assert!(!c.hyperlinks && !c.osc52 && !c.mouse);
    }

    /// **The guess is a default, not a verdict.** Whoever knows their terminal better than a list
    /// of names does needs a way to say so — in both directions.
    #[test]
    fn the_environment_can_overrule_the_guess_either_way() {
        let on = caps(&[("TERM", "xterm-256color"), ("ZYRIS_CODE_HYPERLINKS", "1")]);
        assert!(on.hyperlinks, "an unknown terminal could not be told yes");

        let off = caps(&[("TERM_PROGRAM", "ghostty"), ("ZYRIS_CODE_HYPERLINKS", "off")]);
        assert!(!off.hyperlinks, "a known terminal could not be told no");

        assert!(!caps(&[("TERM_PROGRAM", "ghostty"), ("ZYRIS_CODE_OSC52", "no")]).osc52);
        assert!(!caps(&[("ZYRIS_CODE_MOUSE", "0")]).mouse, "the mouse could not be handed back");
    }

    /// A value that means nothing falls back to the guess rather than to `false` — `MOUSE=maybe`
    /// must not be the same as switching the mouse off.
    #[test]
    fn a_value_that_means_nothing_leaves_the_guess_alone() {
        assert!(caps(&[("ZYRIS_CODE_MOUSE", "maybe")]).mouse);
        assert!(caps(&[("TERM_PROGRAM", "ghostty"), ("ZYRIS_CODE_HYPERLINKS", "sure")]).hyperlinks);
    }
}
