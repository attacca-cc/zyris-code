//! 마크다운을 화면 줄로 옮긴다.
//!
//! 파싱은 `pulldown-cmark`가 하고 **레이아웃은 직접** 한다 — 폭 기반 줄바꿈 +
//! 전각 2칸 + 코드펜스 테두리를 한 번에 주는 크레이트가 없다.
//!
//! **스트리밍 중 미완성 마크다운을 위한 부분 파서를 만들지 않는다.** 델타가 올 때마다
//! 누적 문자열을 통째로 다시 파싱한다. 코드펜스가 안 닫혔으면 pulldown-cmark가 나머지를
//! 코드로 취급하므로 "열린 코드블록"이 저절로 자연스럽게 나온다.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme;

/// 화면에서 차지하는 칸 수. 전각은 2칸이다.
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

pub fn render(src: &str, width: u16) -> Vec<Line<'static>> {
    let owned;
    let src = match optimistic_table(src) {
        Some(fixed) => {
            owned = fixed;
            owned.as_str()
        }
        None => src,
    };
    let width = width.max(8) as usize;
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut buf: Vec<Span<'static>> = Vec::new();
    let mut style = Style::default().fg(theme::TEXT);
    let mut in_code = false;
    let mut list_depth: usize = 0;
    let mut table: Option<Table> = None;

    let parser = Parser::new_ext(src, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    for event in parser {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                style = Style::default().fg(theme::TEXT_HEADING).add_modifier(Modifier::BOLD);
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(&mut out, &mut buf, width, "");
                style = Style::default().fg(theme::TEXT);
            }
            Event::Start(Tag::Emphasis) => style = style.add_modifier(Modifier::ITALIC),
            Event::End(TagEnd::Emphasis) => style = style.remove_modifier(Modifier::ITALIC),
            Event::Start(Tag::Strong) => {
                style = style.fg(theme::TEXT_HEADING).add_modifier(Modifier::BOLD)
            }
            Event::End(TagEnd::Strong) => style = Style::default().fg(theme::TEXT),
            Event::Start(Tag::BlockQuote(_)) => style = Style::default().fg(theme::TEXT_MUTED),
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(&mut out, &mut buf, width, "│ ");
                style = Style::default().fg(theme::TEXT);
            }
            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::End(TagEnd::List(_)) => list_depth = list_depth.saturating_sub(1),
            Event::Start(Tag::Item) => {
                buf.push(Span::styled(
                    format!("{}· ", "  ".repeat(list_depth.saturating_sub(1))),
                    Style::default().fg(theme::ACCENT),
                ));
            }
            Event::End(TagEnd::Item) => flush(&mut out, &mut buf, width, "  "),
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(&mut out, &mut buf, width, "");
                let lang = match &kind {
                    CodeBlockKind::Fenced(l) if !l.is_empty() => l.to_string(),
                    _ => String::new(),
                };
                out.push(Line::from(Span::styled(
                    format!("┌─ {lang} "),
                    Style::default().fg(theme::BORDER_LIGHT),
                )));
                in_code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                out.push(Line::from(Span::styled("└─", Style::default().fg(theme::BORDER_LIGHT))));
                in_code = false;
            }
            Event::Code(t) => {
                // 표 안이면 셀로, 아니면 본문으로.
                if let Some(tb) = &mut table {
                    tb.cell.push_str(&t);
                } else {
                    buf.push(Span::styled(t.to_string(), Style::default().fg(theme::ACCENT)));
                }
            }
            Event::Text(t) if in_code => {
                for raw in t.lines() {
                    out.push(Line::from(vec![
                        Span::styled("│ ", Style::default().fg(theme::BORDER_LIGHT)),
                        Span::styled(raw.to_string(), Style::default().fg(theme::TEXT)),
                    ]));
                }
            }
            // --- 표 ---------------------------------------------------------
            // 셀 안의 텍스트를 모았다가 표가 끝날 때 폭을 맞춰 한 번에 그린다. 열 폭은
            // 모든 행을 봐야 정해지므로 도중에 그릴 수 없다.
            Event::Start(Tag::Table(_)) => {
                flush(&mut out, &mut buf, width, "");
                table = Some(Table::default());
            }
            Event::End(TagEnd::Table) => {
                if let Some(t) = table.take() {
                    out.extend(t.render(width));
                }
            }
            Event::Start(Tag::TableHead) => {
                if let Some(t) = &mut table {
                    t.in_head = true;
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(t) = &mut table {
                    t.end_row();
                    t.in_head = false;
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(t) = &mut table {
                    t.end_row();
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(t) = &mut table {
                    t.end_cell();
                }
            }
            Event::Text(t) if table.is_some() => {
                if let Some(tb) = &mut table {
                    tb.cell.push_str(&t);
                }
            }
            Event::Text(t) => buf.push(Span::styled(t.to_string(), style)),
            Event::SoftBreak | Event::HardBreak => buf.push(Span::styled(" ", style)),
            Event::End(TagEnd::Paragraph) => flush(&mut out, &mut buf, width, ""),
            Event::Rule => out.push(Line::from(Span::styled(
                "─".repeat(width),
                Style::default().fg(theme::BORDER),
            ))),
            _ => {}
        }
    }
    flush(&mut out, &mut buf, width, "");
    out
}

