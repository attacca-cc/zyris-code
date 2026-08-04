//! 세션 타임라인. **`seq`가 신원이고 `cursor`는 진행도다.**
//!
//! attacca는 durable 이벤트를 제자리 갱신하면서 `cursor`만 새로 발급하고 라이브
//! 구독자에게 다시 방송한다(`session_event_repo.rs`의 `cursor = nextval(...)`).
//! 그래서 여기는 언제나 업서트다 — append하면 작업 카드가 턴마다 불어난다.

use std::collections::BTreeMap;

use zyris_attacca::ZDeltaKind;

use crate::event::{Entry, EntryKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// 이 도구 호출 이벤트의 seq. **접힘 상태의 키다** — 도구 줄마다 따로 펴진다.
    pub seq: i64,
    /// 화면에 보이는 짧은 이름. `zyris__arch__terminal__exec`이 아니라 `exec`이다.
    pub name: String,
    /// 이름 옆에 흐리게 붙는 한 조각 — 무엇에 대고 한 일인가.
    pub note: String,
    pub failed: bool,
    /// 펼쳤을 때 보여 줄 것. 비면 펼칠 것이 없어 눌러도 아무 일이 없다.
    pub detail: String,
    /// 파일을 바꾼 도구면 여기 붙는다. 있으면 화면이 JSON 대신 이것을 그린다.
    pub diff: Option<crate::tools::diff::Diff>,
}

/// 작업 카드 안의 한 조각. **온 순서 그대로 늘어선다.**
///
/// 추론을 다 모아 놓고 도구를 그 아래에 몰아 두면 "무엇을 생각하다 무엇을 했는지"가
/// 사라진다. 모델이 실제로 한 일은 생각 → 도구 → 생각 → 도구이므로 그대로 보여 준다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Think(String),
    /// 런 중에 에이전트가 한 말. 도구·추론과 함께 온 순서대로 선다.
    Text(String),
    Step(Step),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    User {
        seq: i64,
        text: String,
    },
    Agent {
        seq: i64,
        text: String,
    },
    Work {
        seq: i64,
        title: String,
        parts: Vec<Part>,
    },
    Error {
        seq: i64,
        message: String,
    },
    Subagent {
        seq: i64,
        summary: String,
    },
    /// 에이전트가 물어본 것. 작업 카드 안이 아니라 **바깥에** 선다 — 답해야 하는 것이다.
    Question {
        seq: i64,
        steps: Vec<crate::question::Step>,
        answered: bool,
    },
    /// 앱이 한 말. 슬래시 명령의 결과와 되돌림 알림이 여기로 온다.
    ///
    /// **`Frame::Notice`와 다르다.** 그쪽은 6초 뒤 사라지는 상태 줄이고, 이것은 읽는
    /// 동안 남아 있어야 하는 것이다 — `/mcp`의 목록을 6초 안에 읽으라고 할 수는 없다.
    System {
        seq: i64,
        text: String,
    },
}

impl Item {
    pub fn seq(&self) -> i64 {
        match self {
            Item::User { seq, .. }
            | Item::Agent { seq, .. }
            | Item::Work { seq, .. }
            | Item::Error { seq, .. }
            | Item::Subagent { seq, .. }
            | Item::System { seq, .. }
            | Item::Question { seq, .. } => *seq,
        }
    }
}

#[derive(Debug, Default)]
pub struct Timeline {
    entries: BTreeMap<i64, Entry>,
    /// 아직 durable로 굳지 않은 답변 텍스트 토막. 앵커 순서로 쌓인다.
    live_text: Vec<LiveText>,
    /// 열려 있는 작업 카드의 추론 델타.
    live_reasoning: String,
    /// 만들어 둔 항목들. **바뀐 게 없으면 다시 만들지 않는다.**
    ///
    /// 예전에는 프레임마다 새로 만들었는데, 그러면 모든 이벤트의 문자열을 매번 복제한다.
    /// 대화가 길어질수록 그 비용이 선형으로 늘어 프레임 예산을 넘겼다 —
    /// `tests/perf.rs`가 재는 것이 이것이다.
    cache: Vec<Item>,
    dirty: bool,
    /// 실제로 다시 만든 횟수. 캐시가 정말 도는지 테스트가 이걸로 확인한다.
    rebuilds: u64,
    /// 앱이 한 말. 서버 이벤트와 섞여 시간 순서대로 선다.
    said: Vec<Said>,
    /// 다음에 줄 앱 항목의 seq. **음수다** — 아래 설명을 볼 것.
    next_said: i64,
}

/// 스트리밍으로 흘러온 답변 텍스트 한 토막.
///
/// durable 이벤트 사이에 흐른 텍스트라 아직 `entries`에 없다. 앵커(`after`) 뒤,
/// 다음 이벤트 앞에 선다. durable `chat_agent`가 오면 그 자리를 넘겨받고 사라진다.
#[derive(Debug, Clone)]
struct LiveText {
    /// 이 토막이 흐르기 시작할 때의 마지막 durable seq.
    after: i64,
    text: String,
}

