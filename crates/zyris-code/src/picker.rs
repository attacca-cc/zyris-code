//! Picking projects and sessions.
//!
//! Pressing ← opens the project list; picking a project goes into that project's session list.
//! Each list has a "New" row at the top.
//!
//! **The row that creates a project needs a name and description.** But the list has no place to type.
//! So that row opens a form (`newproject::Form`) — name and description go in two fields,
//! and the list stays open underneath, so pressing Esc returns to the same spot.
//!
//! This module is pure. Fetching lists and creating sessions is the I/O spot's job.

/// One row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// What picking it becomes. `None` means it's the "New" row.
    pub id: Option<String>,
    pub label: String,
    /// A note shown dimmed on the right.
    pub note: Option<String>,
    /// Whether it can be picked.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    Projects,
    Sessions {
        project_id: String,
        project_name: String,
    },
    /// The list `/agent` opens. Unrelated to the project hierarchy, so there's nowhere to go back.
    Agents,
    /// The slash-command list shown when typing `/`.
    Commands,
    /// The screen-language list `/lang` opens.
    Languages,
}

/// What happens when picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Goes into this project's session list.
    OpenProject { id: String, name: String },
    /// Switches to this session. **Carries the project along** — which project the picked session
    /// belongs to is only known here, and losing it makes the next job·work opened
    /// fall into the default project.
    OpenSession { id: String, project_id: String },
    /// Creates a new session in this project.
    NewSession { project_id: String },
    /// Opens the new-project form (`newproject::Form`). Takes a name and description to create it.
    NewProject,
    /// Goes to this agent. A new session opens on the next message.
    UseAgent { name: String },
    /// Puts this command in the input field. **Doesn't run it immediately** — commands like
    /// `/mode` take arguments, so you must be able to keep typing after picking.
    TypeCommand { text: String },
    /// Switches the screen to this language.
    UseLang { lang: crate::lang::Lang },
    /// A row that can't be picked. Says why.
    Unavailable(String),
}

/// One slot in the list box. **The layout is decided purely; the widget only draws.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// The row at this index.
    Row(usize),
    /// The rule separating "New" from the actual list. The two are rows with different meanings.
    Rule,
    /// More exist on this side. **Without it, the list looks like it ends there.**
    More { count: usize, up: bool },
}

