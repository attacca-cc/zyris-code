//! The card that shows what a job came to. **It takes the input field's spot**, the way the
//! question card does — the turn is over, and this is the sentence the agent wrote to say what
//! came of it.
//!
//! **Nothing here is cut.** A report is somebody's own summary of their own work; it is often a
//! paragraph. It wraps, and the height is the number of lines the wrapping produced. If even that
//! does not fit the room the card is given, the top is shown and the hint says how much is below —
//! the whole sentence is on the timeline above besides, and that scrolls.
//!
//! **One function lays it out** (`lines`), and both the drawing and the height go through it, so
//! the two cannot come apart.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::report::Report;
use crate::theme;
use crate::wrap;

/// The card's colour: green for work that came out, red for work that did not.
fn colour(ok: bool) -> ratatui::style::Color {
    if ok {
        theme::success()
    } else {
        theme::danger()
    }
}

/// The lines the card draws at this width, in at most `max` of them.
pub fn lines(r: &Report, width: u16, max: usize, lang: crate::lang::Lang) -> Vec<Line<'static>> {
    let w = width as usize;
    // Below three there is no room for the rule, one line and the hint.
    let max = max.max(3);
    let body = wrap::line(
        Line::from(Span::styled(r.summary.clone(), Style::default().fg(theme::text()))),
        // Two columns of indent, so the sentence reads as the body under the head.
        w.saturating_sub(2),
    );
    // The rule, the head and the hint take three of the rows.
    let room = max.saturating_sub(3);
    let shown = body.len().min(room);
    let mut hint = lang.report_keys().to_string();
    if body.len() > shown {
        hint.push_str(&lang.pick_more(false, body.len() - shown));
    }

    let mut out = vec![Line::from(Span::styled("─".repeat(w), Style::default().fg(colour(r.ok))))];
    out.push(Line::from(vec![
        Span::styled("■ ", Style::default().fg(colour(r.ok))),
        Span::styled(
            lang.report_head(r.ok),
            Style::default().fg(colour(r.ok)).add_modifier(Modifier::BOLD),
        ),
    ]));
    for line in body.into_iter().take(shown) {
        let mut spans = vec![Span::styled("  ", Style::default().fg(theme::text_muted()))];
        spans.extend(line.spans);
        out.push(Line::from(spans));
    }
    out.push(Line::from(Span::styled(hint, Style::default().fg(theme::text_muted()))));
    out
}

/// How many lines the card wants, never more than the caller can give it.
pub fn height(r: &Report, width: u16, max: u16, lang: crate::lang::Lang) -> u16 {
    lines(r, width, max as usize, lang).len() as u16
}

pub fn draw(frame: &mut Frame, area: Rect, r: &Report, lang: crate::lang::Lang) {
    let rows = lines(r, area.width, area.height as usize, lang);
    frame.render_widget(Paragraph::new(rows), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn report(ok: bool, summary: &str) -> Report {
        Report { seq: 1, ok, summary: summary.into() }
    }

    /// Every row of the screen, wide characters not double-counted.
    fn screen(r: &Report, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        let rows = height(r, w, h, crate::lang::Lang::Ko) as usize;
        let area = Rect { x: 0, y: 0, width: w, height: rows as u16 };
        terminal.draw(|f| draw(f, area, r, crate::lang::Lang::Ko)).expect("draw");
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0u16;
                while x < w {
                    let symbol = buf[(x, y)].symbol();
                    row.push_str(symbol);
                    x += crate::markdown::display_width(symbol).max(1) as u16;
                }
                row
            })
            .collect()
    }

    /// **The report reaches the screen whole, and it is not cut.** A paragraph is the ordinary
    /// shape of one, and the card is where it is read.
    #[test]
    fn a_long_report_arrives_whole() {
        let r = report(
            true,
            "빌드가 통과했습니다. 변경한 파일은 셋이고 테스트는 전부 다시 돌렸습니다. 남은 것은 도구 상세의 \
             접힘 기본값을 정하는 일입니다.",
        );
        let shown = screen(&r, 60, 40).join("\n");
        for word in r.summary.split_whitespace() {
            assert!(shown.contains(word), "the report lost {word:?}:\n{shown}");
        }
        assert!(shown.contains("작업 결과"), "{shown}");
        assert!(shown.contains("성공"), "{shown}");
        assert!(!shown.contains('…'), "{shown}");
    }

    /// **A failure wears the other colour and says so in words.** Colour alone is not a message.
    #[test]
    fn a_failure_says_failed_in_words_and_in_colour() {
        let r = report(false, "테스트가 깨졌습니다.");
        let rows = lines(&r, 40, 10, crate::lang::Lang::Ko);
        let head = rows.iter().find(|l| l.to_string().contains("작업 결과")).expect("a head");
        assert!(head.to_string().contains("실패"), "{head:?}");
        let colour = head.spans.iter().find(|s| s.content.contains("작업 결과")).unwrap().style.fg;
        assert_eq!(colour, Some(theme::danger()));
    }

    /// The card answers to one key, and it says which.
    #[test]
    fn the_card_says_how_to_put_it_away() {
        let r = report(true, "끝났습니다.");
        let shown = screen(&r, 40, 10).join("\n");
        assert!(shown.contains("Esc 닫기"), "{shown}");
    }

    /// **The height the card asks for is the height it draws.** The layout hands it exactly that
    /// many rows, so a disagreement here would leave the last line off the bottom.
    #[test]
    fn the_height_it_asks_for_is_the_height_it_draws() {
        for summary in ["짧은 결과입니다.", "가".repeat(120).as_str()] {
            let r = report(true, summary);
            for max in [6u16, 12, 40] {
                let want = height(&r, 40, max, crate::lang::Lang::Ko) as usize;
                let rows = lines(&r, 40, max as usize, crate::lang::Lang::Ko);
                assert_eq!(rows.len(), want, "height and lines disagree for {max} rows");
                assert!(want <= max.max(3) as usize, "{want} rows asked for of {max}");
            }
        }
    }

    /// A report too long for the room it is given keeps its top and says how much is below — the
    /// whole sentence is on the timeline above, and that scrolls.
    #[test]
    fn a_report_that_does_not_fit_says_how_much_is_below() {
        let r = report(true, &"가나다라마바사아자차".repeat(8));
        let rows = lines(&r, 20, 6, crate::lang::Lang::Ko);
        assert_eq!(rows.len(), 6, "{:?}", rows.iter().map(|l| l.to_string()).collect::<Vec<_>>());
        let hint = rows.last().expect("a hint").to_string();
        assert!(hint.contains('↓'), "nothing says what is missing: {hint}");
    }
}
