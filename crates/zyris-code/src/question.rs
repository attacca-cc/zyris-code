//! The `question` tool, in a form a human can pick from.
//!
//! When the agent calls `question`, the `tool_call` event lands on the timeline **before the tool blocks**.
//! So we receive it from the stream and show it on screen; what the human picks is sent as an **ordinary message**,
//! and the server's `question_waiter` takes it as the answer — there is no separate response API.
//!
//! **Always offer a free-text alternative.** The tool description pins down that "the UI always provides a free-text
//! alternative, so the model must not make up its own 'Other' option"; if we leave it out here, the model
//! believes a path exists that actually doesn't.

use std::collections::HashSet;

use serde_json::Value;

use crate::input::Input;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opt {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub header: Option<String>,
    pub question: String,
    pub multi: bool,
    /// When empty, the step only accepts free text.
    pub options: Vec<Opt>,
}

impl Step {
    /// Index of the free-text row. Right below the options.
    pub fn free_row(&self) -> usize {
        self.options.len()
    }
}

/// The action row that stands at the bottom of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Back to the previous question. Not shown on the first question.
    Back,
    /// Answer and move on to the next.
    Next,
    /// Skip without answering. Shown instead of `Next` on a step where nothing is picked yet.
    Skip,
    /// Send it as is.
    Submit,
    /// Go back to the first question and revise.
    Edit,
    /// Declare that you won't answer.
    Reject,
}

impl Act {
    pub fn label(self, lang: crate::lang::Lang) -> &'static str {
        match self {
            Act::Back => lang.action_back(),
            Act::Next => lang.action_next(),
            Act::Skip => lang.action_skip(),
            Act::Submit => lang.action_submit(),
            Act::Edit => lang.action_edit(),
            Act::Reject => lang.action_reject(),
        }
    }
}

/// What a row is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    Option(usize),
    Free,
    Action(Act),
}