/// 앱이 한 말 하나.
#[derive(Debug, Clone)]
struct Said {
    /// 말할 때 서버 이벤트가 어디까지 와 있었는가. 이 뒤, 다음 이벤트 앞에 놓인다.
    after: i64,
    /// 화면 항목의 신원. **음수라 서버 seq와 절대 안 부딪힌다.**
    ///
    /// 접힘 상태(`Folds`)와 줄 캐시(`rows::Cache`)가 seq를 키로 쓰기 때문에 겹치면
    /// 두 항목이 한 자리를 두고 싸운다. 화면 순서는 seq가 아니라 `build`가 만든
    /// 벡터 순서로 정해지므로, 음수라고 위로 올라가지 않는다.
    seq: i64,
    text: String,
}

impl Timeline {
    pub fn new() -> Self {
        Self { dirty: true, next_said: -1, ..Default::default() }
    }

    /// 앱이 한마디 한다. 슬래시 명령의 결과와 되돌림 알림이 여기로 온다.
    pub fn say(&mut self, text: impl Into<String>) {
        let after = self.entries.keys().next_back().copied().unwrap_or(0);
        self.said.push(Said { after, seq: self.next_said, text: text.into() });
        self.next_said -= 1;
        self.dirty = true;
    }

    /// 화면의 대화를 비운다. **서버의 기록은 그대로다** — `/clear`가 부른다.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.said.clear();
        self.live_text.clear();
        self.live_reasoning.clear();
        self.dirty = true;
    }

    pub fn upsert(&mut self, entry: Entry) {
        // 새 런이 열리면 앞 런의 추론이 딸려가면 안 된다.
        if let EntryKind::WorkStart(_) = &entry.kind {
            self.live_reasoning.clear();
        }
        self.entries.insert(entry.seq, entry);
        self.dirty = true;
    }

    pub fn push_delta(&mut self, kind: ZDeltaKind, text: &str) {
        match kind {
            ZDeltaKind::Assistant => {
                // 토막의 앵커는 "지금 마지막 durable 이벤트 뒤"다. 사이에 durable
                // 이벤트가 끼지 않았으면 같은 토막으로 합쳐져 하나의 답변으로 보이고,
                // 도구 호출이 끼면 새 토막이 되어 그 도구 **앞에** 선다.
                let after = self.entries.keys().next_back().copied().unwrap_or(0);
                match self.live_text.last_mut() {
                    Some(seg) if seg.after == after => seg.text.push_str(text),
                    _ => self.live_text.push(LiveText { after, text: text.to_string() }),
                }
            }
            ZDeltaKind::Reasoning => self.live_reasoning.push_str(text),
        }
        self.dirty = true;
    }

    /// 지금까지 항목을 다시 만든 횟수. 테스트 전용 관찰 창구다.
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// 화면에 그릴 항목들, seq 순서. 바뀐 게 없으면 앞서 만든 것을 그대로 돌려준다.
    ///
    /// 작업 카드의 경계는 서버가 정해 준 것을 그대로 쓴다 — `work_summary`의 `seq`보다
    /// 크고 다음 `work_summary` 전까지인 thinking/tool/todo가 그 카드의 것이다.
    /// attacca의 `WorkSummaryState.seq`가 "이 런에 속한 이벤트의 하한"인 것과 같다.
    pub fn items(&mut self) -> &[Item] {
        if self.dirty {
            self.cache = self.build();
            self.dirty = false;
            self.rebuilds += 1;
        }
        &self.cache
    }

    fn build(&mut self) -> Vec<Item> {
        let mut out: Vec<Item> = Vec::new();
        let mut open_work: Option<usize> = None;
        // 스트리밍 텍스트 토막. 이벤트 사이 제자리에 끼운다 — 끝에 몰아 붙이면
        // 뒤에 온 도구 줄 아래로 밀려 "순서가 뒤바뀐" 화면이 된다(실제로 그렇게
        // 보였다). durable `chat_agent`가 와서 굳으면 그 자리를 넘겨받고 지워진다.
        let mut live: Vec<LiveText> = std::mem::take(&mut self.live_text);
        // 이미 제자리에 놓은 토막까지. 이 build 안에서는 토막을 한 번만 배치한다.
        let mut flushed = 0usize;

        for entry in self.entries.values() {
            let seq = entry.seq;
            // durable 답변이 왔으면 그보다 앞선 토막은 제 몫을 다했다 — 같은 글이
            // 두 벌로 보이면 안 된다.
            if matches!(entry.kind, EntryKind::Agent(_)) {
                live.retain(|s| s.after >= seq);
            }
            // 이 이벤트 앞에 놓일 토막을 지금의 열린 카드에 끼운다.
            flush_live(&mut out, &mut open_work, &live, seq, &mut self.next_said, &mut flushed);
            match &entry.kind {
                EntryKind::User(text) => {
                    open_work = None;
                    out.push(Item::User { seq, text: text.clone() });
                }
                EntryKind::Agent(text) => {
                    // **런 중에 온 답변은 카드 안에 선다** — 도구 사이 메시지가 카드
                    // 밖으로 밀려나면 무엇을 말하다 무엇을 했는지가 흩어진다. 카드가
                    // 없으면 평범한 답변으로.
                    if let Some(at) = open_work {
                        if let Item::Work { parts, .. } = &mut out[at] {
                            parts.push(Part::Text(text.clone()));
                            continue;
                        }
                    }
                    out.push(Item::Agent { seq, text: text.clone() });
                }
                EntryKind::Error(message) => {
                    open_work = None;
                    out.push(Item::Error { seq, message: message.clone() });
                }
                EntryKind::Subagent(summary) => {
                    out.push(Item::Subagent { seq, summary: summary.clone() });
                }
                EntryKind::Question { steps, answered } => {
                    // 질문은 카드 안에 묻히면 안 된다. 런을 닫고 바깥에 세운다.
                    open_work = None;
                    out.push(Item::Question { seq, steps: steps.clone(), answered: *answered });
                }
                EntryKind::WorkStart(title) => {
                    open_work = Some(out.len());
                    out.push(Item::Work { seq, title: title.clone(), parts: Vec::new() });
                }
                EntryKind::Thinking(text) => {
                    let at = card_for(&mut out, &mut open_work, seq);
                    if let Item::Work { parts, .. } = &mut out[at] {
                        push_think(parts, text);
                    }
                }
                EntryKind::Tool { name, summary, failed, detail, diff, .. } => {
                    let at = card_for(&mut out, &mut open_work, seq);
                    if let Item::Work { parts, .. } = &mut out[at] {
                        parts.push(Part::Step(Step {
                            seq,
                            name: short_name(name),
                            // 파일을 바꾼 도구는 **바뀐 파일**이 요약이다 — 인자의
                            // `path`는 상대경로일 수 있고 결과의 것이 정본이다.
                            note: match diff {
                                Some(d) => d.path.clone(),
                                None => summary.clone(),
                            },
                            failed: *failed,
                            detail: detail.clone(),
                            diff: diff.clone(),
                        }));
                    }
                }
                EntryKind::Todo(text) => {
                    let at = card_for(&mut out, &mut open_work, seq);
                    if let Item::Work { parts, .. } = &mut out[at] {
                        parts.push(Part::Step(Step {
                            seq,
                            name: "todo".into(),
                            note: text.clone(),
                            failed: false,
                            detail: String::new(),
                            diff: None,
                        }));
                    }
                }
            }
        }

        // 끝까지 남은 토막 — 열린 카드 끝 또는 독립 답변으로.
        flush_live(&mut out, &mut open_work, &live, i64::MAX, &mut self.next_said, &mut flushed);

        // 아직 durable로 굳지 않은 추론 델타를 열린 카드 **끝에** 얹는다. 도구를 쓴
        // 뒤에 다시 생각하는 중이면 그 도구 아래에 붙어야 순서가 맞는다.
        if !self.live_reasoning.is_empty() {
            if let Some(Item::Work { parts, .. }) = open_work.map(|i| &mut out[i]) {
                push_think(parts, &self.live_reasoning);
            }
        }

        // 미래의 이벤트 뒤에 설 토막(아직 안 온 앵커)은 다시 들고 간다.
        self.live_text = live;
        self.weave_in_what_the_app_said(out)
    }

    /// 앱이 한 말을 서버 이벤트 사이 제자리에 끼운다.
    ///
    /// **한 번에 훑으며 합친다.** 하나씩 `insert`하면 앞서 넣은 것이 뒤 계산을 밀어
    /// 같은 자리에 말이 둘 이상 있을 때 순서가 뒤집힌다.
    ///
    /// 위치는 seq 비교가 아니라 **"after 항목 바로 뒤"**다 — 암시적 작업 카드처럼
    /// 항목 seq가 양수 구간을 벗어나 있어도 말한 자리가 흔들리지 않는다.
    fn weave_in_what_the_app_said(&self, items: Vec<Item>) -> Vec<Item> {
        if self.said.is_empty() {
            return items;
        }
        let mut out = Vec::with_capacity(items.len() + self.said.len());
        let mut said = self.said.iter().peekable();
        let mine = |s: &Said| Item::System { seq: s.seq, text: s.text.clone() };
        // 아무 이벤트도 없을 때 한 말은 맨 앞에 선다.
        while said.peek().is_some_and(|s| s.after == 0) {
            out.push(mine(said.next().expect("방금 봤다")));
        }
        let mut pending: Vec<&Said> = Vec::new();
        for item in items {
            // 이 항목 뒤에 놓일 말들 — after가 이 항목의 seq인 것.
            while said.peek().is_some_and(|s| s.after == item.seq()) {
                pending.push(said.next().expect("방금 봤다"));
            }
            out.push(item);
            if !pending.is_empty() {
                out.extend(pending.drain(..).map(mine));
            }
        }
        out.extend(said.map(mine));
        out
    }
}

