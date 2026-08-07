//! Mode — whether the agent may touch this computer, and where my words go.
//!
//! **It is not sent to the server. There is no reason to send it.** This node is what actually runs
//! tools, so whether to run them is decided here too — attacca doesn't know about this decision and
//! doesn't need to. The decision is made in `tools::gate::decide` and carried to the gate by `tools::bridge`.
//!
//! **The ask mode was removed** (2026-08-02 user decision). Asking for approval on every tool use
//! only broke the flow in practice.
//!
//! The mode decides **two** things. For a long time there was only one; on 2026-08-03 a second one was added.
//!
//! ```text
//!            gate::decide        Route
//!            (run the tool?)    (where my words go)
//! normal     pass               same conversation     ← default
//! plan       deny writes        same conversation
//! work       pass               create_work → planner session
//! job        pass               create_job  → job session
//! ```
//!
//! **The point is that `Normal` and `Plan` share the same route.** So moving between the two
//! doesn't break the conversation, and plan mode can be switched on briefly mid-conversation —
//! that is the only way plan mode is useful. `Work`·`Job`, by contrast, **create something new on
//! the server** — and only once, from the first message; after that they append to an open session.

use ratatui::style::Color;

use serde::{Deserialize, Serialize};

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Mode {
    /// Runs without asking. The default. Talks in one plain session.
    #[default]
    Normal,
    /// Does not run; lays out what to do first.
    Plan,
    /// Opens a work on attacca with the next message as the goal. Two gates and a task graph are attached.
    Work,
    /// Hands off the next message as a task. attacca carries it to the end as one job.
    Job,
}

/// What the next one opens. **The second place where mode gets its meaning.**
///
/// All three end in a **plain session id** (`ZJob::session_id`,
/// `ZWork::planner_session_id`). So the screen, timeline, and folding pass by without knowing what was
/// opened — that is why no drawing code had to be touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Route {
    /// Appends to the current session. Creates one on the spot if there is none.
    #[default]
    Session,
    /// `create_work`. The first message becomes the goal.
    Work,
    /// `create_job`. The first message becomes the task.
    Job,
}

impl Mode {
    /// The name shown on screen. The wording lives in `lang.rs`.
    pub fn label(self, lang: crate::lang::Lang) -> &'static str {
        match self {
            Mode::Normal => lang.mode_normal(),
            Mode::Plan => lang.mode_plan(),
            Mode::Work => lang.mode_work(),
            Mode::Job => lang.mode_job(),
        }
    }

    /// The mode color in the bottom bar.
    ///
    /// **All four have colors.** If the default mode were a dull gray, the whole bottom bar would
    /// read as background and "what mode am I in right now" wouldn't catch the eye — the only colored
    /// thing on that line is the mode, so that is what the eye grabs.
    ///
    /// **Neighbors in the cycle must be far apart in color.** Only one shows at a time, so the
    /// eye compares against the color just before: green → orange → blue → yellow → green.
    pub fn color(self) -> Color {
        match self {
            // The state where tools just run. Green reads as "go ahead".
            Mode::Normal => theme::SUCCESS,
            Mode::Plan => theme::ACCENT,
            Mode::Work => theme::TOOL,
            Mode::Job => theme::WARNING,
        }
    }

    /// Where my words go.
    pub fn route(self) -> Route {
        match self {
            Mode::Normal | Mode::Plan => Route::Session,
            Mode::Work => Route::Work,
            Mode::Job => Route::Job,
        }
    }

    /// The order Shift+Tab cycles through.
    ///
    /// **There is a line between the ones here and the ones not here.** The first two apply to the
    /// current conversation (they don't touch the session); the last two create something new on the
    /// server. The order crosses the line in one direction only once, so an overshoot just needs one more lap.
    pub fn next(self) -> Mode {
        match self {
            Mode::Normal => Mode::Plan,
            Mode::Plan => Mode::Work,
            Mode::Work => Mode::Job,
            Mode::Job => Mode::Normal,
        }
    }

    /// All four. Used by tests and the `/mode` listing — **counted in one place so no mode gets missed.**
    pub const ALL: [Mode; 4] = [Mode::Normal, Mode::Plan, Mode::Work, Mode::Job];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_mode_is_normal() {
        assert_eq!(Mode::default(), Mode::Normal);
        assert_eq!(Mode::default().route(), Route::Session);
    }

    /// One lap comes back to the start. **If you add a mode and don't fix `next`, this catches it.**
    #[test]
    fn cycling_through_every_mode_comes_back() {
        let mut seen = vec![Mode::default()];
        let mut m = Mode::default();
        for _ in 0..Mode::ALL.len() - 1 {
            m = m.next();
            assert!(
                !seen.contains(&m),
                "{m:?} is visited twice — one lap is shorter than the number of modes"
            );
            seen.push(m);
        }
        assert_eq!(m.next(), Mode::default(), "one full lap returns to normal");
        assert_eq!(seen.len(), Mode::ALL.len(), "the cycle must pass through every mode");
    }

    #[test]
    fn every_mode_has_a_label_and_a_colour() {
        for m in Mode::ALL {
            for lang in [crate::lang::Lang::Ko, crate::lang::Lang::En] {
                assert!(!m.label(lang).is_empty(), "{m:?} has an empty name");
            }
            let _ = m.color();
        }
    }

    /// If colors collide, the bottom bar alone doesn't tell which mode it is.
    #[test]
    fn no_two_modes_share_a_colour() {
        for (i, a) in Mode::ALL.iter().enumerate() {
            for b in &Mode::ALL[i + 1..] {
                assert_ne!(a.color(), b.color(), "{a:?} and {b:?} share a colour");
            }
        }
    }

    /// **Normal and plan go to the same place.** If this breaks, turning on plan mode mid-conversation
    /// cuts off what was being said.
    #[test]
    fn normal_and_plan_go_to_the_same_place() {
        assert_eq!(Mode::Normal.route(), Route::Session);
        assert_eq!(Mode::Plan.route(), Route::Session);
    }

    #[test]
    fn work_and_job_each_open_their_own_thing() {
        assert_eq!(Mode::Work.route(), Route::Work);
        assert_eq!(Mode::Job.route(), Route::Job);
    }
}
