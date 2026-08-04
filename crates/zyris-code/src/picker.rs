//! 프로젝트와 세션 고르기.
//!
//! ← 를 누르면 프로젝트 목록이 열리고, 프로젝트를 고르면 그 안의 세션 목록으로 들어간다.
//! 각 목록 맨 위에는 "새로 만들기" 줄이 있다.
//!
//! **프로젝트를 만드는 줄은 이름과 설명을 받아야 한다.** 그런데 목록에는 글자를 칠 자리가
//! 없다. 그래서 그 줄은 양식(`newproject::Form`)을 연다 — 이름과 설명을 두 칸에 나눠
//! 받고, 목록은 그 아래에 그대로 열려 있어서 Esc로 닫으면 다시 그 자리로 돌아온다.
//!
//! 여기는 순수하다. 목록을 가져오고 세션을 만드는 것은 I/O 자리가 한다.

/// 목록의 한 줄.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// 고르면 무엇이 되는가. `None`이면 "새로 만들기" 줄이다.
    pub id: Option<String>,
    pub label: String,
    /// 오른쪽에 흐리게 붙는 설명.
    pub note: Option<String>,
    /// 누를 수 있는가.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    Projects,
    Sessions {
        project_id: String,
        project_name: String,
    },
    /// `/agent`이 여는 목록. 프로젝트 계층과 무관해서 뒤로 갈 곳이 없다.
    Agents,
    /// `/`를 쳤을 때 뜨는 슬래시 명령 목록.
    Commands,
    /// `/lang`이 여는 화면 말 목록.
    Languages,
}

/// 고르면 무슨 일이 벌어지는가.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// 이 프로젝트의 세션 목록으로 들어간다.
    OpenProject { id: String, name: String },
    /// 이 세션으로 갈아탄다. **프로젝트를 같이 들려 보낸다** — 고른 세션이 어느
    /// 프로젝트의 것인지는 여기서만 알 수 있고, 잃으면 다음에 여는 job·work가
    /// 기본 프로젝트로 떨어진다.
    OpenSession { id: String, project_id: String },
    /// 이 프로젝트에 새 세션을 만든다.
    NewSession { project_id: String },
    /// 새 프로젝트 양식(`newproject::Form`)을 연다. 이름과 설명을 받아 만든다.
    NewProject,
    /// 이 에이전트로 간다. 다음 메시지에서 새 세션이 열린다.
    UseAgent { name: String },
    /// 이 명령을 입력란에 넣는다. **바로 실행하지 않는다** — `/mode`처럼 인자를 받는
    /// 것이 있어서 고른 뒤 이어 칠 수 있어야 한다.
    TypeCommand { text: String },
    /// 이 언어로 화면을 바꾼다.
    UseLang { lang: crate::lang::Lang },
    /// 누를 수 없는 줄. 이유를 말해 준다.
    Unavailable(String),
}

/// 목록 상자 안에 들어갈 줄 하나. **배치는 순수하게 정하고 위젯은 그리기만 한다.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// 이 인덱스의 줄.
    Row(usize),
    /// "새로 만들기"와 실제 목록을 가르는 선. 둘은 뜻이 다른 줄이다.
    Rule,
    /// 이쪽으로 몇 개가 더 있다. **없으면 목록이 거기서 끝난 줄 안다.**
    More { count: usize, up: bool },
}

