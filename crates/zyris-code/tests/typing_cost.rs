//! What one frame costs on the wire at **this** machine's size — 211×58 — and what typing costs
//! while a turn is running.
//!
//! ```bash
//! CARGO_TARGET_DIR=... cargo test -j2 -p zyris-code --test typing_cost -- --nocapture --ignored
//! ```

use std::sync::{Arc, Mutex};
use std::time::Instant;

use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use zyris_code::app::{apply, Action, Frame as AppFrame, State};
use zyris_code::event::{Entry, EntryKind};
use zyris_code::widgets;

/// The terminal this was reported from: kitty, 211 columns by 58 rows.
const W: u16 = 211;
const H: u16 = 58;

#[derive(Clone, Default)]
struct Wire(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Wire {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Wire {
    fn take(&self) -> usize {
        let mut b = self.0.lock().unwrap();
        let n = b.len();
        b.clear();
        n
    }
}

fn wire_terminal(wire: Wire) -> Terminal<CrosstermBackend<Wire>> {
    Terminal::with_options(
        CrosstermBackend::new(wire),
        TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 0, W, H)) },
    )
    .expect("in-memory backend")
}

fn draw_bytes(
    term: &mut Terminal<CrosstermBackend<Wire>>,
    wire: &Wire,
    state: &mut State,
) -> (usize, f64) {
    wire.take();
    let start = Instant::now();
    term.draw(|f| widgets::draw(f, state)).unwrap();
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    (wire.take(), ms)
}

fn full_screen() -> State {
    let mut state = State::new();
    state.connected = true;
    state.agent = "Main Agent".into();
    let mut seq = 0i64;
    for i in 0..8 {
        seq += 1;
        apply(
            &mut state,
            &Action::Frame(AppFrame::Event {
                cursor: seq,
                entry: Some(Entry {
                    seq, kind: EntryKind::User(format!("{i}번째 질문입니다"))
                }),
                todo: None,
                plan: None,
            }),
        );
        seq += 1;
        apply(
            &mut state,
            &Action::Frame(AppFrame::Event {
                cursor: seq,
                entry: Some(Entry {
                    seq,
                    kind: EntryKind::Agent("그라데이션 부분은 이렇게 바꾸면 됩니다. ".repeat(30)),
                }),
                todo: None,
                plan: None,
            }),
        );
    }
    state
}

#[test]
#[ignore = "numbers to look at, not a pass/fail"]
fn what_a_frame_costs_at_211x58() {
    let mut state = full_screen();
    let wire = Wire::default();
    let mut term = wire_terminal(wire.clone());

    let (first, ms) = draw_bytes(&mut term, &wire, &mut state);
    println!("\n화면 {W}×{H} = {}칸", W as usize * H as usize);
    println!("  첫 프레임            {first:>8} B  {ms:>6.2}ms");

    // Idle, no turn — nothing at all should go out.
    let (idle, ms) = draw_bytes(&mut term, &wire, &mut state);
    println!("  유휴(턴 없음)         {idle:>8} B  {ms:>6.2}ms");

    // **The case the report is about: a turn is running and nothing changed this tick.**
    // The loop marks `dirty` on every tick while `state.running` (the blinking dot), so this
    // frame goes to the terminal ~60 times a second.
    state.running = true;
    let (running_idle, ms) = draw_bytes(&mut term, &wire, &mut state);
    let (running_idle2, ms2) = draw_bytes(&mut term, &wire, &mut state);
    println!("  유휴(턴 도는 중)      {running_idle:>8} B  {ms:>6.2}ms");
    println!("  유휴(턴 도는 중) 2    {running_idle2:>8} B  {ms2:>6.2}ms");

    // Typing while the turn runs.
    apply(&mut state, &Action::Insert('가'));
    let (typing, ms) = draw_bytes(&mut term, &wire, &mut state);
    println!("  턴 중 글자 하나 침     {typing:>8} B  {ms:>6.2}ms");

    // One streaming chunk.
    apply(
        &mut state,
        &Action::Frame(AppFrame::Delta {
            kind: zyris_attacca::ZDeltaKind::Assistant,
            text: "이어지는 답변입니다. ".into(),
        }),
    );
    let (stream, ms) = draw_bytes(&mut term, &wire, &mut state);
    println!("  스트리밍 한 조각       {stream:>8} B  {ms:>6.2}ms");

    // A run of streaming chunks — the shape of an answer arriving.
    let mut total = 0;
    let mut worst = 0;
    let mut ms_total = 0.0;
    for _ in 0..20 {
        apply(
            &mut state,
            &Action::Frame(AppFrame::Delta {
                kind: zyris_attacca::ZDeltaKind::Assistant,
                text: "이어지는 답변입니다. ".into(),
            }),
        );
        let (n, ms) = draw_bytes(&mut term, &wire, &mut state);
        total += n;
        worst = worst.max(n);
        ms_total += ms;
    }
    println!(
        "  스트리밍 20조각       평균 {:>7} B · 최대 {worst:>7} B · 조각당 {:.2}ms",
        total / 20,
        ms_total / 20.0
    );

    // **A frame where nothing changed but the breath.** The breath is what a turn puts on the
    // screen every tick: the head of the card being worked on fades toward the background and
    // back over 1.6s. Moving the clock forward by one tick is the only way to ask for it, because
    // where it is read from is a clock, not a frame count.
    let mut breath_total = 0;
    let mut breath_ms = 0.0;
    for _ in 0..20 {
        state.breath_origin -= std::time::Duration::from_millis(16);
        let (n, ms) = draw_bytes(&mut term, &wire, &mut state);
        breath_total += n;
        breath_ms += ms;
    }
    println!(
        "  숨쉬기 한 프레임       평균 {:>7} B · 프레임당 {:.2}ms  (60fps로 밀면 {:.0} KB/s)",
        breath_total / 20,
        breath_ms / 20.0,
        (breath_total as f64 / 20.0) * 60.0 / 1024.0
    );
    println!(
        "  → 60fps로 이걸 밀면 {:.0} KB/s, 20fps면 {:.0} KB/s\n",
        (total as f64 / 20.0) * 60.0 / 1024.0,
        (total as f64 / 20.0) * 20.0 / 1024.0
    );
}

/// **How many frames a second the loop pushes while a turn runs.** This is the part the reported
/// flicker is about: the loop marks the frame dirty on every tick while `running`, so an idle
/// screen goes to the terminal at the tick rate for as long as the turn lasts.
#[test]
#[ignore = "numbers to look at, not a pass/fail"]
fn how_many_frames_a_second_while_running() {
    use zyris_code::widgets::activity::blink_on;
    // The blink half-period is a duration, so the dot flips twice an 800ms.
    let flips: Vec<u64> = (0..2000).step_by(10).filter(|ms| blink_on(*ms)).collect();
    println!(
        "\n깜빡임 반주기 이후 2초 동안 점이 켜져 있는 표본 {}개 (10ms 간격 표본 200개 중)",
        flips.len()
    );
    println!("→ 60fps(16ms)로 그리면 2초에 프레임 120개, 그중 화면이 실제로 달라지는 것은 2~3개\n");
}
