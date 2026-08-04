//! 항목을 화면 줄로 편다. **대화 화면의 유일한 정본이다.**
//!
//! 위젯이 그리는 것, 스크롤 창 계산, 카드 접힘 높이가 전부 여기서 나온 같은
//! `Vec<Line>`을 쓴다. 그리는 코드와 세는 코드가 갈라지면 스크롤이 조용히 어긋난다.

use std::collections::HashMap;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::markdown;
use crate::theme;
use crate::timeline::{Item, Part};

/// 대화 좌우 여백.
///
/// **마커가 이 여백을 쓴다.** `▌`·`│`·`●`·`▸`는 여백 자리에 서고 글은 그 안쪽에서
/// 시작한다 — 그래서 글은 가지런한데 마커는 눈에 띄게 튀어나온다.
pub const PAD: u16 = 2;

fn pad() -> Span<'static> {
    Span::styled(" ".repeat(PAD as usize), Style::default().fg(theme::TEXT))
}

/// 글이 실제로 쓸 수 있는 폭.
///
/// 왼쪽 여백만 뺀다 — **오른쪽 여백은 `transcript`가 그리는 자리 자체를 줄여서** 준다.
/// 여기서 또 빼면 오른쪽만 두 배로 벌어진다.
fn body_width(width: u16) -> u16 {
    width.saturating_sub(PAD)
}

/// 카드 하나의 접힘 상태.
///
/// **기본은 접힘이다.** 추론은 훑어보는 것이지 읽는 것이 아니라, 답을 보러 온 화면을
/// 생각 더미가 밀어내면 안 된다. **접혀도 도구 줄은 보인다** — 숨기는 것은 추론
/// 본문뿐이다. 무슨 일을 했는지까지 가리면 사람은 에이전트가 뭘 하는지 모른다.
/// 펴는 것은 사람이 Ctrl+O로 한다.
///
/// **저절로 바뀌는 일은 없다.** 예전에는 추론 중에 펴고 답이 시작되면 접었는데, 그러면
/// 읽고 있던 화면이 스스로 움직이고 "늦게 온 추론이 도로 펴면 안 된다" 같은 예외가
/// 계속 붙었다. 그 알고리즘을 통째로 걷어냈다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fold {
    pub open: bool,
}

pub type Folds = HashMap<i64, Fold>;

/// 답 텍스트에서 "직접 입력"을 표시하는 머리말. `question::answer_text`가 붙인다.
pub const FREE_MARK: &str = "직접 입력:";

/// 그려낸 결과. 줄과 함께 **어느 줄이 어느 작업 카드의 머리인지**를 준다.
///
/// 클릭으로 카드를 접으려면 화면의 그 줄이 무엇인지 알아야 하는데, 그것을 아는 곳은
/// 줄을 만든 여기뿐이다. 위젯이 따로 세면 그리는 것과 어긋난다.
#[derive(Debug, Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    /// 행 인덱스 → 그 행을 누르면 접히고 펴지는 카드의 seq.
    pub cards: HashMap<usize, i64>,
}

impl Rendered {
    /// 각 줄의 평문. 선택 추출이 쓴다.
    pub fn plain(&self) -> Vec<String> {
        self.lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }
}

/// 지금 답하고 있는 질문. 그 카드만 커서와 체크를 그린다.
pub struct Active<'a> {
    pub seq: i64,
    pub answering: &'a crate::question::Answering,
}

pub fn rows(items: &[Item], width: u16, folds: &Folds) -> Rendered {
    rows_with(items, width, folds, None)
}

/// 항목 전체를 한 번에 편다. 테스트와 작은 화면이 쓴다.
///
/// 실제 그리기는 `Cache`를 지나간다 — 대화가 길어지면 이 함수는 매번 전부를 다시
/// 만들어 프레임 예산을 넘긴다.
pub fn rows_with(
    items: &[Item],
    width: u16,
    folds: &Folds,
    active: Option<Active<'_>>,
) -> Rendered {
    let mut cache = Cache::new();
    cache.layout(items, width, folds, active.map(|a| a.seq));
    Rendered { lines: cache.window(0, cache.total()), cards: cache.cards().clone() }
}

/// 항목 하나를 그린 결과.
#[derive(Debug, Clone)]
struct Made {
    lines: Vec<Line<'static>>,
    /// 눌러서 접었다 폈다 하는 줄들. (이 항목 안에서 몇 번째 줄, 어느 seq).
    ///
    /// 작업 카드 머리가 하나 있고, 그 안의 도구 줄마다 하나씩 더 있다 —
    /// **도구는 저마다 따로 펴진다.**
    heads: Vec<(usize, i64)>,
}

/// 이 항목을 그리는 데 영향을 주는 접힘 상태 전부. 캐시 비교가 이걸 본다.
///
/// 항목 자기 것만 보면 도구 줄 하나를 폈을 때 캐시가 "안 바뀌었다"고 여겨 화면이
/// 그대로 있는다.
type Affecting = Vec<(i64, Fold)>;

fn affecting(item: &Item, folds: &Folds) -> Affecting {
    let at = |seq: i64| (seq, folds.get(&seq).copied().unwrap_or_default());
    let mut out = vec![at(item.seq())];
    if let Item::Work { parts, .. } = item {
        out.extend(parts.iter().filter_map(|p| match p {
            Part::Step(s) => Some(at(s.seq)),
            Part::Think(_) => None,
        }));
    }
    out
}

/// 항목이 놓인 자리. `begin`은 **앞의 구분 빈 줄을 포함한** 시작이다.
#[derive(Debug, Clone, Copy)]
struct Slot {
    seq: i64,
    begin: usize,
    /// 앞에 구분 빈 줄이 있는가. 첫 항목만 없다.
    lead_blank: bool,
    len: usize,
}

impl Slot {
    fn end(&self) -> usize {
        self.begin + self.lead_blank as usize + self.len
    }
    /// 이 항목의 첫 실제 줄.
    fn first_line(&self) -> usize {
        self.begin + self.lead_blank as usize
    }
}

/// 만들어 둔 줄을 항목별로 들고 있다가 **바뀐 것만** 다시 만든다.
///
/// 예전에는 프레임마다 모든 항목의 마크다운을 다시 파싱했다. 대화가 길어질수록 비용이
/// 선형으로 늘어, 16턴쯤에서 한 프레임이 20fps 예산의 1.7배가 됐다 — 다 그리기 전에
/// 다음 프레임이 겹쳐 찍히면서 글자가 깨졌다. `tests/perf.rs`가 그 수치를 잰다.
#[derive(Debug, Default)]
pub struct Cache {
    width: u16,
    made: HashMap<i64, (Item, Affecting, Made)>,
    slots: Vec<Slot>,
    total: usize,
    cards: HashMap<usize, i64>,
    /// 실제로 다시 그린 항목 수. 캐시가 정말 도는지 테스트가 이걸로 확인한다.
    renders: u64,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn cards(&self) -> &HashMap<usize, i64> {
        &self.cards
    }

