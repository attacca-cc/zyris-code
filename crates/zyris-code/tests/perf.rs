//! Measures what it costs to draw one frame.
//!
//! "A long answer breaks the screen" and "a long conversation lags" look like the same cause —
//! rebuilding the **whole** conversation every frame. Before fixing it, we measure whether that's actually true.
//!
//! ```bash
//! cargo test -j2 -p zyris-code --test perf -- --nocapture --ignored
//! ```

use std::time::Instant;

use zyris_code::event::{Entry, EntryKind};
use zyris_code::rows::{rows, Cache, Folds};
use zyris_code::timeline::Timeline;

/// Width. Chosen to be close to a real terminal.
const WIDTH: u16 = 100;

/// An answer holding one 300-row table. This is the size that actually broke before.
fn long_table(rows: usize) -> String {
    let mut s = String::from("아래가 결과입니다.\n\n| 경로 | 크기 | 비고 |\n|---|---|---|\n");
    for i in 0..rows {
        s.push_str(&format!(
            "| chrome-profile/Default/chrome_cart_db_{i} | {}KB | 캐시 항목 {i} |\n",
            i * 7 % 900
        ));
    }
    s
}

/// A conversation of `turns` turns. One turn is one user message + a work card + a long answer.
fn conversation(turns: usize, table_rows: usize) -> Timeline {
    let mut t = Timeline::new();
    let mut seq = 0i64;
    for i in 0..turns {
        seq += 1;
        t.upsert(Entry { seq, kind: EntryKind::User(format!("{i}번째 질문입니다")) });
        seq += 1;
        t.upsert(Entry { seq, kind: EntryKind::WorkStart(format!("{i}번째 작업")) });
        seq += 1;
        t.upsert(Entry { seq, kind: EntryKind::Thinking("무엇부터 볼까".repeat(20)) });
        seq += 1;
        t.upsert(Entry { seq, kind: EntryKind::Agent(long_table(table_rows)) });
    }
    t
}

/// Terminal height. This is all the lines that actually appear on screen.
const HEIGHT: usize = 45;

/// The old way — rebuilds everything every frame.
fn frame_all(t: &mut Timeline, folds: &Folds) -> usize {
    rows(t.items(), WIDTH, folds, zyris_code::lang::Lang::En).lines.len()
}

/// The current way — rebuilds only changed items and lays out just the visible window.
fn frame_cached(t: &mut Timeline, cache: &mut Cache, folds: &Folds) -> usize {
    cache.layout(t.items(), WIDTH, folds, None, zyris_code::lang::Lang::En);
    let top = cache.total().saturating_sub(HEIGHT);
    cache.window(top, top + HEIGHT).len()
}

/// Checks whether one frame's cost scales with conversation length. These are eyeball numbers — no assertions.
#[test]
#[ignore = "수치를 보려고 돌리는 것이지 통과/실패를 가리는 것이 아니다"]
fn measure_one_frame() {
    println!("\n턴수 |    줄수 |    예전 | 지금 | 20fps 예산(50ms) 대비");
    println!("-----|---------|---------|------|----------------------");
    for turns in [1usize, 2, 4, 8, 16] {
        let mut t = conversation(turns, 300);
        let folds = Folds::new();
        let lines = frame_all(&mut t, &folds);

        const N: u32 = 5;
        let start = Instant::now();
        for _ in 0..N {
            std::hint::black_box(frame_all(&mut t, &folds));
        }
        let old = start.elapsed() / N;

        let mut cache = Cache::new();
        frame_cached(&mut t, &mut cache, &folds); // warm it up
        let start = Instant::now();
        for _ in 0..N {
            std::hint::black_box(frame_cached(&mut t, &mut cache, &folds));
        }
        let now = start.elapsed() / N;

        println!(
            "{turns:4} | {lines:7} | {:>6.1}ms | {:>4.2}ms | {:>4.0}% → {:.0}%",
            old.as_secs_f64() * 1000.0,
            now.as_secs_f64() * 1000.0,
            old.as_secs_f64() * 1000.0 / 50.0 * 100.0,
            now.as_secs_f64() * 1000.0 / 50.0 * 100.0,
        );
    }
    println!();
}

