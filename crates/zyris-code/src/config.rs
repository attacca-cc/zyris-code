//! Persistent settings — what `/config` shows and changes.
//!
//! **Two settings live here; the language lives with `lang.rs`.** The language file
//! (`~/.config/zyris-code/lang`) was there first and off-screen code already reads it, so
//! `/config` edits it through the same `lang::save` and shows it in the same panel — one
//! source of truth instead of two copies of the same choice.
//!
//! **The approval window is gone.** Asking a human every time a tool left the working
//! directory only broke the flow (2026-08-02 user decision — same reason `ask` mode was
//! removed). What it used to ask is now a policy setting: `allow` runs it, `deny` refuses it.

use serde::{Deserialize, Serialize};

use crate::mode::Mode;

/// What happens when a tool touches a path **outside the working directory.**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DirAccess {
    /// Outside paths run without asking.
    Allow,
    /// Outside paths are refused with a message. The default — the safe side.
    #[default]
    Deny,
}

impl DirAccess {
    /// From what the person typed. Both languages' words are accepted.
    pub fn parse(text: &str) -> Option<DirAccess> {
        match text.trim().to_ascii_lowercase().as_str() {
            "allow" | "허용" | "허락" => Some(DirAccess::Allow),
            "deny" | "refuse" | "거부" | "차단" => Some(DirAccess::Deny),
            _ => None,
        }
    }

    /// The name written to the setting file.
    pub fn code(self) -> &'static str {
        match self {
            DirAccess::Allow => "allow",
            DirAccess::Deny => "deny",
        }
    }
}

/// Which palette the screen is drawn in.
///
/// **`Auto` is a guess, not an answer.** Asking the terminal for its real background means OSC 11
/// and waiting for a reply, which this app does not do — `Terminal::clear()`'s DSR hung it on
/// terminals that never answered. `Auto` reads the `COLORFGBG` hint and settles for dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeChoice {
    /// Work it out from the terminal, and use dark when it does not say.
    #[default]
    Auto,
    Dark,
    Light,
}

impl ThemeChoice {
    /// From what the person typed. Both languages' words are accepted.
    pub fn parse(text: &str) -> Option<ThemeChoice> {
        match text.trim().to_ascii_lowercase().as_str() {
            "auto" | "자동" => Some(ThemeChoice::Auto),
            other => crate::theme::Theme::parse(other).map(|t| match t {
                crate::theme::Theme::Dark => ThemeChoice::Dark,
                crate::theme::Theme::Light => ThemeChoice::Light,
            }),
        }
    }

    /// Which palette this actually resolves to right now.
    pub fn resolve(self) -> crate::theme::Theme {
        match self {
            ThemeChoice::Auto => crate::theme::detect(),
            ThemeChoice::Dark => crate::theme::Theme::Dark,
            ThemeChoice::Light => crate::theme::Theme::Light,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            ThemeChoice::Auto => "auto",
            ThemeChoice::Dark => "dark",
            ThemeChoice::Light => "light",
        }
    }
}

/// The settings. Missing keys fall back to the defaults, so an old file keeps working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Config {
    /// What happens outside the working directory.
    #[serde(default)]
    pub dir_access: DirAccess,
    /// The mode the app opens in. `None` means the built-in default (normal).
    #[serde(default)]
    pub default_mode: Option<Mode>,
    /// Which palette the screen uses. **This app paints no background of its own**, so on a light
    /// terminal the dark palette's text sits at 1.19:1 against the paper — unreadable.
    #[serde(default)]
    pub theme: ThemeChoice,
}

/// The file where the settings live. Same directory as the credentials and the language.
fn store() -> Option<std::path::PathBuf> {
    crate::conn::credential_dir().map(|dir| dir.join("config.json"))
}

impl Config {
    pub fn load() -> Config {
        let Some(at) = store() else { return Config::default() };
        let Ok(text) = std::fs::read_to_string(&at) else { return Config::default() };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "couldn't read the settings ‒ using the defaults");
            Config::default()
        })
    }

    /// Saves the settings. **The app keeps running even if this fails** — they are already
    /// applied for this run, same as `lang::save`.
    pub fn save(&self) {
        let Some(at) = store() else { return };
        if let Some(dir) = at.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(text) = serde_json::to_string(self) else { return };
        if let Err(e) = std::fs::write(&at, text) {
            tracing::warn!(error = %e, "couldn't save the settings");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_deny_outside_and_no_default_mode() {
        let c = Config::default();
        assert_eq!(c.dir_access, DirAccess::Deny);
        assert_eq!(c.default_mode, None);
    }

    #[test]
    fn dir_access_answers_to_both_languages() {
        assert_eq!(DirAccess::parse("allow"), Some(DirAccess::Allow));
        assert_eq!(DirAccess::parse("허용"), Some(DirAccess::Allow));
        assert_eq!(DirAccess::parse("deny"), Some(DirAccess::Deny));
        assert_eq!(DirAccess::parse("거부"), Some(DirAccess::Deny));
        assert_eq!(DirAccess::parse("아무거나"), None);
    }

    /// A file written by `save` must come back through `load` unchanged — that is the
    /// whole round trip the command relies on.
    #[test]
    fn a_saved_config_round_trips() {
        let c = Config {
            dir_access: DirAccess::Allow,
            default_mode: Some(Mode::Job),
            ..Config::default()
        };
        let text = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&text).unwrap();
        assert_eq!(back, c);
    }

    /// A file from before a setting existed (or with a typo) must not take the app down —
    /// the missing key falls back to its default.
    #[test]
    fn a_file_with_missing_keys_falls_back_to_defaults() {
        let back: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(back, Config::default());
    }
}
