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

use ratatui::style::Color;

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

/// How many colours the terminal can draw.
///
/// **The palette is written in 24-bit and sent in whatever the terminal reads.** Every theme colour
/// is `Color::Rgb`, and a terminal that has no 24-bit colour does not ignore `38;2;r;g;b` — macOS
/// Terminal.app misreads its parameters and paints unrelated colours, and the Linux console folds
/// it into eight slots where the text and the dimmed text land on the same one. So the frame is
/// mapped to the nearest colour the terminal does have (`widgets::draw`, through [`Colours::fit`])
/// rather than every call site learning about depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Colours {
    /// `38;2;r;g;b` as written.
    #[default]
    True,
    /// The xterm 256-colour palette. The sixteen base slots are left out of the match: the person's
    /// theme redefines those, so what they look like is anybody's guess.
    Indexed,
    /// The sixteen base colours only.
    Sixteen,
    /// `NO_COLOR`: none at all. crossterm already drops every colour sequence when it is set, so
    /// the frame is left as it is and only the cues that live in colour alone get a stand-in.
    Mono,
}

impl Colours {
    /// Which depth the environment says, over the same lookup as the rest of [`Caps`].
    ///
    /// **`COLORTERM` is the one real answer**, and the terminals that support 24-bit colour set
    /// it — but it is not forwarded over SSH by default, so the terminals already known by name
    /// count as well. After that `TERM` decides: `-256color` gets the palette and anything else the
    /// sixteen, which is what `linux`, `screen` and a bare `xterm` really have. No `TERM` at all is
    /// the Windows console, which has drawn 24-bit colour since Windows 10.
    fn from_env(var: &dyn Fn(&str) -> Option<String>, named: bool) -> Colours {
        if var("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Colours::Mono;
        }
        let colorterm = var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm == "truecolor" || colorterm == "24bit" || named {
            return Colours::True;
        }
        match var("TERM") {
            None => Colours::True,
            Some(t) if t.ends_with("-direct") || t.contains("truecolor") => Colours::True,
            Some(t) if t.contains("256color") => Colours::Indexed,
            Some(_) => Colours::Sixteen,
        }
    }

    /// Whether a cue carried by a background alone would vanish here — the drag's wash, above all.
    pub fn reduced(self) -> bool {
        matches!(self, Colours::Sixteen | Colours::Mono)
    }

    /// `colour` as this terminal can draw it. Anything that is not 24-bit passes through.
    pub fn fit(self, colour: Color) -> Color {
        let Color::Rgb(r, g, b) = colour else { return colour };
        match self {
            Colours::True | Colours::Mono => colour,
            Colours::Indexed => Color::Indexed(nearest_indexed(r, g, b)),
            Colours::Sixteen => Color::Indexed(nearest(&BASE16, (r, g, b)) as u8),
        }
    }
}

