//! 진단용. 화면을 떠서 **어느 칸에 무슨 배경색이 깔리는지**를 그림으로 찍는다.
//!
//! 여백이 어디서 생기는지, 배경이 안 깔린 칸이 있는지는 눈으로 봐야 안다 — 셀 단언은
//! "무엇이 어느 줄에 있는가"만 보므로 색과 빈칸을 못 잡는다.
//!
//! ```bash
//! cargo run -p zyris-code --example screen_dump            # 100x30
//! cargo run -p zyris-code --example screen_dump -- 60 20   # 크기를 정해서
//! ```
//!
//! 기호: `.`=배경 없음(터미널 기본이 샌다) `#`=페이지 배경 `U`=사용자 밴드 `?`=그 밖

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
                c if c == zyris_code::theme::BG => '#',
                c if c == zyris_code::theme::USER_BG => 'U',
                _ => '?',
            });
            let symbol = cell.symbol();
            text.push(if symbol == " " { ' ' } else { symbol.chars().next().unwrap_or(' ') });
        }
        println!("{y:>3} |{bg}| {}", text.trim_end());
    }
}