/// 화면에 보이는 도구 이름.
///
/// attacca는 노드 도구를 `zyris__{노드}__{캐퍼빌리티}__{도구}`로 만들어 보낸다. 그대로 두면
/// 그것만으로 한 줄을 다 먹고, 매 줄이 같은 앞머리로 시작해 **정작 다른 부분이 안 보인다.**
/// 서버 빌트인(`todo_add`·`web_search`)에는 `__`가 없어 그대로 남는다.
///
/// 같은 이름이 둘 있을 수 있지만(`file_io.read`와 `terminal.read`) 옆의 요약이 갈라 준다 —
/// 하나는 경로고 하나는 PTY다.
fn short_name(name: &str) -> String {
    name.rsplit("__").next().unwrap_or(name).to_string()
}

/// 암시적 작업 카드의 항목 seq. 서버 seq(양수)·앱 말(음수 소수)·라이브 답변과 겹치지
/// 않도록 아주 먼 음수로 만든다 — 겹치면 접힘 상태와 줄 캐시가 두 항목을 한 자리로
/// 본다.
fn implicit_seq(first: i64) -> i64 {
    i64::MIN + first
}

/// 앵커가 `up_to`보다 앞선 스트리밍 토막을 지금 자리에 밀어 넣는다.
///
/// 카드가 열려 있으면 그 안에(첫 도구 앞, 아니면 끝), 없으면 독립 답변으로. 토막은
/// 앵커 순서로 쌓이므로 앞에서부터 차례로 본다. **지우지 않는다** — durable가 오기
/// 전까지는 이 토막이 그 텍스트의 유일한 자리이므로, 배치가 매번 처음부터 다시 하므로
/// 두 벌이 될 일도 없다.
fn flush_live(
    out: &mut Vec<Item>,
    open_work: &mut Option<usize>,
    live: &[LiveText],
    up_to: i64,
    next_seq: &mut i64,
    from: &mut usize,
) {
    // 토막은 앵커 순서로 쌓이고, `from`은 이 build에서 이미 배치한 만큼이다.
    // 없으면 같은 토막이 이벤트마다 다시 들어가 두 벌이 된다.
    while *from < live.len() {
        let seg = &live[*from];
        if seg.after >= up_to {
            break;
        }
        match open_work {
            Some(at) => {
                if let Item::Work { parts, .. } = &mut out[*at] {
                    splice_text(parts, seg.after, &seg.text);
                }
            }
            None => {
                // 독립 답변은 앱 말과 같은 음수 seq 공간을 쓴다 — 겹치면 안 된다.
                out.push(Item::Agent { seq: *next_seq, text: seg.text.clone() });
                *next_seq -= 1;
            }
        }
        *from += 1;
    }
}

