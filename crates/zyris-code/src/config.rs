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
    /// What to do when a newer release exists. **Default `notify`**: installing unasked replaces
    /// the program somebody is in the middle of using, and the wait lands on a launch they meant to
    /// spend on something else. `auto` is still a setting away for anyone who wants it.
    #[serde(default)]
    pub update: crate::update::Policy,
}

/// The file where the settings live. Same directory as the credentials and the language.
fn store() -> Option<std::path::PathBuf> {
    crate::conn::credential_dir().map(|dir| dir.join("config.json"))
}

impl Config {
    pub fn load() -> Config {
        let Some(at) = store() else { return Config::default() };
        Config::load_from(&at)
    }

    /// `load`, from an exact path.
    ///
    /// Split out for the gate's re-read (`tools::bridge::settings_at`), which knows which file it
    /// means — and for a test that must read a file it owns rather than the machine's own settings.
    pub fn load_from(at: &std::path::Path) -> Config {
        let Ok(text) = std::fs::read_to_string(at) else { return Config::default() };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "couldn't read the settings ‒ using the defaults");
            Config::default()
        })
    }

    /// Saves the settings. **The app keeps running even if this fails** — they are already
    /// applied for this run, same as `lang::save`.
    ///
    /// **A key this build does not know is carried through, not dropped.** `serde` ignores what it
    /// does not recognize on load, so a window running an older release used to erase whatever a
    /// newer one had written to the same file — its `Config::save` serialized only the fields it
    /// knew. The file is read as the JSON it is, only the keys this build owns are replaced, and
    /// the rest is written back as it was found. The write is atomic (`atomic::write_atomic`) so a
    /// window starting up cannot read it half-written and fall back to the defaults.
    ///
    /// **Only the keys that differ from `before` are written.** This window's copy of the other
    /// settings may be older than the file — another window may have changed them since — and
    /// writing the whole struct put this window's stale values back over that change: `/config dir
    /// deny` in one window was undone by the next `/config theme` in another. `before` is the copy
    /// this change was made from; the file is read now, and only what this change touched moves.
    pub fn save_changes(&self, before: &Config) {
        let Some(at) = store() else { return };
        let existing = std::fs::read_to_string(&at)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        let Some(next) = saved_over(existing, before, self) else { return };
        let Ok(text) = serde_json::to_vec(&next) else { return };
        if let Err(e) = crate::atomic::write_atomic(&at, &text, None) {
            tracing::warn!(error = %e, "couldn't save the settings");
        }
    }

    /// This copy, with the keys that changed between `before` and `after` taken from `after`.
    ///
    /// What the gate does with a change the screen pushes: its own copy may be newer than the
    /// screen's (re-read from the file another window wrote), so only what the screen actually
    /// changed is carried over.
    pub fn overlay(&self, before: &Config, after: &Config) -> Config {
        let (Ok(serde_json::Value::Object(mut mine)), Some(changes)) =
            (serde_json::to_value(self), changed(before, after))
        else {
            return *self;
        };
        mine.extend(changes);
        serde_json::from_value(serde_json::Value::Object(mine)).unwrap_or(*self)
    }
}

/// The keys whose value differs between `before` and `after`, with `after`'s value.
fn changed(before: &Config, after: &Config) -> Option<serde_json::Map<String, serde_json::Value>> {
    let (Ok(serde_json::Value::Object(old)), Ok(serde_json::Value::Object(new))) =
        (serde_json::to_value(before), serde_json::to_value(after))
    else {
        return None;
    };
    Some(new.into_iter().filter(|(key, value)| old.get(key) != Some(value)).collect())
}

/// What the file should hold after a change from `before` to `after`. `None` when nothing
/// changed, so nothing is written.
///
/// Pure, so the rule can be checked without a filesystem. A file that is missing or not an object
/// has nothing worth keeping, and gets the whole of `after`.
fn saved_over(
    existing: Option<serde_json::Value>,
    before: &Config,
    after: &Config,
) -> Option<serde_json::Value> {
    let changes = changed(before, after)?;
    match existing {
        Some(existing @ serde_json::Value::Object(_)) if !changes.is_empty() => {
            Some(merge(Some(existing), serde_json::Value::Object(changes)))
        }
        Some(serde_json::Value::Object(_)) => None,
        _ => serde_json::to_value(after).ok(),
    }
}

