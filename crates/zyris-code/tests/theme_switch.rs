//! **A theme switch repaints what is already on screen.**
//!
//! A test binary of its own because it changes the process-wide theme, which every other screen
//! test reads while it draws; alone in its binary it races nothing.

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use zyris_code::app::{apply, Action, Frame as AppFrame, State};
use zyris_code::config::ThemeChoice;
use zyris_code::event::{Entry, EntryKind};
use zyris_code::theme::{self, Theme};
use zyris_code::widgets;

/// The foreground of the first cell of `needle` on screen.
fn fg_of(state: &mut State, needle: &str) -> Option<ratatui::style::Color> {
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| widgets::draw(f, state)).unwrap();
    let buf = term.backend().buffer().clone();
    for y in 0..buf.area.height {
        let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
        if let Some(at) = row.find(needle) {
            return buf[(at as u16, y)].style().fg;
        }
    }
    None
}

/// The rows already drawn were made in the old palette and cached; keyed on width alone, they
/// stayed pale-on-white (1.2:1) after a switch to the light theme until each one changed.
#[test]
fn a_theme_switch_repaints_rows_already_drawn() {
    let mut s = State::new();
    let entry = Some(Entry { id: None, seq: 1, kind: EntryKind::Agent("answer".into()) });
    apply(&mut s, &Action::Frame(AppFrame::Event { cursor: 1, entry, todo: None, plan: None }));

    theme::set(Theme::Dark);
    let dark = fg_of(&mut s, "answer");
    theme::set(Theme::Light);
    let light = fg_of(&mut s, "answer");
    theme::set(Theme::Dark);

    assert_eq!(dark, Some(theme::palette(ThemeChoice::Dark).text()));
    assert_eq!(light, Some(theme::palette(ThemeChoice::Light).text()));
}
