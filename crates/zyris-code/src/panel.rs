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
use crate::lang::Lang;
use crate::markdown::display_width;
use crate::mode::{Mode, Route};
use crate::plugin::Plugin;
use crate::theme;
use crate::tools::skill::SkillInfo;

/// An open popup panel.
#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    /// The title shown in the box's top border.
    pub title: String,
    /// The styled body lines. Drawn one per row, truncated to the box width.
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
}

impl Setting {
    /// The row label.
    pub fn label(self, lang: Lang) -> &'static str {
        match self {
            Setting::DirAccess => lang.cfg_dir_access(),
            Setting::Language => lang.cfg_language(),
            Setting::DefaultMode => lang.cfg_default_mode(),
            Setting::Theme => lang.cfg_theme(),
        }
    }

    /// How many values this setting cycles through.
    pub fn count(self) -> usize {
        match self {
            Setting::DirAccess | Setting::Language => 2,
            // The unset state plus every mode.
            Setting::DefaultMode => 1 + Mode::ALL.len(),
            Setting::Theme => THEMES.len(),
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
        }
    }
}

/// The palette choices, in the order the row walks them.
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
    pub const ROWS: [Setting; 4] =
        [Setting::DirAccess, Setting::Language, Setting::DefaultMode, Setting::Theme];

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
    fn new(title: String, lines: Vec<Line<'static>>) -> Self {
        Self { title, lines, scroll: 0, button: None, button_focused: false, form: None }
    }

    /// Redraws the body from the form after a key moved the cursor or changed a value.
    ///
    /// **The lines stay the one thing the widget draws.** Letting the widget read the form
    /// instead would put layout in two places, and the two would drift.
    pub fn refresh(&mut self) {
        if let Some(form) = self.form {
            self.title = form.lang.title_config().to_string();
            self.lines = form_lines(&form);
        }
    }

    /// How far the body can scroll. `visible` is the number of body rows the box
    /// shows; the widget clamps `scroll` to this so it never points past the end.
    pub fn max_scroll(&self, visible: usize) -> usize {
        self.lines.len().saturating_sub(visible)
    }

    pub fn scroll_up(&mut self, by: usize) {
        self.scroll = self.scroll.saturating_sub(by);
    }

    pub fn scroll_down(&mut self, by: usize) {
        self.scroll = self.scroll.saturating_add(by);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Builders
// ─────────────────────────────────────────────────────────────────────────────

/// The `/mode` panel — every mode with its description, the current one marked.
pub fn mode(lang: Lang, now: Mode) -> Panel {
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{} · {}", lang.current_mode(), now.label(lang)),
            Style::default().fg(now.color()).add_modifier(Modifier::BOLD),
        )),
        blank(),
    ];
    for m in Mode::ALL {
        let on = m == now;
        let mut spans = vec![
            Span::styled(if on { "❯ " } else { "  " }, Style::default().fg(theme::accent())),
            Span::styled(
                m.label(lang),
                if on {
                    Style::default().fg(m.color()).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::text())
                },
            ),
            Span::styled(" — ", Style::default().fg(theme::border_light())),
            Span::styled(
                lang.mode_desc(m),
                Style::default().fg(if on { theme::text() } else { theme::text_muted() }),
            ),
        ];
        lines.push(Line::from(std::mem::take(&mut spans)));
    }
    lines.push(blank());
    lines.push(muted(lang.mode_cycle_hint().to_string()));
    Panel::new(lang.title_mode().into(), lines)
}

/// The `/mcp` panel — every attached server and how many tools it brought.
/// The `/mcp` panel: what is running, and **what could be**.
///
/// `found` is what other clients already have set up (`mcp::discovery`), with whether this machine
/// has said yes. Showing it is the whole point of discovering it — a list nobody sees may as well
/// not have been read.
pub fn mcp(
    lang: Lang,
    report: &[(String, Result<usize, String>)],
    found: &[(String, String, bool)],
) -> Panel {
    if report.is_empty() && found.is_empty() {
        return Panel::new(lang.title_mcp().into(), vec![muted(lang.mcp_empty().to_string())]);
    }
    let mut lines = Vec::new();
    for (name, outcome) in report {
        let (text, color) = match outcome {
            Ok(n) => (lang.mcp_tools(*n), theme::success()),
            Err(why) => (lang.mcp_failed(why), theme::danger()),
        };
        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(theme::accent())),
            Span::styled(
                name.clone(),
                Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" — ", Style::default().fg(theme::border_light())),
            Span::styled(text, Style::default().fg(color)),
        ]));
    }
    if !found.is_empty() {
        if !lines.is_empty() {
            lines.push(blank());
        }
        lines.push(muted(lang.mcp_found_heading().to_string()));
        for (slug, source, on) in found {
            // **The dot says whether it will start**, because that is the only thing about a
            // discovered entry a person has to decide.
            let (mark, colour) =
                if *on { ("● ", theme::success()) } else { ("○ ", theme::text_muted()) };
            lines.push(Line::from(vec![
                Span::styled(mark, Style::default().fg(colour)),
                Span::styled(
                    slug.clone(),
                    Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" — ", Style::default().fg(theme::border_light())),
                Span::styled(
                    lang.mcp_found_from(source, *on),
                    Style::default().fg(theme::text_muted()),
                ),
            ]));
        }
        lines.push(blank());
        lines.push(muted(lang.mcp_switch_hint().to_string()));
    }
    lines.push(blank());
    lines.push(muted(lang.mcp_config_hint().to_string()));
    Panel::new(lang.title_mcp().into(), lines)
}

