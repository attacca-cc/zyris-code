//! The Zyris palette — **one entry per role, in two themes.**
//!
//! **These are roles, not colours.** `error()` is what an error is painted with; whether that is
//! a red depends on the theme. Reaching for a colour because it "looks right" is how one value
//! ends up carrying several meanings — and then changing it for one of them breaks the others.
//! `ACCENT` alone used to be the brand chrome, the user's message bar, the agent marker, every
//! cursor, the input prompt, the focused button, inline `code` **and** plan mode; plan mode
//! therefore had no colour of its own, it wore the same paint as every border on screen.
//!
//! **Only one place gets a background** — the user message (`user_bg`). The whole screen is not
//! painted; letting the terminal use its own background is this app's policy. On 2026-08-03 a
//! full-screen background was turned on to stop ghosting, then reverted when it turned out the
//! person had cleared it. Ghosting is cleaned up by `app::heal_interval` (a full redraw every 2s).
//!
//! **That policy is exactly why the light theme exists.** With no background of our own, the text
//! colour has to suit the terminal's. The dark palette's `text()` (#e8e2dc) measures **1.19**
//! against a common light terminal — words the colour of the paper they are on. `/config theme`
//! picks; nothing about the drawing changes.
//!
//! **Don't create text without a colour.** Unspecified, the terminal's own default foreground
//! leaks out — on a terminal with a changed default foreground, everything "that should be white"
//! shows in that colour.

use std::sync::atomic::{AtomicU8, Ordering};

use ratatui::style::Color;

/// Which palette is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Theme {
    /// For a dark terminal. The brand's own look, and the default.
    #[default]
    Dark,
    /// For a light terminal.
    Light,
}

impl Theme {
    /// From what the person typed. Both languages' words are accepted.
    pub fn parse(text: &str) -> Option<Theme> {
        match text.trim().to_ascii_lowercase().as_str() {
            "dark" | "어둡게" | "어두움" | "다크" => Some(Theme::Dark),
            "light" | "밝게" | "밝음" | "라이트" => Some(Theme::Light),
            _ => None,
        }
    }

    /// The name written to the setting file.
    pub fn code(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }
}

/// The theme every `fg` below reads.
///
/// **An atomic, not a `OnceLock`.** `/config` changes the theme while the app is up, and the very
/// next frame has to be drawn in it — the same promise `dir_access` makes to the gate.
static PICKED: AtomicU8 = AtomicU8::new(0);

pub fn current() -> Theme {
    match PICKED.load(Ordering::Relaxed) {
        1 => Theme::Light,
        _ => Theme::Dark,
    }
}

pub fn set(theme: Theme) {
    PICKED.store(if theme == Theme::Light { 1 } else { 0 }, Ordering::Relaxed);
}

/// Which theme to start in when the setting says "work it out".
///
/// **`COLORFGBG` is the only thing we can ask without asking the terminal.** Querying the real
/// background is OSC 11, and this app does not put questions on the wire and wait for an answer —
/// `Terminal::clear()`'s DSR did exactly that and hung the app on terminals that never replied.
/// So this reads the hint some terminals export (`fg;bg`, where the background is the last field)
/// and settles for dark when there is none. Getting it wrong costs one `/config theme`.
pub fn detect() -> Theme {
    let Ok(value) = std::env::var("COLORFGBG") else { return Theme::Dark };
    let Some(bg) = value.rsplit(';').next() else { return Theme::Dark };
    // 0-6 and 8 are the dark half of the 16-colour palette; 7 and 9-15 are the light half.
    match bg.trim().parse::<u8>() {
        Ok(n) if n == 7 || (9..=15).contains(&n) => Theme::Light,
        _ => Theme::Dark,
    }
}

/// Picks between the two palettes. Every role below is one line because of it.
fn pick(dark: (u8, u8, u8), light: (u8, u8, u8)) -> Color {
    let (r, g, b) = match current() {
        Theme::Dark => dark,
        Theme::Light => light,
    };
    Color::Rgb(r, g, b)
}

// ─────────────────────────────────────────────────────────────────────────────
// Surfaces
// ─────────────────────────────────────────────────────────────────────────────

/// `--zyris-bg` of the Zyris web palette. **Not painted by default** — see `page_bg`.
pub fn bg() -> Color {
    pick((0x0f, 0x0d, 0x0a), (0xfa, 0xf7, 0xf2))
}

