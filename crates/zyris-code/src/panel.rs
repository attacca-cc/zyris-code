//! Popup panels for `/mode`·`/mcp`·`/skills`·`/plugin`·`/account`·`/status`.
//!
//! These commands used to dump a wall of text into the conversation. A panel shows
//! the same facts in a centered box — a title, rows, a hint line — that closes on
//! Esc or Enter and scrolls with ↑↓ / j·k / PageUp·PageDown / the wheel.
//!
//! **This module is pure.** It builds the styled lines; the widget (`widgets::panel`)
//! only draws, and the keys only scroll or close. Nothing here touches the server or disk.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::DirAccess;
use crate::input::Input;
use crate::lang::Lang;
use crate::markdown::display_width;
use crate::mode::{Mode, Route};
use crate::theme;
use crate::tools::skill::SkillInfo;

/// An open popup panel.
#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    /// The title shown in the box's top border.
    pub title: String,
    /// The styled body lines. Wrapped to the box width — never cut, never dropped.
    pub lines: Vec<Line<'static>>,
    /// Rows scrolled off the top. The widget clamps it to what fits.
    pub scroll: usize,
    /// An action button the panel offers, drawn as its own row above the hint.
    /// Only the account panel carries one so far.
    pub button: Option<PanelButton>,
    /// Whether the button has focus. Tab moves it; Enter/Space then activates.
    pub button_focused: bool,
    /// The editable settings, when this panel is the `/config` form. The other panels
    /// only show, so they carry `None` and their keys stay scroll-and-close.
    pub form: Option<Form>,
    /// The list that is *acted on*, when this panel is `/mcp` or `/plugin`. A cursor over rows,
    /// keys that do something to the row under it, and a sentence saying what the last key did.
    ///
    /// **Held here rather than in `app.rs` for the same reason `Form` is** — `refresh` rebuilds
    /// the body from it, so moving the cursor is a rebuild of the one thing the widget draws
    /// instead of a second copy of the layout living in the key handler.
    pub manager: Option<Manager>,
    /// The mode `Enter` applies, when this panel is the `/mode` list.
    ///
    /// **It is also what says the arrows mean "choose", not "scroll".** `/mode` lists four
    /// modes and draws a cursor beside one of them, so `↑↓` moving a scroll that is already at
    /// the top is not what anybody pressing it means. Every other panel carries `None` and keeps
    /// scroll-and-close.
    pub mode_pick: Option<Mode>,
    /// Every sentence this panel can show under its body — **all of them, not only the one on
    /// screen.**
    ///
    /// The box is sized from these, so it is the same size whichever row the cursor is on. Sized
    /// from the sentence actually up, it grew and shrank under the keys — and its position with
    /// it, since a panel is centred.
    ///
    /// **The one being shown is the last line of `lines`.** That is the whole agreement between
    /// the builder and the widget: `body_and_foot` splits them apart, and the widget keeps the
    /// rows the tallest sentence needs whichever one is up.
    pub foot: Vec<Line<'static>>,
}

// ─────────────────────────────────────────────────────────────────────────────
// The settings form
// ─────────────────────────────────────────────────────────────────────────────

/// One setting the `/config` form can edit.
///
/// **The values are a list, not a free field.** Every setting here has a handful of
/// answers, so ←→ walking that list needs no text box, no parsing and no way to be wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// What happens when a tool touches a path outside the working directory.
    DirAccess,
    /// The language the screen is drawn in. It lives in its own file, not `config.json`.
    Language,
    /// The mode the app opens in.
    DefaultMode,
    /// Which palette the screen is drawn in.
    Theme,
    /// Whether a newer release installs itself.
    Update,
}

impl Setting {
    /// The row label.
    pub fn label(self, lang: Lang) -> &'static str {
        match self {
            Setting::DirAccess => lang.cfg_dir_access(),
            Setting::Language => lang.cfg_language(),
            Setting::DefaultMode => lang.cfg_default_mode(),
            Setting::Theme => lang.cfg_theme(),
            Setting::Update => lang.cfg_update(),
        }
    }

    /// How many values this setting cycles through.
    pub fn count(self) -> usize {
        match self {
            Setting::DirAccess | Setting::Language => 2,
            // The unset state plus every mode.
            Setting::DefaultMode => 1 + Mode::ALL.len(),
            Setting::Theme => THEMES.len(),
            Setting::Update => crate::update::Policy::ALL.len(),
        }
    }

    /// The name of value `i`.
    ///
    /// **A language is named in its own language.** Picking from a list written in a language
    /// you cannot read leaves you unable to tell what you would be choosing.
    pub fn value_label(self, i: usize, lang: Lang) -> &'static str {
        match self {
            Setting::DirAccess => {
                if i == 0 {
                    lang.cfg_dir_allow()
                } else {
                    lang.cfg_dir_deny()
                }
            }
            Setting::Language => if i == 0 { Lang::Ko } else { Lang::En }.name(),
            Setting::DefaultMode => match i.checked_sub(1) {
                None => lang.cfg_off(),
                Some(n) => Mode::ALL[n.min(Mode::ALL.len() - 1)].label(lang),
            },
            Setting::Theme => lang.cfg_theme_name(THEMES[i.min(THEMES.len() - 1)]),
            Setting::Update => lang.cfg_update_name(pick(i)),
        }
    }

    /// One line saying what value `i` actually does.
    pub fn describe(self, i: usize, lang: Lang) -> String {
        match self {
            Setting::DirAccess => lang
                .cfg_dir_desc(if i == 0 { DirAccess::Allow } else { DirAccess::Deny })
                .to_string(),
            Setting::Language => {
                lang.cfg_lang_desc(if i == 0 { Lang::Ko } else { Lang::En }).to_string()
            }
            Setting::DefaultMode => {
                lang.cfg_mode_desc(i.checked_sub(1).map(|n| Mode::ALL[n.min(Mode::ALL.len() - 1)]))
            }
            Setting::Theme => lang.cfg_theme_desc(THEMES[i.min(THEMES.len() - 1)]).to_string(),
            Setting::Update => lang.cfg_update_desc(pick(i)).to_string(),
        }
    }
}

/// The palette choices, in the order the row walks them.
/// The update policy at that index, clamped — the form walks indices and must never be handed one
/// past the end.
fn pick(i: usize) -> crate::update::Policy {
    crate::update::Policy::ALL[i.min(crate::update::Policy::ALL.len() - 1)]
}

const THEMES: [crate::config::ThemeChoice; 3] = [
    crate::config::ThemeChoice::Auto,
    crate::config::ThemeChoice::Dark,
    crate::config::ThemeChoice::Light,
];

/// The `/config` form's editable state.
///
/// **It edits a draft, not the live settings.** Enter takes the draft; Esc throws it away.
/// Holding the draft here — rather than mutating `State.config` as the arrows are pressed —
/// is the whole reason Esc can mean "changed my mind".
///
/// The draft is also what the form *draws* from, so moving the language row re-letters the
/// box on the spot. That preview costs nothing to undo, because nothing was saved yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Form {
    /// The settings as they will be saved if Enter is pressed.
    pub draft: crate::config::Config,
    /// The screen language as it will be saved. It lives in its own file, so it rides
    /// beside `draft` rather than inside it.
    pub lang: Lang,
    /// Which row the cursor sits on.
    pub cursor: usize,
}

impl Form {
    /// The rows, in the order they are drawn.
    pub const ROWS: [Setting; 5] = [
        Setting::DirAccess,
        Setting::Language,
        Setting::DefaultMode,
        Setting::Theme,
        Setting::Update,
    ];

    fn new(lang: Lang, draft: crate::config::Config) -> Form {
        Form { draft, lang, cursor: 0 }
    }

    /// The setting the cursor is on.
    pub fn row(&self) -> Setting {
        Form::ROWS[self.cursor.min(Form::ROWS.len() - 1)]
    }

    /// Moves the cursor. Positive is up. **Wraps** — with three rows, a dead end at either
    /// edge just costs keypresses.
    pub fn move_cursor(&mut self, by: i32) {
        self.cursor = step(self.cursor, -by, Form::ROWS.len());
    }