/// Reads the steps from the tool arguments. If the shape is off, returns `None` — then it is drawn as an ordinary tool call.
pub fn parse(args: &Value) -> Option<Vec<Step>> {
    let raw = args.get("questions")?.as_array()?;
    if raw.is_empty() {
        return None;
    }
    let steps: Vec<Step> = raw
        .iter()
        .filter_map(|q| {
            let question = q.get("question")?.as_str()?.to_string();
            let options = q
                .get("options")
                .and_then(Value::as_array)
                .map(|opts| {
                    opts.iter()
                        .filter_map(|o| {
                            Some(Opt {
                                label: o.get("label")?.as_str()?.to_string(),
                                description: o
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Step {
                header: q.get("header").and_then(Value::as_str).map(str::to_string),
                question,
                multi: q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false),
                options,
            })
        })
        .collect();
    (!steps.is_empty()).then_some(steps)
}

/// The state of currently answering.
#[derive(Debug)]
pub struct Answering {
    pub steps: Vec<Step>,
    pub step: usize,
    /// The current row. If it's `steps[step].free_row()`, it's the free-text row.
    pub cursor: usize,
    chosen: Vec<HashSet<usize>>,
    free: Vec<String>,
    /// Whether the user is actually typing in the free-text row.
    pub typing: bool,
    pub input: Input,
    /// Whether this is the review screen after all questions are asked.
    review: bool,
}

impl Answering {
    pub fn new(steps: Vec<Step>) -> Self {
        let n = steps.len();
        Self {
            steps,
            step: 0,
            cursor: 0,
            chosen: vec![HashSet::new(); n],
            free: vec![String::new(); n],
            typing: false,
            input: Input::new(),
            review: false,
        }
    }

    pub fn current(&self) -> &Step {
        &self.steps[self.step]
    }

    /// The row layout of the current screen.
    ///
    /// **The question screen and the review screen differ.** While questions are being asked, only back-and-forth moves are allowed,
    /// after all are asked the user picks whether to send, fix, or reject — if Submit were mixed in while answering,
    /// it would be easy to send while leaving unseen questions behind.
    pub fn rows(&self) -> Vec<RowKind> {
        if self.review {
            return vec![
                RowKind::Action(Act::Submit),
                RowKind::Action(Act::Edit),
                RowKind::Action(Act::Reject),
            ];
        }
        let step = self.current();
        let mut rows: Vec<RowKind> = (0..step.options.len()).map(RowKind::Option).collect();
        rows.push(RowKind::Free);
        if self.step > 0 {
            rows.push(RowKind::Action(Act::Back));
        }
        // If nothing is picked yet, skip; if something is, next.
        rows.push(RowKind::Action(if self.answered() { Act::Next } else { Act::Skip }));
        rows
    }

    /// Whether we're past the last question and on the review screen.
    pub fn in_review(&self) -> bool {
        self.review
    }

    /// Go to the review screen. Pressing next/skip on the last question lands here.
    pub fn to_review(&mut self) {
        self.review = true;
        self.cursor = 0;
    }

    /// Go back to the first question and revise.
    pub fn to_edit(&mut self) {
        self.review = false;
        self.step = 0;
        self.cursor = 0;
    }

    /// Everything answered so far, made human-readable. The review screen shows it.
    pub fn summary(&self, lang: crate::lang::Lang) -> Vec<(String, String)> {
        self.steps
            .iter()
            .enumerate()
            .map(|(i, step)| {
                let mut picks: Vec<String> = {
                    let mut v: Vec<usize> = self.chosen[i].iter().copied().collect();
                    v.sort_unstable();
                    v.iter().filter_map(|&j| step.options.get(j)).map(|o| o.label.clone()).collect()
                };
                let free = self.free[i].trim();
                if !free.is_empty() {
                    picks.push(format!("✎ {free}"));
                }
                let answer =
                    if picks.is_empty() { lang.skipped().to_string() } else { picks.join(", ") };
                (step.question.clone(), answer)
            })
            .collect()
    }

    pub fn row_at(&self, i: usize) -> Option<RowKind> {
        self.rows().get(i).cloned()
    }

    pub fn is_free_row(&self) -> bool {
        matches!(self.row_at(self.cursor), Some(RowKind::Free))
    }

    pub fn is_chosen(&self, i: usize) -> bool {
        self.chosen[self.step].contains(&i)
    }

    pub fn up(&mut self) {
        if self.typing {
            return;
        }
        let n = self.rows().len();
        self.cursor = (self.cursor + n - 1) % n;
    }

    pub fn down(&mut self) {
        if self.typing {
            return;
        }
        self.cursor = (self.cursor + 1) % self.rows().len();
    }

    /// What was typed directly into this step. The screen shows it.
    pub fn free_text(&self) -> &str {
        &self.free[self.step]
    }

    /// Pick or unpick. For single-select, it replaces the previous pick.
    pub fn toggle(&mut self) {
        match self.row_at(self.cursor) {
            Some(RowKind::Free) => {
                // If something was typed before, put it back so it can be continued and fixed.
                self.input = Input::new();
                self.input.insert_str(&self.free[self.step].clone());
                self.typing = true;
            }
            Some(RowKind::Option(i)) => {
                let multi = self.current().multi;
                let set = &mut self.chosen[self.step];
                if multi {
                    if !set.remove(&i) {
                        set.insert(i);
                    }
                } else {
                    set.clear();
                    set.insert(i);
                }
            }
            _ => {}
        }
    }

    /// Back to the previous question. On the first question, nothing happens.
    pub fn back(&mut self) {
        if self.review {
            self.review = false;
            self.cursor = 0;
        } else if self.step > 0 {
            self.step -= 1;
            self.cursor = 0;
        }
    }

    /// Move on. On the last question, it moves to the review screen.
    ///
    /// It moves on even without an answer — that's what skipping is. Forcing an answer means the asker
    /// doesn't get an answer; the human just picks anything.
    pub fn advance(&mut self) {
        if self.step + 1 < self.steps.len() {
            self.step += 1;
            self.cursor = 0;
        } else {
            self.to_review();
        }
    }

    /// Whether anything has been answered. It's the criterion for being able to submit.
    pub fn any_answered(&self) -> bool {
        (0..self.steps.len()).any(|i| !self.chosen[i].is_empty() || !self.free[i].trim().is_empty())
    }

    /// Whether this step has an answer. Without one, it can't move to the next.
    pub fn answered(&self) -> bool {
        !self.chosen[self.step].is_empty() || !self.free[self.step].trim().is_empty()
    }

    /// Confirm. If there's a next step, go there; if it's the last, return `true`.
    pub fn confirm(&mut self) -> bool {
        if self.typing {
            // Finishing the typing comes first.
            self.free[self.step] = self.input.take();
            self.typing = false;
            return false;
        }
        if !self.answered() {
            return false;
        }
        if self.step + 1 < self.steps.len() {
            self.step += 1;
            self.cursor = 0;
            return false;
        }
        true
    }

    /// The answer to send to the server. Free-form text — the waiter uses the next message verbatim as the answer.
    ///
    /// **Don't just throw the labels.** The model must reconstruct what was asked from the answer alone, so the question is
    /// carried verbatim and even the descriptions of picked items are attached. Free text is flagged as such — the fact that
    /// the answer wasn't among the options is itself information.
    pub fn answer_text(&self, lang: crate::lang::Lang) -> String {
        let mut out = Vec::new();
        for (i, step) in self.steps.iter().enumerate() {
            let mut picked: Vec<usize> = self.chosen[i].iter().copied().collect();
            picked.sort_unstable();

            let mut parts: Vec<String> = picked
                .iter()
                .filter_map(|&j| step.options.get(j))
                .map(|o| match &o.description {
                    // If there's a description, carry it along — the model can see why it was picked.
                    Some(d) if !d.is_empty() => format!("{} ({})", o.label, d),
                    _ => o.label.clone(),
                })
                .collect();

            let free = self.free[i].trim();
            if !free.is_empty() {
                parts.push(format!("{} {free}", lang.free_mark()));
            }
            if parts.is_empty() {
                continue;
            }

            let head = match &step.header {
                Some(h) if !h.is_empty() => format!("[{h}] {}", step.question),
                _ => step.question.clone(),
            };
            let body = if parts.len() > 1 {
                // A step with several picks is written one per line. Joined with commas, it blurs where
                // one item ends.
                parts.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n")
            } else {
                format!("  {}", parts[0])
            };
            out.push(format!("{head}\n{body}"));
        }
        out.join("\n\n")
    }

    /// Answers the user **typed directly**. Pulled out separately to be shown differently in the history.
    pub fn free_answers(&self) -> Vec<String> {
        self.free.iter().map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args() -> Value {
        json!({"questions": [
            {"header": "방식", "question": "어느 쪽으로 갈까요?", "options": [
                {"label": "A안", "description": "빠르다"},
                {"label": "B안"}
            ]},
            {"question": "무엇을 켤까요?", "multiSelect": true, "options": [
                {"label": "로그"}, {"label": "지표"}, {"label": "추적"}
            ]}
        ]})
    }

    #[test]
    fn parsing_reads_every_step_and_option() {
        let steps = parse(&args()).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].header.as_deref(), Some("방식"));
        assert_eq!(steps[0].options[0].description.as_deref(), Some("빠르다"));
        assert!(!steps[0].multi);
        assert!(steps[1].multi);
    }

    /// Without options, the step only accepts free text.
    #[test]
    fn a_step_without_options_is_free_text_only() {
        let steps = parse(&json!({"questions": [{"question": "이름이 뭐예요?"}]})).unwrap();
        assert!(steps[0].options.is_empty());
        assert_eq!(steps[0].free_row(), 0, "자유 입력이 첫 줄이다");
    }

    #[test]
    fn a_malformed_payload_is_not_a_question() {
        assert!(parse(&json!({})).is_none());
        assert!(parse(&json!({"questions": []})).is_none());
    }

    /// There is always one more free-text row at the bottom — the tool promises it.
    #[test]
    fn there_is_always_a_free_text_row_below_the_options() {
        let a = Answering::new(parse(&args()).unwrap());
        assert_eq!(a.current().free_row(), 2, "선택지 2 아래가 자유 입력");
        assert!(a.rows().iter().any(|r| matches!(r, RowKind::Free)));
    }

    /// The action rows depend on the situation. Showing something that isn't there makes the user press it and think it's broken.
    #[test]
    fn the_action_rows_depend_on_where_we_are() {
        let mut a = Answering::new(parse(&args()).unwrap());

        let first: Vec<RowKind> = a.rows();
        assert!(!first.contains(&RowKind::Action(Act::Back)), "첫 질문에는 이전이 없다");
        assert!(first.contains(&RowKind::Action(Act::Skip)), "안 골랐으니 건너뛰기다");
        // **There is no Submit on the question screen.** It's easy to send while leaving unseen questions behind.
        assert!(!first.contains(&RowKind::Action(Act::Submit)), "질문 화면에 제출이 있다");

        a.toggle();
        assert!(a.rows().contains(&RowKind::Action(Act::Next)), "고르면 다음이 된다");
        a.advance();
        assert!(a.rows().contains(&RowKind::Action(Act::Back)), "둘째 질문에는 이전이 있다");

        // Past the last question is the review screen, and only there is there a Submit.
        a.advance();
        assert!(a.in_review());
        let review: Vec<RowKind> = a.rows();
        assert!(review.contains(&RowKind::Action(Act::Submit)));
        assert!(review.contains(&RowKind::Action(Act::Edit)));
        assert!(review.contains(&RowKind::Action(Act::Reject)));
    }

    #[test]
    fn back_returns_to_the_previous_step_keeping_its_answer() {
        let mut a = Answering::new(parse(&args()).unwrap());
        a.toggle();
        a.advance();
        a.back();
        assert_eq!(a.step, 0);
        assert!(a.is_chosen(0), "앞 답이 남아 있어야 한다");
    }

    /// **It moves on even without an answer** — that's what skipping is. Forcing an answer means the asker
    /// doesn't get an answer; the human just picks anything.
    #[test]
    fn an_unanswered_step_can_be_skipped() {
        let mut a = Answering::new(parse(&args()).unwrap());
        assert!(a.rows().contains(&RowKind::Action(Act::Skip)), "건너뛰기가 있어야 한다");
        a.advance();
        assert_eq!(a.step, 1, "건너뛰고 다음으로 갔어야 한다");
    }

    /// A skipped step is marked as such in the summary.
    #[test]
    fn a_skipped_step_is_marked_in_the_summary() {
        let mut a = Answering::new(parse(&args()).unwrap());
        a.advance();
        a.toggle();
        a.advance();
        assert!(a.in_review());
        let s = a.summary(crate::lang::Lang::Ko);
        assert_eq!(s[0].1, "건너뜀");
        assert_ne!(s[1].1, "건너뜀");
    }

    /// Pressing Edit on the review screen returns to the first question.
    #[test]
    fn editing_returns_to_the_first_question() {
        let mut a = Answering::new(parse(&args()).unwrap());
        a.toggle();
        a.advance();
        a.advance();
        assert!(a.in_review());
        a.to_edit();
        assert!(!a.in_review());
        assert_eq!(a.step, 0);
        assert!(a.is_chosen(0), "고른 것은 남아 있어야 한다");
    }

    #[test]
    fn single_select_replaces_the_previous_pick() {
        let mut a = Answering::new(parse(&args()).unwrap());
        a.toggle();
        assert!(a.is_chosen(0));
        a.down();
        a.toggle();
        assert!(!a.is_chosen(0), "단일 선택은 앞의 것을 대신한다");
        assert!(a.is_chosen(1));
    }

    #[test]
    fn multi_select_keeps_both() {
        let mut a = Answering::new(parse(&args()).unwrap());
        a.toggle();
        assert!(!a.confirm(), "첫 단계 확정은 다음으로 넘어간다");
        assert_eq!(a.step, 1);

        a.toggle();
        a.down();
        a.toggle();
        assert!(a.is_chosen(0) && a.is_chosen(1), "다중 선택은 둘 다 남는다");
    }

    #[test]
    fn moving_wraps_around() {
        let mut a = Answering::new(parse(&args()).unwrap());
        let last = a.rows().len() - 1;
        a.up();
        assert_eq!(a.cursor, last, "위로 가면 맨 아래로 돈다");
        assert_eq!(a.row_at(a.cursor), Some(RowKind::Action(Act::Skip)), "맨 아래는 건너뛰기");
        a.down();
        assert_eq!(a.cursor, 0);
    }

    /// You can't move on without picking anything.
    #[test]
    fn confirming_without_an_answer_does_nothing() {
        let mut a = Answering::new(parse(&args()).unwrap());
        assert!(!a.confirm());
        assert_eq!(a.step, 0, "단계가 넘어가면 안 된다");
    }

    #[test]
    fn the_free_text_row_starts_typing_and_confirm_stores_it() {
        let mut a = Answering::new(parse(&args()).unwrap());
        a.cursor = a.current().free_row();
        a.toggle();
        assert!(a.typing);

        for c in "직접 쓴 답".chars() {
            a.input.insert(c);
        }
        assert!(!a.confirm(), "타자를 끝내는 것이 먼저다");
        assert!(!a.typing);
        assert!(a.answered(), "자유 입력도 답이다");
    }

    #[test]
    fn the_answer_text_lists_one_line_per_step() {
        let mut a = Answering::new(parse(&args()).unwrap());
        a.toggle(); // Option A
        a.confirm();
        a.toggle(); // Log
        a.down();
        a.toggle(); // Metrics
        assert!(a.confirm(), "마지막 단계 확정이면 끝이다");

        let text = a.answer_text(crate::lang::Lang::Ko);
        assert!(text.contains("[방식] 어느 쪽으로 갈까요?"), "질문을 그대로 실어야 한다:\n{text}");
        assert!(text.contains("A안 (빠르다)"), "설명까지 실어야 한다:\n{text}");
        assert!(text.contains("- 로그"), "여러 개는 줄로 나눠야 한다:\n{text}");
        assert!(text.contains("- 지표"), "{text}");
    }

    #[test]
    fn a_free_text_answer_appears_in_the_text() {
        let mut a = Answering::new(parse(&json!({"questions": [{"question": "왜죠?"}]})).unwrap());
        a.toggle();
        for c in "그냥요".chars() {
            a.input.insert(c);
        }
        a.confirm();
        let text = a.answer_text(crate::lang::Lang::Ko);
        assert!(text.contains("직접 입력: 그냥요"), "직접 쓴 것임을 밝혀야 한다: {text}");
        assert_eq!(a.free_answers(), vec!["그냥요".to_string()]);
    }
}
