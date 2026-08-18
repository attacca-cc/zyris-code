//! One turn with no screen: send a prompt, print what the agent says, exit.
//!
//! **The node is still announced.** Nothing here is a read-only query — the same capabilities the
//! screen offers are on the wire, so the agent can read and change files here and run commands.
//! That is the difference between this and asking a hosted model a question, and it is why the
//! help text says so out loud rather than leaving it to be discovered.
//!
//! **Only the answer goes to stdout.** Tool calls, reasoning and status are not printed at all, so
//! the output pipes into something else without being filtered first. Logs already go to a file,
//! and the shell notice falls silent once anything has been printed.
//!
//! **A question is answered rather than waited on.** The agent can call `question`, and the turn
//! then blocks until an ordinary message arrives as the reply. With the screen up a person
//! supplies it; here there is nobody, and the turn used to sit there for ever — no output, no
//! exit, a script hung on a decision nobody could see it was waiting for. So a question that
//! arrives with nothing in front of it is shown on stderr, told so, and the run ends unsuccessful.

use std::collections::HashSet;
use std::io::Write;
use std::sync::Arc;

use futures_util::StreamExt;
use zyris_attacca::{AttaccaApi, AttaccaApiClient, ZDeltaKind};

use crate::app::Frame;
use crate::conn::{self, Session};

/// Reads the prompt from stdin, for `zyris -p` with nothing after it.
pub fn prompt_from_stdin() -> anyhow::Result<String> {
    use std::io::Read;
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    let text = text.trim().to_string();
    anyhow::ensure!(!text.is_empty(), "no prompt: give one after -p, or pipe it in");
    Ok(text)
}

