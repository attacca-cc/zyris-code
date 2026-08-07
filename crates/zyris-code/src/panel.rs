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

use crate::lang::Lang;
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
        Self { title, lines, scroll: 0, button: None, button_focused: false }
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
            Span::styled(if on { "❯ " } else { "  " }, Style::default().fg(theme::ACCENT)),
            Span::styled(
                m.label(lang),
                if on {
                    Style::default().fg(m.color()).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT)
                },
            ),
            Span::styled(" — ", Style::default().fg(theme::BORDER_LIGHT)),
            Span::styled(
                lang.mode_desc(m),
                Style::default().fg(if on { theme::TEXT } else { theme::TEXT_MUTED }),
            ),
        ];
        lines.push(Line::from(std::mem::take(&mut spans)));
    }
    lines.push(blank());
    lines.push(muted(lang.mode_cycle_hint().to_string()));
    Panel::new(lang.title_mode().into(), lines)
}

/// The `/mcp` panel — every attached server and how many tools it brought.
pub fn mcp(lang: Lang, report: &[(String, Result<usize, String>)]) -> Panel {
    if report.is_empty() {
        return Panel::new(lang.title_mcp().into(), vec![muted(lang.mcp_empty().to_string())]);
    }
    let mut lines = Vec::new();
    for (name, outcome) in report {
        let (text, color) = match outcome {
            Ok(n) => (lang.mcp_tools(*n), theme::SUCCESS),
            Err(why) => (lang.mcp_failed(why), theme::DANGER),
        };
        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(theme::ACCENT)),
            Span::styled(
                name.clone(),
                Style::default().fg(theme::TEXT_HEADING).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" — ", Style::default().fg(theme::BORDER_LIGHT)),
            Span::styled(text, Style::default().fg(color)),
        ]));
    }
    lines.push(blank());
    lines.push(muted(lang.mcp_config_hint().to_string()));
    Panel::new(lang.title_mcp().into(), lines)
}

/// The `/skills` panel — name and one-line description per skill.
pub fn skills(lang: Lang, skills: &[SkillInfo]) -> Panel {
    if skills.is_empty() {
        return Panel::new(lang.title_skills().into(), vec![muted(lang.skills_empty().to_string())]);
    }
    let lines = skills
        .iter()
        .map(|s| {
            Line::from(vec![
                Span::styled("· ", Style::default().fg(theme::ACCENT)),
                Span::styled(
                    s.name.clone(),
                    Style::default().fg(theme::TEXT_HEADING).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" — ", Style::default().fg(theme::BORDER_LIGHT)),
                Span::styled(s.description.clone(), Style::default().fg(theme::TEXT_MUTED)),
            ])
        })
        .collect();
    Panel::new(lang.title_skills().into(), lines)
}

/// The `/plugin` panel — every fetched plugin, what it ships underneath.
pub fn plugins(lang: Lang, found: &[Plugin]) -> Panel {
    if found.is_empty() {
        return Panel::new(lang.title_plugins().into(), vec![muted(lang.plugins_empty().to_string())]);
    }
    let mut lines = Vec::new();
    for p in found {
        let mut spans = vec![
            Span::styled("· ", Style::default().fg(theme::ACCENT)),
            Span::styled(
                p.name.clone(),
                Style::default().fg(theme::TEXT_HEADING).add_modifier(Modifier::BOLD),
            ),
        ];
        if !p.fetched() {
            spans.push(Span::styled(lang.plugin_hand_placed(), Style::default().fg(theme::TEXT_MUTED)));
        }
        if !p.description.is_empty() {
            spans.push(Span::styled(" — ", Style::default().fg(theme::BORDER_LIGHT)));
            spans.push(Span::styled(p.description.clone(), Style::default().fg(theme::TEXT)));
        }
        lines.push(Line::from(spans));
        for spec in &p.mcp {
            lines.push(muted(format!("    {}", lang.plugin_mcp_line(&spec.slug, &spec.command))));
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
    let scopes_text = if scopes.is_empty() {
        lang.acc_none().to_string()
    } else {
        scopes.join(", ")
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(
                name.to_string(),
                Style::default().fg(theme::TEXT_HEADING).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  ({email})"), Style::default().fg(theme::TEXT_MUTED)),
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
        kv(lang.st_agent(), if info.agent.is_empty() { "-".into() } else { info.agent.to_string() }),
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
                format!(
                    "{}% ({}/{})",
                    pct,
                    crate::usage::compact(used),
                    crate::usage::compact(max)
                )
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
                Style::default().fg(theme::WARNING),
            )));
        }
        Some(Route::Job) => {
            lines.push(blank());
            lines.push(Line::from(Span::styled(
                lang.st_pending_job(),
                Style::default().fg(theme::WARNING),
            )));
        }
        _ => {}
    }
    Panel::new(lang.title_status().into(), lines)
}

// ─────────────────────────────────────────────────────────────────────────────
// Line helpers
// ─────────────────────────────────────────────────────────────────────────────

/// One `label  value` row — the label muted, the value readable.
fn kv(label: &'static str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), Style::default().fg(theme::TEXT_MUTED)),
        Span::styled(value, Style::default().fg(theme::TEXT)),
    ])
}

fn muted(text: String) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().fg(theme::TEXT_MUTED)))
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
        let p = mcp(Lang::Ko, &[]);
        assert!(text(&p)[0].contains("없습니다"), "{:?}", text(&p));
        assert!(p.title.contains("MCP"), "{}", p.title);
    }

    #[test]
    fn a_mcp_report_lists_servers_with_their_outcome() {
        let report = vec![
            ("files".into(), Ok(3)),
            ("broken".into(), Err("없는 명령".into())),
        ];
        let p = mcp(Lang::Ko, &report);
        let joined = text(&p).join("\n");
        assert!(joined.contains("files"), "{joined}");
        assert!(joined.contains("도구 3개"), "{joined}");
        assert!(joined.contains("못 띄웠습니다"), "{joined}");
    }

    #[test]
    fn the_skills_panel_lists_names_and_descriptions() {
        let p = skills(
            Lang::En,
            &[SkillInfo { name: "검색".into(), description: "코드에서 무언가를 찾는다".into() }],
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
