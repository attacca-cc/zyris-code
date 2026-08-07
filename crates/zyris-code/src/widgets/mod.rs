//! Drawing. Widgets receive state like props and only draw — no logic is run here.
//!
//! ```text
//! │   (conversation)                    │
//! │ ● working…                 Esc stop │ what's happening now
//! ├─ ~/zyris-code · ⎇ main +2 ~1 ───────┤ where tools run, and what git says (`repo::spans`)
//! │ > input                             │ input box (grows with content)
//! ├─────────────────────────────────────┤
//! │ base·Main Agent  · 크레딧 5% · 컨텍스트 66% (132.8k/200k) · 총 토큰 11.4M │ bottom bar — usage on the right
//! ```
//!
//! **That upper divider carries standing context, and only in the ordinary input state.** The
//! same row belongs to `ask`, which paints it `ACCENT` to say "this is a question" — context
//! there would blunt a signal doing real work.
//!
//! **The input box is clamped between lines above and below.** If a blank line separated them, the
//! status line would read as the box's heading and the bottom bar as its footer — the drawn lines show at a glance how far the box extends.
//!
//! **No blank lines.** One line was left between the conversation and the activity line, but it
//! only showed as unused space at the bottom of the screen. That one line goes to the conversation.
//!
//! There is no header. The app name and directory only need to be seen once, so there was no
//! reason for them to keep taking space — that is one more line for the conversation.

mod activity;
mod ask;
mod enroll;
mod input;
mod newproject;
mod panel;
mod picker;
mod status;
mod transcript;

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Modifier;
use ratatui::Frame;

use crate::app::State;
use crate::markdown::display_width;
use crate::selection;

pub fn draw(frame: &mut Frame, state: &mut State) {
    let full = frame.area();

    // The whole screen is the app's — with the sidebar gone there is no right column to carve out.
    let area = full;
    // The input box grows with its content. It never exceeds half the screen.
    //
    // **There is only one input slot.** When a question is open the question takes it.
    let input_h = match &state.asking {
        Some((_, a)) => ask::height(a, area.height.saturating_sub(3)).saturating_sub(1),
        None => {
            state.input.height(area.width.saturating_sub(2)).min((area.height / 2).max(1)).max(1)
        }
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),              // conversation
            Constraint::Length(1),           // what's happening now
            Constraint::Length(input_h + 1), // divider + input box
            Constraint::Length(1),           // divider
            Constraint::Length(1),           // bottom bar
        ])
        .split(area);

    transcript::draw(frame, chunks[0], state);
    activity::draw(frame, chunks[1], state);
    match &state.asking {
        Some((_, a)) => {
            // Moving a click to a row requires knowing this area.
            state.ask_area = Some(chunks[2]);
            ask::draw(frame, chunks[2], a, state.lang);
        }
        None => {
            state.ask_area = None;
            input::draw(frame, chunks[2], state);
        }
    }
    input::rule(frame, chunks[3]);
    status::draw(frame, chunks[4], state);

    // The picker overlaps at the very top — while it is open, that is the current task.
    if let Some(p) = &state.picker {
        picker::draw(frame, full, p, state.lang);
    }

    // **The new-project form is laid on top of the picker.** The picker stays below, so pressing Esc
    // to close returns to the same spot.
    if let Some(form) = &state.new_project {
        newproject::draw(frame, full, form, state.lang);
    }

    // **The enrollment code window overlaps on top of that.** Nothing else may be done while viewing
    // the code — key handling also gives it top priority (`on_key`).
    if let Some(view) = &state.enroll {
        enroll::draw(frame, full, view, state.lang);
    }

    // **The popup panel is drawn on top of everything.** It only opens from a slash
    // command, so nothing else is open underneath — it covers the conversation it
    // would otherwise have filled with text.
    if let Some(p) = &mut state.panel {
        panel::draw(frame, full, p, state.lang);
    }

    // **By default no background is painted** — the terminal uses its own. If the app painted, only the area
    // outside the grid (window padding, leftover pixels) would differ in color, making a band at the edges. See `theme::page_bg`.
    //
    // For someone who turned it back on, it is laid over all remaining cells. Every cell must have a background
    // so the ratatui diff clears trailing cells when full-width characters narrow.
    if let Some(bg) = crate::theme::page_bg() {
        for cell in frame.buffer_mut().content.iter_mut() {
            if cell.bg == ratatui::style::Color::Reset {
                cell.bg = bg;
            }
        }
    }

    // **Self-healing frame: force every cell out again.** Overwriting without clearing removes the ghost of
    // trailing cells behind full-width characters — there is no clear, so nothing flickers.
    // `AlwaysUpdate` bypasses the diff's equality comparison and puts every cell of this one frame on
    // the wire. The next draw returns to a fresh buffer (option `None`) and becomes a normal diff.
    if std::mem::take(&mut state.force_update) {
        use ratatui::buffer::CellDiffOption;
        for cell in frame.buffer_mut().content.iter_mut() {
            cell.set_diff_option(CellDiffOption::AlwaysUpdate);
        }
    }
    // **Blank-only heal while a turn runs.** Residue always hides on blank cells, and a
    // space is safe to overlap with anything — on a slow SSH link it can never show the
    // same word twice the way a full rewrite did. The diff skips the cell right after a
    // wide character, so the blank pass never writes under one.
    if std::mem::take(&mut state.force_update_blank) {
        use ratatui::buffer::CellDiffOption;
        for cell in frame.buffer_mut().content.iter_mut() {
            if cell.symbol() == " " {
                cell.set_diff_option(CellDiffOption::AlwaysUpdate);
            }
        }
    }

    // **Mouse selection covers the whole screen — blank space included.** Invert every cell
    // under the drag so the selection is visible everywhere: the transcript, the status
    // line, the enrollment window. This runs after every widget drew and before the
    // frame is flushed, so the highlight rides the same diff as the content.
    if let Some(drag) = state.drag.filter(|d| !d.is_click()) {
        if let Some((r0, c0, r1, c1)) =
            selection::rect(&drag, frame.area().width, frame.area().height)
        {
            let width = frame.area().width as usize;
            let cells = frame.buffer_mut().content.as_mut_slice();
            for y in r0..=r1 {
                for x in c0..=c1 {
                    let idx = y as usize * width + x as usize;
                    if let Some(cell) = cells.get_mut(idx) {
                        cell.modifier.insert(Modifier::REVERSED);
                    }
                }
            }
        }
    }

    // **Snapshot the visible text for mouse selection.** A drag extracts from this, so it
    // must always match the frame that is about to be shown. Cheap: a few thousand string
    // appends per frame against a full terminal redraw.
    state.screen = screen_rows(frame.buffer_mut());

    // **Make links Ctrl+clickable.** The terminal opens an OSC 8 hyperlink on Ctrl+click, so
    // the cells under a link get the hyperlink escape sequence. Runs after the `screen`
    // snapshot — the snapshot must hold plain text, not escape sequences, for mouse selection.
    // The diff sees the escaped symbols, so a changed link rewrites its cells and a removed
    // one reverts to plain.
    inject_links(frame, state);
}