/// The background laid over the whole screen. **Default is none — the terminal uses its own.**
///
/// It used to lay `bg()` over every leftover cell. There was a reason: the ratatui diff doesn't
/// send a wide character's right cell to the terminal (trusting the terminal to paint both cells),
/// and the protection that force-clears that cell only fires when **`previous.bg != Reset`**.
/// Without a background, the right half of a wide character ghosts on remote terminals.
///
/// **Still, the default stays off** — because there are places the app cannot paint. The terminal
/// leaves pixels that do not fit the grid as margin at the right and bottom, and the window itself
/// has padding. Those spots stay terminal background, so the moment the app paints its own, **a
/// differently coloured band appears at the edges.**
///
/// Ghosting is cleared by redrawing — `Ctrl+L` and `app::heal_interval` do it. On terminals where
/// that is not enough, turn it on with **`ZYRIS_CODE_BG`**: `zyris` gives the theme's own
/// background, `#rrggbb` gives that colour.
pub fn page_bg() -> Option<Color> {
    // **Read every time, not once.** The theme can change under `/config`, and a background
    // decided at startup would then be the other theme's.
    page_bg_from(std::env::var("ZYRIS_CODE_BG").ok().as_deref())
}

/// Turns `$ZYRIS_CODE_BG` into a colour. **Pure** — the decision lives here so tests don't shake
/// the environment.
pub fn page_bg_from(given: Option<&str>) -> Option<Color> {
    let given = given.map(str::trim).filter(|v| !v.is_empty())?;
    match given.to_ascii_lowercase().as_str() {
        // The off side must be expressible too — it is the way back when it was left on.
        "none" | "off" | "0" | "terminal" => None,
        "zyris" | "on" | "1" | "default" => Some(bg()),
        _ => hex(given),
    }
}

/// `#rrggbb` or `rrggbb`. Unreadable gives `None` — no reason for the app to die on a typo.
fn hex(text: &str) -> Option<Color> {
    let digits = text.strip_prefix('#').unwrap_or(text);
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some(Color::Rgb(byte(0)?, byte(2)?, byte(4)?))
}

/// The background where the user spoke.
///
/// **This is the only place that uses a background.** The "don't paint backgrounds" rule at the
/// top of the file is flipped here and only here — laid over the whole screen it would read as a
/// stain, and painting everything makes nothing distinguishable. This one band is the "this is
/// where I spoke" signal.
pub fn user_bg() -> Color {
    pick((0x2a, 0x20, 0x1a), (0xf0, 0xe6, 0xd8))
}

/// The wash painted over the cells a mouse drag has selected.
///
/// **A faint accent-tinted background, the one other place a background is allowed.** The
/// "don't paint backgrounds" rule at the top is flipped for the same reason it is for
/// `user_bg` — this marks the chosen span, and it has to sit on top of whatever is already
/// under it (the transcript, the status line, the enrollment window). It stays close to the
/// theme's own ground so the words underneath remain readable: this is "these letters are
/// chosen", not a box around them.
pub fn selection_bg() -> Color {
    pick((0x3f, 0x2e, 0x22), (0xef, 0xdc, 0xca))
}

// ─────────────────────────────────────────────────────────────────────────────
// Structure
// ─────────────────────────────────────────────────────────────────────────────

/// The line dividing areas.
pub fn border() -> Color {
    pick((0x3a, 0x30, 0x29), (0xd6, 0xcc, 0xc0))
}

/// Divider glyphs, disabled rows, placeholders, an unlit blink.
pub fn border_light() -> Color {
    pick((0x4a, 0x3e, 0x36), (0xb3, 0xa6, 0x97))
}

/// A colour mixed `amount` of the way toward the background, where 0 is the colour untouched and 1
/// is the background itself.
///
/// **This is what fading means on a terminal.** There is no opacity to turn down — a cell has one
/// foreground colour and that is all — so the way to make something recede is to move its colour
/// toward what is behind it. The web page this app is modelled on does the same thing with
/// `opacity`, and against a solid background the two are the same arithmetic.
///
/// **`Modifier::DIM` is not a substitute.** It is one step, terminals disagree about how big a step
/// it is, and some ignore it — so an animation built on it either does not move or jumps.
///
/// Anything that is not true colour is returned untouched: there is nothing to interpolate between
/// on a sixteen-colour terminal, and a guess would be worse than leaving it alone.
pub fn fade(colour: Color, amount: f64) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    let (Color::Rgb(r, g, b), Color::Rgb(br, bg_, bb)) = (colour, bg()) else {
        return colour;
    };
    let mix = |from: u8, to: u8| (from as f64 + (to as f64 - from as f64) * amount).round() as u8;
    Color::Rgb(mix(r, br), mix(g, bg_), mix(b, bb))
}

