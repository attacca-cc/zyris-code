//! How long each way of asking "does this terminal do the kitty keyboard protocol?" takes.
//!
//! Run it under a pty that never answers (`scripts/kbprobe.py`) — that is the case that matters,
//! because it is the one every ordinary terminal falls into and the one a timeout has to bound.

use std::time::Instant;

fn main() {
    // **The harness puts the pty in raw mode.** Taking it again here would go through
    // `/dev/tty`, which is not this pty unless the child also claimed it as its controlling
    // terminal — and then the slave stays canonical, so a reply with no newline in it never
    // completes a read and every run looks like a timeout.

    let start = Instant::now();
    let ours = zyris_code::app::probe_kitty_keyboard();
    let ours_took = start.elapsed();

    let start = Instant::now();
    let theirs = crossterm::terminal::supports_keyboard_enhancement();
    let theirs_took = start.elapsed();

    println!("ours   {ours:?} in {}ms", ours_took.as_millis());
    println!("theirs {theirs:?} in {}ms", theirs_took.as_millis());
}