/// xterm's defaults for the sixteen base colours. Only a guess at what the person's theme has, but
/// the nearest slot by these is still the nearest by kind — a red stays a red.
const BASE16: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 0, 0),
    (0, 205, 0),
    (205, 205, 0),
    (0, 0, 238),
    (205, 0, 205),
    (0, 205, 205),
    (229, 229, 229),
    (127, 127, 127),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (92, 92, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

/// Index of the entry closest to `to`, by squared distance.
fn nearest(of: &[(u8, u8, u8)], to: (u8, u8, u8)) -> usize {
    let d = |(r, g, b): (u8, u8, u8)| {
        let (dr, dg, db) = (r as i32 - to.0 as i32, g as i32 - to.1 as i32, b as i32 - to.2 as i32);
        dr * dr + dg * dg + db * db
    };
    (0..of.len()).min_by_key(|&i| d(of[i])).unwrap_or(0)
}

/// The closest of the 6x6x6 cube (16-231) and the grey ramp (232-255).
fn nearest_indexed(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |c: u8| nearest(&LEVELS.map(|l| (l, l, l)), (c, c, c));
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let grey_i = ((r as usize + g as usize + b as usize) / 3).saturating_sub(3) / 10;
    let grey_i = grey_i.min(23);
    let grey = 8 + 10 * grey_i as u8;
    if nearest(&[cube, (grey, grey, grey)], (r, g, b)) == 0 {
        16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8
    } else {
        232 + grey_i as u8
    }
}

/// A one-column stand-in for a cell a CJK-configured terminal would draw two columns wide, or
/// `None` when the cell is drawn the same either way.
///
/// **The screen is laid out in ratatui's widths, and ratatui counts an Ambiguous character as one
/// column.** A terminal set to draw them wide — PuTTY's "ambiguous as wide", iTerm2's and
/// Terminal.app's double-width setting, common among Korean users — draws `—`, `…`, `“`, `·`, `×` or
/// a box-drawing line in two, and the rest of the row slides right by one for each; the diff
/// thinks those cells are right, so the damage stays until a full repaint. The app's own glyphs
/// are policed by `tests/width.rs`; the agent's text cannot be. Swapping the cell for an ASCII
/// look-alike keeps the row where it was measured. Letters (Greek, Cyrillic) are left alone: a row
/// that slides is still readable, a word of question marks is not.
///
/// **Only the screen changes.** The swap happens on the finished frame, after the text a drag
/// copies was taken from it, so a copy still carries the real characters.
///
/// **Opt-in, not guessed from the locale.** Most Korean terminals — Windows Terminal, iTerm2,
/// GNOME Terminal — draw these narrow by default, and swapping them there would only coarsen text
/// that was already right.
pub fn narrow_stand_in(cell: &str) -> Option<&'static str> {
    use unicode_width::UnicodeWidthStr;
    if cell.width_cjk() <= cell.width() {
        return None;
    }
    let mut chars = cell.chars();
    let first = chars.next()?;
    if first.is_alphabetic() {
        return None;
    }
    // Written as escapes: these are what the agent's text may hold, never what the app draws, and
    // `tests/width.rs` reads the app's literals for what it draws.
    Some(match first {
        // Dashes, and the horizontal and vertical lines of box drawing.
        '\u{2010}'..='\u{2015}' => "-",
        '\u{2500}' | '\u{2501}' | '\u{2504}' | '\u{2505}' | '\u{2508}' | '\u{2509}'
        | '\u{254C}' | '\u{254D}' | '\u{2550}' => "-",
        '\u{2502}' | '\u{2503}' | '\u{2506}' | '\u{2507}' | '\u{250A}' | '\u{250B}'
        | '\u{254E}' | '\u{254F}' | '\u{2551}' => "|",
        '\u{2500}'..='\u{257F}' => "+",
        // Half and part blocks standing at an edge read as a bar; the rest as a fill.
        '\u{258C}'..='\u{2590}' => "|",
        '\u{2580}'..='\u{259F}' => "#",
        // Quotes, primes, ellipsis and middle dots.
        '\u{201C}' | '\u{201D}' | '\u{2033}' => "\"",
        '\u{2018}' | '\u{2019}' | '\u{2032}' => "'",
        '\u{2026}' | '\u{00B7}' | '\u{2027}' => ".",
        // Arrows and pointing triangles keep their direction; other shapes are a bullet.
        '\u{2192}' | '\u{21D2}' | '\u{25B6}' | '\u{25BA}' => ">",
        '\u{2190}' | '\u{21D0}' | '\u{25C0}' | '\u{25C4}' => "<",
        '\u{2191}' | '\u{25B2}' => "^",
        '\u{2193}' | '\u{25BC}' => "v",
        '\u{2022}' | '\u{203B}' | '\u{2605}' | '\u{2606}' | '\u{25A0}'..='\u{25FF}' => "*",
        '\u{00D7}' => "x",
        '\u{00F7}' => "/",
        '\u{00B1}' => "+",
        '\u{00B0}' | '\u{00BA}' => "o",
        _ => "?",
    })
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
    /// How the palette has to be sent.
    pub colours: Colours,
    /// Whether this terminal draws East Asian Ambiguous characters two columns wide — see
    /// [`narrow_stand_in`]. Only ever set by `$ZYRIS_CODE_AMBIGUOUS_WIDE`.
    pub ambiguous_wide: bool,
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

        let colours = Colours::from_env(&var, named);

        let ambiguous_wide =
            override_of(var("ZYRIS_CODE_AMBIGUOUS_WIDE").as_deref()).unwrap_or(false);

        Caps { hyperlinks, osc52, mouse, colours, ambiguous_wide }
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

    /// **24-bit colour only where the terminal says so.** Terminal.app, the Linux console and a
    /// `screen` without `RGB` are the ones that got `38;2` and painted it wrong.
    #[test]
    fn the_colour_depth_follows_what_the_terminal_says() {
        assert_eq!(
            caps(&[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")]).colours,
            Colours::True
        );
        assert_eq!(caps(&[("COLORTERM", "24bit"), ("TERM", "screen")]).colours, Colours::True);
        assert_eq!(caps(&[("TERM", "xterm-256color")]).colours, Colours::Indexed);
        assert_eq!(
            caps(&[("TERM_PROGRAM", "Apple_Terminal"), ("TERM", "xterm-256color")]).colours,
            Colours::Indexed
        );
        assert_eq!(caps(&[("TERM", "linux")]).colours, Colours::Sixteen);
        assert_eq!(caps(&[("TERM", "screen")]).colours, Colours::Sixteen);
        assert_eq!(caps(&[("TERM", "xterm-direct")]).colours, Colours::True);
        // Known by name, as over SSH where COLORTERM does not travel.
        assert_eq!(caps(&[("TERM", "xterm-kitty")]).colours, Colours::True);
        assert_eq!(
            caps(&[("LC_TERMINAL", "iTerm2"), ("TERM", "xterm-256color")]).colours,
            Colours::True
        );
        // The Windows console sets no TERM.
        assert_eq!(caps(&[]).colours, Colours::True);
        // NO_COLOR wins over everything; empty means unset, as the convention says.
        assert_eq!(caps(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")]).colours, Colours::Mono);
        assert_eq!(caps(&[("NO_COLOR", ""), ("COLORTERM", "truecolor")]).colours, Colours::True);
    }

    #[test]
    fn a_colour_is_mapped_to_the_nearest_the_terminal_has() {
        let rgb = Color::Rgb(0xe8, 0xe2, 0xdc);
        assert_eq!(Colours::True.fit(rgb), rgb);
        assert_eq!(Colours::Mono.fit(rgb), rgb);
        // Exact cube and grey entries come back as themselves.
        assert_eq!(Colours::Indexed.fit(Color::Rgb(255, 0, 0)), Color::Indexed(196));
        assert_eq!(Colours::Indexed.fit(Color::Rgb(0, 0, 0)), Color::Indexed(16));
        assert_eq!(Colours::Indexed.fit(Color::Rgb(128, 128, 128)), Color::Indexed(244));
        assert_eq!(Colours::Sixteen.fit(Color::Rgb(250, 10, 10)), Color::Indexed(9));
        assert_eq!(Colours::Sixteen.fit(Color::Rgb(0x0f, 0x0d, 0x0a)), Color::Indexed(0));
        // Not ours to touch.
        assert_eq!(Colours::Sixteen.fit(Color::Reset), Color::Reset);
        assert_eq!(Colours::Indexed.fit(Color::Indexed(3)), Color::Indexed(3));
        // Every value lands inside the range its depth may use.
        for v in (0..=255u8).step_by(5) {
            for c in [Color::Rgb(v, 255 - v, v / 2), Color::Rgb(v, v, v)] {
                assert!(matches!(Colours::Indexed.fit(c), Color::Indexed(16..=255)), "{c:?}");
                assert!(matches!(Colours::Sixteen.fit(c), Color::Indexed(0..=15)), "{c:?}");
            }
        }
    }

    /// **Only the cells a wide-ambiguous terminal would draw in two columns are swapped**, and only
    /// for something one column wide. Hangul, ASCII and the app's own narrow glyphs are untouched.
    #[test]
    fn an_ambiguous_cell_gets_a_one_column_stand_in() {
        assert_eq!(narrow_stand_in("—"), Some("-"));
        assert_eq!(narrow_stand_in("…"), Some("."));
        assert_eq!(narrow_stand_in("│"), Some("|"));
        assert_eq!(narrow_stand_in("●"), Some("*"));
        assert_eq!(narrow_stand_in("→"), Some(">"));
        assert_eq!(narrow_stand_in("\u{2460}"), Some("?"), "① has no look-alike");
        assert_eq!(narrow_stand_in("▌"), Some("|"));
        for same in ["a", "한", " ", "-", "Ж", "α"] {
            assert_eq!(narrow_stand_in(same), None, "{same:?} was swapped");
        }
        assert!(!caps(&[]).ambiguous_wide, "off unless asked for");
        assert!(caps(&[("ZYRIS_CODE_AMBIGUOUS_WIDE", "1")]).ambiguous_wide);
    }

    /// A value that means nothing falls back to the guess rather than to `false` — `MOUSE=maybe`
    /// must not be the same as switching the mouse off.
    #[test]
    fn a_value_that_means_nothing_leaves_the_guess_alone() {
        assert!(caps(&[("ZYRIS_CODE_MOUSE", "maybe")]).mouse);
        assert!(caps(&[("TERM_PROGRAM", "ghostty"), ("ZYRIS_CODE_HYPERLINKS", "sure")]).hyperlinks);
    }
}