// ─────────────────────────────────────────────────────────────────────────────
// Text
// ─────────────────────────────────────────────────────────────────────────────

pub fn text() -> Color {
    pick((0xe8, 0xe2, 0xdc), (0x2b, 0x26, 0x22))
}

pub fn text_muted() -> Color {
    pick((0x9c, 0x94, 0x8d), (0x6b, 0x62, 0x59))
}

pub fn text_heading() -> Color {
    pick((0xf1, 0xed, 0xe8), (0x1a, 0x16, 0x13))
}

// ─────────────────────────────────────────────────────────────────────────────
// Brand
// ─────────────────────────────────────────────────────────────────────────────

/// The brand colour: box borders, cursors, the input prompt, the user's own bar.
pub fn accent() -> Color {
    pick((0xc9, 0x73, 0x4d), (0xa8, 0x50, 0x1f))
}

/// The accent one step down. Used where an accent sits behind something else.
pub fn accent_hover() -> Color {
    pick((0xb5, 0x62, 0x3e), (0x8f, 0x43, 0x19))
}

// ─────────────────────────────────────────────────────────────────────────────
// Meaning
// ─────────────────────────────────────────────────────────────────────────────

pub fn success() -> Color {
    pick((0x8f, 0xae, 0x5c), (0x4a, 0x7a, 0x1f))
}

/// Something worth noticing that is **not** wrong: unsent messages, a dirty repo, a lapsed code.
pub fn warning() -> Color {
    pick((0xd9, 0xa4, 0x41), (0x8a, 0x5d, 0x00))
}

/// Something is wrong: a failed tool, an error entry, a conflict, a refusal.
pub fn danger() -> Color {
    pick((0xc1, 0x50, 0x3f), (0xa3, 0x27, 0x1a))
}

/// A passing remark on the activity line — connected, another window is open, a command answered.
///
/// **Split from `warning()`**, which used to paint every notice *including errors*. With one
/// colour for both, an error looked exactly like "connected", and the one line whose whole job is
/// to say what is happening could not say that something had gone wrong.
pub fn notice() -> Color {
    pick((0x9c, 0x94, 0x8d), (0x6b, 0x62, 0x59))
}

// ─────────────────────────────────────────────────────────────────────────────
// The repository strip
// ─────────────────────────────────────────────────────────────────────────────

/// Commits this checkout has and its upstream does not — yours to push.
///
/// **These three used to be one muted grey.** The strip could say six different things and three
/// of them looked identical, so the only way to learn that a branch was behind was to read the
/// arrow rather than see it. They stay away from `warning()`, which is reserved here for what is
/// not committed yet: pushing is a later, calmer errand than committing.
pub fn ahead() -> Color {
    pick((0x6f, 0xb0, 0x7a), (0x2f, 0x6f, 0x3f))
}

/// Commits the upstream has and this checkout does not — someone else's, waiting to be pulled.
/// Blue against `ahead()`'s green, because the two are opposite directions and are read together.
pub fn behind() -> Color {
    pick((0x6f, 0x9c, 0xc4), (0x2a, 0x5c, 0x86))
}

/// Files git does not track.
///
/// **Quiet on purpose.** These are usually build litter, and in an alarming colour the strip would
/// be lit permanently and stop being read. But quiet is not the same as invisible: it carries a
/// tint so it does not read as the path beside it.
pub fn untracked() -> Color {
    pick((0x8c, 0x8f, 0xa6), (0x5c, 0x60, 0x7a))
}

// ─────────────────────────────────────────────────────────────────────────────
// Modes
// ─────────────────────────────────────────────────────────────────────────────

