//! 한 프레임 그리는 데 드는 비용을 잰다.
//!
//! "답이 길어지면 화면이 깨진다"와 "대화가 길어지면 랙이 걸린다"는 같은 원인으로 보인다 —
//! 매 프레임 대화 **전체**를 다시 만든다는 것. 고치기 전에 그것이 사실인지부터 재 둔다.
//!
//! ```bash
//! cargo test -j2 -p zyris-code --test perf -- --nocapture --ignored
//! ```

use std::time::Instant;

use zyris_code::event::{Entry, EntryKind};
use zyris_code::rows::{rows, Cache, Folds};
use zyris_code::timeline::Timeline;

/// 폭. 실제 터미널과 비슷하게 잡는다.
const WIDTH: u16 = 100;

/// 300줄짜리 표 하나가 든 답변. 실제로 깨졌던 그 크기다.
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

/// 대화 `turns`턴. 한 턴은 사용자 한 마디 + 작업 카드 + 긴 답변이다.
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

/// 터미널 높이. 실제로 화면에 나오는 줄은 이만큼뿐이다.
const HEIGHT: usize = 45;

/// 예전 방식 — 매 프레임 전부 다시 만든다.
fn frame_all(t: &mut Timeline, folds: &Folds) -> usize {
    rows(t.items(), WIDTH, folds).lines.len()
}

/// 지금 방식 — 바뀐 항목만 다시 만들고 보이는 창만 편다.
fn frame_cached(t: &mut Timeline, cache: &mut Cache, folds: &Folds) -> usize {
    cache.layout(t.items(), WIDTH, folds, None);
    let top = cache.total().saturating_sub(HEIGHT);
    cache.window(top, top + HEIGHT).len()
}

/// 한 프레임 비용이 대화 길이에 비례하는지 본다. 눈으로 읽는 수치다 — 단언하지 않는다.
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
        frame_cached(&mut t, &mut cache, &folds); // 데운다
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

/// 스트리밍 한 번. 델타가 올 때마다 한 프레임 그린다 — 실제로 도는 모양 그대로다.
///
/// 여기가 진짜 부하다. 앞선 대화가 길어도 **바뀌는 것은 마지막 답변 하나뿐**이라
/// 비용이 앞선 대화 길이를 따라가면 안 된다.
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

/// 가만히 두면 아무 일도 하지 않아야 한다. 유휴 프레임이 CPU를 먹으면 안 된다.
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

// ─── 화면이 아니라 **선로**에 무엇이 실리는가 ────────────────────────────────
//
// "SSH에서만 깨진다"의 답은 프레임을 만드는 비용이 아니라 **터미널로 나가는 바이트 수**에
// 있다. ratatui는 바뀐 칸만 보내지만, 대화가 한 줄 올라가면 화면의 거의 모든 칸이 바뀐다.
// 여기서는 그 바이트를 실제로 세어 본다 — TestBackend는 바이트를 만들지 않으므로 진짜
// crossterm 백엔드를 메모리 버퍼에 물려 쓴다.

use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use zyris_code::app::{apply, Action, Frame as AppFrame, State};
use zyris_code::widgets;

/// 태블릿 SSH 클라이언트에서 찍힌 화면과 비슷한 크기.
const W: u16 = 200;
const H: u16 = 40;

/// 나간 바이트를 세는 쓰기 대상. 백엔드가 버퍼를 안 내주므로 우리가 쥐고 있는다.
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
    // Fixed 뷰포트를 쓰는 이유는 하나다 — 진짜 tty가 없어도 크기를 물어보지 않는다.
    Terminal::with_options(
        CrosstermBackend::new(wire),
        TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 0, W, H)) },
    )
    .expect("메모리 백엔드")
}

/// 한 번 그리고, 그 사이 선로에 실린 바이트 수를 돌려준다.
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

    // 대화를 좀 쌓아 화면을 채운다.
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

    // 1) 아무것도 안 바뀐 프레임.
    let idle = draw_bytes(&mut term, &wire, &mut state);

    // 2) 글자 하나 친 프레임 — 입력란만 바뀐다.
    apply(&mut state, &Action::Insert('가'));
    let typing = draw_bytes(&mut term, &wire, &mut state);

    // 3) 스트리밍 한 조각 — 답이 자라며 **대화가 한 줄 올라간다**.
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

    // 4) 통째로 다시 그리기 — 자가 치유 한 번의 값. 새 터미널의 첫 프레임과 같다
    //    (`clear()`는 진짜 tty에 커서를 물어봐서 여기서는 못 쓴다).
    let wire2 = Wire::default();
    let mut fresh = wire_terminal(wire2.clone());
    let heal = draw_bytes(&mut fresh, &wire2, &mut state);

    // 5) 빈 칸만 다시 그리기 — 턴 중 자가 치유의 값. 직전 프레임을 일반 diff로
    //    고정해 두고 끼워 넣어 **증분**으로 잰다(새 터미널 첫 프레임은 전부를
    //    내보내므로 비교가 안 된다). 치유 프레임 뒤의 다음 일반 프레임도 잰다 —
    //    prev 버퍼에 AlwaysUpdate가 남아 빈 칸을 한 번 더 내보낸다.
    apply(
        &mut state,
        &Action::Frame(AppFrame::Delta {
            kind: zyris_attacca::ZDeltaKind::Assistant,
            text: "마지막 조각. ".into(),
        }),
    );
    draw_bytes(&mut term, &wire, &mut state); // prev를 일반 프레임으로 고정
    wire.take();
    state.force_update_blank = true;
    term.draw(|f| widgets::draw(f, &mut state)).unwrap();
    let blank_heal = wire.take();
    let blank_next = draw_bytes(&mut term, &wire, &mut state);

    println!("\n화면 {W}×{H} = {}칸", W as usize * H as usize);
    println!("  첫 프레임          {first:>8} B");
    println!("  아무 변화 없음      {idle:>8} B");
    println!("  글자 하나 침        {typing:>8} B");
    println!("  스트리밍 한 조각    {avg:>8} B  (평균, {}개)", stream.len());
    println!("  통째로 다시 그리기  {heal:>8} B");
    println!("  빈 칸만 다시 그리기 {blank_heal:>8} B  (+다음 프레임 {blank_next} B)");
    println!("\n스트리밍 중 초당:");
    for fps in [10, 20] {
        println!("  {fps:>2}fps  {:>9} B/s", avg * fps);
    }
}

/// **전각 글자 뒤 칸에는 아무것도 안 나간다.**
///
/// 이게 SSH 화면에 글자 부스러기가 남는 구조적 이유다. ratatui는 전각 글자를 한 칸에
/// 적고 **그다음 칸은 건드리지 않는다** — 터미널이 두 칸을 다 칠해 준다고 믿기 때문이다.
/// 그 믿음을 지키지 않는 터미널(글자는 한 칸으로 그리면서 두 칸을 잡는 종류)에서는 뒤
/// 칸에 있던 옛 글자가 그대로 비쳐 보인다.
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