/// 몇 줄이 들어가는지 정하고 그 안에 무엇을 놓을지 배치한다.
///
/// **커서는 언제나 보인다.** 그리고 잘린 쪽에는 몇 개가 더 있는지 적는다 — 안 적으면
/// 목록이 거기서 끝난 줄 알고 아래를 안 본다.
pub fn slots(rows: &[Row], cursor: usize, height: usize) -> Vec<Slot> {
    if rows.is_empty() || height == 0 {
        return Vec::new();
    }
    let has_create = rows.first().is_some_and(|r| r.id.is_none());

    // 표시줄과 가름선이 자리를 먹으므로 몇 줄이 남는지가 스스로에게 달려 있다.
    // 두어 바퀴면 고정점에 닿는다.
    let (mut room, mut start, mut end) = (height, 0usize, rows.len());
    for _ in 0..3 {
        let visible = room.max(1).min(rows.len());
        start = cursor.saturating_sub(visible.saturating_sub(1));
        end = (start + visible).min(rows.len());
        let extra = (start > 0) as usize
            + (end < rows.len()) as usize
            // 가름선은 "새로 만들기"가 실제로 보이고 그 아래에 뭔가 있을 때만 긋는다.
            + (has_create && start == 0 && end > 1) as usize;
        let want = height.saturating_sub(extra).max(1);
        if want == room {
            break;
        }
        room = want;
    }

    let mut out = Vec::with_capacity(height);
    if start > 0 {
        out.push(Slot::More { count: start, up: true });
    }
    for i in start..end {
        out.push(Slot::Row(i));
        if has_create && i == 0 && end > 1 {
            out.push(Slot::Rule);
        }
    }
    if end < rows.len() {
        out.push(Slot::More { count: rows.len() - end, up: false });
    }
    // **아주 좁으면 곁들임부터 버린다.** 표시줄도 가름선도 커서가 보이는 것보다 뒤다 —
    // 상자를 뚫고 나가면 화면이 무너진다.
    while out.len() > height {
        match out.iter().rposition(|s| !matches!(s, Slot::Row(_))) {
            Some(i) => {
                out.remove(i);
            }
            None => out.truncate(height),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    pub level: Level,
    pub rows: Vec<Row>,
    pub cursor: usize,
    /// 목록을 아직 받아오는 중인가.
    pub loading: bool,
}

impl Picker {
    /// 목록을 기다리는 빈 프로젝트 화면.
    pub fn loading_projects() -> Self {
        Self { level: Level::Projects, rows: Vec::new(), cursor: 0, loading: true }
    }

    /// 프로젝트 목록. 맨 위는 새 프로젝트 줄이다.
    pub fn projects(items: Vec<(String, String, bool)>, lang: crate::lang::Lang) -> Self {
        let mut rows = vec![Row {
            id: None,
            label: lang.new_project().into(),
            // 누르면 무슨 일이 벌어지는지 그 자리에서 말한다.
            note: Some(lang.new_project_note().into()),
            enabled: true,
        }];
        rows.extend(items.into_iter().map(|(id, name, is_default)| Row {
            id: Some(id),
            label: name,
            note: is_default.then(|| lang.default_project().to_string()),
            enabled: true,
        }));
        // 첫 줄은 못 누르는 '새 프로젝트'라 두 번째(첫 실제 프로젝트)에서 시작한다.
        let cursor = if rows.len() > 1 { 1 } else { 0 };
        Self { level: Level::Projects, rows, cursor, loading: false }
    }

    /// 한 프로젝트의 세션 목록. 맨 위는 새 세션 줄이다.
    pub fn sessions(
        project_id: String,
        project_name: String,
        items: Vec<(String, String, bool)>,
        lang: crate::lang::Lang,
    ) -> Self {
        let mut rows =
            vec![Row { id: None, label: lang.new_thread().into(), note: None, enabled: true }];
        rows.extend(items.into_iter().map(|(id, title, running)| Row {
            id: Some(id),
            label: title,
            note: running.then(|| lang.running().to_string()),
            enabled: true,
        }));
        Self {
            level: Level::Sessions { project_id, project_name },
            rows,
            cursor: 0,
            loading: false,
        }
    }

    /// 에이전트 목록. `/agent`이 연다.
    pub fn agents(rows: Vec<Row>) -> Self {
        Self { level: Level::Agents, rows, cursor: 0, loading: false }
    }

    /// 슬래시 명령 목록. `/`를 치면 열린다.
    ///
    /// **목록은 `command::catalogue()` 하나에서 온다** — 여기서 따로 적으면 하나가 낡는다.
    /// 언어는 화면이 정해 준다(`State.lang`).
    pub fn commands(lang: crate::lang::Lang) -> Self {
        let rows = crate::command::catalogue(lang)
            .into_iter()
            .map(|(name, note)| Row {
                id: Some(name.to_string()),
                label: name.to_string(),
                note: Some(note.to_string()),
                enabled: true,
            })
            .collect();
        Self { level: Level::Commands, rows, cursor: 0, loading: false }
    }

    /// 화면 말 목록. **이름은 저마다 제 언어로 적는다** — 지금 못 읽는 말로 적혀 있으면
    /// 무엇을 고르는지 알 수 없다. 커서는 지금 쓰는 언어에 둔다.
    pub fn languages(now: crate::lang::Lang) -> Self {
        use crate::lang::Lang;
        let rows: Vec<Row> = [Lang::En, Lang::Ko]
            .into_iter()
            .map(|lang| Row {
                id: Some(lang.code().to_string()),
                label: lang.name().to_string(),
                note: (lang == now).then(|| now.in_use().to_string()),
                enabled: true,
            })
            .collect();
        let cursor = rows.iter().position(|r| r.id.as_deref() == Some(now.code())).unwrap_or(0);
        Self { level: Level::Languages, rows, cursor, loading: false }
    }

    /// 친 글로 목록을 좁힌다. 남는 것이 없으면 그대로 둔다 — 빈 목록은 고장으로 보인다.
    pub fn narrow(&mut self, typed: &str, lang: crate::lang::Lang) {
        if !matches!(self.level, Level::Commands) {
            return;
        }
        *self = Picker::commands(lang);
        self.rows.retain(|r| r.label.starts_with(typed));
        self.cursor = 0;
    }

    /// 이 줄이 "새로 만들기"인가. 목록과 갈라 놓는 기준이다.
    pub fn is_create(&self, i: usize) -> bool {
        self.rows.get(i).is_some_and(|r| r.id.is_none())
    }

    pub fn up(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.cursor = (self.cursor + self.rows.len() - 1) % self.rows.len();
    }

    pub fn down(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.cursor = (self.cursor + 1) % self.rows.len();
    }

    /// 지금 줄을 고른다.
    pub fn pick(&self) -> Option<Pick> {
        let row = self.rows.get(self.cursor)?;
        if !row.enabled {
            return Some(Pick::Unavailable(
                row.note.clone().unwrap_or_else(|| "지금은 고를 수 없습니다".into()),
            ));
        }
        match (&self.level, &row.id) {
            (Level::Projects, Some(id)) => {
                Some(Pick::OpenProject { id: id.clone(), name: row.label.clone() })
            }
            (Level::Sessions { project_id, .. }, Some(id)) => {
                Some(Pick::OpenSession { id: id.clone(), project_id: project_id.clone() })
            }
            (Level::Sessions { project_id, .. }, None) => {
                Some(Pick::NewSession { project_id: project_id.clone() })
            }
            // **바로 만들지 않는다.** 이름과 설명을 받아야 한다 — 양식이 그 자리를 맡는다.
            (Level::Projects, None) => Some(Pick::NewProject),
            (Level::Agents, Some(name)) => Some(Pick::UseAgent { name: name.clone() }),
            (Level::Commands, Some(text)) => Some(Pick::TypeCommand { text: text.clone() }),
            (Level::Languages, Some(code)) => {
                crate::lang::Lang::parse(code).map(|lang| Pick::UseLang { lang })
            }
            (Level::Agents, None) | (Level::Commands, None) | (Level::Languages, None) => None,
        }
    }

    /// 화면 제목.
    pub fn title(&self, lang: crate::lang::Lang) -> String {
        match &self.level {
            Level::Projects => lang.projects().into(),
            Level::Sessions { project_name, .. } => lang.threads_in(project_name),
            Level::Agents => lang.agents().into(),
            Level::Commands => lang.commands().into(),
            Level::Languages => lang.language().into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projects() -> Picker {
        Picker::projects(
            vec![("p1".into(), "기본 프로젝트".into(), true), ("p2".into(), "zyris".into(), false)],
            crate::lang::Lang::Ko,
        )
    }

    fn many(n: usize) -> Vec<Row> {
        let mut rows = vec![Row {
            id: None,
            label: crate::lang::Lang::Ko.new_thread().into(),
            note: None,
            enabled: true,
        }];
        rows.extend((0..n).map(|i| Row {
            id: Some(format!("s{i}")),
            label: format!("thread {i}"),
            note: None,
            enabled: true,
        }));
        rows
    }

    /// 다 들어가면 표시줄이 없다. 안 잘렸는데 "더 있다"고 하면 거짓말이다.
    #[test]
    fn a_list_that_fits_gets_no_overflow_marks() {
        let rows = many(3);
        let got = slots(&rows, 0, 10);
        assert!(!got.iter().any(|s| matches!(s, Slot::More { .. })), "{got:?}");
        assert_eq!(got.iter().filter(|s| matches!(s, Slot::Row(_))).count(), 4);
    }

    /// **잘린 쪽에는 몇 개가 더 있는지 적는다.** 안 적으면 목록이 거기서 끝난 줄 안다.
    #[test]
    fn a_long_list_says_how_many_are_left_below() {
        let rows = many(30);
        let got = slots(&rows, 0, 8);
        let Some(Slot::More { count, up }) = got.last() else {
            panic!("아래로 남은 개수가 없다: {got:?}");
        };
        assert!(!up);
        let shown = got.iter().filter(|s| matches!(s, Slot::Row(_))).count();
        assert_eq!(shown + count, rows.len(), "센 것이 안 맞는다: {got:?}");
    }

    /// 아래로 내려가면 위쪽에도 남은 개수가 붙는다.
    #[test]
    fn scrolling_down_marks_what_is_left_above() {
        let rows = many(30);
        let got = slots(&rows, 20, 8);
        assert!(
            matches!(got.first(), Some(Slot::More { up: true, .. })),
            "위로 남은 개수가 없다: {got:?}"
        );
    }

    /// **커서는 언제나 보인다.** 안 보이면 무엇을 고르는지 알 수 없다.
    #[test]
    fn the_cursor_is_always_inside_the_window() {
        let rows = many(40);
        for cursor in [0, 1, 7, 20, 40] {
            let got = slots(&rows, cursor, 9);
            assert!(
                got.iter().any(|s| matches!(s, Slot::Row(i) if *i == cursor)),
                "커서 {cursor}가 안 보인다: {got:?}"
            );
        }
    }

    /// 배치가 주어진 높이를 넘으면 상자를 뚫고 나간다.
    #[test]
    fn the_layout_never_exceeds_the_height_it_was_given() {
        let rows = many(40);
        for height in 1..14 {
            for cursor in [0, 5, 39] {
                let got = slots(&rows, cursor, height);
                assert!(got.len() <= height, "{height}줄에 {}줄: {got:?}", got.len());
            }
        }
    }

    /// **"새로 만들기"는 목록과 갈라 놓는다.** 붙여 두면 세션 하나로 읽힌다.
    #[test]
    fn the_create_row_is_ruled_off_from_the_list() {
        let got = slots(&many(3), 0, 10);
        assert_eq!(got[0], Slot::Row(0));
        assert_eq!(got[1], Slot::Rule, "가름선이 없다: {got:?}");
        assert_eq!(got[2], Slot::Row(1));
    }

    /// 만들 줄이 안 보이는 자리까지 내려갔으면 가름선도 없다.
    #[test]
    fn there_is_no_rule_once_the_create_row_scrolls_away() {
        let got = slots(&many(30), 25, 8);
        assert!(!got.contains(&Slot::Rule), "{got:?}");
    }

    /// 만들 줄이 없는 목록(에이전트·명령)에는 가름선을 긋지 않는다.
    #[test]
    fn a_list_without_a_create_row_has_no_rule() {
        let got = slots(&Picker::commands(crate::lang::Lang::Ko).rows, 0, 20);
        assert!(!got.contains(&Slot::Rule), "{got:?}");
    }

    fn agents() -> Picker {
        Picker::agents(
            ["Main Agent", "Zyris Code"]
                .into_iter()
                .map(|n| Row { id: Some(n.into()), label: n.into(), note: None, enabled: true })
                .collect(),
        )
    }

    /// 에이전트를 고르면 그 이름이 나와야 한다 — 세션을 새로 열 때 그것으로 찾는다.
    #[test]
    fn picking_an_agent_gives_back_its_name() {
        let mut p = agents();
        p.down();
        assert_eq!(p.pick(), Some(Pick::UseAgent { name: "Zyris Code".into() }));
    }

    /// **목록과 파서가 갈라지면 고른 것이 안 먹는다.** 목록은 `command::catalogue()`에서 온다.
    #[test]
    fn every_command_row_is_something_the_parser_knows() {
        for row in Picker::commands(crate::lang::Lang::Ko).rows {
            assert!(crate::command::parse(&row.label).is_some(), "{}", row.label);
        }
    }

    /// **고르면 입력란에 들어갈 뿐 바로 돌지 않는다.** `/mode`는 인자를 받는다.
    #[test]
    fn picking_a_command_only_types_it() {
        let p = Picker::commands(crate::lang::Lang::Ko);
        assert_eq!(p.pick(), Some(Pick::TypeCommand { text: "/help".into() }));
    }

    /// 친 글로 좁혀진다. 안 좁혀지면 목록에서 눈으로 찾아야 한다.
    #[test]
    fn typing_narrows_the_command_rows() {
        let mut p = Picker::commands(crate::lang::Lang::Ko);
        p.narrow("/mo", crate::lang::Lang::Ko);
        assert_eq!(p.rows.len(), 1, "{:?}", p.rows);
        assert_eq!(p.rows[0].label, "/mode");
        // 다시 넓히면 돌아와야 한다 — 한 글자 지우고 못 찾으면 못 쓴다.
        p.narrow("/m", crate::lang::Lang::Ko);
        assert!(p.rows.iter().any(|r| r.label == "/mcp"), "{:?}", p.rows);
    }

    #[test]
    fn the_project_list_starts_with_a_create_row() {
        let p = projects();
        assert_eq!(p.rows[0].label, "＋ 새 프로젝트");
        assert!(p.rows[0].enabled, "이제 만들 수 있다 — `projects:write`가 생겼다");
        assert_eq!(p.rows.len(), 3);
    }

    /// 못 누르는 줄에 커서를 두고 시작하면 Enter가 헛돈다. 첫 프로젝트에서 시작한다.
    #[test]
    fn the_cursor_starts_on_the_first_real_project() {
        assert_eq!(projects().cursor, 1);
    }

    #[test]
    fn picking_a_project_opens_its_sessions() {
        let p = projects();
        assert_eq!(
            p.pick(),
            Some(Pick::OpenProject { id: "p1".into(), name: "기본 프로젝트".into() })
        );
    }

    /// **누른다고 바로 만들지 않는다.** 이름과 설명을 받아야 한다 — 양식을 연다.
    #[test]
    fn picking_the_create_row_opens_the_form() {
        let mut p = projects();
        p.cursor = 0;
        assert_eq!(p.pick(), Some(Pick::NewProject));
    }

    #[test]
    fn the_session_list_starts_with_a_usable_create_row() {
        let s = Picker::sessions(
            "p1".into(),
            "기본".into(),
            vec![("s1".into(), "지난 대화".into(), false)],
            crate::lang::Lang::Ko,
        );
        assert_eq!(s.rows[0].label, "＋ 새 쓰레드");
        assert!(s.rows[0].enabled, "thread는 만들 수 있다");
        assert_eq!(s.cursor, 0);
        assert_eq!(s.pick(), Some(Pick::NewSession { project_id: "p1".into() }));
    }

    #[test]
    fn picking_a_session_opens_it() {
        let mut s = Picker::sessions(
            "p1".into(),
            "기본".into(),
            vec![("s1".into(), "지난 대화".into(), true)],
            crate::lang::Lang::Ko,
        );
        s.down();
        assert_eq!(
            s.pick(),
            Some(Pick::OpenSession { id: "s1".into(), project_id: "p1".into() })
        );
        assert_eq!(s.rows[1].note.as_deref(), Some("작업 중"));
    }

    #[test]
    fn moving_wraps_around() {
        let mut p = projects();
        p.cursor = 0;
        p.up();
        assert_eq!(p.cursor, 2);
        p.down();
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn an_empty_list_does_not_panic() {
        let mut p = Picker::loading_projects();
        p.up();
        p.down();
        assert!(p.pick().is_none());
    }

    #[test]
    fn the_title_says_where_we_are() {
        assert_eq!(projects().title(crate::lang::Lang::Ko), "프로젝트");
        let s = Picker::sessions("p1".into(), "zyris".into(), vec![], crate::lang::Lang::Ko);
        assert!(s.title(crate::lang::Lang::Ko).contains("zyris"));
    }
}