/// 토막을 카드 안 제자리에 끼운다 — 앵커(`after`) 뒤에 온 첫 도구 **앞**, 도구가
/// 없으면 끝. 추론 줄 사이에 끼어 있어도 온 순서가 유지된다.
fn splice_text(parts: &mut Vec<Part>, after: i64, text: &str) {
    let at = parts
        .iter()
        .position(|p| matches!(p, Part::Step(s) if s.seq > after))
        .unwrap_or(parts.len());
    parts.insert(at, Part::Text(text.to_string()));
}

/// 추론을 덧붙인다. 도구 없이 이어진 추론은 한 덩어리로 합친다 —
/// 사이에 아무 일도 없었으면 나눠 봐야 읽기만 나빠진다.
/// 지금 열려 있는 작업 카드. **없으면 만든다.**
///
/// attacca는 런이 시작될 때 `work_summary`를 만들지만, 첫 추론 델타가 그보다 먼저 닿는
/// 턴이 있고 아예 `work_summary`가 없는 턴도 있다(도구를 안 쓰는 짧은 답). 예전에는 그때
/// 추론과 도구 호출을 **조용히 버렸다** — 사람 눈에는 "프롬프트를 보냈는데 화면에 아무것도
/// 안 뜬다"로 보인다. 접힌 것과 다르다. 아예 없다.
///
/// 그래서 갈 곳이 없으면 제목 없는 카드를 연다. 제목은 `work_summary`가 뒤늦게 와도
/// 붙는다 — 이벤트는 seq 순으로 다시 세워지므로 그때는 이 자리가 진짜 카드가 된다.
fn card_for(out: &mut Vec<Item>, open_work: &mut Option<usize>, seq: i64) -> usize {
    if let Some(at) = *open_work {
        return at;
    }
    let at = out.len();
    // **암시적 카드의 접힘 키를 첫 부분의 seq와 겹치지 않게** 만든다. 겹치면
    // 카드를 펼 때 첫 도구의 상세도 같은 키를 공유해 같이 펼쳐진다(실제로 그렇게
    // 보였다 — 추론 없는 툴 전용 턴에서 카드를 누르면 첫 도구의 인자·결과가 열린다).
    out.push(Item::Work { seq: implicit_seq(seq), title: String::new(), parts: Vec::new() });
    *open_work = Some(at);
    at
}