    /// Cycles the value under the cursor. Positive is right. **Wraps**, so on the two-value
    /// rows either arrow toggles and there is nothing to aim at.
    pub fn shift(&mut self, by: i32) {
        let row = self.row();
        let next = step(self.chosen(row), by, row.count());
        self.set(row, next);
    }

    /// Which value index `setting` currently holds in the draft.
    pub fn chosen(&self, setting: Setting) -> usize {
        match setting {
            Setting::DirAccess => usize::from(self.draft.dir_access == DirAccess::Deny),
            Setting::Language => usize::from(self.lang == Lang::En),
            Setting::DefaultMode => match self.draft.default_mode {
                None => 0,
                Some(m) => 1 + Mode::ALL.iter().position(|it| *it == m).unwrap_or(0),
            },
            Setting::Theme => THEMES.iter().position(|t| *t == self.draft.theme).unwrap_or(0),
            Setting::Update => {
                crate::update::Policy::ALL.iter().position(|p| *p == self.draft.update).unwrap_or(0)
            }
        }
    }

    fn set(&mut self, setting: Setting, i: usize) {
        match setting {
            Setting::DirAccess => {
                self.draft.dir_access = if i == 0 { DirAccess::Allow } else { DirAccess::Deny }
            }
            Setting::Language => self.lang = if i == 0 { Lang::Ko } else { Lang::En },
            Setting::DefaultMode => {
                self.draft.default_mode =
                    i.checked_sub(1).map(|n| Mode::ALL[n.min(Mode::ALL.len() - 1)])
            }
            Setting::Theme => self.draft.theme = THEMES[i.min(THEMES.len() - 1)],
            Setting::Update => self.draft.update = pick(i),
        }
    }
}

/// Walks `at` by `by` around a ring of `len`. Negative steps go backwards.
fn step(at: usize, by: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as i64;
    (((at as i64 + by as i64) % len + len) % len) as usize
}

/// A button a panel can offer. The widget draws it; `app.rs` turns activation
/// into the same path as the matching slash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelButton {
    /// Log out on this device — the same as `/account logout`.
    Logout,
}

impl Panel {
    /// A panel holding pre-styled lines. `pub(crate)` because the widget's own tests build one
    /// directly — every panel a person sees comes from a builder below.
    pub(crate) fn new(title: String, lines: Vec<Line<'static>>) -> Self {
        Self {
            title,
            lines,
            scroll: 0,
            button: None,
            button_focused: false,
            form: None,
            manager: None,
            mode_pick: None,
            foot: Vec::new(),
        }
    }

    /// Redraws the body from the form after a key moved the cursor or changed a value.
    ///
    /// **The lines stay the one thing the widget draws.** Letting the widget read the form
    /// instead would put layout in two places, and the two would drift.
    pub fn refresh(&mut self) {
        if let Some(form) = self.form {
            self.title = form.lang.title_config().to_string();
            self.lines = form_lines(&form);
            self.foot = config_foot(&form);
        }
        if let Some(manager) = &self.manager {
            self.lines = manager_lines(manager);
        }
    }

    /// The body, and the foot line that follows it — `None` when this panel has no foot.
    ///
    /// See `Panel::foot` for why the two are held apart.
    pub fn body_and_foot(&self) -> (&[Line<'static>], Option<&Line<'static>>) {
        match (self.foot.is_empty(), self.lines.split_last()) {
            (true, _) | (false, None) => (self.lines.as_slice(), None),
            (false, Some((foot, body))) => (body, Some(foot)),
        }
    }

    pub fn scroll_up(&mut self, by: usize) {
        self.scroll = self.scroll.saturating_sub(by);
    }

    pub fn scroll_down(&mut self, by: usize) {
        self.scroll = self.scroll.saturating_add(by);
    }
}

/// How far the box may scroll.
///
/// **`drawn` counts the lines that were drawn, not the lines the panel holds**: one of them may
/// have wrapped into several, and a scroll is measured in what is on screen.
pub fn max_scroll(drawn: usize, visible: usize) -> usize {
    drawn.saturating_sub(visible)
}

// ─────────────────────────────────────────────────────────────────────────────
// Builders
// ─────────────────────────────────────────────────────────────────────────────

/// The `/mode` panel — every mode with its own sentence, the cursor on the mode you are in.
///
/// **Rows are one line each and the sentence goes below the list.** Beside each row the sentences
/// made the list ragged — the rows are one word and the sentences are twenty — and cut to fit they
/// lost their end. One line under the list says the whole thing and leaves the list clean. It is
/// the shape `/config` already uses for the setting under the cursor.
///
/// **`pick` is where the cursor is**, so the panel can be rebuilt as the arrows move. `None` opens
/// it on the mode you are in.
pub fn mode(lang: Lang, now: Mode, pick: Option<Mode>) -> Panel {
    // Opening the panel puts the cursor on the mode you are in, so Enter on an untouched panel
    // changes nothing.
    let on = pick.unwrap_or(now);
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{} ∙ {}", lang.current_mode(), now.label(lang)),
            Style::default().fg(now.color()).add_modifier(Modifier::BOLD),
        )),
        blank(),
    ];
    for m in Mode::ALL {
        let cursor = m == on;
        lines.push(Line::from(vec![
            Span::styled(if cursor { "❯ " } else { "  " }, Style::default().fg(theme::accent())),
            Span::styled(
                m.label(lang),
                if cursor {
                    Style::default().fg(m.color()).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::text())
                },
            ),
        ]));
    }
    lines.push(blank());
    lines.push(Line::from(bold_spans(lang.mode_desc(on))));
    let mut panel = Panel::new(lang.title_mode().into(), lines);
    panel.mode_pick = Some(on);
    // **All four sentences, so the box is one size on all four rows.** Built from the one on
    // screen, it grew and shrank as the arrows moved.
    panel.foot = Mode::ALL.iter().map(|m| Line::from(bold_spans(lang.mode_desc(*m)))).collect();
    panel
}

/// Turns `**bold**` into bold spans.
///
/// **The mode sentences carry markdown and this panel draws raw text** — `**일**` was going to the
/// screen with its asterisks on. There is no renderer wanted here: one marker, one line.
fn bold_spans(text: &'static str) -> Vec<Span<'static>> {
    let plain = || Style::default().fg(theme::text());
    let mut spans = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("**") {
        let (before, after) = rest.split_at(start);
        if !before.is_empty() {
            spans.push(Span::styled(before.to_string(), plain()));
        }
        // An unclosed marker is not a marker — the rest is plain text.
        let Some(end) = after[2..].find("**") else {
            spans.push(Span::styled(after.to_string(), plain()));
            return spans;
        };
        spans.push(Span::styled(
            after[2..2 + end].to_string(),
            plain().add_modifier(Modifier::BOLD),
        ));
        rest = &after[2 + end + 2..];
    }
    if !rest.is_empty() {
        spans.push(Span::styled(rest.to_string(), plain()));
    }
    spans
}

// ─────────────────────────────────────────────────────────────────────────────
// The managers — `/mcp` and `/plugin`
// ─────────────────────────────────────────────────────────────────────────────

/// Which list a manager is showing. The two differ in what a row is and in what a key does to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerKind {
    Mcp,
    Plugins,
}

impl ManagerKind {
    /// The heading in the box's top border.
    pub fn title(self, lang: Lang) -> &'static str {
        match self {
            ManagerKind::Mcp => lang.title_mcp(),
            ManagerKind::Plugins => lang.title_plugins(),
        }
    }
}

/// Where a server or a plugin came from — **which is also what may be done to it.**
///
/// Shown because it is the whole basis for trusting the thing, and read by the keys because the
/// same act means different things in different places: switching a repository server on is an
/// approval to run somebody else's program, while switching one written in this app's own config
/// on is nothing (it is already on — it starts itself). Removing a fetched plugin deletes a
/// directory; removing a discovered server only forgets a yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// `~/.config/zyris-code/mcp.json` — written by this person, for this app.
    User,
    /// `./.mcp.json` — written in the repository.
    Project,
    /// Another program's config file. Read, never written.
    Elsewhere,
    /// A plugin ships it.
    Plugin(String),
    /// Fetched into this app's own plugin directory.
    Fetched,
    /// Placed in the repository's `.zyris-code/plugins/` by hand.
    InProject,
    /// Put somewhere on this machine by hand.
    HandPlaced,
}