/// 스트리밍 중 아직 구분선이 안 온 표를 **미리 표로 그린다.**
///
/// 마크다운은 `|---|---|` 줄이 와야 표인 줄 알 수 있다. 그래서 그 줄이 도착하기 전까지는
/// 파이프가 그대로 글자로 보이다가 갑자기 표로 바뀐다 — 흐르는 답변에서 눈에 거슬린다.
/// 끝에 파이프로 시작하는 줄만 쌓여 있으면 구분선을 지어내 넣어 미리 표로 만든다.
///
/// 진짜 구분선이 뒤따라 오면 그때는 이 함수가 아무 일도 하지 않고 원본이 그대로 파싱된다.
fn optimistic_table(src: &str) -> Option<String> {
    let joined;
    let src = match split_glued_rows(src) {
        Some(fixed) => {
            joined = fixed;
            joined.as_str()
        }
        None => src,
    };
    let lines: Vec<&str> = src.lines().collect();
    // 끝에서부터 파이프로 시작하는 줄이 몇 개인지 센다.
    let start = lines.iter().rposition(|l| !l.trim_start().starts_with('|')).map_or(0, |i| i + 1);
    let block = &lines[start..];
    if block.is_empty() {
        return None;
    }

    let is_delim = |l: &str| {
        let t = l.trim().trim_matches('|');
        !t.is_empty() && t.chars().all(|c| matches!(c, '-' | ':' | '|' | ' '))
    };

    // 헤더는 구분선이 아닌 첫 줄이다.
    let header = block[0];
    if is_delim(header) {
        return None; // 구분선만 덜렁 온 상태. 아직 표로 볼 근거가 없다.
    }
    // 첫 칸에 글자가 들어오기 전(`| `)에는 그릴 것이 없다. 빈 표를 세우는 편이 더 나쁘다.
    let inner = header.trim().trim_matches('|');
    if inner.trim().is_empty() {
        return None;
    }
    let cols = inner.split('|').count();

    // 뒤따르는 구분선이 **헤더와 열 수가 맞으면** 손대지 않는다. 안 맞으면(아직 오는 중)
    // 그 줄을 빼고 제대로 된 것을 끼워 넣는다 — 그대로 두면 표가 아니라 글로 떨어져
    // 표↔평문을 오가며 깜박인다.
    let delim_ok = block
        .get(1)
        .is_some_and(|l| is_delim(l) && l.trim().trim_matches('|').split('|').count() == cols);

    // **표 앞에는 빈 줄이 있어야 한다.** 앞 문장에 바로 붙으면 pulldown-cmark가 그 파이프
    // 줄을 문단의 연장으로 먹어 표가 되지 않는다 — 빈 줄이 오느냐에 따라 표↔글을 오간다.
    let needs_gap = start > 0 && !lines[start - 1].trim().is_empty();

    if delim_ok && !needs_gap {
        return None;
    }

    let body: Vec<&str> = block[1..].iter().copied().filter(|l| !is_delim(l)).collect();
    let _ = &body;
    let delim = format!("|{}", "---|".repeat(cols));

    let mut out: Vec<String> = lines[..start].iter().map(|s| s.to_string()).collect();
    if needs_gap {
        out.push(String::new());
    }
    out.push(header.to_string());
    // 진짜 구분선이 이미 제대로 왔으면 그것을 쓴다.
    out.push(if delim_ok { block[1].to_string() } else { delim });
    out.extend(body.iter().map(|s| s.to_string()));
    Some(out.join("\n"))
}