    pub fn renders(&self) -> u64 {
        self.renders
    }

    /// 어느 항목이 몇 번째 줄에 놓이는지 정한다. 바뀐 항목만 다시 그린다.
    ///
    /// `skip`은 지금 아래 패널에서 답하고 있는 질문의 seq다 — 대화 안에 또 그리면
    /// 같은 질문이 두 벌로 보인다.
    pub fn layout(&mut self, items: &[Item], width: u16, folds: &Folds, skip: Option<i64>) {
        // 폭이 바뀌면 줄바꿈 자리가 전부 달라진다. 통째로 버린다.
        if self.width != width {
            self.width = width;
            self.made.clear();
        }
        self.slots.clear();
        self.cards.clear();

        let mut pos = 0usize;
        for item in items {
            let seq = item.seq();
            if skip == Some(seq) {
                continue;
            }
            let now = affecting(item, folds);
            let fresh = match self.made.get(&seq) {
                Some((was, had, _)) => was == item && *had == now,
                None => false,
            };
            if !fresh {
                let made = make(item, width, folds);
                self.made.insert(seq, (item.clone(), now, made));
                self.renders += 1;
            }
            let made = &self.made[&seq].2;
            let slot =
                Slot { seq, begin: pos, lead_blank: !self.slots.is_empty(), len: made.lines.len() };
            pos = slot.end();
            for (rel, at) in &made.heads {
                self.cards.insert(slot.first_line() + rel, *at);
            }
            self.slots.push(slot);
        }
        self.total = pos;

        // 화면에서 사라진 항목의 줄까지 들고 있을 이유는 없다.
        if self.made.len() > self.slots.len() * 2 + 8 {
            let live: std::collections::HashSet<i64> = self.slots.iter().map(|s| s.seq).collect();
            self.made.retain(|seq, _| live.contains(seq));
        }
    }

    /// `[from, to)` 구간의 줄만 만들어 낸다. **보이는 만큼만 하는 것이 요점이다.**
    pub fn window(&self, from: usize, to: usize) -> Vec<Line<'static>> {
        let to = to.min(self.total);
        if from >= to {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(to - from);
        for slot in &self.slots {
            if slot.end() <= from {
                continue;
            }
            if slot.begin >= to {
                break;
            }
            let made = &self.made[&slot.seq].2;
            for i in slot.begin.max(from)..slot.end().min(to) {
                let inner = i - slot.begin;
                out.push(match (slot.lead_blank, inner) {
                    (true, 0) => blank(),
                    (true, n) => made.lines[n - 1].clone(),
                    (false, n) => made.lines[n].clone(),
                });
            }
        }
        out
    }

    /// 모든 줄의 평문. **선택을 복사할 때만 부른다** — 프레임마다 부르면 안 된다.
    pub fn plain(&self) -> Vec<String> {
        self.window(0, self.total)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }
}

fn blank() -> Line<'static> {
    Line::from(Span::styled("", Style::default().fg(theme::TEXT)))
}