impl Origin {
    /// Whether the entry lives in a file this app may write.
    pub fn ours(&self) -> bool {
        matches!(self, Origin::User | Origin::Project)
    }
}

/// What the key that takes something away means here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// Take the entry out of one of our own config files.
    FromFile,
    /// Forget that this machine said yes to something another program set up.
    Approval,
    /// Delete a fetched plugin's directory, `git` clone and all.
    Directory,
}

/// One row, and everything the block under the list says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerRow {
    /// What the keys act on — a server's slug, or a plugin's name.
    pub id: String,
    /// The row itself.
    pub title: String,
    /// The muted sentence at its right.
    pub subtitle: String,
    /// `label value` pairs, drawn under the list only while the cursor is on this row.
    pub detail: Vec<(String, String)>,
    pub origin: Origin,
    /// Whether it can be switched, and which way it is now. `None` — there is nothing to switch.
    pub toggle: Option<bool>,
    /// Whether it can be removed, and what removing it means.
    pub remove: Option<Removal>,
}

/// The list a manager draws and acts on.
#[derive(Debug, Clone, PartialEq)]
pub struct Manager {
    pub kind: ManagerKind,
    pub rows: Vec<ManagerRow>,
    /// Where the cursor is. Kept inside `rows` by [`Manager::move_cursor`].
    pub cursor: usize,
    /// The add form, when one is open. **It takes the panel's keys** — characters go into it, not
    /// into the list — and closing it leaves the list exactly as it was.
    pub form: Option<ManagerForm>,
    /// What to say when there is nothing to list. **Held here rather than drawn from `lang`**, so
    /// the widget stays a pure function of the panel: the builder puts the sentence in, in the
    /// screen's language.
    ///
    /// It is also why an empty list is still a manager: `a` is how the first server gets added,
    /// and a list that cannot be added to until it is non-empty is a trap.
    pub empty: Option<String>,
    /// What the last key said, when it has something to say — the question asked before something
    /// is taken away, or why the key does nothing on this row.
    ///
    /// **What an act *did* is not said here.** That goes to the conversation, the way every other
    /// command's answer does, and the row itself shows the new state — a sentence inside the box
    /// would have to be a sentence the box was already sized for (`note_room`).
    pub note: Option<String>,
    /// A destructive act waiting to be confirmed, **naming the row it is about.** By the time the
    /// second press arrives the cursor may have moved, and deleting whatever the cursor happens to
    /// be on is the accident this exists to prevent.
    pub confirm: Option<String>,
    /// The widest sentence this panel can put in `note`. **Part of the sizing**, so asking a
    /// question does not resize the box under the eye.
    pub note_room: usize,
    /// The screen's language, for the two parts of a manager that are drawn from it: the sentence
    /// under an open form, and the line an empty list says.
    pub lang: Lang,
}

impl Manager {
    pub fn new(kind: ManagerKind, rows: Vec<ManagerRow>, note_room: usize, lang: Lang) -> Manager {
        Manager {
            kind,
            rows,
            cursor: 0,
            form: None,
            empty: None,
            note: None,
            confirm: None,
            note_room,
            lang,
        }
    }

    /// The row the cursor is on, if there is one.
    pub fn row(&self) -> Option<&ManagerRow> {
        self.rows.get(self.cursor)
    }

    /// Moves the cursor. Positive is down. **It stops at the ends** rather than wrapping — a list
    /// has a top and a bottom, unlike the four modes `/mode` walks round.
    pub fn move_cursor(&mut self, by: i32) {
        let last = self.rows.len().saturating_sub(1) as i32;
        self.cursor = (self.cursor as i32 + by).clamp(0, last) as usize;
        // **A moved cursor takes the question down.** Confirming a name the cursor has left is
        // exactly the accident `confirm` exists to prevent.
        self.confirm = None;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The add form
// ─────────────────────────────────────────────────────────────────────────────

/// One field of a manager's form.
///
/// **A field knows its own name, not just its label.** The label is in the screen's language; the
/// key is what the caller looks a field up by when it turns the form into an act.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub key: &'static str,
    pub label: String,
    pub kind: FieldKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    /// Free text, with a caret the person moves.
    Text { value: Input },
    /// One of a few answers, walked with ←→ so no typo can be made.
    Choice { options: Vec<String>, chosen: usize },
}

impl Field {
    pub fn text(key: &'static str, label: &str, start: &str) -> Field {
        let mut value = Input::new();
        value.insert_str(start);
        Field { key, label: label.to_string(), kind: FieldKind::Text { value } }
    }

    /// A row that walks a short list. **The first option is where it starts**, so an untouched form
    /// is already the answer most people want.
    pub fn choice(key: &'static str, label: &str, options: &[&str]) -> Field {
        Field {
            key,
            label: label.to_string(),
            kind: FieldKind::Choice {
                options: options.iter().map(|o| o.to_string()).collect(),
                chosen: 0,
            },
        }
    }

    /// What this field holds, as it will be used. Text is trimmed — a trailing space in a name is a
    /// different name to every other program.
    pub fn value(&self) -> String {
        match &self.kind {
            FieldKind::Text { value } => value.text.trim().to_string(),
            FieldKind::Choice { options, chosen } => {
                options.get(*chosen).cloned().unwrap_or_default()
            }
        }
    }

    pub fn editor(&mut self) -> Option<&mut Input> {
        match &mut self.kind {
            FieldKind::Text { value } => Some(value),
            FieldKind::Choice { .. } => None,
        }
    }

    /// Walks a choice. Positive is right, and it wraps — with two answers there is nothing to aim
    /// at. A text field ignores this; its ←→ move the caret.
    pub fn shift(&mut self, by: i32) {
        if let FieldKind::Choice { options, chosen } = &mut self.kind {
            if !options.is_empty() {
                *chosen = step(*chosen, by, options.len());
            }
        }
    }
}

/// A manager's add form: the fields, where the cursor is, and what Enter last said about it.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagerForm {
    pub kind: ManagerKind,
    pub fields: Vec<Field>,
    /// Which field the caret is in.
    pub cursor: usize,
    /// What Enter refused, if it did — one sentence, drawn under the fields.
    pub complaint: Option<String>,
}

impl ManagerForm {
    /// The `/mcp` form: what to call it, how to reach it, and which file to write it into.
    ///
    /// **The order is the order people answer in** — a name, then the kind of thing it is, then
    /// what it runs — and `where` is last because it is the one with a default worth having.
    pub fn mcp(lang: Lang) -> ManagerForm {
        ManagerForm {
            kind: ManagerKind::Mcp,
            fields: vec![
                Field::text("name", lang.f_name(), ""),
                Field::choice("kind", lang.f_kind(), &["stdio", "http"]),
                Field::text("command", lang.f_command(), ""),
                Field::text("args", lang.f_args(), ""),
                Field::text("env", lang.f_env(), ""),
                Field::choice("where", lang.f_where(), &[lang.f_machine(), lang.f_project()]),
            ],
            cursor: 0,
            complaint: None,
        }
    }

    /// The `/plugin` form: a place to fetch from, and where to put it.
    pub fn plugin(lang: Lang) -> ManagerForm {
        ManagerForm {
            kind: ManagerKind::Plugins,
            fields: vec![
                Field::text("source", lang.f_source(), ""),
                Field::choice("where", lang.f_where(), &[lang.f_machine(), lang.f_project()]),
            ],
            cursor: 0,
            complaint: None,
        }
    }

    pub fn field(&self) -> Option<&Field> {
        self.fields.get(self.cursor)
    }

    /// The value of the field with this key, or `None` when the form has no such field — which is
    /// how the caller tells a `stdio` form from an `http` one.
    pub fn get(&self, key: &str) -> Option<String> {
        self.fields.iter().find(|f| f.key == key).map(Field::value)
    }