/// Wraps the cells under each visible link in an OSC 8 hyperlink sequence, so the terminal
/// opens the URL on Ctrl+click.
///
/// The escape sequence becomes part of the cell symbol. `ForcedWidth` tells the diff how many
/// columns the cell really occupies — without it, the escape bytes would inflate the computed
/// width and the diff would misplace every following cell. A wide glyph advances the column by
/// its own width (2), so its empty placeholder cell is never visited and never written over.
fn inject_links(frame: &mut Frame, state: &State) {
    use ratatui::buffer::CellDiffOption;
    use std::num::NonZeroU16;

    if state.view_links.is_empty() {
        return;
    }
    let (ox, oy) = state.view_origin;
    for (i, links) in state.view_links.iter().enumerate() {
        let y = oy + i as u16;
        for link in links {
            let open = format!("\x1b]8;;{}\x1b\\", link.url);
            let mut col = link.start;
            while col < link.end {
                let x = ox + col as u16;
                let Some(cell) = frame.buffer_mut().cell_mut((x, y)) else { break };
                let sym = cell.symbol().to_string();
                let w = crate::markdown::display_width(&sym).max(1) as u16;
                // Wide glyphs occupy two buffer cells (the second is an empty placeholder).
                // Advancing by the glyph's own width skips that placeholder, so it is never
                // written over — the wide char's trailing column stays intact.
                cell.set_symbol(&format!("{open}{sym}\x1b]8;;\x1b\\"));
                cell.set_diff_option(CellDiffOption::ForcedWidth(
                    NonZeroU16::new(w).expect("a glyph is at least one column wide"),
                ));
                col += w as usize;
            }
        }
    }
}

/// The visible text of the frame, one row per screen line. Mouse selection reads from this —
/// dragging anywhere extracts what the cursor covers.
fn screen_rows(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    let width = buffer.area.width as usize;
    let mut rows = vec![String::new(); buffer.area.height as usize];
    for (y, row) in rows.iter_mut().enumerate() {
        let cells = &buffer.content[y * width..(y + 1) * width];
        let mut i = 0;
        while i < cells.len() {
            let sym = cells[i].symbol();
            row.push_str(sym);
            // A full-width glyph occupies two cells; the second is a placeholder with an
            // empty symbol, which `symbol()` reports as a space. Consuming it by position
            // keeps the glyph whole — extraction measures columns with `display_width` anyway.
            if display_width(sym) >= 2 {
                i += 2;
            } else {
                i += 1;
            }
        }
    }
    rows
}

/// What appears on the activity line. It takes a time so tests can pin the elapsed time and inspect.
pub fn activity_parts_at(
    state: &State,
    now: std::time::Instant,
) -> (ratatui::style::Color, String, &'static str) {
    activity::parts_at(state, now)
}

/// Which row this y coordinate is on in the question screen. Used by click handling.
pub fn ask_row_at(
    a: &crate::question::Answering,
    area: ratatui::layout::Rect,
    y: u16,
) -> Option<usize> {
    ask::row_at(a, area, y)
}
