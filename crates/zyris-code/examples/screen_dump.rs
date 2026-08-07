//! Diagnostic. Dumps the screen and renders **which background color lands in which cell** as a picture.
//!
//! Where the margins come from and whether any cell misses its background can only be seen by eye —
//! cell assertions only see "what is on which line", so they miss colors and blanks.
//!
//! ```bash
//! cargo run -p zyris-code --example screen_dump            # 100x30
//! cargo run -p zyris-code --example screen_dump -- 60 20   # with a chosen size
//! ```
//!
//! Legend: `.`=no background (terminal default shows through) `#`=page background `U`=user band `?`=other

use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;
use zyris_code::app::{apply, Action, State};
use zyris_code::widgets;

fn main() {
    let mut args = std::env::args().skip(1);
    let w: u16 = args.next().and_then(|a| a.parse().ok()).unwrap_or(100);
    let h: u16 = args.next().and_then(|a| a.parse().ok()).unwrap_or(30);

    let mut state = State::new();
    state.connected = true;
    state.timeline.say("**안녕하세요.** 화면을 뜨는 중입니다.");
    for c in "여백을 봅니다".chars() {
        apply(&mut state, &Action::Insert(c));
    }

    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| widgets::draw(f, &mut state)).unwrap();
    let buf = term.backend().buffer().clone();

    println!("{w}x{h}\n");
    for y in 0..h {
        let mut bg = String::new();
        let mut text = String::new();
        for x in 0..w {
            let cell = &buf[(x, y)];
            bg.push(match cell.bg {
                Color::Reset => '.',
                c if c == zyris_code::theme::bg() => '#',
                c if c == zyris_code::theme::user_bg() => 'U',
                _ => '?',
            });
            let symbol = cell.symbol();
            text.push(if symbol == " " { ' ' } else { symbol.chars().next().unwrap_or(' ') });
        }
        println!("{y:>3} |{bg}| {}", text.trim_end());
    }
}