    /// Which option the field with this key is on, as an index.
    pub fn chosen(&self, key: &str) -> Option<usize> {
        self.fields.iter().find(|f| f.key == key).and_then(|f| match &f.kind {
            FieldKind::Choice { chosen, .. } => Some(*chosen),
            FieldKind::Text { .. } => None,
        })
    }

    /// Moves between fields. Positive is down, and it stops at the ends — a form has a top and a
    /// bottom, and Enter is what takes it.
    pub fn move_cursor(&mut self, by: i32) {
        let last = self.fields.len().saturating_sub(1) as i32;
        self.cursor = (self.cursor as i32 + by).clamp(0, last) as usize;
        self.complaint = None;
    }

    /// Walks the value in the field under the caret. Positive is right — a choice goes round its
    /// options, a text field moves its caret.
    pub fn shift(&mut self, by: i32) {
        if let Some(field) = self.fields.get_mut(self.cursor) {
            field.shift(by);
        }
        self.complaint = None;
    }

    /// Puts the caret on the field with this key — what a refusal does, so the missing answer is
    /// the one being typed into.
    pub fn focus(&mut self, key: &str) {
        if let Some(at) = self.fields.iter().position(|f| f.key == key) {
            self.cursor = at;
        }
    }

    /// The text field the caret is in, if it is in one.
    pub fn editor(&mut self) -> Option<&mut Input> {
        self.fields.get_mut(self.cursor).and_then(Field::editor)
    }

    /// The sentence under the fields: what Enter refused, or what the field under the caret is for.
    pub fn sentence(&self, lang: Lang) -> String {
        match &self.complaint {
            Some(why) => why.clone(),
            None => match self.field() {
                Some(field) => lang.f_hint(field.key),
                None => String::new(),
            },
        }
    }
}

/// Draws the form: one row per field, the caret's row marked, and the sentence under them.
///
/// **Not `form_lines`** — that name belongs to the `/config` form, which is a different shape for a
/// different job. Colliding on it compiled into a call to the wrong one.
fn manager_form_lines(form: &ManagerForm, lang: Lang) -> Vec<Line<'static>> {
    let label_w = form.fields.iter().map(|f| display_width(&f.label)).max().unwrap_or(0);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (at, field) in form.fields.iter().enumerate() {
        let on = at == form.cursor;
        let mut spans = vec![
            Span::styled(if on { "❯ " } else { "  " }, Style::default().fg(theme::accent())),
            Span::styled(
                format!(
                    "{}{:pad$}",
                    field.label,
                    "",
                    pad = label_w - display_width(&field.label) + 2
                ),
                if on {
                    Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::text_muted())
                },
            ),
        ];
        let value = if on {
            Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::text())
        };
        match &field.kind {
            FieldKind::Text { value: input } => {
                let chars: Vec<char> = input.text.chars().collect();
                let caret = input.cursor.min(chars.len());
                let before: String = chars[..caret].iter().collect();
                let after: String = chars[caret..].iter().collect();
                spans.push(Span::styled("[ ", Style::default().fg(theme::border_light())));
                spans.push(Span::styled(before, value));
                // **The caret is drawn, not parked in the terminal.** `|` is narrow and every font
                // has it, where the block glyphs a caret would want are East Asian Ambiguous
                // (`tests/width.rs`).
                if on {
                    spans.push(Span::styled("|", Style::default().fg(theme::accent())));
                }
                spans.push(Span::styled(after, value));
                spans.push(Span::styled(" ]", Style::default().fg(theme::border_light())));
            }
            FieldKind::Choice { options, chosen } => {
                spans.push(Span::styled("< ", Style::default().fg(theme::border_light())));
                spans.push(Span::styled(options.get(*chosen).cloned().unwrap_or_default(), value));
                spans.push(Span::styled(" >", Style::default().fg(theme::border_light())));
            }
        }
        lines.push(Line::from(spans));
    }
    lines.push(blank());
    lines.push(Line::from(Span::styled(
        form.sentence(lang),
        if form.complaint.is_some() {
            Style::default().fg(theme::warning())
        } else {
            Style::default().fg(theme::text_muted())
        },
    )));
    lines
}

/// What a row draws before its name: the cursor, and whether the thing is on.
const ROW_MARK: usize = 4;
/// The gap between a detail's label and its value.
const DETAIL_GAP: usize = 2;
/// How far a detail block is indented, under the row it belongs to.
const DETAIL_INDENT: &str = "    ";

/// Draws a manager: the rows with the cursor marked, the block under it, and the sentence.
///
/// **The box is one size on every row.** A detail block is a different shape for each row, so the
/// drawn block is padded to the tallest of them and every line to the widest of them. A box that
/// grew and shrank as the cursor moved is what `/mode` and `/config` were both fixed for, and the
/// same rule holds here.
fn manager_lines(manager: &Manager) -> Vec<Line<'static>> {
    let lang = manager.lang;
    // **An open form replaces the list.** Its keys are the form's — characters go into a field —
    // and drawing the rows underneath would say the arrows still move a cursor that is now inside
    // the form.
    if let Some(form) = &manager.form {
        let lines = manager_form_lines(form, lang);
        let width = lines
            .iter()
            .map(|line| line.spans.iter().map(|s| display_width(&s.content)).sum::<usize>())
            .max()
            .unwrap_or(0)
            .max(manager.note_room);
        return lines.into_iter().map(|line| pad_to(line, width)).collect();
    }
    if manager.rows.is_empty() {
        return vec![muted(manager.empty.clone().unwrap_or_default())];
    }
    let label_w = manager
        .rows
        .iter()
        .flat_map(|row| row.detail.iter().map(|(label, _)| display_width(label)))
        .max()
        .unwrap_or(0);
    let width = manager
        .rows
        .iter()
        .flat_map(|row| {
            let head = ROW_MARK + display_width(&row.title) + 3 + display_width(&row.subtitle);
            let details: Vec<usize> = row
                .detail
                .iter()
                .map(|(_label, value)| {
                    display_width(DETAIL_INDENT) + label_w + DETAIL_GAP + display_width(value)
                })
                .collect();
            std::iter::once(head).chain(details)
        })
        .max()
        .unwrap_or(0)
        .max(manager.note_room);
    let height = manager.rows.iter().map(|row| row.detail.len()).max().unwrap_or(0);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, row) in manager.rows.iter().enumerate() {
        let on_cursor = i == manager.cursor;
        let on = row.toggle == Some(true);
        lines.push(Line::from(vec![
            Span::styled(if on_cursor { "❯ " } else { "  " }, Style::default().fg(theme::accent())),
            // **The dot is the state.** Filled and green is running or enabled; hollow is not.
            Span::styled(
                if on { "● " } else { "○ " },
                Style::default().fg(if on { theme::success() } else { theme::text_muted() }),
            ),
            Span::styled(
                row.title.clone(),
                Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ‒ ", Style::default().fg(theme::border_light())),
            Span::styled(row.subtitle.clone(), Style::default().fg(theme::text_muted())),
        ]));
    }
    lines.push(blank());
    if let Some(row) = manager.row() {
        for (label, value) in &row.detail {
            lines.push(Line::from(vec![
                Span::styled(DETAIL_INDENT.to_string(), Style::default()),
                Span::styled(
                    format!(
                        "{label}{:gap$}",
                        "",
                        gap = label_w - display_width(label) + DETAIL_GAP
                    ),
                    Style::default().fg(theme::text_muted()),
                ),
                Span::styled(value.clone(), Style::default().fg(theme::text())),
            ]));
        }
        // Blank rows rather than a shorter box — the tallest block decides for all of them.
        for _ in row.detail.len()..height {
            lines.push(blank());
        }
    }
    lines.push(blank());
    lines.push(match &manager.note {
        Some(note) => Line::from(Span::styled(note.clone(), Style::default().fg(theme::warning()))),
        None => blank(),
    });
    lines.into_iter().map(|line| pad_to(line, width)).collect()
}