/// 항목 하나를 줄로 편다. 여기가 마크다운을 파싱하는 유일한 자리다.
fn make(item: &Item, width: u16, folds: &Folds) -> Made {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut heads: Vec<(usize, i64)> = Vec::new();
    let fold = folds.get(&item.seq()).copied().unwrap_or_default();
    match item {
        Item::User { text, .. } => {
            for (i, raw) in text.lines().enumerate() {
                // **직접 써 넣은 답은 다르게 보인다.** 선택지에 없던 답이라는 사실
                // 자체가 정보이고, 고른 것과 섞여 보이면 그 구별이 사라진다.
                // 답 줄은 `  - 직접 입력: …` 꼴로 온다 — 목록 기호를 먼저 벗겨야
                // 머리말이 보인다.
                let bare = raw.trim_start().trim_start_matches("- ").trim_start();
                let typed = bare.starts_with(FREE_MARK);
                let body = if typed { bare.trim_start_matches(FREE_MARK).trim() } else { raw };
                let style = if typed {
                    Style::default().fg(theme::ACCENT_HOVER).add_modifier(Modifier::ITALIC)
                } else {
                    Style::default().fg(theme::TEXT)
                };
                // **막대는 모든 줄에 선다.** 첫 줄에만 세우면 두 번째 줄부터
                // 답변과 구별되지 않는다 — 긴 질문일수록 그 구간이 길다.
                let _ = i;
                let mut spans = vec![Span::styled("▌ ", Style::default().fg(theme::ACCENT))];
                if typed {
                    spans.push(Span::styled("✎ ", Style::default().fg(theme::ACCENT_HOVER)));
                }
                for line in markdown::render(body, body_width(width)) {
                    let mut row = spans.clone();
                    row.extend(line.spans.into_iter().map(|sp| {
                        if typed {
                            Span::styled(sp.content.to_string(), style)
                        } else {
                            sp
                        }
                    }));
                    // **배경은 span이 아니라 줄에 얹는다.** span에 칠하면 글자 폭에서
                    // 끊겨 얼룩이 되고, 폭을 채우려고 공백을 넣으면 그 공백이
                    // `plain()`을 타고 클립보드로 나간다. 화면 폭까지 늘리는 일은
                    // 그리는 자리(`widgets::transcript::stretch`)가 한다.
                    out.push(Line::from(row).style(Style::default().bg(theme::USER_BG)));
                    spans = vec![Span::styled("▌ ", Style::default().fg(theme::ACCENT))];
                }
            }
        }
        Item::Agent { text, .. } => {
            // 본문은 여백 안쪽에서 시작한다. 마커가 붙는 줄들과 글이 같은 열에 서야
            // 눈이 편하다 — 여백 바깥으로 나오는 것은 마커뿐이다.
            //
            // **첫 줄에만 마커를 둔다.** 모든 줄에 붙이면 인용문처럼 읽히고, 답변은
            // 대화에서 가장 긴 덩어리라 그 효과가 화면을 덮는다. 마커가 아예 없으면
            // 답변만 표식이 없어 "기본값이라 깨끗한" 게 아니라 그냥 안 갈린다.
            for (i, line) in markdown::render(text, body_width(width)).into_iter().enumerate() {
                let mut spans = match i {
                    0 => vec![Span::styled("◆ ", Style::default().fg(theme::ACCENT))],
                    _ => vec![pad()],
                };
                spans.extend(line.spans);
                out.push(Line::from(spans));
            }
        }
        Item::Error { message, .. } => {
            out.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(theme::DANGER)),
                Span::styled(message.clone(), Style::default().fg(theme::DANGER)),
            ]));
        }
        // 지금 답하고 있는 질문은 여기 오지 않는다 — `layout`이 걸러 낸다.
        Item::Question { steps, answered, .. } => {
            out.extend(question_rows(steps, *answered, width));
        }
        // 앱이 한 말. 사람도 에이전트도 아닌 제3의 목소리라 마커를 따로 준다.
        Item::System { text, .. } => {
            for (i, line) in markdown::render(text, body_width(width)).into_iter().enumerate() {
                let mut spans = vec![Span::styled(
                    if i == 0 { "◈ " } else { "  " },
                    Style::default().fg(theme::TEXT_MUTED),
                )];
                spans.extend(line.spans);
                out.push(Line::from(spans));
            }
        }
        Item::Subagent { summary, .. } => {
            out.push(Line::from(vec![
                Span::styled("└ ", Style::default().fg(theme::TEXT_MUTED)),
                Span::styled(summary.clone(), Style::default().fg(theme::TEXT_MUTED)),
            ]));
        }
        Item::Work { seq, title, parts } => {
            let marker = if fold.open { "▾ " } else { "▸ " };
            // 제목은 런 시작 시점에 비어 있고 작은 모델이 나중에 채운다.
            let head = if title.is_empty() { "작업 중…" } else { title.as_str() };
            // 이 줄을 누르면 이 카드가 접히고 펴진다. 어느 화면 줄인지는
            // 배치를 아는 `layout`이 정한다.
            heads.push((0, *seq));
            let tools = parts.iter().filter(|p| matches!(p, Part::Step(_))).count();
            let mut card = vec![
                Span::styled(marker, Style::default().fg(theme::ACCENT)),
                Span::styled(
                    head.to_string(),
                    Style::default().fg(theme::TEXT_MUTED).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  ·  도구 {tools}개"),
                    Style::default().fg(theme::TEXT_MUTED),
                ),
            ];
            // **접혀도 무엇이 바뀌었는지는 보인다.** 수치는 도구 줄에만 있고 그건 카드를
            // 펴야 보이므로, 턴이 끝나 카드가 접히면 흔적이 통째로 사라진다.
            // 아무것도 안 바꾼 카드에 `+0 −0`을 붙이면 그건 그것대로 시끄럽다.
            let (added, removed) = parts.iter().fold((0u32, 0u32), |(a, r), p| match p {
                Part::Step(s) => match &s.diff {
                    Some(d) => (a + d.added, r + d.removed),
                    None => (a, r),
                },
                _ => (a, r),
            });
            if added + removed > 0 {
                card.extend(counts(added, removed));
            }
            out.push(Line::from(card));

            // **도구 줄은 접혀도 보인다.** 카드 접힘은 추론을 숨기는 것이지 무슨
            // 일을 했는지까지 가리는 것이 아니다 — 툴 사용은 대화의 흐름이라
            // 생각 더미에 묻히면 사람은 에이전트가 뭘 하는지 모른다. 펼치면
            // 생각 줄이 그 자리에 끼어들어 "생각 → 도구 → 생각 → 도구" 순서가
            // 그대로 보인다.
            for part in parts {
                match part {
                    Part::Think(text) if fold.open => {
                        // **추론 줄도 눌러서 카드를 접고 펼 수 있다.** Ctrl+O와 같은
                        // 동작 — 클릭은 카드 접힘을 토글하지 도구 상세를 열지 않는다.
                        for line in markdown::render(text, body_width(width)) {
                            heads.push((out.len(), *seq));
                            let mut spans = vec![Span::styled(
                                "┊ ",
                                Style::default().fg(theme::BORDER_LIGHT),
                            )];
                            // 본문을 통째로 흐리게 다시 칠한다. 답변과 같은 밝기면
                            // 무엇이 결론인지 안 보인다 — 추론 안의 강조나 인라인
                            // 코드 색을 잃는 것은 그 대가로 치른다.
                            spans.extend(line.spans.into_iter().map(|s| {
                                Span::styled(
                                    s.content.to_string(),
                                    s.style.fg(theme::TEXT_MUTED),
                                )
                            }));
                            out.push(Line::from(spans));
                        }
                    }
                    Part::Step(step) => {
                        let dot = if step.failed { theme::DANGER } else { theme::SUCCESS };
                        // 펼칠 것이 있는 줄만 누를 수 있다. 눌러도 아무 일이
                        // 없으면 고장으로 보인다.
                        let can_open = !step.detail.is_empty() || step.diff.is_some();
                        let open = can_open && folds.get(&step.seq).is_some_and(|f| f.open);
                        if can_open {
                            heads.push((out.len(), step.seq));
                        }
                        // **이름과 요약을 따로 칠한다.** 추론이 흐린 색으로 화면을
                        // 채우는데 도구까지 같은 색이면 "무엇을 했는가"가 생각
                        // 더미에 묻힌다. 훑을 때 눈이 잡는 것은 이 이름이다.
                        let mut head = vec![
                            Span::styled("● ", Style::default().fg(dot)),
                            Span::styled(
                                step.name.clone(),
                                Style::default().fg(theme::TOOL).add_modifier(Modifier::BOLD),
                            ),
                        ];
                        if !step.note.is_empty() {
                            head.push(Span::styled(
                                format!("  {}", step.note),
                                Style::default().fg(theme::TOOL_ARG),
                            ));
                        }
                        // **접혀 있어도 얼마나 바뀌었는지는 보인다.** 펴야 알 수
                        // 있으면 훑어보는 것만으로는 무슨 일이 있었는지 모른다.
                        if let Some(d) = &step.diff {
                            head.extend(counts(d.added, d.removed));
                        }
                        // 펼칠 수 있다는 것을 알려 준다. 모르면 아무도 안 누른다.
                        head.push(Span::styled(
                            match (can_open, open) {
                                (false, _) => "",
                                (true, false) => "  ▸",
                                (true, true) => "  ▾",
                            },
                            Style::default().fg(theme::BORDER_LIGHT),
                        ));
                        out.push(Line::from(head));
                        if open {
                            match &step.diff {
                                // **diff가 있으면 그것만 보여 준다.** 같은 내용을
                                // JSON으로 한 번 더 얹으면 화면만 두 배로 길어지고
                                // 사람이 읽는 것은 diff 쪽이다.
                                Some(d) => {
                                    for line in &d.lines {
                                        out.push(diff_line(line, body_width(width) as usize));
                                    }
                                }
                                // 상세는 **섹션별로** 그린다. `event::tool_detail`이
                                // "인자/출력/결과/오류" 머리말로 조립하므로 그것을
                                // 색·마커로 갈라 보여 준다 — JSON 덤프와 셸 출력이
                                // 한 덩어리로 보이면 무엇이 인자고 무엇이 결과인지
                                // 눈이 매번 읽어야 한다.
                                None => {
                                    out.extend(tool_detail_lines(
                                        &step.detail,
                                        body_width(width),
                                    ));
                                }
                            }
                        }
                    }
                    Part::Think(_) => {}
                }
            }
        }
    }
    Made { lines: out, heads }
}

