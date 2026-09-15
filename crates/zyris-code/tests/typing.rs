//! What Enter does when a character lands in the same read as it — the input method's commit —
//! and what one keystroke costs on the wire.
//!
//! ```bash
//! cargo test -j2 -p zyris-code --test typing -- --nocapture --ignored
//! ```

use std::io::{Read, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize};

struct Session {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output: mpsc::Receiver<Vec<u8>>,
    seen: Vec<u8>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl Session {
    fn start() -> Session {
        let pty = portable_pty::native_pty_system()
            .openpty(PtySize { rows: 30, cols: 100, pixel_width: 0, pixel_height: 0 })
            .expect("pty");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_zyris-code"));
        cmd.env("ZYRIS_NODE_TOKEN", "znt_typing_probe_not_a_real_token");
        cmd.env("ZYRIS_SERVER_URL", "ws://127.0.0.1:1");
        let dir = std::env::temp_dir().join(format!("zyris-typing-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        cmd.env("ZYRIS_CONFIG_DIR", &dir);
        cmd.env("ZYRIS_CODE_HEAL_MS", "0");
        cmd.env("ZYRIS_CODE_GIT_MS", "0");
        cmd.env("ZYRIS_CODE_LOG", dir.join("log").to_string_lossy().to_string());
        let child = pty.slave.spawn_command(cmd).expect("spawn");
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().expect("reader");
        let writer = Arc::new(Mutex::new(pty.master.take_writer().expect("writer")));
        let (tx, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Session { child, writer, output, seen: Vec::new(), _master: pty.master }
    }

    fn send(&mut self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.seen).into_owned()
    }

    fn wait_until(&mut self, patience: Duration, done: impl Fn(&Session) -> bool) -> bool {
        let deadline = Instant::now() + patience;
        loop {
            if done(self) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            match self.output.recv_timeout(left.min(Duration::from_millis(100))) {
                Ok(chunk) => self.seen.extend_from_slice(&chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return done(self),
            }
        }
    }

    /// Everything said until the app has been quiet for `quiet`.
    fn collect(&mut self, quiet: Duration, patience: Duration) -> Vec<u8> {
        let deadline = Instant::now() + patience;
        let mut out = Vec::new();
        loop {
            if Instant::now() >= deadline {
                return out;
            }
            match self.output.recv_timeout(quiet) {
                Ok(chunk) => {
                    self.seen.extend_from_slice(&chunk);
                    out.extend_from_slice(&chunk);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return out,
                Err(mpsc::RecvTimeoutError::Disconnected) => return out,
            }
        }
    }

    fn wait_ready(&mut self) {
        let ok = self.wait_until(Duration::from_secs(20), |s| {
            let t = s.text();
            t.contains("normal") || t.contains("일반")
        });
        assert!(ok, "the bottom bar never appeared:\n{}", self.text());
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How many rows the frame touched and how many bytes it took.
fn describe(label: &str, blob: &[u8]) {
    let mut rows = std::collections::BTreeSet::new();
    let mut i = 0;
    while i < blob.len() {
        if blob[i] == 0x1b && blob.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < blob.len() && !(blob[j] >= 0x40 && blob[j] <= 0x7e) {
                j += 1;
            }
            let args = &blob[i + 2..j.min(blob.len())];
            if j < blob.len() && (blob[j] == b'H' || blob[j] == b'f') {
                if let Some(first) = args.split(|b| *b == b';').next() {
                    if let Ok(n) = std::str::from_utf8(first).unwrap_or("").parse::<u32>() {
                        rows.insert(n);
                    }
                }
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    println!("{label:<34} bytes={:>7}  rows-touched={:>3}", blob.len(), rows.len());
}

/// **What a keystroke and an Enter cost on a real pty** — the shape of the answer, not a
/// pass/fail.
///
/// **What this does *not* reach.** With the server refusing, the app never leaves its
/// pre-connection loop — the one that waits for the first connection — and that loop calls
/// `on_key` directly: the paste-burst rule (`PasteBurst`) is not on that path at all. So nothing
/// here says anything about `enter_becomes_newline`; the Enter cases are locked in `app::tests`,
/// where the decision is a pure function. What this shows is that the *screen* takes an Enter in
/// one write with a character without a newline appearing in the draft — and the byte cost of a
/// tick at the geometry the two reports came from.
#[test]
#[ignore = "numbers to look at, not a pass/fail"]
fn an_enter_in_the_same_read_as_a_character() {
    let mut app = Session::start();
    app.wait_ready();
    app.collect(Duration::from_millis(400), Duration::from_secs(2));

    // `hi` and the Enter in one write: a commit and an Enter with no gap.
    app.send(b"hi\r");
    describe("hi + Enter in one write", &app.collect(Duration::from_millis(600), Duration::from_secs(3)));

    // What a paste looks like to the pty: one write, many keys, an Enter inside it.
    app.send(b"aaaaaa\rbbbbbb");
    describe("paste with an Enter inside", &app.collect(Duration::from_millis(600), Duration::from_secs(3)));

    // And one keystroke at a time, for scale.
    app.send(b"x");
    describe("one keystroke", &app.collect(Duration::from_millis(250), Duration::from_secs(2)));

    println!("\n--- the last screen, escaped (tail) ---");
    let all = app.text();
    let bytes = all.as_bytes();
    let tail = &bytes[bytes.len().saturating_sub(800)..];
    let mut escaped = String::new();
    for &b in tail {
        match b {
            0x1b => escaped.push_str("\\e"),
            b'\r' => escaped.push_str("\\r"),
            b'\n' => escaped.push_str("\\n\n"),
            b if b < 0x20 => escaped.push_str(&format!("\\x{b:02x}")),
            b => escaped.push(b as char),
        }
    }
    println!("{escaped}");
}