/// Pads a line with spaces to `width` columns. **Trailing spaces are invisible**; what they buy is
/// a box that keeps one width while the cursor moves between rows of different lengths.
fn pad_to(line: Line<'static>, width: usize) -> Line<'static> {
    let have: usize = line.spans.iter().map(|s| display_width(&s.content)).sum();
    if have >= width {
        return line;
    }
    let mut spans = line.spans;
    spans.push(Span::raw(" ".repeat(width - have)));
    Line::from(spans)
}

/// The `/mcp` panel: every server this machine knows about, and what can be done with each.
///
/// **What is running and what could be are one list.** Two sections meant the same server appeared
/// twice — once as something that failed to start and once as the candidate it came from — and the
/// cursor could only ever act on one of them. One row per server carries both: its state, where it
/// came from, and what the keys mean there.
pub fn mcp_manager(lang: Lang, rows: Vec<ManagerRow>) -> Panel {
    // **Empty is still a manager.** `a` is how the first server gets added, and a list that cannot
    // be added to until it has something in it is a trap.
    let room = room_for(lang, &rows);
    let mut panel = Panel::new(lang.title_mcp().into(), Vec::new());
    let mut manager = Manager::new(ManagerKind::Mcp, rows, room, lang);
    manager.empty = Some(lang.mcp_empty().to_string());
    panel.manager = Some(manager);
    panel.refresh();
    panel
}

/// The `/plugin` panel: every plugin, what it contributes, and what can be done with it.
pub fn plugin_manager(lang: Lang, rows: Vec<ManagerRow>) -> Panel {
    let room = room_for(lang, &rows);
    let mut panel = Panel::new(lang.title_plugins().into(), Vec::new());
    let mut manager = Manager::new(ManagerKind::Plugins, rows, room, lang);
    manager.empty = Some(lang.plugins_empty().to_string());
    panel.manager = Some(manager);
    panel.refresh();
    panel
}

/// The widest sentence this panel can put under its list — **every question it can ask, over every
/// row it can ask about.** Sized from the one being shown, the box would grow the moment a key was
/// pressed, which is the resizing all of this exists to prevent.
fn room_for(lang: Lang, rows: &[ManagerRow]) -> usize {
    rows.iter()
        .flat_map(|row| {
            [
                display_width(&lang.manager_confirm(&row.id)),
                display_width(&lang.manager_cannot(&row.id)),
            ]
        })
        .max()
        .unwrap_or(0)
}

/// The `/skills` panel — one entry per skill: its name, and the sentence under it saying when to
/// use it.
///
/// **The name on the row, the sentence under it.** Beside the name, a skill's description made the
/// list ragged — one row one word wide and the next a paragraph — and worse, it put descriptions in
/// two places at once: a short one sat on its row while a long one wrapped below it, so the same
/// list changed shape from row to row (reported 2026-09-13). Below is where a description lives
/// everywhere else in this app: the picker's note area, `/mode`'s sentence.
pub fn skills(lang: Lang, skills: &[SkillInfo]) -> Panel {
    if skills.is_empty() {
        return Panel::new(
            lang.title_skills().into(),
            vec![muted(lang.skills_empty().to_string())],
        );
    }
    let mut lines = Vec::new();
    for s in skills {
        lines.push(Line::from(vec![
            Span::styled("∙ ", Style::default().fg(theme::accent())),
            Span::styled(
                s.name.clone(),
                Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
            ),
        ]));
        // **Two columns, so the sentence starts under the name it belongs to** — the same two
        // columns `∙ ` takes, which is also what a wrapped line hangs under (`wrap::line`).
        if !s.description.trim().is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {}", s.description),
                Style::default().fg(theme::text_muted()),
            )));
        }
    }
    Panel::new(lang.title_skills().into(), lines)
}

/// The `/account` panel — who this node is attached as. Carries a logout button
/// so the action is one Tab + Enter away instead of remembering the command.
pub fn account(
    lang: Lang,
    name: &str,
    email: &str,
    user_id: &str,
    plan: Option<&str>,
    credits: Option<&str>,
    scopes: &[String],
) -> Panel {
    let scopes_text =
        if scopes.is_empty() { lang.acc_none().to_string() } else { scopes.join(", ") };
    let lines = vec![
        Line::from(vec![
            Span::styled(
                name.to_string(),
                Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  ({email})"), Style::default().fg(theme::text_muted())),
        ]),
        blank(),
        kv(lang.acc_id(), user_id.to_string()),
        kv(lang.acc_plan(), plan.unwrap_or_else(|| lang.panel_dash()).to_string()),
        kv(lang.credits(), credits.unwrap_or_else(|| lang.panel_dash()).to_string()),
        kv(lang.acc_scopes(), scopes_text),
        blank(),
        muted(lang.acc_logout_note().to_string()),
    ];
    let mut panel = Panel::new(lang.title_account().into(), lines);
    panel.button = Some(PanelButton::Logout);
    panel
}

/// The `/status` panel — the current session's picture, the same facts `status_text`
/// used to dump as one paragraph.
pub fn status(lang: Lang, info: &crate::lang::StatusInfo) -> Panel {
    let thread = match info.session_id {
        Some(id) => id.to_string(),
        None => lang.st_thread_none().to_string(),
    };
    let project = match info.project {
        Some(p) => p.to_string(),
        None => lang.st_project_default().to_string(),
    };
    let mut lines = vec![
        kv(lang.st_thread(), thread),
        kv(lang.st_project(), project),
        kv(
            lang.st_agent(),
            if info.agent.is_empty() { "-".into() } else { info.agent.to_string() },
        ),
        kv(lang.st_mode(), info.mode.to_string()),
    ];
    let u = info.usage;
    if let Some(model) = &u.model {
        lines.push(kv(lang.st_model(), model.clone()));
    }
    if let Some(credits) = &u.credits_used {
        lines.push(kv(lang.credits(), credits.clone()));
    }
    if let Some(used) = u.context_tokens {
        let text = match crate::usage::context_limit(u.model.as_deref()) {
            Some(max) => {
                let pct = if max > 0 { used.saturating_mul(100) / max } else { 0 };
                format!("{}% ({}/{})", pct, crate::usage::compact(used), crate::usage::compact(max))
            }
            None => crate::usage::compact(used),
        };
        lines.push(kv(lang.context(), text));
    }
    if let Some(tokens) = u.total_tokens {
        lines.push(kv(lang.total_tokens(), crate::usage::compact(tokens)));
    }
    lines.push(kv(lang.st_cwd(), info.cwd.display().to_string()));
    match info.pending {
        Some(Route::Work) => {
            lines.push(blank());
            lines.push(Line::from(Span::styled(
                lang.st_pending_work(),
                Style::default().fg(theme::warning()),
            )));
        }
        Some(Route::Job) => {
            lines.push(blank());
            lines.push(Line::from(Span::styled(
                lang.st_pending_job(),
                Style::default().fg(theme::warning()),
            )));
        }
        _ => {}
    }
    Panel::new(lang.title_status().into(), lines)
}

/// The `/config` form — one row per setting, its value walked with ←→.
///
/// **It changes things, unlike the other panels.** ↑↓ pick the row, ←→ pick the value,
/// Enter saves and closes, Esc closes and throws the draft away.
pub fn config(lang: Lang, config: crate::config::Config) -> Panel {
    let form = Form::new(lang, config);
    let mut panel = Panel::new(lang.title_config().into(), form_lines(&form));
    panel.form = Some(form);
    // The same rule as `/mode`: sized for the longest sentence the form can show, so `↑↓` and
    // `←→` never resize the box.
    panel.foot = config_foot(&form);
    panel
}

/// Every sentence the config form can put under its rows: one per setting, per value.
fn config_foot(form: &Form) -> Vec<Line<'static>> {
    Form::ROWS
        .into_iter()
        .flat_map(|setting| (0..setting.count()).map(move |i| setting.describe(i, form.lang)))
        .map(muted)
        .collect()
}

/// The gap between the label column and the value field.
const LABEL_GAP: usize = 4;

