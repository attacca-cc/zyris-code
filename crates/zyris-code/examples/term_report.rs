//! What this terminal looks like from inside the app.
//!
//! Prints the capability guess (`term.rs`) next to the raw environment it was made from, so a
//! report from a machine nobody here can reach says *why* it decided what it decided rather than
//! only what. Run by `scripts/windows_check.bat`, and useful on any platform.
//!
//! **It draws nothing and takes no keys**, so it is safe to run from a script whose output is
//! being redirected to a file.

fn main() {
    let caps = zyris_code::term::Caps::detect();

    println!("== capability guess ==");
    println!("hyperlinks (OSC 8)   {}", caps.hyperlinks);
    println!("clipboard  (OSC 52)  {}", caps.osc52);
    println!("mouse tracking       {}", caps.mouse);

    println!();
    println!("== what it was decided from ==");
    for key in [
        "TERM",
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "LC_TERMINAL",
        "COLORTERM",
        "WT_SESSION",
        "WT_PROFILE_ID",
        "ConEmuANSI",
        "SESSIONNAME",
        "OS",
        "NO_COLOR",
        "ZYRIS_CODE_HYPERLINKS",
        "ZYRIS_CODE_OSC52",
        "ZYRIS_CODE_MOUSE",
    ] {
        match std::env::var(key) {
            Ok(v) => println!("{key:<22} {v}"),
            Err(_) => println!("{key:<22} <unset>"),
        }
    }

    println!();
    println!("== terminal size ==");
    match crossterm::terminal::size() {
        Ok((w, h)) => println!("{w} columns x {h} rows"),
        Err(e) => println!("could not be read: {e}"),
    }

    // **The width question this app cannot answer for itself.** These are East Asian Ambiguous:
    // one column by the table, two in a terminal configured for CJK. Every one of them is drawn
    // by the UI, and a single column of drift corrupts the rest of the row for good — the cell
    // diff believes it is already right. Printed between markers so a human can see, on their own
    // screen, whether the closing marker sits where it should.
    println!();
    println!("== ambiguous-width characters ==");
    println!("Every ] below should sit in the SAME column. One further right means that");
    println!("character is drawn two columns wide here, and every cell after it shifts.");
    for (name, ch) in [
        ("dot         U+25CF", '●'),
        ("middle dot  U+00B7", '·'),
        ("h line      U+2500", '─'),
        ("v line      U+2502", '│'),
        ("diamond     U+25C6", '◆'),
        ("up arrow    U+2191", '↑'),
        ("ellipsis    U+2026", '…'),
        ("check       U+2713", '✓'),
        ("(ascii ref)       ", '*'),
    ] {
        println!("[{ch}] {name}");
    }
}