/// Sends `prompt` and prints the answer as it arrives.
///
/// Returns once the turn ends. The caller keeps the runner alive around this, so a tool call
/// arriving mid-turn is served the same way it would be with the screen up.
pub async fn run(
    api: Arc<AttaccaApiClient>,
    bridge: crate::tools::bridge::Bridge,
    prompt: &str,
) -> anyhow::Result<()> {
    let agent_id = Session::agent_id(&api)
        .await
        .map_err(|e| anyhow::anyhow!("could not find the agent: {e}"))?;

    // The preamble carries the skill list and this directory's instruction files, exactly as it
    // does for the screen — an agent that behaves differently here would be a second product.
    let mut session = Session::new(bridge.preamble());
    let opened = session
        .open_for(&api, &agent_id, prompt, crate::mode::Mode::Normal)
        .await
        .map_err(|e| anyhow::anyhow!("could not open a thread: {e}"))?;

    // **Opening can carry the first message.** Sending it again would land as a second instruction
    // and be answered twice — the same trap the screen has in `Opened::sent`.
    if !opened.sent {
        conn::within(&api, api.send_message(opened.id.clone(), prompt.to_string(), vec![]))
            .await
            .map_err(|e| anyhow::anyhow!("could not send: {e}"))?;
    }

    let mut stream = conn::within(&api, api.turn_events(opened.id.clone(), None))
        .await
        .map_err(|e| anyhow::anyhow!("could not follow the turn: {e}"))?;

    let mut out = std::io::stdout();
    // **Whether anything was streamed decides what the durable event is for.** The agent's words
    // arrive twice: as deltas while they are produced, and again as the settled event. Printing
    // both would say everything twice; printing only the settled one would sit silent through a
    // long turn and then produce a wall. So deltas are printed live, and the settled text is the
    // fallback for a deployment that sends no deltas at all.
    let mut streamed = false;
    let mut settled = String::new();
    // **The stream does not end when the turn does.** `turn_events` is a subscription to the
    // session, so it stays open for whatever comes next — with the screen up that is the point,
    // and here it means waiting for the stream to close would wait for ever. The turn's own status
    // is the end: `running` going false, once it has been seen true.
    //
    // Seen true is what makes it safe. Subscribing can land before the turn is under way, and the
    // `false` that arrives then means "not started", not "finished" — stopping on it would print
    // nothing and call that success.
    let mut started = stream.head.running;
    // **Answered once each, keyed by `seq`.** A question's event is rewritten in place as it is
    // answered, so the same one comes round again — replying twice would put a second message into
    // the thread and the agent would read it as a new instruction.
    let mut answered_questions: HashSet<i64> = HashSet::new();

    while let Some(frame) = stream.items.next().await {
        let frame = match frame {
            Ok(f) => conn::frame_from(f),
            // A dropped stream mid-turn is not a silent success. What was printed stays printed.
            Err(e) => anyhow::bail!("the turn stream dropped: {e}"),
        };
        match frame {
            Frame::Status { running } => {
                if running {
                    started = true;
                } else if started {
                    break;
                }
            }
            Frame::Delta { kind: ZDeltaKind::Assistant, text } => {
                streamed = true;
                started = true;
                write!(out, "{text}")?;
                // **Flushed as it goes.** stdout to a pipe is block-buffered, so without this a
                // reader on the other end sees nothing until the turn ends.
                out.flush()?;
            }
            Frame::Event { entry: Some(entry), .. } => {
                // Anything at all on the wire means the turn is under way — a deployment that
                // never sends a running status must still be able to finish.
                started = true;
                let seq = entry.seq;
                match entry.kind {
                    crate::event::EntryKind::Agent(text) => settled = text,
                    // Unanswered, and nobody here to answer it.
                    crate::event::EntryKind::Question { steps, answered: false }
                        if answered_questions.insert(seq) =>
                    {
                        let lang = crate::lang::current();
                        // Straight to stderr, and flushed with it: stdout is being written to at
                        // the same time and belongs to the answer alone.
                        eprintln!("{}", lang.question_unattended_notice());
                        for step in &steps {
                            eprintln!("  {}", describe(step));
                        }
                        // The reply is an ordinary message — the server's question waiter takes
                        // the next one as the answer. There is no separate response API.
                        conn::within(
                            &api,
                            api.send_message(
                                opened.id.clone(),
                                lang.question_unattended().to_string(),
                                vec![],
                            ),
                        )
                        .await
                        .map_err(|e| anyhow::anyhow!("could not decline the question: {e}"))?;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if !streamed && !settled.is_empty() {
        write!(out, "{settled}")?;
    }
    // One newline at the end, so a shell prompt does not land on the last line of the answer.
    writeln!(out)?;
    out.flush()?;

    // **Unsuccessful, even though the turn finished and said something.** What came back was
    // produced without a decision that was asked for, and a script cannot tell that from the text.
    // The answer stays on stdout either way — it is usually the explanation of what was needed.
    anyhow::ensure!(
        answered_questions.is_empty(),
        "the agent asked something and there was no screen to answer it on; \
         it was told so and the turn went on without the answer"
    );
    Ok(())
}

/// One question on one line: what was asked, and what it offered to choose from.
///
/// **The options are named.** Without them the line says a decision was wanted but not which
/// decisions were on the table, which is exactly what somebody re-running this with the answer
/// baked into the prompt needs to know.
fn describe(step: &crate::question::Step) -> String {
    let asked = match &step.header {
        Some(header) if !header.is_empty() => format!("[{header}] {}", step.question),
        _ => step.question.clone(),
    };
    if step.options.is_empty() {
        return asked;
    }
    let offered: Vec<&str> = step.options.iter().map(|o| o.label.as_str()).collect();
    format!("{asked} ({})", offered.join(" / "))
}

#[cfg(test)]
mod tests {
    use super::describe;
    use crate::question::{Opt, Step};

    fn step(header: Option<&str>, question: &str, options: &[&str]) -> Step {
        Step {
            header: header.map(str::to_string),
            question: question.to_string(),
            multi: false,
            options: options
                .iter()
                .map(|label| Opt { label: (*label).to_string(), description: None })
                .collect(),
        }
    }

    /// **The options are the useful half.** Somebody reading this in a script's log is deciding
    /// what to bake into the prompt so the run does not stop again, and they cannot do that from
    /// the question alone.
    #[test]
    fn a_question_is_reported_with_what_it_offered() {
        let said = describe(&step(Some("File"), "It already exists. Replace it?", &["yes", "no"]));
        assert_eq!(said, "[File] It already exists. Replace it? (yes / no)");
    }

    /// A free-text step declares no options — the tool's own contract — and inventing an empty
    /// pair of brackets for it would read as "it offered nothing", which is the opposite.
    #[test]
    fn a_question_with_nothing_to_pick_from_is_just_the_question() {
        assert_eq!(
            describe(&step(None, "What should it be called?", &[])),
            "What should it be called?"
        );
    }

    /// An absent header and a header set to nothing are the same thing to a reader, so neither
    /// gets brackets. The server sends the empty one.
    #[test]
    fn an_empty_header_is_no_header() {
        assert_eq!(describe(&step(Some(""), "Which one?", &["a"])), "Which one? (a)");
    }
}