fn push_think(parts: &mut Vec<Part>, text: &str) {
    match parts.last_mut() {
        Some(Part::Think(prev)) => {
            prev.push('\n');
            prev.push_str(text);
        }
        _ => parts.push(Part::Think(text.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Entry, EntryKind};

    fn e(seq: i64, kind: EntryKind) -> Entry {
        Entry { seq, kind }
    }

    fn texts(t: &mut Timeline) -> Vec<String> {
        t.items()
            .iter()
            .map(|i| match i {
                Item::User { text, .. } | Item::Agent { text, .. } => text.clone(),
                Item::System { text, .. } => format!("앱: {text}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// **앱이 한 말은 말한 자리에 선다.** 맨 위나 맨 아래로 밀려나면 무엇에 대한
    /// 답인지 알 수 없다.
    #[test]
    fn what_the_app_says_lands_where_it_was_said() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("안녕".into())));
        t.say("작업 디렉터리는 /home/ruma입니다");
        t.upsert(e(2, EntryKind::Agent("네".into())));
        assert_eq!(
            texts(&mut t),
            vec!["안녕", "앱: 작업 디렉터리는 /home/ruma입니다", "네"],
            "말한 자리에 안 섰다"
        );
    }

    /// 같은 자리에 두 마디를 하면 **한 순서**여야 한다.
    #[test]
    fn two_things_said_in_a_row_keep_their_order() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("안녕".into())));
        t.say("첫째");
        t.say("둘째");
        t.upsert(e(2, EntryKind::Agent("네".into())));
        assert_eq!(texts(&mut t), vec!["안녕", "앱: 첫째", "앱: 둘째", "네"]);
    }

    /// **seq가 서버 이벤트와 겹치면 안 된다.** 접힘 상태와 줄 캐시가 seq를 키로 쓴다.
    #[test]
    fn what_the_app_says_never_collides_with_a_server_seq() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("안녕".into())));
        t.say("하나");
        t.say("둘");
        let seqs: Vec<i64> = t.items().iter().map(|i| i.seq()).collect();
        let unique: std::collections::HashSet<i64> = seqs.iter().copied().collect();
        assert_eq!(seqs.len(), unique.len(), "seq가 겹친다: {seqs:?}");
        assert!(seqs.iter().filter(|s| **s < 0).count() == 2, "{seqs:?}");
    }

    /// `/clear`는 **화면만** 비운다. 서버의 기록을 지우는 것이 아니다.
    #[test]
    fn clearing_empties_the_screen() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("안녕".into())));
        t.say("무언가");
        t.clear();
        assert!(t.items().is_empty(), "{:?}", t.items());
    }

    fn step_at(seq: i64, name: &str, note: &str) -> Step {
        Step {
            seq,
            name: name.into(),
            note: note.into(),
            failed: false,
            detail: "인자\n{}".into(),
            diff: None,
        }
    }

    /// 갱신된 이벤트는 같은 seq로 다시 온다. append하면 카드가 둘이 된다.
    #[test]
    fn re_receiving_the_same_seq_replaces_it_instead_of_appending() {
        let mut t = Timeline::new();
        t.upsert(e(10, EntryKind::WorkStart(String::new())));
        t.upsert(e(10, EntryKind::WorkStart("스크롤 계산 위치를 찾는 중".into())));

        let items = t.items();
        assert_eq!(items.len(), 1, "같은 seq는 하나로 남아야 한다");
        match &items[0] {
            Item::Work { title, .. } => assert_eq!(title, "스크롤 계산 위치를 찾는 중"),
            other => panic!("작업 카드여야 한다: {other:?}"),
        }
    }

    /// **작업 카드보다 먼저 온 추론도 보여야 한다.**
    ///
    /// attacca는 런이 시작될 때 `work_summary`를 만들지만, 첫 추론 델타가 그보다 먼저
    /// 닿는 턴이 있고 아예 `work_summary`가 없는 턴도 있다(도구를 안 쓰는 짧은 답).
    /// 그때 추론을 버리면 사람 눈에는 **"프롬프트를 보냈는데 아무 일도 안 일어난다"**로
    /// 보인다 — 접힌 것도 아니고 화면에 아무것도 없다.
    #[test]
    fn thinking_that_arrives_before_any_work_card_still_shows() {
        let mut t = Timeline::default();
        t.upsert(e(1, EntryKind::Thinking("무엇부터 볼까".into())));

        let items = t.items().to_vec();
        assert_eq!(items.len(), 1, "카드가 하나 생겨야 한다: {items:?}");
        match &items[0] {
            Item::Work { parts, .. } => {
                assert_eq!(parts, &vec![Part::Think("무엇부터 볼까".into())])
            }
            other => panic!("작업 카드여야 한다: {other:?}"),
        }
    }

    /// 도구 호출도 같다. **버려지면 무엇을 했는지가 통째로 사라진다.**
    #[test]
    fn a_tool_call_before_any_work_card_still_shows() {
        let mut t = Timeline::default();
        t.upsert(e(
            1,
            EntryKind::Tool {
                name: "zyris__arch__search__grep".into(),
                summary: "fn main".into(),
                failed: false,
                detail: String::new(),
                todo: None,
                diff: None,
            },
        ));
        match &t.items()[0] {
            Item::Work { parts, .. } => assert_eq!(parts.len(), 1, "{parts:?}"),
            other => panic!("작업 카드여야 한다: {other:?}"),
        }
    }

    /// **암시적 카드의 접힘 키는 첫 도구의 것과 겹치면 안 된다.** 겹치면 카드를
    /// 펼 때 첫 도구의 상세도 같은 키를 공유해 같이 펼쳐진다 — 추론 없는 툴 전용
    /// 턴에서 카드를 누르면 첫 도구의 인자·결과가 열린다(실제로 그렇게 보였다).
    #[test]
    fn an_implicit_cards_fold_key_never_collides_with_its_first_tool() {
        let mut t = Timeline::new();
        t.upsert(e(
            2,
            EntryKind::Tool {
                name: "zyris__arch__terminal__exec".into(),
                summary: "커밋".into(),
                failed: false,
                detail: "인자\n{}\n\n출력\nok".into(),
                todo: None,
                diff: None,
            },
        ));
        let Item::Work { seq, parts, .. } = &t.items()[0] else {
            panic!("암시적 카드여야 한다");
        };
        let Part::Step(first) = &parts[0] else {
            panic!("첫 부분은 도구여야 한다");
        };
        assert_ne!(*seq, first.seq, "카드 접힘 키가 첫 도구와 겹친다");
    }

    /// **스트리밍 답변은 턴이 시작된 자리에 선다.** 끝에 붙이면 그 뒤에 온 도구 줄
    /// 아래로 밀려 순서가 뒤바뀐 것처럼 보인다(실제로 그렇게 보였다).
    #[test]
    fn the_streaming_answer_sits_where_the_turn_started() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("질문".into())));
        t.push_delta(ZDeltaKind::Assistant, "전부 통과");
        t.upsert(e(
            2,
            EntryKind::Tool {
                name: "zyris__arch__terminal__exec".into(),
                summary: "커밋".into(),
                failed: false,
                detail: "인자\n{}\n\n출력\nok".into(),
                todo: None,
                diff: None,
            },
        ));
        let items = t.items().to_vec();
        let agent_at = items
            .iter()
            .position(|i| matches!(i, Item::Agent { text, .. } if text == "전부 통과"));
        let work_at = items.iter().position(|i| matches!(i, Item::Work { .. }));
        assert!(agent_at.is_some() && work_at.is_some(), "{items:?}");
        assert!(agent_at.unwrap() < work_at.unwrap(), "텍스트가 도구 아래로 밀렸다: {items:?}");
    }

    /// **뒤늦게 온 `work_summary`가 그 카드를 이어받는다.** 이벤트는 seq 순으로 다시
    /// 세워지므로, 제목이 붙은 카드 하나로 합쳐져야 한다 — 둘로 갈라지면 같은 런이
    /// 화면에서 두 덩이가 된다.
    #[test]
    fn a_late_work_summary_takes_over_the_card_it_belongs_to() {
        let mut t = Timeline::default();
        t.upsert(e(2, EntryKind::Thinking("먼저 구조를 보자".into())));
        t.upsert(e(1, EntryKind::WorkStart("리팩터링".into())));

        let items = t.items().to_vec();
        assert_eq!(items.len(), 1, "카드가 둘로 갈라졌다: {items:?}");
        match &items[0] {
            Item::Work { title, parts, .. } => {
                assert_eq!(title, "리팩터링");
                assert_eq!(parts, &vec![Part::Think("먼저 구조를 보자".into())]);
            }
            other => panic!("작업 카드여야 한다: {other:?}"),
        }
    }

    /// 카드의 경계는 서버가 정한다 — 다음 work_summary 전까지가 이 카드의 것이다.
    #[test]
    fn a_work_card_owns_the_steps_until_the_next_work_summary() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("첫 런".into())));
        t.upsert(e(2, EntryKind::Thinking("먼저 구조를 보자".into())));
        t.upsert(e(
            3,
            EntryKind::Tool {
                name: "grep".into(),
                summary: "viewport".into(),
                failed: false,
                detail: "인자\n{}".into(),
                todo: None,
                diff: None,
            },
        ));
        t.upsert(e(4, EntryKind::WorkStart("둘째 런".into())));
        t.upsert(e(
            5,
            EntryKind::Tool {
                name: "read".into(),
                summary: "rows.rs".into(),
                failed: false,
                detail: "인자\n{}".into(),
                todo: None,
                diff: None,
            },
        ));

        let items = t.items();
        assert_eq!(items.len(), 2);
        match (&items[0], &items[1]) {
            (Item::Work { parts: first, .. }, Item::Work { parts: second, .. }) => {
                assert_eq!(
                    first,
                    &vec![
                        Part::Think("먼저 구조를 보자".into()),
                        Part::Step(step_at(3, "grep", "viewport")),
                    ],
                    "첫 카드는 생각 다음 도구"
                );
                assert_eq!(second.len(), 1, "둘째 카드는 read 하나");
            }
            other => panic!("작업 카드 둘이어야 한다: {other:?}"),
        }
    }

    /// **도구는 쓴 그 자리에 선다.** 추론을 다 모아 놓고 도구를 아래에 몰면
    /// 무엇을 생각하다 무엇을 했는지가 사라진다.
    #[test]
    fn thinking_and_tools_stay_in_the_order_they_happened() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("런".into())));
        t.upsert(e(2, EntryKind::Thinking("먼저 어디를 볼까".into())));
        t.upsert(e(
            3,
            EntryKind::Tool {
                name: "grep".into(),
                summary: "rows".into(),
                failed: false,
                detail: "인자\n{}".into(),
                todo: None,
                diff: None,
            },
        ));
        t.upsert(e(4, EntryKind::Thinking("찾았다. 이제 고치자".into())));
        t.upsert(e(
            5,
            EntryKind::Tool {
                name: "edit".into(),
                summary: "rows.rs".into(),
                failed: false,
                detail: "인자\n{}".into(),
                todo: None,
                diff: None,
            },
        ));

        let Item::Work { parts, .. } = &t.items()[0] else { panic!("작업 카드여야 한다") };
        assert_eq!(
            parts,
            &vec![
                Part::Think("먼저 어디를 볼까".into()),
                Part::Step(step_at(3, "grep", "rows")),
                Part::Think("찾았다. 이제 고치자".into()),
                Part::Step(step_at(5, "edit", "rows.rs")),
            ]
        );
    }

    /// 도구 없이 이어진 추론은 한 덩어리다. 사이에 아무 일도 없었으면 나눌 이유가 없다.
    #[test]
    fn back_to_back_thinking_merges_into_one_block() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("런".into())));
        t.upsert(e(2, EntryKind::Thinking("첫 생각".into())));
        t.upsert(e(3, EntryKind::Thinking("이어지는 생각".into())));

        let Item::Work { parts, .. } = &t.items()[0] else { panic!("작업 카드여야 한다") };
        assert_eq!(parts, &vec![Part::Think("첫 생각\n이어지는 생각".into())]);
    }

    /// 도구를 쓴 뒤 흘러오는 추론 델타는 **그 도구 아래에** 붙어야 한다.
    #[test]
    fn reasoning_deltas_after_a_tool_land_below_that_tool() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("런".into())));
        t.upsert(e(2, EntryKind::Thinking("먼저 보자".into())));
        t.upsert(e(
            3,
            EntryKind::Tool {
                name: "grep".into(),
                summary: "rows".into(),
                failed: false,
                detail: "인자\n{}".into(),
                todo: None,
                diff: None,
            },
        ));
        t.push_delta(ZDeltaKind::Reasoning, "결과를 읽어 보니");

        let Item::Work { parts, .. } = &t.items()[0] else { panic!("작업 카드여야 한다") };
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert_eq!(parts[2], Part::Think("결과를 읽어 보니".into()));
    }

    /// 답변 델타는 durable 이벤트가 오기 전까지 화면에 보여야 한다.
    #[test]
    fn assistant_deltas_show_before_the_durable_event_arrives() {
        let mut t = Timeline::new();
        t.push_delta(ZDeltaKind::Assistant, "답변이 ");
        t.push_delta(ZDeltaKind::Assistant, "흘러온다");

        match t.items().last().unwrap() {
            Item::Agent { text, .. } => assert_eq!(text, "답변이 흘러온다"),
            other => panic!("답변이어야 한다: {other:?}"),
        }
    }

    /// durable 이벤트가 오면 델타 버퍼가 중복으로 남으면 안 된다.
    #[test]
    fn the_durable_agent_event_supersedes_the_delta_buffer() {
        let mut t = Timeline::new();
        t.push_delta(ZDeltaKind::Assistant, "답변이 흘러온다");
        t.upsert(e(7, EntryKind::Agent("답변이 흘러온다".into())));

        let agents = t.items().iter().filter(|i| matches!(i, Item::Agent { .. })).count();
        assert_eq!(agents, 1, "델타와 durable이 겹쳐 두 벌이 되면 안 된다");
    }

    /// 추론 델타는 열려 있는 작업 카드 안으로 들어간다.
    #[test]
    fn reasoning_deltas_go_into_the_open_work_card() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart(String::new())));
        t.push_delta(ZDeltaKind::Reasoning, "무엇부터 볼까");

        match &t.items()[0] {
            Item::Work { parts, .. } => {
                assert_eq!(parts, &vec![Part::Think("무엇부터 볼까".into())])
            }
            other => panic!("작업 카드여야 한다: {other:?}"),
        }
    }

    /// 바뀐 게 없으면 항목을 다시 만들지 않는다.
    ///
    /// 프레임마다 다시 만들면 모든 이벤트의 문자열을 매번 복제한다. 대화가 길어질수록
    /// 그 비용이 선형으로 늘어 화면이 밀린다 — 실제로 그것 때문에 랙이 걸렸다.
    #[test]
    fn items_are_not_rebuilt_when_nothing_changed() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::User("안녕".into())));

        t.items();
        t.items();
        t.items();
        assert_eq!(t.rebuilds(), 1, "안 바뀌었으면 한 번만 만들어야 한다");

        t.push_delta(ZDeltaKind::Assistant, "답");
        t.items();
        assert_eq!(t.rebuilds(), 2, "델타가 오면 다시 만들어야 한다");

        t.upsert(e(2, EntryKind::Agent("답".into())));
        t.items();
        assert_eq!(t.rebuilds(), 3, "이벤트가 오면 다시 만들어야 한다");
    }

    /// 새 런이 시작되면 앞 런의 추론 델타가 딸려오면 안 된다.
    #[test]
    fn a_new_work_run_starts_with_empty_reasoning() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("첫 런".into())));
        t.push_delta(ZDeltaKind::Reasoning, "첫 런의 생각");
        t.upsert(e(2, EntryKind::WorkStart("둘째 런".into())));

        match &t.items()[1] {
            Item::Work { parts, .. } => assert!(parts.is_empty(), "{parts:?}"),
            other => panic!("작업 카드여야 한다: {other:?}"),
        }
    }

    fn tool_at(seq: i64, name: &str, note: &str) -> Entry {
        e(
            seq,
            EntryKind::Tool {
                name: name.into(),
                summary: note.into(),
                failed: false,
                // `step_at` 기대값과 맞추기 위해 상세도 같은 꼴을 쓴다.
                detail: "인자\n{}".into(),
                todo: None,
                diff: None,
            },
        )
    }

    /// **런 중에 온 답변은 카드 안에 선다** — 도구 사이 메시지가 카드 밖으로
    /// 밀려나면 "무엇을 말하다 무엇을 했는지"가 흩어진다. 카드도 조각나면 안 된다.
    #[test]
    fn agent_text_during_a_run_goes_into_the_card_in_order() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("커밋".into())));
        t.upsert(e(2, EntryKind::Agent("이제 커밋합니다".into())));
        t.upsert(tool_at(3, "exec", "git commit"));
        t.upsert(e(4, EntryKind::Agent("커밋하고 푸시하는 중".into())));
        t.upsert(tool_at(5, "exec", "git push"));

        let items = t.items().to_vec();
        assert_eq!(items.len(), 1, "카드가 쪼개지면 안 된다: {items:?}");
        match &items[0] {
            Item::Work { parts, .. } => assert_eq!(
                parts,
                &vec![
                    Part::Text("이제 커밋합니다".into()),
                    Part::Step(step_at(3, "exec", "git commit")),
                    Part::Text("커밋하고 푸시하는 중".into()),
                    Part::Step(step_at(5, "exec", "git push")),
                ],
                "말한 순서 그대로 카드 안에 있어야 한다"
            ),
            other => panic!("작업 카드여야 한다: {other:?}"),
        }
    }

    /// **스트리밍 텍스트는 그 뒤에 오는 도구 앞에 선다** — 카드 안, 도구 줄 위.
    #[test]
    fn live_text_during_a_run_lands_before_the_following_tool() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("커밋".into())));
        t.push_delta(ZDeltaKind::Assistant, "전부 통과. 이제 커밋합니다");
        t.upsert(tool_at(2, "exec", "git commit"));
        let Item::Work { parts, .. } = &t.items()[0] else {
            panic!("작업 카드여야 한다");
        };
        assert_eq!(
            parts,
            &vec![
                Part::Text("전부 통과. 이제 커밋합니다".into()),
                Part::Step(step_at(2, "exec", "git commit")),
            ],
            "텍스트가 도구 아래로 밀렸다"
        );
    }

    /// **도구로 갈라진 스트리밍 텍스트는 두 토막으로 나뉜다** — 하나로 합쳐지면
    /// 두 메시지가 한 덩어리로 보인다.
    #[test]
    fn live_text_split_by_a_tool_stays_two_segments() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("커밋".into())));
        t.push_delta(ZDeltaKind::Assistant, "이제 커밋합니다");
        t.upsert(tool_at(2, "exec", "git commit"));
        t.push_delta(ZDeltaKind::Assistant, "커밋하고 푸시하는 중");
        let Item::Work { parts, .. } = &t.items()[0] else {
            panic!("작업 카드여야 한다");
        };
        assert_eq!(
            parts,
            &vec![
                Part::Text("이제 커밋합니다".into()),
                Part::Step(step_at(2, "exec", "git commit")),
                Part::Text("커밋하고 푸시하는 중".into()),
            ],
            "도구 사이 두 토막이 합쳐졌다: {parts:?}"
        );
    }

    /// **사용자가 끼어들면 그 뒤의 텍스트는 카드 밖 독립 답변이다.**
    #[test]
    fn text_after_a_user_interrupt_stays_standalone() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("런".into())));
        t.upsert(tool_at(2, "grep", "rows"));
        t.upsert(e(3, EntryKind::User("잠깐".into())));
        t.push_delta(ZDeltaKind::Assistant, "네 알겠습니다");

        let items = t.items().to_vec();
        let work = items.iter().filter(|i| matches!(i, Item::Work { .. })).count();
        assert_eq!(work, 1, "끼어든 뒤에 새 카드가 생기면 안 된다: {items:?}");
        let user_at = items.iter().position(|i| matches!(i, Item::User { .. })).unwrap();
        let agent_at =
            items.iter().position(|i| matches!(i, Item::Agent { text, .. } if text == "네 알겠습니다"))
                .expect("독립 답변이 없다");
        assert!(user_at < agent_at, "답변이 사용자 말 뒤에 있어야 한다: {items:?}");
    }

    /// **토막은 다시 만들어도 두 벌이 되지 않는다** — 배치가 매번 처음부터 다시 하므로
    /// 토막이 남아 있어도 카드 안에는 한 번만 들어간다.
    #[test]
    fn live_segments_are_not_duplicated_across_rebuilds() {
        let mut t = Timeline::new();
        t.upsert(e(1, EntryKind::WorkStart("런".into())));
        t.push_delta(ZDeltaKind::Assistant, "생각");
        let _ = t.items();
        t.upsert(tool_at(2, "grep", "rows"));
        let Item::Work { parts, .. } = &t.items()[0] else {
            panic!("작업 카드여야 한다");
        };
        assert_eq!(
            parts,
            &vec![
                Part::Text("생각".into()),
                Part::Step(step_at(2, "grep", "rows")),
            ],
            "토막이 두 번 들어갔다: {parts:?}"
        );
    }
}
