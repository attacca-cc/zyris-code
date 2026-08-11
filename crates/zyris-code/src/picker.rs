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

/// How a thread (session) is doing, shown as a coloured dot in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStatus {
    /// A turn is running right now. Drawn orange and blinking.
    Running,
    /// The last turn finished cleanly.
    Success,
    /// The last turn ended in an error.
    Failed,
}

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
    /// A per-thread status dot. Only session rows carry one; the rest stay `None`.
    pub status: Option<ThreadStatus>,
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

/// Where the window sits after the cursor moved to `cursor`.
///
/// **The window only moves when the cursor would leave it.** It used to be derived from the
/// cursor alone — `start = cursor - (visible - 1)` — which pins the cursor to the bottom edge,
/// so every ↑ scrolled the list by one even though there were rows above the cursor still on
/// screen. Walking back up then never reached the `↑ N more` mark: it kept retreating.
pub fn window_top(len: usize, cursor: usize, top: usize, visible: usize) -> usize {
    let last = len.saturating_sub(visible);
    if cursor < top {
        // Off the top — follow it up, one row at a time.
        return cursor;
    }
    if cursor >= top + visible {
        // Off the bottom — the cursor becomes the last visible row.
        return (cursor + 1 - visible).min(last);
    }
    // Still inside. **Stay put**, except that a shrinking list must not leave the window
    // hanging past the end.
    top.min(last)
}