/// One streaming pass. Each delta draws one frame — exactly how it runs in production.
///
/// This is where the real load is. Even with a long prior conversation, **only the last answer
/// changes**, so the cost must not track the length of the prior conversation.
#[test]
#[ignore = "수치를 보려고 돌리는 것이지 통과/실패를 가리는 것이 아니다"]
fn measure_streaming_deltas() {
    println!("\n앞선 대화 | 델타 하나당 (예전) | 델타 하나당 (지금)");
    println!("----------|--------------------|-------------------");
    let chunk = "| chrome-profile/Default/chrome_cart_db | 42KB | 캐시 항목 |\n";
    for turns in [0usize, 4, 8] {
        let folds = Folds::new();

        let mut t = conversation(turns, 300);
        let start = Instant::now();
        for _ in 0..50 {
            t.push_delta(zyris_attacca::ZDeltaKind::Assistant, chunk);
            std::hint::black_box(frame_all(&mut t, &folds));
        }
        let old = start.elapsed() / 50;

        let mut t = conversation(turns, 300);
        let mut cache = Cache::new();
        let start = Instant::now();
        for _ in 0..50 {
            t.push_delta(zyris_attacca::ZDeltaKind::Assistant, chunk);
            std::hint::black_box(frame_cached(&mut t, &mut cache, &folds));
        }
        let now = start.elapsed() / 50;

        println!(
            "{turns:9} | {:>17.1}ms | {:>16.1}ms",
            old.as_secs_f64() * 1000.0,
            now.as_secs_f64() * 1000.0
        );
    }
    println!();
}

/// Left alone, it should do nothing. Idle frames must not eat CPU.
#[test]
#[ignore = "수치를 보려고 돌리는 것이지 통과/실패를 가리는 것이 아니다"]
fn measure_idle_frames() {
    let mut t = conversation(16, 300);
    let folds = Folds::new();
    let mut cache = Cache::new();
    frame_cached(&mut t, &mut cache, &folds);

    let start = Instant::now();
    for _ in 0..100 {
        std::hint::black_box(frame_cached(&mut t, &mut cache, &folds));
    }
    println!(
        "\n유휴 프레임 100개(16턴 4959줄): {:.1}ms · 하나당 {:.2}ms\n",
        start.elapsed().as_secs_f64() * 1000.0,
        start.elapsed().as_secs_f64() * 1000.0 / 100.0
    );
}

// ─── What goes out on the **wire**, not the screen ──────────────────────────
//
// The answer to "it only breaks over SSH" is not the cost of building frames but the **number of
// bytes going to the terminal**. ratatui sends only changed cells, but when the conversation
// scrolls up one line, nearly every cell on screen changes. Here we actually count those bytes —
// TestBackend doesn't produce bytes, so we attach a real crossterm backend to an in-memory buffer.

use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use zyris_code::app::{apply, Action, Frame as AppFrame, State};
use zyris_code::widgets;

/// Roughly the size of a screen captured from a tablet SSH client.
const W: u16 = 200;
const H: u16 = 40;

/// A write target that counts the bytes that go out. The backend won't hand out its buffer, so we hold one ourselves.
#[derive(Clone, Default)]
struct Wire(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

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
    // There's one reason to use a Fixed viewport — it doesn't ask for a size even without a real tty.
    Terminal::with_options(
        CrosstermBackend::new(wire),
        TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 0, W, H)) },
    )
    .expect("메모리 백엔드")
}

/// Draws once and returns the number of bytes that went on the wire in between.
fn draw_bytes(
    term: &mut Terminal<CrosstermBackend<Wire>>,
    wire: &Wire,
    state: &mut State,
) -> usize {
    wire.take();
    term.draw(|f| widgets::draw(f, state)).unwrap();
    wire.take()
}