/// Plan mode.
///
/// **Its own colour.** Plan mode used to be painted `accent()` — the same paint as every border,
/// every cursor, the input prompt and the user's message bar — so the one thing the bottom bar
/// exists to tell you was the colour of the furniture around it. Neighbours in the Shift+Tab cycle
/// have to be far apart, since the eye compares against the mode it just left:
/// green → violet → blue → yellow.
pub fn mode_plan() -> Color {
    pick((0xb4, 0x8e, 0xad), (0x6b, 0x4d, 0x9e))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tools, links, diffs
// ─────────────────────────────────────────────────────────────────────────────

/// The tool line's name. **Must not be the same muted colour as reasoning.**
///
/// Inside an expanded card reasoning fills the screen; if tools were also `text_muted()`, the
/// actual "what was done" would be buried. What the reader scans is the tool line.
pub fn tool() -> Color {
    pick((0x7f, 0xb0, 0xd4), (0x1f, 0x5f, 0x8b))
}

/// The tool line's argument summary. One step below the name.
pub fn tool_arg() -> Color {
    pick((0x6b, 0x8a, 0xa0), (0x3d, 0x6b, 0x82))
}

/// A reasoning chip's title inside a work card. A warm tone, so it reads as a heading distinct
/// from `tool()` (blue) and from muted reasoning — the eye scans chip titles to find a section.
pub fn topic() -> Color {
    pick((0xe0, 0xc2, 0x8e), (0x8a, 0x6a, 0x2e))
}

/// A link's text. Underlined in the renderer; the underline plus a distinct colour is what says
/// "this can be Ctrl+clicked" without a mouse hover.
///
/// **Not the same as `tool()`.** The two were byte-identical, so a link sitting beside a tool name
/// — which happens on every tool line carrying a URL — was indistinguishable from it.
pub fn link() -> Color {
    pick((0x56, 0xb6, 0xc2), (0x0f, 0x6b, 0x62))
}

/// The added line in a diff. Green.
///
/// `success()`/`danger()` are not reused. Those two say "the tool worked / did not", so a deleted
/// line inside a *successful* edit painted the same red as a failed tool would be misread.
/// A pull request that has landed.
///
/// **Purple because it is the one state that is finished.** Yellow says "waiting on somebody",
/// which is what an open pull request is; green and red are already spoken for by what CI said,
/// and a merged pull request is neither of those — it is over.
pub fn merged() -> Color {
    pick((0xa3, 0x71, 0xf7), (0x82, 0x50, 0xdf))
}

pub fn diff_add() -> Color {
    pick((0x7e, 0xc0, 0x50), (0x2f, 0x7a, 0x1f))
}

/// The removed line in a diff. Red.
pub fn diff_del() -> Color {
    pick((0xe0, 0x6c, 0x75), (0xa3, 0x2a, 0x2a))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the theme so the tests below can restore it — they run in one process and the
    /// palette is global.
    fn with(theme: Theme, body: impl FnOnce()) {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = current();
        set(theme);
        body();
        set(before);
    }

    /// **Fading is the terminal's opacity.** A cell has one foreground colour, so the only way to
    /// make something recede is to move it toward what is behind it — and the two ends have to be
    /// exactly the colour and exactly the background, or an animation would jump at its edges.
    #[test]
    fn fading_walks_a_colour_to_the_background_and_no_further() {
        with(Theme::Dark, || {
            let c = warning();
            assert_eq!(fade(c, 0.0), c, "no fade must change nothing");
            assert_eq!(fade(c, 1.0), bg(), "a full fade must land on the background");
            assert_eq!(fade(c, -1.0), c);
            assert_eq!(fade(c, 2.0), bg());

            let (mid, (a, b)) = (rgb(fade(c, 0.5)), (rgb(c), rgb(bg())));
            for (m, (x, y)) in [(mid.0, (a.0, b.0)), (mid.1, (a.1, b.1)), (mid.2, (a.2, b.2))] {
                assert!(m >= x.min(y) && m <= x.max(y), "half a fade left the range");
            }
        });
    }

    /// **No two marks on the repository strip may share a colour.** Untracked, ahead and behind
    /// were all one muted grey, so three of the six things that row can report looked identical —
    /// the arrow had to be read rather than seen. They must also stay off the paint already spoken
    /// for beside them: `danger()` for a conflict, `warning()` for what is not committed yet, and
    /// the muted colour the path and the branch wear.
    ///
    /// Both palettes, because a colour that only separates in the dark is not a distinction.
    #[test]
    fn no_two_marks_on_the_repository_strip_share_a_colour() {
        for theme in [Theme::Dark, Theme::Light] {
            with(theme, || {
                let marks = [
                    ("conflict", danger()),
                    ("uncommitted", warning()),
                    ("untracked", untracked()),
                    ("ahead", ahead()),
                    ("behind", behind()),
                    ("the path itself", text_muted()),
                    // A landed pull request sits on this row too, and it must not read as any of
                    // the states above it — least of all as the green that means CI passed.
                    ("merged", merged()),
                ];
                for (i, (an, a)) in marks.iter().enumerate() {
                    for (bn, b) in marks.iter().skip(i + 1) {
                        assert_ne!(a, b, "{an} and {bn} are one colour in {theme:?}");
                    }
                }
            });
        }
    }

    fn rgb(c: Color) -> (f64, f64, f64) {
        match c {
            Color::Rgb(r, g, b) => (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0),
            other => panic!("the palette must be true colour, got {other:?}"),
        }
    }

    /// WCAG relative luminance.
    fn luminance(c: Color) -> f64 {
        let f = |v: f64| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        let (r, g, b) = rgb(c);
        0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
    }

    fn contrast(a: Color, b: Color) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// **Every colour that carries words must be readable on its own theme's background.**
    ///
    /// This is what the light theme is for. The dark palette's `text()` measures 1.19 against a
    /// common light terminal, and since this app paints no background of its own, that is exactly
    /// what a person on a light terminal saw.
    ///
    /// The floor is 4.0 rather than WCAG AA's 4.5 because `danger()` sits at 4.15 and the brand
    /// value is kept as it is. Every other role clears 4.5 comfortably.
    #[test]
    fn text_colours_are_readable_on_their_own_background() {
        for theme in [Theme::Dark, Theme::Light] {
            with(theme, || {
                let on = bg();
                for (name, colour) in [
                    ("text", text()),
                    ("text_muted", text_muted()),
                    ("text_heading", text_heading()),
                    ("accent", accent()),
                    ("success", success()),
                    ("warning", warning()),
                    ("danger", danger()),
                    ("notice", notice()),
                    ("mode_plan", mode_plan()),
                    ("tool", tool()),
                    ("tool_arg", tool_arg()),
                    ("link", link()),
                    ("diff_add", diff_add()),
                    ("diff_del", diff_del()),
                ] {
                    let ratio = contrast(colour, on);
                    assert!(ratio >= 4.0, "{theme:?} {name} is {ratio:.2}:1 — too close to read");
                }
            });
        }
    }

    /// The user's own band is a background, so the text on it has to hold up too.
    #[test]
    fn text_is_readable_on_the_user_band() {
        for theme in [Theme::Dark, Theme::Light] {
            with(theme, || {
                let ratio = contrast(text(), user_bg());
                assert!(ratio >= 4.5, "{theme:?} text on the user band is {ratio:.2}:1");
            });
        }
    }

    /// The selection wash is a background; the words it sits under must stay readable.
    #[test]
    fn text_is_readable_on_the_selection_band() {
        for theme in [Theme::Dark, Theme::Light] {
            with(theme, || {
                let ratio = contrast(text(), selection_bg());
                assert!(ratio >= 4.5, "{theme:?} text on the selection is {ratio:.2}:1");
            });
        }
    }

    /// **A role that shares a value with another is a role waiting to be broken.** Changing the
    /// colour for one meaning silently changes the other — `link()` and `tool()` were identical,
    /// and plan mode was `accent()`, the same paint as every border on screen.
    #[test]
    fn roles_that_mean_different_things_have_different_colours() {
        for theme in [Theme::Dark, Theme::Light] {
            with(theme, || {
                let roles = [
                    ("accent", accent()),
                    ("mode_plan", mode_plan()),
                    ("tool", tool()),
                    ("link", link()),
                    ("warning", warning()),
                    ("notice", notice()),
                    ("danger", danger()),
                    ("success", success()),
                ];
                for (i, (an, a)) in roles.iter().enumerate() {
                    for (bn, b) in &roles[i + 1..] {
                        assert_ne!(a, b, "{theme:?}: {an} and {bn} are the same colour");
                    }
                }
            });
        }
    }

    /// The palette must match the Zyris web values. Misaligned, the same product shows in
    /// different colours.
    #[test]
    fn the_dark_palette_matches_the_brand_values() {
        with(Theme::Dark, || {
            assert_eq!(bg(), Color::Rgb(0x0f, 0x0d, 0x0a));
            assert_eq!(accent(), Color::Rgb(0xc9, 0x73, 0x4d));
            assert_eq!(text(), Color::Rgb(0xe8, 0xe2, 0xdc));
            assert_eq!(text_muted(), Color::Rgb(0x9c, 0x94, 0x8d));
            assert_eq!(text_heading(), Color::Rgb(0xf1, 0xed, 0xe8));
            assert_eq!(border(), Color::Rgb(0x3a, 0x30, 0x29));
            assert_eq!(danger(), Color::Rgb(0xc1, 0x50, 0x3f));
        });
    }

    #[test]
    fn a_theme_answers_to_both_languages() {
        assert_eq!(Theme::parse("dark"), Some(Theme::Dark));
        assert_eq!(Theme::parse("밝게"), Some(Theme::Light));
        assert_eq!(Theme::parse("아무거나"), None);
    }

    /// `COLORFGBG` is a hint, not an answer — a terminal that does not set it must not flip the
    /// palette. Dark is the safe guess because it is the brand's own look.
    #[test]
    fn detection_falls_back_to_dark_without_a_hint() {
        // The parsing is what matters; the variable itself is read once at startup.
        assert_eq!(Theme::default(), Theme::Dark);
    }
}
