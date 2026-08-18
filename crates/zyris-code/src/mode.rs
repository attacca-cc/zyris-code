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
//! plan       deny until the     create_job{plan_mode} → job session
//!            plan is approved
//! work       pass               create_work → planner session
//! job        pass               create_job  → job session
//! ```
//!
//! **Plan mode is attacca's, not one assembled here** (2026-08-18 user decision). It used to be a
//! plain session that this node simply refused to run writes on, with nothing on the server side
//! knowing a plan was being made — a workflow of our own wearing the name. attacca has the real
//! thing: `plan_mode` seeds the session with guidance to investigate and hand the plan back with
//! `submit_plan`, which parks the turn on the user until they decide.
//!
//! **It has to be a job, because that is the only door the protocol opens.** `ZNewJob` carries
//! `plan_mode`; `ZNewSession` has no such field and attacca's zyris gateway passes `false` for
//! every session a node opens. So plan mode no longer continues the conversation you are in — it
//! opens one — and the note that used to stand here saying `Normal` and `Plan` share a route is
//! what changed. Getting the real thing was worth the switch.
//!
//! `Work`·`Job` were already like this: they **create something new on the server**, once, from
//! the first message; after that they append to the open session.

use ratatui::style::Color;

use serde::{Deserialize, Serialize};

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Mode {
    /// Runs without asking. The default. Talks in one plain session.
    #[default]
    Normal,
    /// Investigates and hands back a plan to approve before anything is done. attacca's plan mode
    /// (`ZNewJob::plan_mode`), not a local imitation of one.
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
    /// `create_job` with `plan_mode`. The first message becomes the thing to plan, and the agent
    /// hands a plan back rather than doing it.
    ///
    /// **The same call as [`Route::Job`] with one flag flipped**, which is exactly what it is on
    /// the server too — a job whose session was seeded with plan guidance.
    Plan,
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
            Mode::Normal => theme::success(),
            // **Its own colour, not the accent.** Plan mode used to wear `accent()` — the same
            // paint as every box border, every cursor, the input prompt and the user's own
            // message bar — so the one thing the bottom bar exists to say was the colour of the
            // furniture around it.
            Mode::Plan => theme::mode_plan(),
            Mode::Work => theme::tool(),
            Mode::Job => theme::warning(),
        }
    }

    /// Where my words go.
    pub fn route(self) -> Route {
        match self {
            Mode::Normal => Route::Session,
            Mode::Plan => Route::Plan,
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

    /// **Only the plain mode stays in the conversation.** This used to say that plan mode did too,
    /// and that was the shape of the problem: plan mode was a plain session this node happened to
    /// refuse writes on, with nothing on the server knowing a plan was being made. attacca's plan
    /// mode is a flag set when the thing is created, and `ZNewJob` is the only place the protocol
    /// carries it — so asking for a plan opens something, the way asking for work does.
    #[test]
    fn only_the_plain_mode_carries_on_the_conversation() {
        assert_eq!(Mode::Normal.route(), Route::Session);
        for opens_its_own in [Mode::Plan, Mode::Work, Mode::Job] {
            assert_ne!(
                opens_its_own.route(),
                Route::Session,
                "{opens_its_own:?} quietly appended to the conversation instead of opening",
            );
        }
    }

    /// **Plan is a job with one flag flipped**, which is what it is on the server too — a job
    /// whose session was seeded with attacca's plan guidance. They must not be the same route
    /// though, or asking for a plan sets the work going instead.
    #[test]
    fn each_of_the_three_opens_its_own_thing() {
        assert_eq!(Mode::Work.route(), Route::Work);
        assert_eq!(Mode::Job.route(), Route::Job);
        assert_eq!(Mode::Plan.route(), Route::Plan);
        assert_ne!(Route::Plan, Route::Job);
    }
}