/// Draws the form: a leading blank, one row per setting, then the description of the row
/// under the cursor.
///
/// **The number of lines never changes.** The box is sized from it, so a form that grew a
/// line when a description wrapped would make the box jump as the cursor moved.
fn form_lines(form: &Form) -> Vec<Line<'static>> {
    let lang = form.lang;
    // **One column for every label, one for every value.** The widest label and the widest
    // value of *any* row set them, so `<` and `>` land in the same place on every row and
    // stay put as the value changes — a bracket that jumps under ← reads as breakage.
    let mut label_w = 0;
    let mut value_w = 0;
    for setting in Form::ROWS {
        label_w = label_w.max(display_width(setting.label(lang)));
        for i in 0..setting.count() {
            value_w = value_w.max(display_width(setting.value_label(i, lang)));
        }
    }

    let mut lines = vec![blank()];
    for (row, setting) in Form::ROWS.into_iter().enumerate() {
        let on = row == form.cursor;
        let label = setting.label(lang);
        let value = setting.value_label(form.chosen(setting), lang);
        // The value sits centered in its field, so both brackets keep their distance.
        let slack = value_w.saturating_sub(display_width(value));
        let left = slack / 2;
        let (bracket, text) = if on {
            (
                Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
                Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
            )
        } else {
            (Style::default().fg(theme::border_light()), Style::default().fg(theme::text()))
        };
        lines.push(Line::from(vec![
            Span::styled(if on { "❯ " } else { "  " }, Style::default().fg(theme::accent())),
            Span::styled(
                format!(
                    "{label}{:pad$}{:gap$}",
                    "",
                    "",
                    pad = label_w.saturating_sub(display_width(label)),
                    gap = LABEL_GAP
                ),
                if on {
                    Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::text())
                },
            ),
            Span::styled("< ", bracket),
            Span::styled(
                format!("{:left$}{value}{:right$}", "", "", left = left, right = slack - left),
                text,
            ),
            Span::styled(" >", bracket),
        ]));
    }
    lines.push(blank());
    // **What the value under the cursor actually does.** The row label names the setting;
    // it cannot say what `allow` will let through.
    let row = form.row();
    lines.push(muted(row.describe(form.chosen(row), lang)));
    lines
}

// ─────────────────────────────────────────────────────────────────────────────
// Line helpers
// ─────────────────────────────────────────────────────────────────────────────

/// One `label  value` row — the label muted, the value readable.
fn kv(label: &'static str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), Style::default().fg(theme::text_muted())),
        Span::styled(value, Style::default().fg(theme::text())),
    ])
}

fn muted(text: String) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().fg(theme::text_muted())))
}