#[test]
#[ignore = "수동 측정용"]
fn measure_bytes_on_the_wire() {
    let mut state = State::new();
    state.connected = true;
    state.agent = "Main Agent".into();

    // Stack up some conversation to fill the screen.
    let mut seq = 0i64;
    for i in 0..6 {
        seq += 1;
        let entry = Entry { seq, kind: EntryKind::User(format!("{i}번째 질문입니다")) };
        apply(&mut state, &Action::Frame(AppFrame::Event { cursor: seq, entry: Some(entry) }));
        seq += 1;
        let entry = Entry {
            seq,
            kind: EntryKind::Agent("그라데이션 부분은 이렇게 바꾸면 됩니다. ".repeat(30)),
        };
        apply(&mut state, &Action::Frame(AppFrame::Event { cursor: seq, entry: Some(entry) }));
    }

    let wire = Wire::default();
    let mut term = wire_terminal(wire.clone());
    let first = draw_bytes(&mut term, &wire, &mut state);

    // 1) A frame where nothing changed.
    let idle = draw_bytes(&mut term, &wire, &mut state);

    // 2) A frame with one character typed — only the input field changes.
    apply(&mut state, &Action::Insert('가'));
    let typing = draw_bytes(&mut term, &wire, &mut state);

    // 3) One streaming chunk — the answer grows and **the conversation scrolls up one line**.
    let mut stream = Vec::new();
    for _ in 0..20 {
        apply(
            &mut state,
            &Action::Frame(AppFrame::Delta {
                kind: zyris_attacca::ZDeltaKind::Assistant,
                text: "이어지는 답변입니다. ".into(),
            }),
        );
        stream.push(draw_bytes(&mut term, &wire, &mut state));
    }
    let avg: usize = stream.iter().sum::<usize>() / stream.len();

    // 4) A full redraw — the cost of one self-heal. Same as a fresh terminal's first frame
    //    (`clear()` asks a real tty for the cursor, so we can't use it here).
    let wire2 = Wire::default();
    let mut fresh = wire_terminal(wire2.clone());
    let heal = draw_bytes(&mut fresh, &wire2, &mut state);

    println!("\n화면 {W}×{H} = {}칸", W as usize * H as usize);
    println!("  첫 프레임          {first:>8} B");
    println!("  아무 변화 없음      {idle:>8} B");
    println!("  글자 하나 침        {typing:>8} B");
    println!("  스트리밍 한 조각    {avg:>8} B  (평균, {}개)", stream.len());
    println!("  통째로 다시 그리기  {heal:>8} B");
    println!("\n스트리밍 중 초당:");
    for fps in [10, 20] {
        println!("  {fps:>2}fps  {:>9} B/s", avg * fps);
    }
}

/// **Nothing goes out in the cell after a wide character.**
///
/// This is the structural reason stray glyphs remain on SSH screens. ratatui writes a wide
/// character into one cell and **doesn't touch the next one** — it trusts the terminal to paint
/// both cells. On a terminal that doesn't honor that trust (the kind that draws the glyph in one
/// cell while reserving two), the old glyph in the following cell shows through.
#[test]
#[ignore = "수동 측정용"]
fn measure_what_goes_out_behind_a_wide_character() {
    use ratatui::backend::Backend;
    use ratatui::buffer::Buffer;

    let area = Rect::new(0, 0, 10, 1);
    let mut prev = Buffer::empty(area);
    prev.set_string(0, 0, "abcdefghij", ratatui::style::Style::default());
    let mut next = Buffer::empty(area);
    next.set_string(0, 0, "한글", ratatui::style::Style::default());

    let wire = Wire::default();
    let mut backend = CrosstermBackend::new(wire.clone());
    backend.draw(prev.diff_iter(&next)).unwrap();

    let bytes = wire.0.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&bytes).replace('\x1b', "ESC");
    println!("\n이전: abcdefghij");
    println!("이후: 한글       (뒤 여섯 칸은 공백)");
    println!("선로에 나간 것: {text:?}");
    println!(
        "'b'와 'd' 자리에 무엇이 나갔나: {}",
        if text.contains("한 ") || text.contains("한글") && !text.contains("한 글") {
            "아무것도 — 전각 뒤 칸은 건너뛴다"
        } else {
            "무언가 나갔다"
        }
    );
}
