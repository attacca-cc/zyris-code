//! The popup panel — the centered box `/mode`·`/mcp`·`/skills`·`/plugin`·`/account`·`/status`
//! open instead of dumping a wall of text into the conversation.
//!
//! **Drawing only.** The lines come pre-styled from `crate::panel`; keys only scroll
//! (`Action::PanelScroll`) or close (`Action::PanelClose`).

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::markdown::display_width;
use crate::panel::Panel;
use crate::theme;
use crate::wrap;

pub fn draw(frame: &mut Frame, area: Rect, panel: &mut Panel, lang: crate::lang::Lang) {
    let has_button = panel.button.is_some();
    // **A panel answers to different keys depending on what it is, so it says which.** Settled
    // here, before the width, because the hint is a line the box must be wide enough for.
    // **A manager says what its own keys are.** They are not scroll-and-close — `d` takes
    // something away — and the hint is the only place that says so.
    // **An open form has its own keys again.** While it is up, letters go into a field, so the
    // list's hint would be a lie.
    let keys = match panel.manager.as_ref() {
        Some(manager) if manager.form.is_some() => lang.add_keys(),
        Some(manager) => lang.manager_keys(manager.kind),
        None => match (
            panel.form.map(|f| f.lang),
            panel.mode_pick.is_some(),
            has_button,
            panel.button_focused,
        ) {
            (Some(draft), _, _, _) => draft.form_keys(),
            (None, true, _, _) => lang.mode_pick_keys().to_string(),
            (None, false, true, true) => lang.panel_keys_button_focused(),
            (None, false, true, false) => lang.panel_keys_button(),
            (None, false, false, _) => lang.panel_keys(),
        },
    };
    // **The foot is measured too, and it is the foot that decides.** `Panel::foot` holds every
    // sentence the panel can show, so the box is one size whichever row the cursor is on.
    let (content, foot) = panel.body_and_foot();
    let widest = content
        .iter()
        .chain(panel.foot.iter())
        .map(|line| line.spans.iter().map(|s| display_width(&s.content)).sum::<usize>())
        // The title is drawn with a space either side of it, inside the border.
        .chain(std::iter::once(display_width(&panel.title) + 4))
        .chain(std::iter::once(display_width(&keys)))
        .max()
        .unwrap_or(0);
    // Never wider than the screen: a floor above it drew the box past a narrow terminal's edge.
    let w = (widest as u16 + 4).min(area.width.saturating_sub(4)).max(20.min(area.width));
    let inner_w = w.saturating_sub(2) as usize;
    // **The key hint wraps like everything else in the box.** It was one line cut at the border,
    // and on a narrow terminal the half that said what `d` and `u` do was the half that went.
    let hint = wrap::line(Line::from(Span::styled(keys, theme::muted())), inner_w);
    // The rows the foot keeps, whichever sentence is up. **A box that resizes under the keys is
    // what all of this exists to prevent** — that is why `foot` holds every sentence, not one.
    let foot_rows =
        panel.foot.iter().map(|line| wrap::line(line.clone(), inner_w).len()).max().unwrap_or(0);
    let mut body: Vec<Line<'static>> =
        content.iter().cloned().flat_map(|line| wrap::line(line, inner_w)).collect();
    if let Some(foot) = foot {
        // **Blank rows rather than a shorter box.** A sentence of one line and a sentence of two
        // must not give two different heights.
        let shown = wrap::line(foot.clone(), inner_w).len();
        for _ in shown..foot_rows {
            body.push(Line::from(""));
        }
        body.extend(wrap::line(foot.clone(), inner_w));
    }
    // The box grows with the content, never taller than four fifths of the screen.
    // A button adds its own row between the body and the hint.
    let fixed = hint.len() as u16 + u16::from(has_button);
    let want_h = (body.len() as u16).saturating_add(2 + fixed).max(5);
    let h = want_h.min(area.height.saturating_mul(4) / 5).max(3);
    let inner = super::overlay(frame, area, w, h, &panel.title);

    let width = inner.width as usize;
    // The last rows are the hint; the button (when present) sits above it; everything above that
    // is body.
    let body_rows = inner.height.saturating_sub(fixed) as usize;

    // **The cursor stays in view.** A list with a cursor (`❯ `) — a manager, `/mode`, a form — is
    // moved with the arrows, which never scroll, and the box is capped at four fifths of the
    // screen: on a short terminal the cursor walked off the bottom and Enter acted on a row
    // nobody could see.
    let has_cursor = panel.manager.is_some() || panel.mode_pick.is_some() || panel.form.is_some();
    let at = body.iter().position(|l| l.spans.first().is_some_and(|s| s.content == "❯ "));
    if let (true, Some(at)) = (has_cursor, at) {
        panel.scroll = scroll_to_show(panel.scroll, at, body_rows);
    }

    // Clamp the scroll to what actually fits, then draw that window. **The count is of drawn
    // lines** — one of the panel's own lines may have wrapped into several — so it comes from
    // `body`, not from `panel.lines`.
    let max = crate::panel::max_scroll(body.len(), body_rows);
    if panel.scroll > max {
        panel.scroll = max;
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    for line in body.iter().skip(panel.scroll).take(body_rows) {
        lines.push(line.clone());
    }
    while lines.len() < body_rows {
        lines.push(Line::from(""));
    }
    if let Some(button) = panel.button {
        lines.push(button_line(button, panel.button_focused, lang, width));
    }
    lines.extend(hint);

    frame.render_widget(Paragraph::new(lines), inner);
}

/// The scroll that keeps line `at` inside a window of `rows` lines, moving `scroll` as little as it
/// can.
fn scroll_to_show(scroll: usize, at: usize, rows: usize) -> usize {
    if at < scroll {
        at
    } else if rows > 0 && at >= scroll + rows {
        at + 1 - rows
    } else {
        scroll
    }
}

/// The button row — `[ 로그아웃 ]` when resting, marked `▶ … ◀` and accented
/// when focused. Centered, like a dialog's button.
fn button_line(
    button: crate::panel::PanelButton,
    focused: bool,
    lang: crate::lang::Lang,
    width: usize,
) -> Line<'static> {
    let label = match button {
        crate::panel::PanelButton::Logout => lang.acc_logout_button(),
    };
    let text = if focused { format!("▶ [ {label} ] ◀") } else { format!("[ {label} ]") };
    let pad = " ".repeat(width.saturating_sub(display_width(&text)) / 2);
    // **At rest it is ordinary text; the warning comes when it is about to be pressed.** Painting
    // it `danger()` while nothing had happened made a button that was merely sitting there look
    // like something had already gone wrong. Now the red arrives exactly when it means something:
    // this is the key that logs you out.
    let style = if focused {
        Style::default().fg(theme::danger()).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::text())
    };
    Line::from(Span::styled(format!("{pad}{text}"), style))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Every body line, wrapped the way the widget wraps them.
    fn body(panel: &Panel, w: u16) -> Vec<String> {
        panel
            .lines
            .iter()
            .cloned()
            .flat_map(|line| wrap::line(line, w.saturating_sub(2) as usize))
            .map(|line| line.to_string())
            .collect()
    }

    /// Renders the box and gives back every row of the screen, wide characters not double-counted.
    fn render(panel: &mut Panel, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal.draw(|f| draw(f, f.area(), panel, crate::lang::Lang::Ko)).expect("draw");
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0u16;
                while x < w {
                    let symbol = buf[(x, y)].symbol();
                    row.push_str(symbol);
                    x += display_width(symbol).max(1) as u16;
                }
                row
            })
            .collect()
    }

    /// **A long line wraps; it does not end in `…`.** A mode's description is the box's whole
    /// reason for being up, and it used to be cut at the right border.
    #[test]
    fn a_long_line_wraps_instead_of_ending_in_an_ellipsis() {
        let long = "가".repeat(30);
        let panel = Panel::new("t".into(), vec![Line::from(long.clone())]);
        let rows = body(&panel, 40);
        assert!(rows.len() > 1, "nothing wrapped: {rows:?}");
        assert!(!rows.iter().any(|r| r.contains('…')), "{rows:?}");
        assert_eq!(rows.concat(), long, "characters went missing: {rows:?}");
        assert!(rows.iter().all(|r| display_width(r) <= 38), "{rows:?}");
    }

    /// A line that already fits is untouched — nothing reflows that had nothing wrong with it.
    #[test]
    fn a_line_that_fits_is_untouched() {
        let panel = Panel::new("t".into(), vec![Line::from("안녕")]);
        assert_eq!(body(&panel, 40), vec!["안녕".to_string()]);
    }

    /// **The box is sized from the wrapped lines, so the tail is on screen.** A box sized from the
    /// lines the panel held would drop the wrapped rows past its bottom — the same loss as the
    /// `…`, only harder to notice.
    #[test]
    fn the_end_of_a_wrapped_line_reaches_the_screen() {
        let mut panel = Panel::new("t".into(), vec![Line::from("가".repeat(30))]);
        let screen = render(&mut panel, 80, 24).join("\n");
        let drawn = screen.chars().filter(|c| *c == '가').count();
        assert_eq!(drawn, 30, "the box dropped the end of the line:\n{screen}");
        assert!(!screen.contains('…'), "{screen}");
    }

    /// A very tall panel still stops at four fifths of the screen and scrolls the rest.
    #[test]
    fn a_panel_taller_than_the_screen_stops_short_and_scrolls() {
        let mut panel =
            Panel::new("t".into(), (0..80).map(|i| Line::from(format!("row {i}"))).collect());
        let screen = render(&mut panel, 80, 24).join("\n");
        assert!(screen.contains("row 0"), "{screen}");
        assert!(!screen.contains("row 79"), "the box outgrew the screen:\n{screen}");
        panel.scroll = 1000;
        let screen = render(&mut panel, 80, 24).join("\n");
        assert!(screen.contains("row 79"), "scrolling past the end lost the last row:\n{screen}");
    }

    /// **The cursor stays on screen on a short terminal.** The arrows move it without scrolling,
    /// so it used to walk off the bottom of a capped box and Enter acted on a row out of sight.
    #[test]
    fn the_cursor_row_stays_in_view_on_a_short_terminal() {
        let last = *crate::mode::Mode::ALL.last().expect("modes");
        let mut panel =
            crate::panel::mode(crate::lang::Lang::Ko, crate::mode::Mode::Normal, Some(last));
        let screen = render(&mut panel, 60, 9);
        assert!(screen.iter().any(|r| r.contains('❯')), "the cursor is off screen:\n{screen:#?}");
    }

    /// **The key hint wraps rather than being cut at the border.**
    #[test]
    fn the_key_hint_wraps_on_a_narrow_terminal() {
        let mut panel = crate::panel::mode(crate::lang::Lang::En, crate::mode::Mode::Normal, None);
        let keys = crate::lang::Lang::Ko.mode_pick_keys();
        let screen = render(&mut panel, 30, 40).join(" ");
        let last = keys.split_whitespace().last().expect("a hint");
        assert!(screen.contains(last), "{last:?} was cut from the hint:\n{screen}");
    }

    /// The box the widget drew: its top and bottom row, and its left and right column.
    fn box_rect(screen: &[String]) -> (usize, usize, usize, usize) {
        let top = screen.iter().position(|r| r.contains('┌')).expect("no box was drawn");
        let bottom = screen.iter().rposition(|r| r.contains('└')).expect("no box was drawn");
        let left = screen[top].find('┌').expect("no left corner");
        let right = screen[top].rfind('┐').expect("no right corner");
        (top, bottom, left, right)
    }

    /// **The box does not resize as the cursor moves.** `/mode`'s sentence used to be what the box
    /// was sized from, so moving the cursor onto a mode with a longer sentence grew the window —
    /// and moved it, because a panel is centred. Every sentence is kept now (`Panel::foot`) and the
    /// box is one size whichever one is up.
    #[test]
    fn the_mode_box_is_one_size_on_every_row() {
        let rects: Vec<_> = crate::mode::Mode::ALL
            .iter()
            .map(|m| {
                let mut panel =
                    crate::panel::mode(crate::lang::Lang::Ko, crate::mode::Mode::Normal, Some(*m));
                box_rect(&render(&mut panel, 100, 24))
            })
            .collect();
        assert!(rects.windows(2).all(|w| w[0] == w[1]), "the box moved: {rects:?}");
    }

    /// `/config` is the other panel whose sentence changes under the cursor — one per setting and
    /// value. `↑↓` and `←→` must leave the box exactly where it was.
    #[test]
    fn the_config_box_is_one_size_on_every_row_and_value() {
        let mut panel =
            crate::panel::config(crate::lang::Lang::Ko, crate::config::Config::default());
        let mut rects = Vec::new();
        for row in 0..crate::panel::Form::ROWS.len() {
            if crate::panel::Form::ROWS[row] == crate::panel::Setting::Language {
                // **The language row is left out on purpose.** Choosing a language re-letters the
                // whole box on the spot — its title, its hint and every sentence are then in the
                // other language, so a different size there is the point, not a fault.
                continue;
            }
            panel.form.as_mut().expect("the config panel carries a form").cursor = row;
            for _ in 0..crate::panel::Form::ROWS[row].count() {
                panel.refresh();
                rects.push(box_rect(&render(&mut panel, 100, 24)));
                panel.form.as_mut().expect("the config panel carries a form").shift(1);
            }
        }
        assert!(rects.windows(2).all(|w| w[0] == w[1]), "the box moved: {rects:?}");
    }
}