/// 한 줄에 붙어 버린 표 행을 떼어 놓는다.
///
/// 델타는 줄바꿈을 늦게 실어 오기도 한다. 그러면 `| 가 | 1 || 나 | 2 |`처럼 두 행이 한
/// 줄로 붙고, 열이 잔뜩 늘어난 아주 넓은 표가 그려진다 — 줄바꿈이 도착하면 정상으로
/// 돌아가고 다음 행에서 또 반복해 표가 깜박인다.
///
/// 진짜 빈 칸은 `| |`처럼 사이에 공백을 두고 쓰므로, 공백 없는 `||`는 붙은 자리로 본다.
fn split_glued_rows(src: &str) -> Option<String> {
    if !src.contains("||") {
        return None;
    }
    let mut changed = false;
    let out: Vec<String> = src
        .lines()
        .map(|line| {
            if !line.trim_start().starts_with('|') || !line.contains("||") {
                return line.to_string();
            }
            changed = true;
            line.split("||")
                .map(|part| {
                    let p = part.trim_end();
                    let p = p.strip_prefix('|').unwrap_or(p);
                    let p = p.strip_suffix('|').unwrap_or(p);
                    format!("|{p}|")
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    changed.then(|| out.join("\n"))
}

/// 표 하나를 다 모을 때까지 들고 있는 상태.
///
/// 열 폭은 **모든 행을 봐야** 정해지므로 도중에 그릴 수 없다. 그래서 셀 텍스트만 쌓아 두고
/// `End(Table)`에서 한 번에 그린다.
#[derive(Default)]
struct Table {
    head: Vec<String>,
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    in_head: bool,
}

impl Table {
    fn end_cell(&mut self) {
        self.row.push(std::mem::take(&mut self.cell).trim().to_string());
    }

    fn end_row(&mut self) {
        let row = std::mem::take(&mut self.row);
        if row.is_empty() {
            return;
        }
        if self.in_head {
            self.head = row;
        } else {
            self.rows.push(row);
        }
    }

    /// 열 폭을 맞춰 그린다. 폭이 모자라면 열을 **비례해서 줄이고**, 넘치는 셀은 `…`로 자른다.
    fn render(&self, width: usize) -> Vec<Line<'static>> {
        let cols = self.head.len().max(self.rows.iter().map(Vec::len).max().unwrap_or(0));
        if cols == 0 {
            return Vec::new();
        }

        // 각 열이 원하는 폭.
        let mut w = vec![0usize; cols];
        for r in std::iter::once(&self.head).chain(self.rows.iter()) {
            for (i, cell) in r.iter().enumerate().take(cols) {
                w[i] = w[i].max(display_width(cell));
            }
        }

        // 구분선과 여백이 먹는 칸: 열마다 "│ " + 마지막에 "│"
        let chrome = cols * 3 + 1;
        let budget = width.saturating_sub(chrome).max(cols);
        let total: usize = w.iter().sum();
        if total > budget {
            // 넓은 열부터 깎는다. 최소 한 칸은 남긴다.
            let mut over = total - budget;
            while over > 0 {
                let Some(i) = (0..cols).max_by_key(|&i| w[i]) else {
                    break;
                };
                if w[i] <= 1 {
                    break;
                }
                w[i] -= 1;
                over -= 1;
            }
        }

        let mut out = Vec::new();
        let border = |l: &str, m: &str, r: &str, w: &[usize]| {
            let mid: Vec<String> = w.iter().map(|n| "─".repeat(n + 2)).collect();
            Line::from(Span::styled(
                format!("{l}{}{r}", mid.join(m)),
                Style::default().fg(theme::BORDER_LIGHT),
            ))
        };

        out.push(border("┌", "┬", "┐", &w));
        if !self.head.is_empty() {
            out.push(row_line(&self.head, &w, theme::TEXT_HEADING, true));
            out.push(border("├", "┼", "┤", &w));
        }
        for r in &self.rows {
            out.push(row_line(r, &w, theme::TEXT, false));
        }
        out.push(border("└", "┴", "┘", &w));
        out
    }
}

/// 한 행. 셀을 폭에 맞춰 자르고 오른쪽을 공백으로 채운다.
fn row_line(cells: &[String], w: &[usize], fg: ratatui::style::Color, bold: bool) -> Line<'static> {
    let mut spans = Vec::new();
    let bar = Style::default().fg(theme::BORDER_LIGHT);
    let mut text = Style::default().fg(fg);
    if bold {
        text = text.add_modifier(Modifier::BOLD);
    }
    for (i, target) in w.iter().enumerate() {
        spans.push(Span::styled("│ ", bar));
        let cell = cells.get(i).map(String::as_str).unwrap_or("");
        let shown = truncate_to(cell, *target);
        let pad = target.saturating_sub(display_width(&shown));
        spans.push(Span::styled(shown, text));
        spans.push(Span::styled(" ".repeat(pad + 1), text));
    }
    spans.push(Span::styled("│", bar));
    Line::from(spans)
}

/// 폭을 넘지 않게 자른다. 자르면 마지막에 `…`를 둔다 — 잘렸다는 것이 보여야 한다.
fn truncate_to(s: &str, limit: usize) -> String {
    if display_width(s) <= limit {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        // `…`가 들어갈 한 칸을 남긴다.
        if display_width(&out) + display_width(&ch.to_string()) > limit.saturating_sub(1) {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// 모아 둔 span들을 폭에 맞춰 줄로 접는다. 전각은 2칸으로 센다.
fn flush(out: &mut Vec<Line<'static>>, buf: &mut Vec<Span<'static>>, width: usize, indent: &str) {
    if buf.is_empty() {
        return;
    }
    let indent_w = display_width(indent);
    let limit = width.saturating_sub(indent_w).max(1);
    let mut line: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;

    for span in buf.drain(..) {
        for word in split_keeping_spaces(&span.content) {
            let w = display_width(&word);
            if used + w > limit && used > 0 {
                out.push(Line::from(std::mem::take(&mut line)));
                used = 0;
                // 줄 첫머리로 넘어온 공백은 버린다 — 들여쓰기처럼 보인다.
                if word.trim().is_empty() {
                    continue;
                }
            }
            used += w;
            line.push(Span::styled(word, span.style));
        }
    }
    if !line.is_empty() {
        out.push(Line::from(line));
    }
}

/// 공백을 버리지 않고 단어 단위로 자른다 — 버리면 문장이 붙어 버린다.
fn split_keeping_spaces(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch == ' ' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.push(" ".to_string());
        } else {
            cur.push(ch);
            // 전각에는 단어 경계가 없다 — 글자마다 끊어야 폭을 넘지 않는다.
            if ch.len_utf8() > 1 && display_width(&cur) >= 2 {
                out.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    /// 전각은 2칸이다. 바이트 길이나 char 개수로 세면 한글에서 어긋난다.
    #[test]
    fn korean_counts_as_two_columns() {
        assert_eq!(display_width("한글"), 4);
        assert_eq!(display_width("ab"), 2);
        assert_eq!(display_width("한a"), 3);
    }

    #[test]
    fn a_paragraph_wraps_at_the_given_width() {
        let out = plain(&render("hello world foo bar", 11));
        assert_eq!(out, vec!["hello world", "foo bar"]);
    }

    /// 한글 문단이 폭을 넘으면 안 된다.
    #[test]
    fn a_korean_paragraph_never_exceeds_the_width() {
        for line in render("가나다라마바사아자차카타파하", 10) {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(display_width(&text) <= 10, "폭을 넘었다: {text:?}");
        }
    }

    /// 스트리밍 중에는 코드펜스가 아직 닫히지 않는다. 그래도 코드로 보여야 한다.
    #[test]
    fn an_unclosed_code_fence_still_renders_as_code() {
        let out = plain(&render("```rust\nfn main() {\n", 40));
        assert!(out.iter().any(|l| l.contains("fn main() {")), "코드 본문이 보여야 한다: {out:?}");
        assert!(out.iter().any(|l| l.contains("rust")), "언어 라벨이 보여야 한다: {out:?}");
    }

    /// 색이 없는 span을 만들면 터미널 기본 전경색이 샌다.
    #[test]
    fn every_span_has_a_foreground_colour() {
        let lines = render("# 제목\n\n본문 **강조** 와 `코드`\n\n- 목록", 40);
        for line in &lines {
            for span in &line.spans {
                assert!(span.style.fg.is_some(), "색 없는 span: {:?}", span.content);
            }
        }
    }

    const TABLE: &str = "\
| 이름 | 값 |
|---|---|
| 가나다 | 1 |
| ab | 22 |";

    #[test]
    fn a_table_renders_every_cell() {
        let out = plain(&render(TABLE, 40));
        let all = out.join("\n");
        for cell in ["이름", "값", "가나다", "1", "ab", "22"] {
            assert!(all.contains(cell), "{cell:?}가 없다:\n{all}");
        }
    }

    /// 열이 세로로 맞아야 표로 읽힌다. 전각 폭을 안 세면 한글 열에서 어긋난다.
    #[test]
    fn table_columns_line_up_even_with_wide_characters() {
        let out = plain(&render(TABLE, 40));
        let rows: Vec<&String> = out.iter().filter(|l| l.contains('│')).collect();
        assert!(rows.len() >= 3, "표 줄이 모자라다: {out:?}");

        let cols: Vec<Vec<usize>> = rows
            .iter()
            .map(|l| {
                let mut acc = Vec::new();
                let mut w = 0;
                for ch in l.chars() {
                    if ch == '│' {
                        acc.push(w);
                    }
                    w += display_width(&ch.to_string());
                }
                acc
            })
            .collect();
        for c in &cols {
            assert_eq!(c, &cols[0], "구분선 위치가 줄마다 다르다:\n{}", out.join("\n"));
        }
    }

    #[test]
    fn a_table_never_exceeds_the_width() {
        for line in render(TABLE, 24) {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(display_width(&text) <= 24, "폭을 넘었다: {text:?}");
        }
    }

    /// 구분선이 오기 전에도 표로 보여야 한다 — 안 그러면 파이프가 글자로 보이다가
    /// 갑자기 표로 바뀌어 흐르는 답변에서 눈에 거슬린다.
    #[test]
    fn a_table_renders_before_its_delimiter_row_arrives() {
        let out = plain(&render("| 이름 | 값 |", 30));
        assert!(out.iter().any(|l| l.contains('┌')), "표 테두리가 없다: {out:?}");
        assert!(out.iter().any(|l| l.contains("이름")), "{out:?}");
    }

    /// 행이 하나씩 늘 때마다 표에 바로 붙어야 한다.
    #[test]
    fn streaming_rows_appear_one_by_one() {
        let one = plain(&render("| 이름 | 값 |\n|---|---|\n| 가 | 1 |", 30));
        let two = plain(&render("| 이름 | 값 |\n|---|---|\n| 가 | 1 |\n| 나 | 2 |", 30));
        assert!(one.iter().any(|l| l.contains('가')));
        assert!(!one.iter().any(|l| l.contains('나')), "아직 안 온 행이 보인다");
        assert!(two.iter().any(|l| l.contains('나')), "새 행이 안 붙었다");
    }

    /// 진짜 구분선이 있으면 지어내지 않는다.
    #[test]
    fn a_real_delimiter_row_is_left_alone() {
        let md = "| 이름 | 값 |\n|---|---|\n| 가 | 1 |";
        let out = plain(&render(md, 30));
        let borders = out.iter().filter(|l| l.contains('├')).count();
        assert_eq!(borders, 1, "구분선이 두 벌이 됐다: {out:?}");
    }

    /// 파이프가 하나뿐인 평범한 글은 표가 아니다.
    #[test]
    fn a_lone_pipe_is_not_a_table() {
        let out = plain(&render("a | b 는 논리 연산", 30));
        assert!(!out.iter().any(|l| l.contains('┌')), "{out:?}");
    }

    /// **한 글자씩 흘러드는 내내 표여야 한다.** 중간에 평문으로 떨어지면 표↔글을
    /// 오가며 깜박인다 — 실제로 그렇게 보였다.
    #[test]
    fn a_streaming_table_never_falls_back_to_plain_text() {
        let full = "| 이름 | 값 |\n|---|---|\n| 가 | 1 |\n| 나 | 2 |";
        let chars: Vec<char> = full.chars().collect();
        // 첫 파이프와 글자 하나가 온 뒤부터는 계속 표여야 한다.
        for n in 4..=chars.len() {
            let partial: String = chars[..n].iter().collect();
            let out: Vec<String> = render(&partial, 30)
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            assert!(
                out.iter().any(|l| l.contains('┌') || l.contains('│')),
                "{n}자에서 평문으로 떨어졌다:\n{}",
                out.join("\n")
            );
        }
    }

    /// 부분 구분선이 와도 표가 유지돼야 한다.
    #[test]
    fn a_half_written_delimiter_row_keeps_the_table() {
        for partial in ["| 이름 | 값 |\n|", "| 이름 | 값 |\n|--", "| 이름 | 값 |\n|---|"] {
            let out: Vec<String> = render(partial, 30)
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            assert!(out.iter().any(|l| l.contains('┌')), "{partial:?}에서 표가 아니다: {out:?}");
        }
    }

    /// **앞 문장에 바로 붙은 표도 표여야 한다.** 빈 줄이 없으면 파서가 문단의 연장으로
    /// 먹는다 — 빈 줄이 오느냐에 따라 표↔글을 오가며 깜박였다.
    #[test]
    fn a_table_glued_to_the_previous_line_still_renders() {
        let out = plain(&render("다음은 표입니다:\n| 이름 | 값 |\n|---|---|\n| 가 | 1 |", 40));
        assert!(out.iter().any(|l| l.contains('┌')), "표가 아니다:\n{}", out.join("\n"));
        assert!(out.iter().any(|l| l.contains("다음은 표입니다")), "앞 글이 사라졌다");
    }

    /// 앞 글이 붙은 채로 한 글자씩 흘러들어도 내내 표여야 한다.
    #[test]
    fn a_glued_streaming_table_never_flickers() {
        let full = "설명:\n| 이름 | 값 |\n|---|---|\n| 가 | 1 |\n| 나 | 2 |";
        let chars: Vec<char> = full.chars().collect();
        // 첫 칸에 글자가 들어온 뒤부터 — 그전에는 표로 그릴 것이 없다.
        let head = "설명:\n| 이".chars().count();
        for n in head..=chars.len() {
            let partial: String = chars[..n].iter().collect();
            let out: Vec<String> = render(&partial, 40)
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            assert!(
                out.iter().any(|l| l.contains('┌') || l.contains('│')),
                "{n}자에서 표가 아니다:\n{}",
                out.join("\n")
            );
        }
    }

    /// **한 줄에 붙어 버린 행이 열을 늘리면 안 된다.**
    ///
    /// 델타가 줄바꿈을 늦게 실어 오면 두 행이 한 줄로 붙는다. 그대로 두면 열이 잔뜩
    /// 늘어난 아주 넓은 표가 되고, 줄바꿈이 도착하면 정상으로 돌아가 깜박인다.
    #[test]
    fn rows_glued_onto_one_line_are_split_apart() {
        let out = plain(&render("설명:\n\n| 이름 | 값 || 가 | 1 |", 44));
        let cols = out
            .iter()
            .find(|l| l.contains('┌'))
            .map(|l| l.matches('┬').count())
            .expect("표가 없다");
        assert_eq!(cols, 1, "열이 늘어났다 (구분자 {cols}개):\n{}", out.join("\n"));
        assert!(out.iter().any(|l| l.contains("가")), "붙었던 행이 사라졌다");
    }

    /// 사이에 공백이 있는 진짜 빈 칸은 건드리지 않는다.
    #[test]
    fn a_genuine_empty_cell_is_left_alone() {
        let out = plain(&render("| a | b |\n|---|---|\n| 1 | |", 30));
        assert!(out.iter().any(|l| l.contains('1')), "{out:?}");
        let rows = out.iter().filter(|l| l.starts_with('│')).count();
        assert_eq!(rows, 2, "행이 쪼개졌다: {out:?}");
    }

    #[test]
    fn a_bullet_list_is_marked() {
        let out = plain(&render("- 첫째\n- 둘째", 40));
        assert_eq!(out.len(), 2);
        assert!(out[0].contains("첫째"));
        assert!(out[0].trim_start().starts_with('·') || out[0].trim_start().starts_with('-'));
    }
}