/// This build's keys written over the file's, every other key left as it was found.
///
/// Pure, so the rule can be checked without a filesystem — and it is the rule, not the writing,
/// that keeps a newer release's settings alive.
fn merge(existing: Option<serde_json::Value>, mine: serde_json::Value) -> serde_json::Value {
    match (existing, mine) {
        (Some(serde_json::Value::Object(mut theirs)), serde_json::Value::Object(ours)) => {
            for (key, value) in ours {
                theirs.insert(key, value);
            }
            serde_json::Value::Object(theirs)
        }
        // Absent, or not an object at all: ours is the whole of it.
        (_, mine) => mine,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **This build's keys win, and everything else in the file survives.** Written the other way
    /// round, every save by an older release quietly deleted a newer release's settings.
    #[test]
    fn a_save_keeps_keys_from_a_newer_build() {
        let existing = serde_json::json!({
            "dir_access": "allow",
            "from_the_future": 42,
            "another": {"nested": true},
        });
        let mine = serde_json::json!({"dir_access": "deny", "theme": "dark"});
        let merged = merge(Some(existing), mine);
        assert_eq!(merged["dir_access"], serde_json::json!("deny"), "this build's value must win");
        assert_eq!(merged["theme"], serde_json::json!("dark"));
        assert_eq!(merged["from_the_future"], serde_json::json!(42), "a newer key was dropped");
        assert_eq!(merged["another"], serde_json::json!({"nested": true}));
    }

    /// Nothing on disk, or a file that is not an object: this build's value is the whole of it.
    #[test]
    fn a_missing_or_odd_file_just_takes_this_builds_value() {
        let mine = serde_json::json!({"dir_access": "deny"});
        assert_eq!(merge(None, mine.clone()), mine);
        let merged = merge(Some(serde_json::json!("not an object")), mine.clone());
        assert_eq!(merged, mine);
    }

    /// **A save moves only the key it changed.** Window B's copy still says `allow` after window A
    /// wrote `deny`; B changing its theme must not write its stale `allow` back.
    #[test]
    fn a_save_writes_only_the_key_it_changed() {
        let stale = Config { dir_access: DirAccess::Allow, ..Config::default() };
        let on_disk =
            serde_json::to_value(Config { dir_access: DirAccess::Deny, ..stale }).unwrap();
        let after = Config { theme: ThemeChoice::Dark, ..stale };

        let next = saved_over(Some(on_disk.clone()), &stale, &after).expect("a change to write");
        let read: Config = serde_json::from_value(next).unwrap();
        assert_eq!(read.dir_access, DirAccess::Deny, "the stale value was written back");
        assert_eq!(read.theme, ThemeChoice::Dark);

        // Nothing changed: nothing is written.
        assert_eq!(saved_over(Some(on_disk), &stale, &stale), None);
        // Nothing on disk: the whole copy, as before.
        let fresh = saved_over(None, &stale, &after).unwrap();
        assert_eq!(serde_json::from_value::<Config>(fresh).unwrap(), after);
    }

    /// The gate's copy takes what the screen changed and keeps what it re-read itself.
    #[test]
    fn an_overlay_takes_only_what_changed() {
        let gate = Config { dir_access: DirAccess::Deny, ..Config::default() };
        let before = Config { dir_access: DirAccess::Allow, ..Config::default() };
        let after = Config { theme: ThemeChoice::Light, ..before };
        let now = gate.overlay(&before, &after);
        assert_eq!(now.dir_access, DirAccess::Deny);
        assert_eq!(now.theme, ThemeChoice::Light);
        assert_eq!(gate.overlay(&before, &before), gate);
    }

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