/// Decides how many rows fit and lays out what goes in them.
///
/// **The cursor is always visible.** And the cut side notes how many more there are — without it,
/// the list looks like it ends there and the user won't look below.
pub fn slots(rows: &[Row], cursor: usize, height: usize) -> Vec<Slot> {
    if rows.is_empty() || height == 0 {
        return Vec::new();
    }
    let has_create = rows.first().is_some_and(|r| r.id.is_none());

    // The overflow marks and rule take space, so how many rows remain depends on itself.
    // A couple of rounds reaches the fixed point.
    let (mut room, mut start, mut end) = (height, 0usize, rows.len());
    for _ in 0..3 {
        let visible = room.max(1).min(rows.len());
        start = cursor.saturating_sub(visible.saturating_sub(1));
        end = (start + visible).min(rows.len());
        let extra = (start > 0) as usize
            + (end < rows.len()) as usize
            // The rule is drawn only when "New" is actually visible and something is below it.
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
    // **When very narrow, drop the extras first.** Both the overflow marks and the rule come after keeping the cursor visible —
    // if it breaks out of the box, the screen collapses.
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
    /// Whether the list is still loading.
    pub loading: bool,
}

impl Picker {
    /// An empty projects screen waiting for the list.
    pub fn loading_projects() -> Self {
        Self { level: Level::Projects, rows: Vec::new(), cursor: 0, loading: true }
    }

    /// The projects list. The top row is new-project.
    pub fn projects(items: Vec<(String, String, bool)>, lang: crate::lang::Lang) -> Self {
        let mut rows = vec![Row {
            id: None,
            label: lang.new_project().into(),
            // Says right there what happens when picked.
            note: Some(lang.new_project_note().into()),
            enabled: true,
        }];
        rows.extend(items.into_iter().map(|(id, name, is_default)| Row {
            id: Some(id),
            label: name,
            note: is_default.then(|| lang.default_project().to_string()),
            enabled: true,
        }));
        // The first row is the unselectable 'new project', so start on the second (first real project).
        let cursor = if rows.len() > 1 { 1 } else { 0 };
        Self { level: Level::Projects, rows, cursor, loading: false }
    }

    /// One project's session list. The top row is new-session.
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

    /// The agents list. Opened by `/agent`.
    pub fn agents(rows: Vec<Row>) -> Self {
        Self { level: Level::Agents, rows, cursor: 0, loading: false }
    }

    /// The slash-command list. Opens when `/` is typed.
    ///
    /// **The list comes from the single `command::catalogue()`** — writing it here too lets one go stale.
    /// The language is decided by the screen (`State.lang`).
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

    /// The screen-language list. **Each name is written in its own language** — if it were in a language
    /// you can't read, you couldn't tell what you're picking. The cursor sits on the language in use.
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

    /// Narrows the list by typed text. If nothing remains, leaves it as is — an empty list looks broken.
    pub fn narrow(&mut self, typed: &str, lang: crate::lang::Lang) {
        if !matches!(self.level, Level::Commands) {
            return;
        }
        *self = Picker::commands(lang);
        self.rows.retain(|r| r.label.starts_with(typed));
        self.cursor = 0;
    }

    /// Whether this row is "New". The criterion separating it from the list.
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

    /// Picks the current row.
    pub fn pick(&self) -> Option<Pick> {
        let row = self.rows.get(self.cursor)?;
        if !row.enabled {
            return Some(Pick::Unavailable(
                row.note
                    .clone()
                    .unwrap_or_else(|| crate::lang::current().cannot_choose().to_string()),
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
            // **Doesn't create right away.** It needs a name and description — the form takes that spot.
            (Level::Projects, None) => Some(Pick::NewProject),
            (Level::Agents, Some(name)) => Some(Pick::UseAgent { name: name.clone() }),
            (Level::Commands, Some(text)) => Some(Pick::TypeCommand { text: text.clone() }),
            (Level::Languages, Some(code)) => {
                crate::lang::Lang::parse(code).map(|lang| Pick::UseLang { lang })
            }
            (Level::Agents, None) | (Level::Commands, None) | (Level::Languages, None) => None,
        }
    }

    /// The screen title.
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

    /// When everything fits there are no overflow marks. Saying "more" when nothing is cut is a lie.
    #[test]
    fn a_list_that_fits_gets_no_overflow_marks() {
        let rows = many(3);
        let got = slots(&rows, 0, 10);
        assert!(!got.iter().any(|s| matches!(s, Slot::More { .. })), "{got:?}");
        assert_eq!(got.iter().filter(|s| matches!(s, Slot::Row(_))).count(), 4);
    }

    /// **The cut side says how many more there are.** Without it, the list looks like it ends there.
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

    /// Scrolling down adds a count of what's left above.
    #[test]
    fn scrolling_down_marks_what_is_left_above() {
        let rows = many(30);
        let got = slots(&rows, 20, 8);
        assert!(
            matches!(got.first(), Some(Slot::More { up: true, .. })),
            "위로 남은 개수가 없다: {got:?}"
        );
    }

    /// **The cursor is always visible.** If it weren't, you couldn't tell what you're picking.
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

    /// The layout must never exceed the height it was given.
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

    /// **"New" is ruled off from the list.** Kept together, it reads as one more session.
    #[test]
    fn the_create_row_is_ruled_off_from_the_list() {
        let got = slots(&many(3), 0, 10);
        assert_eq!(got[0], Slot::Row(0));
        assert_eq!(got[1], Slot::Rule, "가름선이 없다: {got:?}");
        assert_eq!(got[2], Slot::Row(1));
    }

    /// Once scrolled past where the create row is visible, there's no rule either.
    #[test]
    fn there_is_no_rule_once_the_create_row_scrolls_away() {
        let got = slots(&many(30), 25, 8);
        assert!(!got.contains(&Slot::Rule), "{got:?}");
    }

    /// Lists without a create row (agents·commands) get no rule.
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

    /// Picking an agent must give back its name — that's how it's looked up when opening a new session.
    #[test]
    fn picking_an_agent_gives_back_its_name() {
        let mut p = agents();
        p.down();
        assert_eq!(p.pick(), Some(Pick::UseAgent { name: "Zyris Code".into() }));
    }

    /// **If the list and parser diverge, the pick doesn't work.** The list comes from `command::catalogue()`.
    #[test]
    fn every_command_row_is_something_the_parser_knows() {
        for row in Picker::commands(crate::lang::Lang::Ko).rows {
            assert!(crate::command::parse(&row.label).is_some(), "{}", row.label);
        }
    }

    /// **Picking only puts it in the input field; it doesn't run.** `/mode` takes arguments.
    #[test]
    fn picking_a_command_only_types_it() {
        let p = Picker::commands(crate::lang::Lang::Ko);
        assert_eq!(p.pick(), Some(Pick::TypeCommand { text: "/help".into() }));
    }

    /// Narrowed by typed text. Without it, you'd have to find it by eye in the list.
    #[test]
    fn typing_narrows_the_command_rows() {
        let mut p = Picker::commands(crate::lang::Lang::Ko);
        p.narrow("/mo", crate::lang::Lang::Ko);
        assert_eq!(p.rows.len(), 1, "{:?}", p.rows);
        assert_eq!(p.rows[0].label, "/mode");
        // Widening again must bring it back — if deleting one character lost it, it's unusable.
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

    /// Starting with the cursor on an unselectable row makes Enter spin. Start on the first project.
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

    /// **Picking doesn't create right away.** It needs a name and description — the form opens.
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
        assert_eq!(s.pick(), Some(Pick::OpenSession { id: "s1".into(), project_id: "p1".into() }));
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