fn blank() -> Line<'static> {
    Line::from("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Lang;
    use crate::usage::Usage;

    fn text(panel: &Panel) -> Vec<String> {
        panel.lines.iter().map(|l| l.to_string()).collect()
    }

    /// The current mode is marked with ❯ and all four are listed — a mode nobody can
    /// reach is useless.
    #[test]
    fn the_mode_panel_lists_every_mode_and_marks_the_current_one() {
        let p = mode(Lang::Ko, Mode::Plan, None);
        assert_eq!(p.title, "모드");
        let lines = text(&p);
        let joined = lines.join("\n");
        for m in Mode::ALL {
            assert!(joined.contains(m.label(Lang::Ko)), "{m:?} is missing: {joined}");
        }
        // The current one is the only line starting with ❯.
        let marked: Vec<&String> = lines.iter().filter(|l| l.contains('❯')).collect();
        assert_eq!(marked.len(), 1, "{lines:?}");
        assert!(marked[0].contains("계획"), "{marked:?}");
        // And it is the one `Enter` would apply, so an untouched panel changes nothing.
        assert_eq!(p.mode_pick, Some(Mode::Plan));
    }

    /// **The sentence under the list follows the cursor.** Rows are one line each now — the
    /// sentence beside them made the list ragged, and cut to fit it lost its end.
    #[test]
    fn the_mode_panel_says_the_sentence_of_the_row_the_cursor_is_on() {
        let p = mode(Lang::Ko, Mode::Normal, Some(Mode::Work));
        let lines = text(&p);
        let joined = lines.join("\n");
        // The cursor sits on 일 and carries that sentence, not the current mode's.
        assert!(lines.iter().any(|l| l.starts_with('❯') && l.contains('일')), "{lines:?}");
        assert!(joined.contains("태스크로 쪼갭니다"), "{joined}");
        assert!(
            !joined.contains("물어보지 않고"),
            "the current mode's sentence was drawn: {joined}"
        );
        assert_eq!(p.mode_pick, Some(Mode::Work));
    }

    /// **`**일**` must not reach the screen with its asterisks on.** The mode sentences carry
    /// markdown and this panel draws plain text — a marker printed raw is a character nobody
    /// typed, in the middle of the one sentence the panel exists to say.
    #[test]
    fn a_markdown_marker_in_a_mode_sentence_is_rendered_not_printed() {
        let p = mode(Lang::Ko, Mode::Work, None);
        let joined = text(&p).join("\n");
        assert!(!joined.contains('*'), "the markers were printed: {joined}");
        assert!(joined.contains("일(work)"), "the emphasised word lost its company: {joined}");
    }

    /// **Every sentence is kept, so the box can be sized for the tallest.** Built from the one on
    /// screen, the box grew and shrank as the arrows moved — and its position with it.
    #[test]
    fn the_mode_panel_keeps_all_four_sentences_for_sizing() {
        let p = mode(Lang::Ko, Mode::Normal, Some(Mode::Plan));
        assert_eq!(p.foot.len(), Mode::ALL.len(), "{:?}", text(&p));
        let joined: String = p.foot.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        for word in ["물어보지", "돌리지", "쪼갭니다", "되묻는"] {
            assert!(joined.contains(word), "{word} missing from the foot: {joined}");
        }
        // The one on screen is the last line of the body — the agreement `body_and_foot` keeps.
        let (body, foot) = p.body_and_foot();
        assert_eq!(body.len(), p.lines.len() - 1);
        assert!(
            foot.is_some_and(|f| f.to_string().contains("먼저 할 일을")),
            "the sentence for the row under the cursor is not the one shown"
        );
    }

    /// The config form's foot holds one sentence per setting **and per value** — that is what
    /// keeps its box the same size while `←→` walks the values on a row.
    #[test]
    fn the_config_foot_covers_every_row_and_value() {
        let p = config(Lang::Ko, crate::config::Config::default());
        let want: usize = Form::ROWS.iter().map(|s| s.count()).sum();
        assert_eq!(p.foot.len(), want, "{:?}", text(&p));
        let (body, foot) = p.body_and_foot();
        assert_eq!(body.len(), p.lines.len() - 1);
        assert!(foot.is_some(), "no sentence was shown");
    }

    /// One row of a manager, so these tests say what the panel draws rather than repeating the
    /// shape. **Every field is given**, so adding one to `ManagerRow` breaks this in one place.
    fn row(
        id: &str,
        subtitle: &str,
        origin: Origin,
        toggle: Option<bool>,
        detail: &[(&str, &str)],
    ) -> ManagerRow {
        ManagerRow {
            id: id.into(),
            title: id.into(),
            subtitle: subtitle.into(),
            detail: detail.iter().map(|(l, v)| (l.to_string(), v.to_string())).collect(),
            origin,
            toggle,
            remove: None,
        }
    }

    #[test]
    fn an_empty_manager_says_there_are_none_and_where_to_write_them() {
        let p = mcp_manager(Lang::Ko, Vec::new());
        assert!(text(&p)[0].contains("없습니다"), "{:?}", text(&p));
        assert!(p.title.contains("MCP"), "{}", p.title);
        // **Still a manager, so `a` can add the first one.** A list that cannot be added to until
        // it has something in it is a trap, and the key hint has to be the manager's.
        let manager = p.manager.as_ref().expect("an empty list is still something to act on");
        assert!(manager.rows.is_empty());
        assert!(manager.empty.is_some(), "it does not say there are none");

        let p = plugin_manager(Lang::Ko, Vec::new());
        assert!(text(&p)[0].contains("없습니다"), "{:?}", text(&p));
        assert!(p.manager.is_some(), "there would be no way to install the first one");
    }

    /// **The cursor decides which block is shown**, which is the whole reason the detail is drawn
    /// under the list rather than beside each row, where it made the list ragged.
    #[test]
    fn the_block_under_the_list_follows_the_cursor() {
        let rows = vec![
            row("github", "돌고 있습니다", Origin::User, None, &[("실행", "npx -y gh")]),
            row(
                "playwright",
                "Cursor ‒ 꺼짐",
                Origin::Elsewhere,
                Some(false),
                &[("실행", "npx -y @playwright/mcp")],
            ),
        ];
        let mut p = mcp_manager(Lang::Ko, rows);
        let first = text(&p).join("\n");
        assert!(first.contains("npx -y gh"), "{first}");
        assert!(!first.contains("@playwright/mcp"), "both blocks were drawn: {first}");

        p.manager.as_mut().expect("a manager").move_cursor(1);
        p.refresh();
        let second = text(&p).join("\n");
        assert!(second.contains("@playwright/mcp"), "{second}");
        assert!(!second.contains("npx -y gh"), "the old block stayed: {second}");
    }

    /// **The box is one size on every row.** A detail block is a different shape for each row, and
    /// a box that grew as the cursor moved would move as well — a panel is centred.
    #[test]
    fn the_manager_box_is_one_size_on_every_row() {
        let rows = vec![
            row("a", "짧음", Origin::User, None, &[("실행", "x")]),
            row(
                "b",
                "아주 길고 긴 설명입니다",
                Origin::Elsewhere,
                Some(true),
                &[
                    ("실행", "npx -y @playwright/mcp --with-a-long-argument"),
                    ("환경변수", "GITHUB_TOKEN, ANOTHER_ONE"),
                    ("도구", "12개"),
                ],
            ),
        ];
        let mut p = mcp_manager(Lang::Ko, rows);
        let shape = |p: &Panel| {
            (
                p.lines.len(),
                p.lines.iter().map(|l| display_width(&l.to_string())).collect::<Vec<_>>(),
            )
        };
        let want = shape(&p);
        for at in 0..p.manager.as_ref().expect("a manager").rows.len() {
            p.manager.as_mut().expect("a manager").cursor = at;
            p.refresh();
            assert_eq!(shape(&p), want, "the box changed shape on row {at}");
        }
    }

    /// **Asking a question does not resize the box.** The sentence under the list is measured
    /// against every question the panel can ask (`room_for`), so pressing `d` does not move it.
    #[test]
    fn asking_before_a_removal_does_not_resize_the_box() {
        let rows = vec![row("x", "받아 둔 것", Origin::Fetched, Some(true), &[("자리", "/tmp/x")])];
        let mut p = plugin_manager(Lang::Ko, rows);
        let before = p.lines.len();
        p.manager.as_mut().expect("a manager").note = Some(Lang::Ko.manager_confirm("x"));
        p.refresh();
        let joined = text(&p).join("\n");
        assert!(joined.contains("한 번 더"), "{joined}");
        assert_eq!(p.lines.len(), before, "the question changed the box's height");
    }

    /// **A moved cursor takes the question down**, and it stops at the ends rather than wrapping —
    /// confirming a name the cursor has left is the accident the name exists to prevent.
    #[test]
    fn the_cursor_stops_at_the_ends_and_drops_the_question() {
        let rows = vec![
            row("a", "", Origin::Fetched, Some(true), &[]),
            row("b", "", Origin::Fetched, Some(true), &[]),
        ];
        let mut manager = Manager::new(ManagerKind::Plugins, rows, 0, Lang::Ko);
        manager.confirm = Some("a".into());
        manager.move_cursor(1);
        assert_eq!(manager.confirm, None, "the question outlived the row it named");
        assert_eq!(manager.row().expect("a row").id, "b");
        manager.move_cursor(5);
        assert_eq!(manager.row().expect("a row").id, "b", "it wrapped at the bottom");
        manager.move_cursor(-5);
        assert_eq!(manager.row().expect("a row").id, "a", "it wrapped at the top");
    }

    /// A plugin row carries what its manifest says. **The panel used to show a name and a sentence
    /// and nothing else** — this is the "too little information" it was asked for.
    #[test]
    fn a_plugin_row_carries_what_its_manifest_says() {
        let rows = vec![ManagerRow {
            id: "superpowers".into(),
            title: "superpowers".into(),
            subtitle: "받아 둔 것".into(),
            detail: vec![
                ("판".into(), "1.2.3".into()),
                ("설명".into(), "무엇을 하는지".into()),
                ("주는 것".into(), "명령 2개 ‒ 스킬 1개".into()),
            ],
            origin: Origin::Fetched,
            toggle: Some(true),
            remove: Some(Removal::Directory),
        }];
        let joined = text(&plugin_manager(Lang::Ko, rows)).join("\n");
        assert!(joined.contains("superpowers"), "{joined}");
        assert!(joined.contains("1.2.3"), "{joined}");
        assert!(joined.contains("무엇을 하는지"), "{joined}");
        assert!(joined.contains("명령 2개"), "{joined}");
    }

    #[test]
    fn the_skills_panel_lists_names_and_descriptions() {
        let p = skills(
            Lang::En,
            &[SkillInfo {
                name: "검색".into(), description: "코드에서 무언가를 찾는다".into()
            }],
        );
        let joined = text(&p).join("\n");
        assert!(joined.contains("검색"), "{joined}");
        assert!(joined.contains("코드에서 무언가를 찾는다"), "{joined}");
    }

    /// **The name on its own row, the description under it.** Beside the name a short description
    /// sat on the row while a long one wrapped below, so one list had two shapes — reported
    /// 2026-09-13, and the reason the picker's notes all moved under the list as well.
    #[test]
    fn a_skills_description_goes_under_its_name_not_beside_it() {
        let p = skills(
            Lang::Ko,
            &[
                SkillInfo { name: "짧은".into(), description: "짧은 설명".into() },
                SkillInfo {
                    name: "긴것".into(), description: "아주 길고 긴 설명입니다".into()
                },
            ],
        );
        let lines = text(&p);
        let name = lines.iter().position(|l| l.contains("짧은") && !l.contains("설명"));
        let name = name.expect("no row carries the name alone");
        assert_eq!(lines[name], "∙ 짧은", "the name shares its row: {lines:?}");
        assert_eq!(lines[name + 1], "  짧은 설명", "the description is not under it: {lines:?}");
        // And the long one is placed the same way — the list keeps one shape.
        let long = lines.iter().position(|l| l.contains("긴것")).expect("{lines:?}");
        assert_eq!(lines[long], "∙ 긴것", "{lines:?}");
        assert!(
            lines[long + 1].starts_with("  아주 길고") && !lines[long].contains("설명"),
            "the long description is placed differently: {lines:?}"
        );
    }

    /// **A row that cannot be switched says so by having nothing to switch.** The dot, the keys and
    /// the detail all come from the row, so the panel is honest about what it does not know how to
    /// change rather than offering a key that would fail.
    #[test]
    fn a_row_that_cannot_be_switched_offers_nothing() {
        let rows = vec![ManagerRow {
            id: "local".into(),
            title: "local".into(),
            subtitle: "직접 둔 것".into(),
            detail: vec![("자리".into(), "/tmp/plugins/local".into())],
            origin: Origin::HandPlaced,
            toggle: None,
            remove: None,
        }];
        let p = plugin_manager(Lang::Ko, rows);
        let manager = p.manager.as_ref().expect("a manager");
        let row = manager.row().expect("a row");
        assert_eq!(row.toggle, None, "it claims to be switchable");
        assert_eq!(row.remove, None, "it claims to be removable");
        // The dot is hollow, because nothing is on as far as this panel can say.
        let joined = text(&p).join("\n");
        assert!(joined.contains('○'), "{joined}");
    }

    #[test]
    fn the_status_panel_shows_the_session_picture() {
        let info = crate::lang::StatusInfo {
            session_id: Some("세션-1"),
            project: Some("프로젝트-1"),
            agent: "Main Agent",
            mode: "work",
            cwd: std::path::Path::new("/tmp/zyris"),
            usage: &Usage { model: Some("claude-opus-5-1m".into()), ..Usage::default() },
            pending: None,
        };
        let p = status(Lang::Ko, &info);
        let joined = text(&p).join("\n");
        assert!(joined.contains("세션-1"), "{joined}");
        assert!(joined.contains("프로젝트-1"), "{joined}");
        assert!(joined.contains("Main Agent"), "{joined}");
        assert!(joined.contains("claude-opus-5-1m"), "{joined}");
    }

    /// The account panel carries a logout button — the one thing worth doing there —
    /// while the other panels carry none, so Tab does nothing on them.
    #[test]
    fn the_account_panel_carries_a_logout_button() {
        let p = account(Lang::Ko, "루마", "me@standoor.org", "user-1", None, None, &[]);
        assert_eq!(p.button, Some(PanelButton::Logout));
        let p = mode(Lang::Ko, Mode::Normal, None);
        assert_eq!(p.button, None, "a panel without an action must not show a button");
    }

    /// The columns `<` and `>` sit in, per row that has them.
    fn brackets(panel: &Panel) -> Vec<(usize, usize)> {
        panel
            .lines
            .iter()
            .filter_map(|l| {
                let s = l.to_string();
                let open = s.find('<')?;
                let close = s.rfind('>')?;
                Some((display_width(&s[..open]), display_width(&s[..close])))
            })
            .collect()
    }

    /// The form shows one row per setting, each carrying its current value between
    /// brackets — a setting you can't see the value of might as well not exist.
    #[test]
    fn the_config_form_shows_every_setting_with_its_current_value() {
        let cfg = crate::config::Config {
            dir_access: DirAccess::Allow,
            default_mode: Some(Mode::Job),
            ..Default::default()
        };
        let p = config(Lang::Ko, cfg);
        let joined = text(&p).join("\n");
        for label in ["다른 디렉토리 접근", "언어", "기본 모드"] {
            assert!(joined.contains(label), "{label} is missing: {joined}");
        }
        assert!(joined.contains("< "), "no value field: {joined}");
        // Exactly the values the settings hold — not the ones they don't.
        assert!(joined.contains("허용"), "{joined}");
        assert!(joined.contains("작업"), "{joined}");
        assert!(!joined.contains("거부"), "deny is not the current value: {joined}");
        assert_eq!(brackets(&p).len(), Form::ROWS.len(), "one field per setting: {joined}");
    }

    /// **The brackets never move.** They share one column across every row and stay there
    /// through every value — a `>` that slides as you press ← reads as breakage.
    ///
    /// The exception is the language row, and it is not one: picking a language re-letters
    /// the whole box, so the columns are re-measured for the new words. The promise is per
    /// language, which is why this checks both.
    #[test]
    fn the_brackets_hold_one_column_through_every_value() {
        for lang in [Lang::Ko, Lang::En] {
            let mut p = config(lang, crate::config::Config::default());
            let want = brackets(&p);
            assert_eq!(want.len(), Form::ROWS.len());
            assert!(want.windows(2).all(|w| w[0] == w[1]), "{lang:?} rows disagree: {want:?}");

            for (row, setting) in Form::ROWS.into_iter().enumerate() {
                if setting == Setting::Language {
                    continue;
                }
                for _ in 0..setting.count() {
                    let form = p.form.as_mut().expect("the config panel carries a form");
                    form.cursor = row;
                    form.shift(1);
                    p.refresh();
                    assert_eq!(brackets(&p), want, "{lang:?} moved on row {row}: {:?}", text(&p));
                }
            }
        }
    }

    /// ←→ walk the values and come back around, so the two-value rows toggle either way
    /// and no row has a dead end.
    #[test]
    fn the_values_wrap_around_at_both_ends() {
        let mut form = Form::new(Lang::Ko, crate::config::Config::default());
        assert_eq!(form.draft.dir_access, DirAccess::Deny);
        form.shift(1);
        assert_eq!(form.draft.dir_access, DirAccess::Allow, "wrapped forward");
        form.shift(-1);
        assert_eq!(form.draft.dir_access, DirAccess::Deny, "wrapped back");

        // The mode row has five values; walking past the last returns to the first.
        form.cursor = 2;
        for _ in 0..Setting::DefaultMode.count() {
            form.shift(1);
        }
        assert_eq!(form.draft.default_mode, None, "a full lap lands where it started");
        form.shift(-1);
        assert_eq!(form.draft.default_mode, Some(Mode::ALL[Mode::ALL.len() - 1]));
    }

    /// ↑↓ wrap too, and the description under the rows follows the cursor — it explains
    /// the value being pointed at, which is the only thing the row label cannot say.
    #[test]
    fn the_description_follows_the_cursor() {
        let mut p = config(Lang::Ko, crate::config::Config::default());
        let deny = text(&p).join("\n");
        assert!(deny.contains(Lang::Ko.cfg_dir_desc(DirAccess::Deny)), "{deny}");

        p.form.as_mut().unwrap().move_cursor(-1); // down to the language row
        p.refresh();
        let language = text(&p).join("\n");
        assert!(language.contains(Lang::Ko.cfg_lang_desc(Lang::Ko)), "{language}");
        assert!(!language.contains(Lang::Ko.cfg_dir_desc(DirAccess::Deny)), "{language}");

        p.form.as_mut().unwrap().move_cursor(1); // back up, wrapping is not needed here
        p.refresh();
        assert!(text(&p).join("\n").contains(Lang::Ko.cfg_dir_desc(DirAccess::Deny)));

        // Up from the first row lands on the last, whatever the last happens to be.
        p.form.as_mut().unwrap().move_cursor(1);
        assert_eq!(p.form.unwrap().row(), Form::ROWS[Form::ROWS.len() - 1]);
    }

    /// **The box never changes height.** It is sized from the line count, so a form that
    /// grew a line as the cursor moved would make the box jump under the eye.
    #[test]
    fn the_form_is_always_the_same_height() {
        let mut p = config(Lang::Ko, crate::config::Config::default());
        let rows = p.lines.len();
        for row in 0..Form::ROWS.len() {
            for _ in 0..Form::ROWS[row].count() {
                let form = p.form.as_mut().unwrap();
                form.cursor = row;
                form.shift(1);
                p.refresh();
                assert_eq!(p.lines.len(), rows, "height moved on row {row}");
            }
        }
    }

    /// Moving the language row re-letters the box on the spot — the form draws from the
    /// draft, so you see what Enter would give you before you press it.
    #[test]
    fn changing_the_language_row_reletters_the_form() {
        let mut p = config(Lang::Ko, crate::config::Config::default());
        assert_eq!(p.title, Lang::Ko.title_config());
        p.form.as_mut().unwrap().cursor = 1;
        p.form.as_mut().unwrap().shift(1);
        p.refresh();
        assert_eq!(p.form.unwrap().lang, Lang::En);
        assert_eq!(p.title, Lang::En.title_config(), "the title follows the draft");
        assert!(text(&p).join("\n").contains(Lang::En.cfg_dir_access()), "{:?}", text(&p));
    }

    /// The draft starts from the settings it was handed — the form opens on the truth,
    /// not on the defaults.
    #[test]
    fn the_draft_starts_from_the_settings_it_was_given() {
        let cfg = crate::config::Config {
            dir_access: DirAccess::Allow,
            default_mode: Some(Mode::Plan),
            ..Default::default()
        };
        let p = config(Lang::En, cfg);
        let form = p.form.expect("the config panel carries a form");
        assert_eq!(form.draft, cfg);
        assert_eq!(form.lang, Lang::En);
        assert_eq!(form.cursor, 0);
    }

    /// Scrolling never goes below zero, and `max_scroll` says when the end is reached.
    #[test]
    fn scrolling_clamps_at_the_top_and_the_bottom_is_measurable() {
        let mut p = Panel::new("t".into(), vec![Line::from("a"); 5]);
        p.scroll_up(10);
        assert_eq!(p.scroll, 0);
        p.scroll_down(10);
        assert_eq!(p.scroll, 10);
        assert_eq!(max_scroll(5, 3), 2, "5 lines in a 3-row box scroll by 2");
    }
}