/// 요약 줄에 붙는 `  +12 −3`.
///
/// **두 수를 따로 칠한다.** 한 색으로 붙여 두면 어느 쪽이 는 것인지 눈이 한 번 더
/// 읽어야 안다. 빼기는 하이픈이 아니라 U+2212라 더하기와 폭이 같다 — 숫자가 가지런하다.
fn counts(added: u32, removed: u32) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("  +{added}"), Style::default().fg(theme::DIFF_ADD)),
        Span::styled(format!(" −{removed}"), Style::default().fg(theme::DIFF_DEL)),
    ]
}

/// diff 한 줄. **색만 칠하고 배경은 칠하지 않는다** — 배경을 칠하면 선택 영역과 싸운다.
///
/// 승인 화면(`widgets/approve.rs`)도 이것을 쓴다. 실행 전 미리보기와 실행 뒤 기록이
/// 다르게 보이면 사람이 같은 것인 줄 모른다. `pub(crate)`인 이유다.
///
/// `width`는 글이 쓸 수 있는 폭(`body_width`)이고 들여쓰기 두 칸은 여기서 뺀다.
pub(crate) fn diff_line(line: &crate::tools::diff::DiffLine, width: usize) -> Line<'static> {
    use crate::tools::diff::DiffLine;
    let (text, colour) = match line {
        DiffLine::Add(s) => (format!("+{s}"), theme::DIFF_ADD),
        DiffLine::Del(s) => (format!("-{s}"), theme::DIFF_DEL),
        DiffLine::Keep(s) => (format!(" {s}"), theme::TEXT_MUTED),
        DiffLine::Skip(n) => (format!(" … {n}줄 생략"), theme::BORDER_LIGHT),
    };
    Line::from(vec![
        Span::styled("  ", Style::default().fg(theme::BORDER_LIGHT)),
        Span::styled(clip_to(text, width.saturating_sub(2)), Style::default().fg(colour)),
    ])
}