/// Decides how many rows fit and lays out what goes in them, given where the window was.
///
/// Returns the layout and where the window ended up — the caller stores that back, and it is
/// what makes scrolling stick instead of snapping to the cursor every keypress.
///
/// **The cursor is always visible.** And the cut side notes how many more there are — without it,
/// the list looks like it ends there and the user won't look below.
pub fn slots(rows: &[Row], cursor: usize, top: usize, height: usize) -> (Vec<Slot>, usize) {
    if rows.is_empty() || height == 0 {
        return (Vec::new(), 0);
    }
    let has_create = rows.first().is_some_and(|r| r.id.is_none());

    // The overflow marks and rule take space, so how many rows remain depends on itself.
    // A couple of rounds reaches the fixed point.
    let (mut room, mut start, mut end) = (height, 0usize, rows.len());
    for _ in 0..3 {
        let visible = room.max(1).min(rows.len());
        start = window_top(rows.len(), cursor, top, visible);
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
    (out, start)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    pub level: Level,
    pub rows: Vec<Row>,
    pub cursor: usize,
    /// The first visible row. **Remembered rather than derived from the cursor** — deriving it
    /// pins the cursor to an edge and makes the list scroll on every keypress. `slots` returns
    /// where it ended up and the widget stores it back.
    pub top: usize,
    /// Whether the list is still loading.
    pub loading: bool,
}

impl Picker {
    /// An empty projects screen waiting for the list.
    ///
    /// **The box goes up before the answer does.** Fetching the list takes a request, and on a
    /// big account not a fast one — with nothing on screen until it returns, pressing ← looks
    /// like the app stopped responding.
    pub fn loading_projects() -> Self {
        Self { level: Level::Projects, rows: Vec::new(), cursor: 0, top: 0, loading: true }
    }

    /// An empty thread list waiting for the list. Carries the project it belongs to, so `Esc`
    /// still knows where back is while it loads.
    pub fn loading_sessions(project_id: String, project_name: String) -> Self {
        Self {
            level: Level::Sessions { project_id, project_name },
            rows: Vec::new(),
            cursor: 0,
            top: 0,
            loading: true,
        }
    }

    /// The projects list. The top row is new-project.
    pub fn projects(items: Vec<(String, String, bool)>, lang: crate::lang::Lang) -> Self {
        let mut rows = vec![Row {
            id: None,
            label: lang.new_project().into(),
            // Says right there what happens when picked.
            note: Some(lang.new_project_note().into()),
            enabled: true,
            status: None,
        }];
        rows.extend(items.into_iter().map(|(id, name, is_default)| Row {
            id: Some(id),
            label: name,
            note: is_default.then(|| lang.default_project().to_string()),
            enabled: true,
            status: None,
        }));
        // The first row is the unselectable 'new project', so start on the second (first real project).
        let cursor = if rows.len() > 1 { 1 } else { 0 };
        Self { level: Level::Projects, rows, cursor, top: 0, loading: false }
    }

    /// One project's session list. The top row is new-session.
    pub fn sessions(
        project_id: String,
        project_name: String,
        items: Vec<(String, String, Option<ThreadStatus>)>,
        lang: crate::lang::Lang,
    ) -> Self {
        let mut rows =
            vec![Row { id: None, label: lang.new_thread().into(), note: None, enabled: true, status: None }];
        rows.extend(items.into_iter().map(|(id, title, status)| Row {
            id: Some(id),
            label: title,
            note: None,
            enabled: true,
            status,
        }));
        Self {
            level: Level::Sessions { project_id, project_name },
            rows,
            cursor: 0,
            top: 0,
            loading: false,
        }
    }

    /// An empty agents screen waiting for the list.
    pub fn loading_agents() -> Self {
        Self { level: Level::Agents, rows: Vec::new(), cursor: 0, top: 0, loading: true }
    }

    /// The agents list. Opened by `/agent`.
    pub fn agents(rows: Vec<Row>) -> Self {
        Self { level: Level::Agents, rows, cursor: 0, top: 0, loading: false }
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
                status: None,
            })
            .collect();
        Self { level: Level::Commands, rows, cursor: 0, top: 0, loading: false }
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
            (Level::Agents, None) | (Level::Commands, None) => None,
        }
    }

    /// The screen title.
    pub fn title(&self, lang: crate::lang::Lang) -> String {
        match &self.level {
            Level::Projects => lang.projects().into(),
            Level::Sessions { project_name, .. } => lang.threads_in(project_name),
            Level::Agents => lang.agents().into(),
            Level::Commands => lang.commands().into(),
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
            status: None,
        }];
        rows.extend((0..n).map(|i| Row {
            id: Some(format!("s{i}")),
            label: format!("thread {i}"),
            note: None,
            enabled: true,
            status: None,
        }));
        rows
    }

    /// The layout from a window that has not been scrolled yet — what these assertions are about.
    fn lay(rows: &[Row], cursor: usize, height: usize) -> Vec<Slot> {
        slots(rows, cursor, 0, height).0
    }

    /// When everything fits there are no overflow marks. Saying "more" when nothing is cut is a lie.
    #[test]
    fn a_list_that_fits_gets_no_overflow_marks() {
        let rows = many(3);
        let got = lay(&rows, 0, 10);
        assert!(!got.iter().any(|s| matches!(s, Slot::More { .. })), "{got:?}");
        assert_eq!(got.iter().filter(|s| matches!(s, Slot::Row(_))).count(), 4);
    }

    /// **The cut side says how many more there are.** Without it, the list looks like it ends there.
    #[test]
    fn a_long_list_says_how_many_are_left_below() {
        let rows = many(30);
        let got = lay(&rows, 0, 8);
        let Some(Slot::More { count, up }) = got.last() else {
            panic!("the count left below is missing: {got:?}");
        };
        assert!(!up);
        let shown = got.iter().filter(|s| matches!(s, Slot::Row(_))).count();
        assert_eq!(shown + count, rows.len(), "the count is wrong: {got:?}");
    }

    /// Scrolling down adds a count of what's left above.
    #[test]
    fn scrolling_down_marks_what_is_left_above() {
        let rows = many(30);
        let got = lay(&rows, 20, 8);
        assert!(
            matches!(got.first(), Some(Slot::More { up: true, .. })),
            "no count of what is left above: {got:?}"
        );
    }

    /// **Going back up does not scroll until the cursor reaches the top of the window.**
    ///
    /// The window used to be derived from the cursor alone, which pinned the cursor to the
    /// bottom edge: one ↑ from the bottom moved `↑ 25 more` to `↑ 26 more` even though the
    /// cursor was nowhere near it. Walking up then never reached the mark — it retreated with
    /// every keypress, and the rows above it could not be read.
    #[test]
    fn walking_back_up_holds_the_window_until_the_cursor_reaches_it() {
        let rows = many(40);
        // Scrolled to the bottom, then four steps back up.
        let (_, top) = slots(&rows, 39, 0, 9);
        assert!(top > 0, "it did not scroll down at all");
        let mut at = top;
        for cursor in (36..=38).rev() {
            let (_, now) = slots(&rows, cursor, at, 9);
            assert_eq!(now, at, "the window moved while the cursor was still inside it");
            at = now;
        }
        // Once the cursor reaches the first visible row, one more ↑ moves the window by one.
        let (_, now) = slots(&rows, at - 1, at, 9);
        assert_eq!(now, at - 1, "at the top edge it must follow the cursor");
    }

    /// The same the other way — walking down holds until the cursor reaches the bottom row.
    #[test]
    fn walking_down_holds_the_window_until_the_cursor_reaches_the_bottom() {
        let rows = many(40);
        let (first, _) = slots(&rows, 0, 0, 9);
        let visible = first.iter().filter(|s| matches!(s, Slot::Row(_))).count();
        for cursor in 1..visible {
            let (_, now) = slots(&rows, cursor, 0, 9);
            assert_eq!(now, 0, "the list scrolled while row {cursor} was still on screen");
        }
    }

    /// A window left hanging past the end of a list that shrank is pulled back.
    #[test]
    fn a_window_past_the_end_of_a_shrunken_list_is_pulled_back() {
        let rows = many(6);
        let (got, top) = slots(&rows, 0, 30, 9);
        assert_eq!(top, 0, "{got:?}");
        assert!(got.iter().any(|s| matches!(s, Slot::Row(0))), "{got:?}");
    }

    /// **The cursor is always visible.** If it weren't, you couldn't tell what you're picking.
    #[test]
    fn the_cursor_is_always_inside_the_window() {
        let rows = many(40);
        for cursor in [0, 1, 7, 20, 40] {
            let got = lay(&rows, cursor, 9);
            assert!(
                got.iter().any(|s| matches!(s, Slot::Row(i) if *i == cursor)),
                "the cursor at {cursor} is not on screen: {got:?}"
            );
        }
    }

    /// The layout must never exceed the height it was given.
    #[test]
    fn the_layout_never_exceeds_the_height_it_was_given() {
        let rows = many(40);
        for height in 1..14 {
            for cursor in [0, 5, 39] {
                let got = lay(&rows, cursor, height);
                assert!(got.len() <= height, "{} rows in a height of {height}: {got:?}", got.len());
            }
        }
    }

    /// **"New" is ruled off from the list.** Kept together, it reads as one more session.
    #[test]
    fn the_create_row_is_ruled_off_from_the_list() {
        let got = lay(&many(3), 0, 10);
        assert_eq!(got[0], Slot::Row(0));
        assert_eq!(got[1], Slot::Rule, "no rule: {got:?}");
        assert_eq!(got[2], Slot::Row(1));
    }

    /// Once scrolled past where the create row is visible, there's no rule either.
    #[test]
    fn there_is_no_rule_once_the_create_row_scrolls_away() {
        let got = lay(&many(30), 25, 8);
        assert!(!got.contains(&Slot::Rule), "{got:?}");
    }

    /// Lists without a create row (agents·commands) get no rule.
    #[test]
    fn a_list_without_a_create_row_has_no_rule() {
        let got = lay(&Picker::commands(crate::lang::Lang::Ko).rows, 0, 20);
        assert!(!got.contains(&Slot::Rule), "{got:?}");
    }

    fn agents() -> Picker {
        Picker::agents(
            ["Main Agent", "Zyris Code"]
                .into_iter()
                .map(|n| {
                    Row { id: Some(n.into()), label: n.into(), note: None, enabled: true, status: None }
                })
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
        assert!(p.rows[0].enabled, "it can be created now — `projects:write` exists");
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
            vec![("s1".into(), "지난 대화".into(), None)],
            crate::lang::Lang::Ko,
        );
        assert_eq!(s.rows[0].label, "＋ 새 쓰레드");
        assert!(s.rows[0].enabled, "a thread can be created");
        assert_eq!(s.cursor, 0);
        assert_eq!(s.pick(), Some(Pick::NewSession { project_id: "p1".into() }));
    }

    #[test]
    fn picking_a_session_opens_it() {
        let mut s = Picker::sessions(
            "p1".into(),
            "기본".into(),
            vec![("s1".into(), "지난 대화".into(), Some(ThreadStatus::Running))],
            crate::lang::Lang::Ko,
        );
        s.down();
        assert_eq!(s.pick(), Some(Pick::OpenSession { id: "s1".into(), project_id: "p1".into() }));
        assert_eq!(s.rows[1].status, Some(ThreadStatus::Running));
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
