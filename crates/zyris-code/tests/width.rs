//! Which characters the screen is allowed to be drawn with.
//!
//! **The width table has an answer that is not an answer.** East Asian Width marks a character
//! `Ambiguous` when it is one column in a Western context and two in a CJK one, and the terminal
//! decides which — from its font, its locale, or a setting nobody remembers turning on.
//! `unicode-width` reports one column, so this app lays the row out for one, and a terminal
//! drawing two shifts everything after it by a column.
//!
//! **In a cell-addressed screen that damage does not heal.** The app says "put this at column 43";
//! having written to the wrong cell, the diff then believes that cell is already right and never
//! touches it again. A program that prints lines and lets the terminal wrap gets a slightly odd
//! wrap and nothing more — which is most of why apps built that way look fine everywhere.
//!
//! So this walks the string literals the app draws with and fails on any ambiguous character that
//! is not on the list below. The list is not a wish; every entry is a decision with a reason.

use std::collections::BTreeSet;

use unicode_width::UnicodeWidthChar;

/// Characters kept despite being ambiguous, and why.
///
/// **Adding to this list is a decision, not a formality.** Each one is a character that will sit a
/// column out of place on a terminal configured for CJK, taking the rest of its row with it.
const KEPT: &[(char, &str)] = &[
    // No narrow filled circle exists in Unicode. `◉` and `▪` are narrow but neither is a dot, and
    // this is the status marker on threads, tasks and the activity line — the shape carries it.
    ('●', "the status dot: no narrow filled circle exists"),
    ('○', "its empty twin, which has to match it"),
    ('◆', "what the agent said: kept to match the dot"),
    ('◈', "its variant, likewise"),
    // `▲▼△▽` are all ambiguous too, and the narrow ones are either small triangles (which read as
    // chevrons, not arrows), emoji-presentation arrows that terminals draw in colour and double
    // width, or shapes with thin monospace font coverage.
    ('↑', "no full-size narrow arrow exists that fonts reliably have"),
    ('↓', "likewise"),
    ('←', "likewise"),
    ('→', "likewise"),
    ('▶', "the same problem in a triangle"),
    ('◀', "likewise"),
    // Narrow box drawing is sixteen characters, all dashes and half-lines: no corners, no
    // junctions, not even the rounded ones. Tables and code fences cannot be drawn without them,
    // and the overlay borders are ratatui's own symbols anyway.
    ('─', "box drawing has no narrow corners, so half a box cannot be swapped"),
    ('│', "likewise"),
    ('┌', "likewise"),
    ('┐', "likewise"),
    ('└', "likewise"),
    ('┘', "likewise"),
    ('├', "likewise"),
    ('┤', "likewise"),
    ('┬', "likewise"),
    ('┴', "likewise"),
    ('┼', "likewise"),
    ('┊', "likewise"),
    ('▌', "the user's bar: the narrow half-block is the other side, which reads as shifted"),
    // `⋯` is narrow but poorly covered by monospace fonts, and `...` costs two more columns in
    // exactly the places that ran out of room to begin with.
    ('…', "the truncation mark, where a column is what there was not enough of"),
    ('•', "a bullet, in text the terminal is not laying out"),
    ('×', "in prose, not in a laid-out row"),
];

/// Every character the app can draw, taken from the string and char literals of the code that
/// draws. Test modules are skipped: an assertion message is read from a test log, not laid out on
/// a screen, and holding it to the screen's rules only makes the prose worse.
fn drawn_characters() -> BTreeSet<char> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = BTreeSet::new();
    let mut files = Vec::new();
    collect(&root, &mut files);
    for path in files {
        let text = std::fs::read_to_string(&path).expect("could not read a source file");
        for line in skip_test_modules(&text) {
            for ch in literals(line) {
                if !ch.is_ascii() {
                    found.insert(ch);
                }
            }
        }
    }
    found
}

fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("could not walk the source") {
        let path = entry.expect("could not read an entry").path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The lines outside any `#[cfg(test)]` module, found by counting braces from the attribute.
fn skip_test_modules(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("#[cfg(test)]") {
            let (mut depth, mut opened) = (0i32, false);
            while i < lines.len() {
                depth += lines[i].matches('{').count() as i32;
                depth -= lines[i].matches('}').count() as i32;
                opened |= lines[i].contains('{');
                i += 1;
                if opened && depth <= 0 {
                    break;
                }
            }
            continue;
        }
        keep.push(lines[i]);
        i += 1;
    }
    keep
}

/// The characters inside string and char literals on one line, with a trailing `//` comment cut
/// off first. Comments are prose for whoever is reading the code, not text for the screen.
fn literals(line: &str) -> Vec<char> {
    if line.trim_start().starts_with("//") {
        return Vec::new();
    }
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut in_string) = (0, false);
    while i < chars.len() {
        match chars[i] {
            '"' if i == 0 || chars[i - 1] != '\\' => in_string = !in_string,
            '/' if !in_string && chars.get(i + 1) == Some(&'/') => break,
            // A char literal: one character between quotes. `'static` never matches, since what
            // follows its first character is a letter rather than the closing quote.
            '\'' if !in_string && chars.get(i + 2) == Some(&'\'') => {
                out.push(chars[i + 1]);
                i += 2;
            }
            c if in_string => out.push(c),
            _ => {}
        }
        i += 1;
    }
    out
}

/// **Nothing new may be laid out with a character whose width the terminal decides.**
///
/// Without this the swap that removed them silently comes back: `·` is easier to type than `∙`,
/// reads the same in an editor, and differs only on somebody else's terminal.
#[test]
fn no_new_ambiguous_width_characters_reach_the_screen() {
    let kept: BTreeSet<char> = KEPT.iter().map(|(c, _)| *c).collect();
    let mut offenders = Vec::new();

    for ch in drawn_characters() {
        // Ambiguous is exactly this: one answer in a Western context, another in a CJK one.
        let ambiguous = ch.width() != ch.width_cjk();
        if ambiguous && !kept.contains(&ch) {
            offenders.push(ch);
        }
    }

    assert!(
        offenders.is_empty(),
        "these are one column here and two on a terminal set up for CJK, which shifts the rest of \
         the row and cannot be repaired by a redraw: {offenders:?}\n\
         Swap them for a narrow character, or add them to KEPT in this file with the reason.",
    );
}

/// The list has to stay honest: a character that stopped being ambiguous, or stopped being used,
/// is a line that now only misleads whoever reads it next.
#[test]
fn every_kept_character_is_still_ambiguous_and_still_used() {
    let drawn = drawn_characters();
    for (ch, why) in KEPT {
        assert_ne!(
            ch.width(),
            ch.width_cjk(),
            "{ch:?} is not ambiguous, so keeping it needs no excuse ({why})"
        );
        assert!(drawn.contains(ch), "{ch:?} is no longer drawn anywhere — drop it from KEPT");
    }
}