/// 폭을 넘으면 자른다. **접지 않는다** — 코드 한 줄이 화면 몇 줄로 늘어나면 무엇이
/// 바뀌었는지 훑어보기 어렵고, 접힌 뒷부분이 다음 줄의 `+`/`-`처럼 읽힌다.
fn clip_to(text: String, width: usize) -> String {
    let limit = width.max(8);
    if markdown::display_width(&text) <= limit {
        return text;
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = markdown::display_width(&ch.to_string()).max(1);
        if used + w > limit - 1 {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// 폭에 맞춰 접는다. **칸 수로만 자른다** — 도구 상세는 JSON이나 원문이라
/// 마크다운으로 해석하면 안 된다.
fn wrap_plain(text: &str, width: u16) -> Vec<String> {
    let limit = (width as usize).max(8);
    let mut out = Vec::new();
    for raw in text.lines() {
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut used = 0usize;
        for ch in raw.chars() {
            let w = markdown::display_width(&ch.to_string()).max(1);
            if used + w > limit {
                out.push(std::mem::take(&mut cur));
                used = 0;
            }
            cur.push(ch);
            used += w;
        }
        out.push(cur);
    }
    out
}

/// 도구 상세의 섹션 머리말. `event::tool_detail`이 이 이름으로 조립한다.
const TOOL_SECTIONS: [&str; 4] = ["인자", "출력", "결과", "오류"];

/// 도구 상세("인자\n…\n\n출력\n…")를 **섹션별로** 그린다.
///
/// 그대로 평문으로 늘어놓으면 JSON 덤프와 셸 출력이 구분되지 않는다. 머리말 줄은
/// 색·마커로 따로 칠하고 본문은 그 아래로 들여쓴다. 마크다운으로 해석하지 않는
/// 것은 예전과 같다 — JSON의 `*`·`_`가 강조로 먹히면 원문이 망가진다.
fn tool_detail_lines(detail: &str, width: u16) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut section: Option<&'static str> = None;
    // 머리말은 앞이 빈 줄(또는 첫 줄)에서만 인정한다 — 본문에 같은 단어가 있어도
    // 섞이지 않게.
    let mut prev_blank = true;
    for raw in detail.lines() {
        let trimmed = raw.trim();
        // 머리말은 `TOOL_SECTIONS`의 원소를 그대로 집는다 — `detail`에서 빌린 조각을 담으면
        // `section`이 `detail`보다 오래 살 수 없다.
        let head = TOOL_SECTIONS.iter().copied().find(|s| *s == trimmed);
        if let (Some(head), true) = (head, prev_blank) {
            section = Some(head);
            let (mark, color) = match head {
                "인자" => ("⎿ 인자", theme::TOOL_ARG),
                "출력" => ("⎿ 출력", theme::ACCENT),
                "결과" => ("⎿ 결과", theme::BORDER_LIGHT),
                _ => ("⎿ 오류", theme::DANGER),
            };
            out.push(Line::from(Span::styled(
                mark.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )));
            prev_blank = false;
            continue;
        }
        let color = match section {
            Some("오류") => theme::DANGER,
            Some("출력") => theme::TEXT,
            _ => theme::TEXT_MUTED,
        };
        for (i, line) in wrap_plain(raw, width).into_iter().enumerate() {
            out.push(Line::from(vec![
                Span::styled(
                    if i == 0 { "  ⎿ " } else { "    " },
                    Style::default().fg(theme::BORDER_LIGHT),
                ),
                Span::styled(line, Style::default().fg(color)),
            ]));
        }
        prev_blank = trimmed.is_empty();
    }
    out
}

/// 질문 카드. 답을 기다리는 동안은 고를 수 있고, 답이 간 뒤에는 읽기만 한다.
fn question_rows(
    steps: &[crate::question::Step],
    answered: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let Some(step) = steps.first() else {
        return out;
    };

    let mark = if answered { "✓" } else { "?" };
    let head_colour = if answered { theme::TEXT_MUTED } else { theme::ACCENT };
    let mut head = vec![Span::styled(format!("{mark} "), Style::default().fg(head_colour))];
    if let Some(h) = &step.header {
        head.push(Span::styled(format!("[{h}] "), Style::default().fg(theme::TEXT_MUTED)));
    }
    head.push(Span::styled(
        step.question.clone(),
        Style::default().fg(theme::TEXT_HEADING).add_modifier(Modifier::BOLD),
    ));
    if steps.len() > 1 {
        head.push(Span::styled(
            format!("  ·  {}단계", steps.len()),
            Style::default().fg(theme::TEXT_MUTED),
        ));
    }
    out.push(Line::from(head));

    let _ = width;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{Item, Step};

    fn plain(r: &Rendered) -> Vec<String> {
        r.plain()
    }

    /// 도구 줄 하나. **카드의 seq와 겹치지 않아야 한다** — 캐시 키가 seq다.
    fn step_at(seq: i64, label: &str) -> Step {
        Step {
            seq,
            name: label.into(),
            note: "viewport".into(),
            failed: false,
            detail: "인자\n{\"pattern\": \"viewport\"}".into(),
            diff: None,
        }
    }

    fn work_at(seq: i64) -> Item {
        Item::Work {
            seq,
            title: "스크롤 계산 위치를 찾는 중".into(),
            parts: vec![
                Part::Think("먼저 구조를 보자".into()),
                Part::Step(step_at(seq * 100, "grep")),
            ],
        }
    }

    fn work() -> Item {
        work_at(1)
    }

    /// **접혀도 도구 줄은 보인다.** 카드 접힘은 추론을 숨기는 것이지 무슨 일을
    /// 했는지까지 가리는 것이 아니다 — 툴 사용은 대화의 흐름이라 생각 더미에
    /// 묻히면 사람은 에이전트가 뭘 하는지 모른다.
    #[test]
    fn a_folded_card_hides_thinking_but_shows_tools() {
        let folds = Folds::from([(1, Fold::default())]);
        let out = plain(&rows(&[work()], 40, &folds));
        assert!(out[0].contains("스크롤 계산 위치를 찾는 중"), "머리줄이 없다: {out:?}");
        assert!(out.iter().any(|l| l.contains("grep")), "접혀도 도구는 보여야 한다: {out:?}");
        assert!(
            !out.iter().any(|l| l.contains("먼저 구조를 보자")),
            "추론은 접혀 있어야 한다: {out:?}"
        );
    }

    /// 펼치면 생각이 도구 사이에 끼어든다 — 온 순서 그대로다.
    #[test]
    fn an_open_card_interleaves_thinking_with_tools() {
        let folds = Folds::from([(1, Fold { open: true })]);
        let out = plain(&rows(&[work()], 40, &folds));
        let think = out.iter().position(|l| l.contains("먼저 구조를 보자")).expect("생각이 없다");
        let tool = out.iter().position(|l| l.contains("grep")).expect("도구가 없다");
        assert!(think < tool, "생각이 도구보다 앞에 있어야 한다: {out:?}");
    }

    #[test]
    fn an_open_card_shows_reasoning_and_steps() {
        let folds = Folds::from([(1, Fold { open: true })]);
        let out = plain(&rows(&[work()], 40, &folds));
        assert!(out.iter().any(|l| l.contains("먼저 구조를 보자")), "{out:?}");
        assert!(out.iter().any(|l| l.contains("grep")), "{out:?}");
    }

    /// **머리줄은 도구를 몇 번 썼는지 말한다.** 세는 것이 `Part::Step`, 곧 도구 호출이라
    /// "단계"는 그것을 가리키지 않는다.
    #[test]
    fn a_card_head_counts_tools_not_steps() {
        let out = plain(&rows(&[work()], 60, &Folds::new()));
        assert!(out[0].contains("도구 1개"), "{out:?}");
        assert!(!out[0].contains("단계"), "{out:?}");
    }

    /// 접힘 상태를 모르는 카드는 접혀 있어야 한다 — 기본은 조용한 화면이다.
    /// (도구 줄은 그 화면에도 선다.)
    #[test]
    fn a_card_with_no_fold_state_defaults_to_folded() {
        let out = plain(&rows(&[work()], 40, &Folds::new()));
        assert!(out[0].contains("스크롤 계산 위치를 찾는 중"), "{out:?}");
        assert!(!out.iter().any(|l| l.contains("먼저 구조를 보자")), "추론이 보인다: {out:?}");
        assert!(out.iter().any(|l| l.contains("grep")), "도구가 안 보인다: {out:?}");
    }

    #[test]
    fn a_user_message_is_marked_with_the_accent_bar() {
        let out = plain(&rows(&[Item::User { seq: 1, text: "안녕".into() }], 40, &Folds::new()));
        assert!(out[0].starts_with('▌'), "{out:?}");
    }

    /// **도구 줄은 와이어 이름을 그대로 쓰지 않는다.** `zyris__arch__terminal__exec`을
    /// 그대로 두면 그것만으로 한 줄을 다 먹고, 매 줄이 같은 앞머리로 시작해 정작
    /// 다른 부분이 안 보인다.
    #[test]
    fn a_tool_row_shows_the_short_name() {
        let items = [work_at(1)];
        let folds = Folds::from([(1, Fold { open: true })]);
        let out = plain(&rows(&items, 60, &folds));
        let row = out.iter().find(|l| l.contains("grep")).expect("{out:?}");
        assert!(!row.contains("zyris__"), "와이어 이름이 그대로 나온다: {row:?}");
    }

    /// **도구는 추론과 다른 색이어야 한다.** 펼친 카드에서 추론이 화면을 채우는데
    /// 도구까지 흐린 색이면 "무엇을 했는가"가 생각 더미에 묻힌다.
    #[test]
    fn a_tool_row_stands_out_from_the_reasoning_around_it() {
        let items = [work_at(1)];
        let folds = Folds::from([(1, Fold { open: true })]);
        let r = rows(&items, 60, &folds);
        let row = r
            .lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("grep")))
            .expect("도구 줄이 없다");
        let name = row.spans.iter().find(|s| s.content.contains("grep")).unwrap();
        assert_eq!(name.style.fg, Some(theme::TOOL));
        assert_ne!(name.style.fg, Some(theme::TEXT_MUTED), "추론과 같은 색이면 안 된다");
    }

    /// 이름과 요약은 **따로 칠해진다** — 한 span이면 색을 나눌 수 없다.
    #[test]
    fn the_name_and_its_summary_are_coloured_apart() {
        let items = [work_at(1)];
        let folds = Folds::from([(1, Fold { open: true })]);
        let r = rows(&items, 60, &folds);
        let row = r
            .lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("grep")))
            .expect("도구 줄이 없다");
        let note = row.spans.iter().find(|s| s.content.contains("viewport")).expect("요약이 없다");
        assert_eq!(note.style.fg, Some(theme::TOOL_ARG));
    }

    /// **접혀도 무엇이 바뀌었는지는 보인다.** 머리줄과 도구 줄 양쪽에 수치가 있다.
    #[test]
    fn a_folded_card_still_shows_how_much_changed() {
        let d = crate::tools::diff::Diff::parse("-a\n+b\n", "src/app.rs", 12, 3).unwrap();
        let items = [Item::Work {
            seq: 1,
            title: "고치는 중".into(),
            parts: vec![Part::Step(Step {
                seq: 100,
                name: "edit".into(),
                note: "src/app.rs".into(),
                failed: false,
                detail: String::new(),
                diff: Some(d),
            })],
        }];
        let out = plain(&rows(&items, 60, &Folds::new()));
        assert!(out[0].contains("+12"), "머리줄에 수치가 없다: {out:?}");
        assert!(out[0].contains("−3"), "머리줄에 수치가 없다: {out:?}");
        assert!(
            out.iter().any(|l| l.contains("src/app.rs")),
            "접힌 상태에도 도구 줄은 보여야 한다: {out:?}"
        );
    }

    /// 아무것도 안 바꾼 카드에 `+0 −0`을 붙이면 그건 그것대로 시끄럽다.
    #[test]
    fn a_card_that_changed_nothing_gets_no_counts() {
        let out = plain(&rows(&[work()], 60, &Folds::new()));
        assert!(!out[0].contains('+'), "{out:?}");
    }

    /// **복사에 꼬리 공백이 섞이면 안 된다.** 배경을 칠하려고 줄을 채운 것이 클립보드로
    /// 새어 나가면 붙여넣은 코드가 망가진다. 채우는 일은 `transcript`가 그릴 때만 한다.
    #[test]
    fn the_band_never_leaks_trailing_spaces_into_the_copy() {
        let items = [Item::User { seq: 1, text: "안녕".into() }];
        for line in plain(&rows(&items, 60, &Folds::new())) {
            assert_eq!(line, line.trim_end(), "꼬리 공백이 붙었다: {line:?}");
        }
    }

    /// 배경은 span이 아니라 줄에 얹혀야 한다 — span에 칠하면 글자 폭에서 끊긴다.
    #[test]
    fn the_user_band_rides_on_the_line_not_the_spans() {
        let items = [Item::User { seq: 1, text: "안녕".into() }];
        let r = rows(&items, 60, &Folds::new());
        assert_eq!(r.lines[0].style.bg, Some(theme::USER_BG));
        assert!(r.lines[0].spans.iter().all(|s| s.style.bg.is_none()), "span에 배경이 칠해졌다");
    }

    /// **막대는 모든 줄에 선다.** 첫 줄에만 있으면 두 번째 줄부터는 답변과 구별되지 않는다.
    #[test]
    fn the_user_bar_runs_down_every_line() {
        let items = [Item::User { seq: 1, text: "첫 줄\n둘째 줄".into() }];
        let out = plain(&rows(&items, 40, &Folds::new()));
        assert!(out.len() >= 2, "{out:?}");
        assert!(out.iter().all(|l| l.starts_with('▌')), "{out:?}");
    }

    /// 줄바꿈으로 이어진 줄에도 막대가 서야 한다 — 긴 문장 하나가 여러 줄이 되는 쪽이다.
    #[test]
    fn the_user_bar_also_runs_down_wrapped_lines() {
        let long = "아주 긴 문장을 하나 적어서 좁은 폭에서 반드시 여러 줄로 접히게 만든다";
        let items = [Item::User { seq: 1, text: long.into() }];
        let out = plain(&rows(&items, 24, &Folds::new()));
        assert!(out.len() >= 2, "안 접혔다: {out:?}");
        assert!(out.iter().all(|l| l.starts_with('▌')), "{out:?}");
    }

    /// **답변에도 마커가 있어야 한다.** 다른 것에는 다 있는데 답변만 없으면
    /// "기본값이라 깨끗한" 게 아니라 그냥 안 갈린다.
    #[test]
    fn an_agent_answer_is_marked_too() {
        let items = [Item::Agent { seq: 1, text: "그건 rows.rs가 정합니다.".into() }];
        let out = plain(&rows(&items, 40, &Folds::new()));
        assert!(out[0].starts_with('◆'), "{out:?}");
        assert!(out[0].contains("rows.rs가 정합니다"), "{out:?}");
    }

    /// 마커는 첫 줄에만. 모든 줄에 붙이면 그건 인용문처럼 읽힌다.
    #[test]
    fn only_the_first_line_of_an_answer_is_marked() {
        let items = [Item::Agent { seq: 1, text: "첫 줄\n\n둘째 문단".into() }];
        let out = plain(&rows(&items, 40, &Folds::new()));
        assert_eq!(out.iter().filter(|l| l.starts_with('◆')).count(), 1, "{out:?}");
    }

    /// **추론은 코드블록과 다른 거터를 쓴다.** 둘 다 `│ `면 같은 것으로 읽힌다 —
    /// 답변 안의 코드블록이 `markdown.rs`에서 `│ `를 쓰고 있다.
    #[test]
    fn reasoning_does_not_use_the_code_block_gutter() {
        let folds = Folds::from([(1, Fold { open: true })]);
        let out = plain(&rows(&[work()], 40, &folds));
        let think = out.iter().find(|l| l.contains("먼저 구조를 보자")).expect("추론 줄이 없다");
        assert!(think.starts_with('┊'), "{think:?}");
    }

    /// 추론은 답변보다 흐려야 한다. 같은 밝기면 무엇이 결론인지 안 보인다.
    #[test]
    fn reasoning_is_dimmer_than_the_answer() {
        let folds = Folds::from([(1, Fold { open: true })]);
        let r = rows(&[work()], 40, &folds);
        // 마크다운은 낱말을 여러 span으로 쪼갠다 — span 하나에서 찾으면 못 만난다.
        let joined =
            |l: &Line<'static>| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() };
        let line = r
            .lines
            .iter()
            .find(|l| joined(l).contains("먼저 구조를 보자"))
            .expect("추론 줄이 없다");
        // 거터(`┊`)는 글이 아니라 선이라 그대로 BORDER_LIGHT다. 본문만 본다.
        assert!(
            line.spans
                .iter()
                .skip(1)
                .filter(|s| !s.content.trim().is_empty())
                .all(|s| s.style.fg == Some(theme::TEXT_MUTED)),
            "추론 본문이 흐리지 않다: {:?}",
            line.spans.iter().map(|s| (s.content.clone(), s.style.fg)).collect::<Vec<_>>()
        );
    }

    /// 오류는 절대 조용히 지나가면 안 된다.
    #[test]
    fn an_error_is_always_visible_and_red() {
        let items = [Item::Error { seq: 1, message: "크레딧이 부족합니다".into() }];
        let r = rows(&items, 40, &Folds::new());
        assert!(plain(&r).iter().any(|l| l.contains("크레딧이 부족합니다")));
        assert!(
            r.lines.iter().flat_map(|l| &l.spans).any(|s| s.style.fg == Some(crate::theme::DANGER)),
            "오류는 빨간색이어야 한다"
        );
    }

    /// 실제 화면에 서는 것들을 섞어 놓은 대화. **`seq`는 서로 달라야 한다** —
    /// `Timeline`이 BTreeMap 키로 보장하는 성질이고, 겹치면 캐시가 서로를 밀어낸다.
    fn mixed() -> Vec<Item> {
        vec![
            Item::User { seq: 1, text: "표 좀 그려 줘".into() },
            work_at(2),
            Item::Agent {
                seq: 3,
                text: "| 경로 | 크기 |\n|---|---|\n| a | 1 |\n| b | 2 |\n\n끝입니다.".into(),
            },
            Item::Error { seq: 4, message: "크레딧이 부족합니다".into() },
            Item::Subagent { seq: 5, summary: "하위 에이전트가 끝났다".into() },
        ]
    }

    /// **캐시가 예전과 똑같이 그려야 한다.** 빨라졌는데 다르게 그리면 아무 소용이 없다.
    #[test]
    fn the_cache_draws_exactly_what_the_plain_path_draws() {
        let items = mixed();
        for folds in [Folds::new(), Folds::from([(2, Fold { open: true })])] {
            let want = rows(&items, 40, &folds);
            let mut cache = Cache::new();
            cache.layout(&items, 40, &folds, None);

            assert_eq!(cache.total(), want.lines.len(), "줄 수가 같아야 한다");
            assert_eq!(cache.plain(), want.plain(), "내용이 같아야 한다");
            assert_eq!(cache.cards(), &want.cards, "카드 머리 위치가 같아야 한다");
        }
    }

    /// 창은 그 구간만 준다. 화면 밖까지 만들면 대화 길이만큼 무거워진다.
    #[test]
    fn a_window_gives_back_only_that_slice() {
        let items = mixed();
        let folds = Folds::new();
        let all = rows(&items, 40, &folds).plain();

        let mut cache = Cache::new();
        cache.layout(&items, 40, &folds, None);
        for (from, to) in [(0usize, 3usize), (2, 5), (1, cache.total()), (0, cache.total())] {
            let got: Vec<String> = cache
                .window(from, to)
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            assert_eq!(got, all[from..to], "{from}..{to} 구간이 어긋난다");
        }
        assert!(cache.window(3, 3).is_empty(), "빈 구간은 빈 결과다");
        assert_eq!(cache.window(0, 9999).len(), cache.total(), "끝을 넘겨도 잘려야 한다");
    }

    /// **바뀐 항목만 다시 그린다.** 이게 깨지면 대화 길이만큼 다시 느려진다.
    #[test]
    fn only_the_changed_item_is_drawn_again() {
        let mut items = mixed();
        let folds = Folds::new();
        let mut cache = Cache::new();

        cache.layout(&items, 40, &folds, None);
        let first = cache.renders();
        assert_eq!(first, items.len() as u64, "처음에는 전부 그린다");

        cache.layout(&items, 40, &folds, None);
        assert_eq!(cache.renders(), first, "안 바뀌었으면 한 줄도 다시 그리지 않는다");

        // 답변에 델타가 붙었다 — 그 항목 하나만 다시 그려야 한다.
        if let Item::Agent { text, .. } = &mut items[2] {
            text.push_str("| c | 3 |\n");
        }
        cache.layout(&items, 40, &folds, None);
        assert_eq!(cache.renders(), first + 1, "바뀐 하나만 다시 그린다");
    }

    /// 접었다 펴면 그 카드만 다시 그린다.
    #[test]
    fn folding_a_card_redraws_only_that_card() {
        let items = mixed();
        let mut cache = Cache::new();
        cache.layout(&items, 40, &Folds::new(), None);
        let before = cache.renders();

        let opened = Folds::from([(2, Fold { open: true })]);
        cache.layout(&items, 40, &opened, None);
        assert_eq!(cache.renders(), before + 1, "펼친 카드 하나만 다시 그린다");
        assert!(
            cache.plain().iter().any(|l| l.contains("먼저 구조를 보자")),
            "펼쳤으면 추론이 보여야 한다"
        );
    }

    /// 폭이 바뀌면 줄바꿈 자리가 전부 달라진다 — 통째로 다시 그려야 한다.
    #[test]
    fn a_width_change_redraws_everything() {
        let items = mixed();
        let folds = Folds::new();
        let mut cache = Cache::new();
        cache.layout(&items, 40, &folds, None);
        let before = cache.renders();

        cache.layout(&items, 80, &folds, None);
        assert_eq!(cache.renders(), before + items.len() as u64);
        assert_eq!(cache.plain(), rows(&items, 80, &folds).plain());
    }

    /// 지금 답하고 있는 질문은 대화 안에 그리지 않는다 — 아래 패널에 있다.
    #[test]
    fn the_question_being_answered_is_left_out_of_the_transcript() {
        let steps = vec![crate::question::Step {
            header: None,
            question: "어느 쪽으로 갈까요".into(),
            options: vec![],
            multi: false,
        }];
        let items = vec![
            Item::User { seq: 1, text: "골라 줘".into() },
            Item::Question { seq: 2, steps, answered: false },
        ];
        let mut cache = Cache::new();
        cache.layout(&items, 40, &Folds::new(), Some(2));
        assert!(
            !cache.plain().iter().any(|l| l.contains("어느 쪽으로 갈까요")),
            "답하는 중인 질문이 두 벌로 보이면 안 된다: {:?}",
            cache.plain()
        );

        cache.layout(&items, 40, &Folds::new(), None);
        assert!(
            cache.plain().iter().any(|l| l.contains("어느 쪽으로 갈까요")),
            "답이 끝나면 대화에 남아야 한다"
        );
    }

    /// 도구는 처음엔 한 줄이고, 펴면 인자와 결과가 나온다.
    #[test]
    fn a_tool_row_starts_as_one_line_and_opens_into_its_detail() {
        let items = [work_at(1)];
        let card_open = Folds::from([(1, Fold { open: true })]);

        let shut = plain(&rows(&items, 60, &card_open));
        assert!(shut.iter().any(|l| l.contains("grep")), "{shut:?}");
        assert!(
            !shut.iter().any(|l| l.contains("viewport\"}")),
            "안 폈는데 상세가 보인다: {shut:?}"
        );
        assert!(
            shut.iter().any(|l| l.contains("grep") && l.contains('▸')),
            "펼 수 있다는 표시가 없다: {shut:?}"
        );

        let mut both = card_open.clone();
        both.insert(100, Fold { open: true });
        let open = plain(&rows(&items, 60, &both));
        assert!(open.iter().any(|l| l.contains("인자")), "상세가 안 나온다: {open:?}");
        assert!(open.iter().any(|l| l.contains("viewport")), "{open:?}");
    }

    /// 도구 줄을 눌러 펼 수 있어야 한다 — 누를 자리를 `cards`가 알려 준다.
    #[test]
    fn a_tool_row_is_clickable_on_its_own() {
        let items = [work_at(1)];
        let folds = Folds::from([(1, Fold { open: true })]);
        let r = rows(&items, 60, &folds);

        // 추론 줄도 카드 접힘을 토글하므로 같은 seq가 여러 줄에 걸린다 — 보는 것은
        // "어느 seq를 누를 수 있는가"이지 줄 수가 아니다.
        let mut by_seq: Vec<i64> = r.cards.values().copied().collect();
        by_seq.sort();
        by_seq.dedup();
        assert_eq!(by_seq, vec![1, 100], "카드 머리와 도구 줄 둘 다 눌려야 한다");

        // 도구 줄의 행이 실제로 그 도구 줄이어야 한다 — 어긋나면 엉뚱한 게 펴진다.
        let lines = r.plain();
        let row = r.cards.iter().find(|(_, s)| **s == 100).map(|(r, _)| *r).unwrap();
        assert!(lines[row].contains("grep"), "{:?}", lines[row]);
    }

    /// **접힌 카드의 도구 줄도 눌러 펼 수 있어야 한다** — 보이는 것이면 누를 수
    /// 있어야 한다. 도구 상세는 카드 접힘과 무관하게 따로 펴진다.
    #[test]
    fn folded_tool_rows_are_still_clickable() {
        let items = [work_at(1)];
        let r = rows(&items, 60, &Folds::new());
        let mut by_seq: Vec<i64> = r.cards.values().copied().collect();
        by_seq.sort();
        assert_eq!(by_seq, vec![1, 100], "머리와 도구 줄 둘 다 눌려야 한다");

        let lines = r.plain();
        let row = r.cards.iter().find(|(_, s)| **s == 100).map(|(r, _)| *r).unwrap();
        assert!(lines[row].contains("grep"), "{:?}", lines[row]);
    }

    /// **추론 줄도 누를 수 있어야 한다** — 클릭하면 카드가 접히고 펴진다(Ctrl+O와
    /// 같은 동작). 도구 상세를 여는 것이 아니다.
    #[test]
    fn a_thinking_line_maps_to_the_card_fold() {
        let items = [work_at(1)];
        let r = rows(&items, 60, &Folds::from([(1, Fold { open: true })]));
        let lines = r.plain();
        let row = lines.iter().position(|l| l.contains("먼저 구조를 보자")).expect("추론 줄");
        let seq = r.cards.get(&row).copied().expect("추론 줄이 클릭 대상이어야 한다");
        assert_eq!(seq, 1, "추론 줄 클릭은 카드를 접고 펴야 한다");
    }

    /// **펼친 도구 상세는 섹션 머리말이 갈라 보인다.** "인자/출력/결과/오류" —
    /// 무엇이 인자고 무엇이 결과인지 눈이 한 번에 읽혀야 한다.
    #[test]
    fn tool_detail_lines_label_their_sections() {
        let out = tool_detail_lines("인자\n{\"cmd\": \"git push\"}\n\n출력\nUp to date", 60);
        let plain: Vec<String> = out
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(plain[0].contains("⎿ 인자"), "첫 줄이 머리말이어야 한다: {plain:?}");
        assert!(plain.iter().any(|l| l.contains("⎿ 출력")), "출력 머리말이 없다: {plain:?}");
        assert!(plain.iter().any(|l| l.contains("git push")), "인자 본문이 없다: {plain:?}");
        assert!(plain.iter().any(|l| l.contains("Up to date")), "출력 본문이 없다: {plain:?}");

        // 오류 섹션은 위험색으로 칠한다.
        let err = tool_detail_lines("오류\nboom", 60);
        assert!(
            err.iter().any(|l| {
                l.spans.iter().any(|s| s.content == "boom" && s.style.fg == Some(theme::DANGER))
            }),
            "오류 본문이 위험색이 아니다: {err:?}"
        );
    }

    /// 펼칠 것이 없는 도구는 눌러도 아무 일이 없어야 하니 누를 수 있는 척도 하지 않는다.
    #[test]
    fn a_tool_with_nothing_to_show_is_not_clickable() {
        let items = [Item::Work {
            seq: 1,
            title: "런".into(),
            parts: vec![Part::Step(Step {
                seq: 100,
                name: "todo".into(),
                note: "정리".into(),
                failed: false,
                detail: String::new(),
                diff: None,
            })],
        }];
        let folds = Folds::from([(1, Fold { open: true })]);
        let r = rows(&items, 60, &folds);
        assert_eq!(
            r.cards.values().copied().collect::<Vec<_>>(),
            vec![1],
            "상세가 없는 도구가 눌리는 줄로 잡혔다"
        );
        assert!(
            !r.plain().iter().any(|l| l.contains('▸') && l.contains("todo")),
            "{:?}",
            r.plain()
        );
    }

    /// **도구 하나를 펴면 그 항목이 다시 그려져야 한다.**
    /// 항목 자기 접힘만 보면 캐시가 "안 바뀌었다"고 여겨 화면이 그대로 있는다.
    #[test]
    fn opening_a_tool_row_redraws_the_card() {
        let items = [work_at(1)];
        let card_open = Folds::from([(1, Fold { open: true })]);
        let mut cache = Cache::new();
        cache.layout(&items, 60, &card_open, None);
        let before = cache.renders();

        let mut both = card_open.clone();
        both.insert(100, Fold { open: true });
        cache.layout(&items, 60, &both, None);
        assert_eq!(cache.renders(), before + 1, "도구를 폈는데 다시 안 그렸다");
        assert!(cache.plain().iter().any(|l| l.contains("인자")), "{:?}", cache.plain());
    }

    /// 제목이 아직 안 온 카드도 자리를 지켜야 한다.
    #[test]
    fn a_work_card_without_a_title_yet_says_it_is_working() {
        let items = [Item::Work { seq: 1, title: String::new(), parts: vec![] }];
        let out = plain(&rows(&items, 40, &Folds::new()));
        assert!(out[0].contains("작업 중"), "{out:?}");
    }
}