/// The `/skills` panel — name and one-line description per skill.
pub fn skills(lang: Lang, skills: &[SkillInfo]) -> Panel {
    if skills.is_empty() {
        return Panel::new(
            lang.title_skills().into(),
            vec![muted(lang.skills_empty().to_string())],
        );
    }
    let lines = skills
        .iter()
        .map(|s| {
            Line::from(vec![
                Span::styled("· ", Style::default().fg(theme::accent())),
                Span::styled(
                    s.name.clone(),
                    Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" — ", Style::default().fg(theme::border_light())),
                Span::styled(s.description.clone(), Style::default().fg(theme::text_muted())),
            ])
        })
        .collect();
    Panel::new(lang.title_skills().into(), lines)
}

/// The `/plugin` panel — every fetched plugin, what it ships underneath.
pub fn plugins(lang: Lang, found: &[Plugin]) -> Panel {
    if found.is_empty() {
        return Panel::new(
            lang.title_plugins().into(),
            vec![muted(lang.plugins_empty().to_string())],
        );
    }
    let mut lines = Vec::new();
    for p in found {
        let mut spans = vec![
            Span::styled("· ", Style::default().fg(theme::accent())),
            Span::styled(
                p.name.clone(),
                Style::default().fg(theme::text_heading()).add_modifier(Modifier::BOLD),
            ),
        ];
        if !p.fetched() {
            spans.push(Span::styled(
                lang.plugin_hand_placed(),
                Style::default().fg(theme::text_muted()),
            ));
        }
        if !p.description.is_empty() {
            spans.push(Span::styled(" — ", Style::default().fg(theme::border_light())));
            spans.push(Span::styled(p.description.clone(), Style::default().fg(theme::text())));
        }
        lines.push(Line::from(spans));
        for spec in &p.mcp {
            lines.push(muted(format!(
                "    {}",
                lang.plugin_mcp_line(&spec.slug, &spec.transport.summary())
            )));
        }
        if p.skills.is_some() {
            lines.push(muted(format!("    {}", lang.plugin_skills_line())));
        }
        lines.push(blank());
    }
    // A trailing blank after the last plugin reads as empty space.
    if lines.last().is_some_and(|l| l.spans.is_empty()) {
        lines.pop();
    }
    Panel::new(lang.title_plugins().into(), lines)
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
    panel
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
        let p = mode(Lang::Ko, Mode::Plan);
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
    }

    #[test]
    fn an_empty_mcp_report_says_there_are_none_and_where_to_write_them() {
        let p = mcp(Lang::Ko, &[], &[]);
        assert!(text(&p)[0].contains("없습니다"), "{:?}", text(&p));
        assert!(p.title.contains("MCP"), "{}", p.title);
    }

    #[test]
    fn a_mcp_report_lists_servers_with_their_outcome() {
        let report = vec![("files".into(), Ok(3)), ("broken".into(), Err("없는 명령".into()))];
        let p = mcp(Lang::Ko, &report, &[]);
        let joined = text(&p).join("\n");
        assert!(joined.contains("files"), "{joined}");
        assert!(joined.contains("도구 3개"), "{joined}");
        assert!(joined.contains("못 띄웠습니다"), "{joined}");
    }

    /// **What was found is shown even when nothing is running.** A list nobody sees may as well
    /// not have been read, and with no servers of our own the panel used to say only "there are
    /// none" while three sat in another client's config.
    #[test]
    fn servers_found_in_another_program_are_offered_with_a_way_to_turn_them_on() {
        let found = vec![("playwright".to_string(), "Cursor".to_string(), false)];
        let p = mcp(Lang::Ko, &[], &found);
        let joined = text(&p).join("\n");
        assert!(joined.contains("playwright"), "{joined}");
        assert!(
            joined.contains("Cursor"),
            "where it came from is the basis for trusting it: {joined}"
        );
        assert!(joined.contains("/mcp on"), "no way to turn it on: {joined}");
        assert!(!joined.contains("없습니다"), "it said there were none: {joined}");
    }

    /// On and off must not read the same. The dot carries it, so the words have to as well.
    #[test]
    fn a_server_that_is_on_reads_differently_from_one_that_is_off() {
        let on = text(&mcp(Lang::Ko, &[], &[("a".into(), "Cursor".into(), true)])).join("\n");
        let off = text(&mcp(Lang::Ko, &[], &[("a".into(), "Cursor".into(), false)])).join("\n");
        assert_ne!(on, off);
        assert!(on.contains("켭니다"), "{on}");
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
        let p = mode(Lang::Ko, Mode::Normal);
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
        assert_eq!(p.max_scroll(3), 2, "5 lines in a 3-row box scroll by 2");
    }
}
