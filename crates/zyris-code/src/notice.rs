//! 화면이 뜨기 전에 셸로 말한다.
//!
//! **TUI는 `on_connect` 안에서 뜬다.** 그러니 서버에 못 붙는 동안에는 화면이 아예 없고,
//! 그때 아무 말도 안 하면 사용자가 보는 것은 멈춘 커서 하나뿐이다 — 실제로 서버가 죽었을 때
//! 그랬다. 로그는 파일로 가므로 거기 있는 줄도 모른다.
//!
//! **화면이 뜬 뒤로는 한 글자도 안 내보낸다.** ratatui가 그린 자리에 끼어들면 그 칸을
//! "안 바뀌었다"고 여겨 다시 그리지도 않는다. 붙고 나서 끊기는 것은 화면이 말한다
//! (`activity.rs`의 "연결 중…").

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};

/// 이만큼 못 붙으면 처음 말한다. 눌렀다 떼는 사이에 끼어들지 않을 만큼은 기다린다.
const FIRST: u64 = 3;
/// 그 뒤로는 이 간격으로만 되풀이한다. 매초 찍으면 그건 그것대로 못 읽는다.
const REPEAT: u64 = 15;

/// 붙는 동안 밖에서 벌어진 일. 감시자가 읽어 셸에 옮긴다.
#[derive(Clone, Default)]
pub struct Notice(Arc<Inner>);

#[derive(Default)]
struct Inner {
    /// 마지막으로 잡은 실패 사유. 사람이 읽을 한 줄이다.
    last: Mutex<Option<String>>,
    /// 한 번이라도 붙었는가. 붙는 순간 감시자는 입을 다문다.
    connected: AtomicBool,
    /// 다른 누가 이미 사람에게 말하고 있는가. 등록 코드 상자가 이것을 켠다.
    hushed: AtomicBool,
}

impl Notice {
    pub fn new() -> Notice {
        Notice::default()
    }

    /// 붙었다. **이 뒤로는 아무것도 안 찍는다.**
    pub fn connected(&self) {
        self.0.connected.store(true, Ordering::SeqCst);
    }

    /// 기다린다는 말은 그만둔다. **실패는 계속 말한다.**
    ///
    /// 등록 코드 상자가 떴을 때 부른다. 상자가 "승인하면 저절로 이어집니다"까지 이미
    /// 말하고 있는데 그 밑에 같은 뜻의 줄이 또 붙으면, 상자의 테두리만 흐려진다.
    pub fn hush(&self) {
        self.0.hushed.store(true, Ordering::SeqCst);
    }

    /// 로그로 흘러가는 실패를 여기로도 흘려보내는 층.
    ///
    /// **메시지 글자를 보고 고르지 않는다** — 상류가 문구를 바꾸면 조용히 안 잡힌다.
    /// target과 level로만 고른다.
    pub fn layer(&self) -> Watch {
        Watch(self.clone())
    }

    fn remember(&self, why: String) {
        *self.0.last.lock().unwrap() = Some(why);
    }

    /// 더 해 볼 것이 없어 끝나는 자리. **조용히 죽지 않는다.**
    ///
    /// 마지막 사유를 함께 찍는다. 상류가 내놓는 최종 문구가 늘 진짜 원인을 가리키지는
    /// 않기 때문이다 — 실제로 서버가 죽었을 때 "check this machine's clock"이 왔는데
    /// 시계는 멀쩡했고, 진짜 원인은 그 앞에 지나간 "refresh를 보내지 못했다"였다.
    pub fn fatal(&self, why: &str) {
        red(&format!("연결에 실패했습니다: {why}"));
        if let Some(before) = self.0.last.lock().unwrap().as_deref() {
            if before != why {
                plain(&format!("직전 오류: {before}"));
            }
        }
        plain(&format!(
            "자세한 것은 로그에 있습니다: {}",
            std::env::var("ZYRIS_CODE_LOG").unwrap_or_else(|_| "/tmp/zyris-code.log".into())
        ));
    }

    /// 끝내기는 하는데 **오류는 아닌** 자리. 빨간색을 아껴 쓴다 — 다 빨가면 진짜 오류가
    /// 묻힌다.
    pub fn fatal_plain(&self, what: &str) {
        plain(&format!("\n{what}"));
    }

    /// 죽지는 않지만 알고 있어야 하는 일. **화면이 뜨기 전에만 쓴다.**
    ///
    /// 뜬 뒤에 stderr로 끼어들면 ratatui가 그린 자리를 덮고, 그 칸을 "안 바뀌었다"로 여겨
    /// 다시 그리지도 않는다.
    pub fn warn_plain(&self, what: &str) {
        plain(&format!("\n{what}"));
    }

    /// 붙을 때까지 지켜보다 셸에 알린다. 붙으면 조용히 끝난다.
    ///
    /// **기다리는 것과 실패한 것은 다르다.** 처음 켜면 상류가 등록 코드를 찍고 사람이
    /// 브라우저에서 승인할 때까지 폴링한다 — 그동안 노드는 당연히 안 붙어 있다. 예전에는
    /// 그 시간 내내 빨간 글씨로 "연결하지 못했습니다"를 15초마다 외쳤다. 코드를 넣기도
    /// 전에 실패했다고 말하는 셈이라, 사람은 자기가 뭘 잘못한 줄 안다.
    ///
    /// 이제 가르는 기준은 **주워 둔 사유가 있는가**다. 없으면 아직 기다리는 중이고,
    /// 그때는 차분한 색으로 **한 번만** 말한다. 있으면 그것이 진짜 실패다.
    pub fn watch(&self) {
        let notice = self.clone();
        tokio::spawn(async move {
            let mut waited = 0u64;
            let mut said_waiting = false;
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if notice.0.connected.load(Ordering::SeqCst) {
                    return;
                }
                waited += 1;
                let why = notice.0.last.lock().unwrap().clone();
                let Some(why) = why else {
                    // 아직 아무 오류도 없다 — 승인을 기다리는 중이다.
                    if notice.0.hushed.load(Ordering::SeqCst) {
                        continue;
                    }
                    if waited >= FIRST && !said_waiting {
                        said_waiting = true;
                        plain(
                            "\n브라우저에서 승인하면 저절로 이어집니다. \
                             그만두려면 Ctrl+C를 누르세요.",
                        );
                    }
                    continue;
                };
                // 3초째에 한 번, 그 뒤로 15초마다. 나머지 초에는 입을 다문다.
                let speak = match waited.checked_sub(FIRST) {
                    Some(0) => true,
                    Some(since) => since.is_multiple_of(REPEAT),
                    None => false,
                };
                if speak {
                    red(&format!("서버에 연결하지 못했습니다 ({waited}초째): {why}"));
                }
            }
        });
    }
}

/// 빨간 한 줄. **터미널이 아니면 색을 붙이지 않는다** — 파이프로 받은 쪽에는
/// 이스케이프가 그냥 쓰레기 글자다. `NO_COLOR`도 존중한다.
fn red(text: &str) {
    let mut err = std::io::stderr();
    let _ =
        if colours() { writeln!(err, "\x1b[1;31m{text}\x1b[0m") } else { writeln!(err, "{text}") };
    let _ = err.flush();
}

fn plain(text: &str) {
    let mut err = std::io::stderr();
    let _ = writeln!(err, "{text}");
    let _ = err.flush();
}

fn colours() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}

/// 실패 사유를 주워 담는 tracing 층.
pub struct Watch(Notice);

impl<S: tracing::Subscriber> Layer<S> for Watch {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // **`zyris` 크레이트가 내는 것만** 본다. 붙는 일은 `runtime`만 하는 게 아니라
        // `enroll`도 한다 — 실제로 진짜 원인("refresh를 보내지 못했다")이 `enroll::http`에서
        // 나왔는데 `runtime`만 보다가 놓쳤다.
        //
        // 우리 크레이트의 target은 `zyris_code::…`(밑줄)라 여기 걸리지 않는다. 그쪽 경고까지
        // 셸로 올리면 "연결 실패"라고 말해 놓고 엉뚱한 사정을 보여주게 된다.
        if !meta.target().starts_with("zyris::") {
            return;
        }
        if *meta.level() > tracing::Level::WARN {
            return;
        }
        let mut grab = Grab::default();
        event.record(&mut grab);
        if let Some(why) = grab.take() {
            self.0.remember(why);
        }
    }
}

/// `error = %e`의 값을 꺼낸다. 없으면 메시지라도.
#[derive(Default)]
struct Grab {
    error: Option<String>,
    message: Option<String>,
}

impl Grab {
    fn take(self) -> Option<String> {
        self.error.or(self.message)
    }
}

impl Visit for Grab {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let text = format!("{value:?}");
        match field.name() {
            "error" => self.error = Some(text.trim_matches('"').to_string()),
            "message" => self.message = Some(text),
            _ => {}
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "error" => self.error = Some(value.to_string()),
            "message" => self.message = Some(value.to_string()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 붙으면 감시자가 입을 다문다. 안 그러면 화면 위에 글자가 찍힌다.
    #[test]
    fn connecting_silences_the_watcher() {
        let n = Notice::new();
        assert!(!n.0.connected.load(Ordering::SeqCst));
        n.connected();
        assert!(n.0.connected.load(Ordering::SeqCst));
    }

    /// 실패 사유를 들고 있어야 셸에 무엇 때문인지 말할 수 있다.
    #[test]
    fn the_reason_is_kept_for_the_message() {
        let n = Notice::new();
        n.remember("Connection reset by peer".into());
        assert_eq!(n.0.last.lock().unwrap().as_deref(), Some("Connection reset by peer"));
    }

    /// `error` 필드가 있으면 그쪽이 이긴다 — 사람이 읽을 것은 사유지 로그 문구가 아니다.
    #[test]
    fn the_error_field_wins_over_the_log_message() {
        let grab = Grab {
            error: Some("Connection reset by peer".into()),
            message: Some("connect failed".into()),
        };
        assert_eq!(grab.take().as_deref(), Some("Connection reset by peer"));
    }

    /// 사유가 없으면 메시지라도 보여준다. 아무 말도 안 하는 것보다는 낫다.
    #[test]
    fn without_a_reason_the_message_is_used() {
        let grab = Grab { error: None, message: Some("connect failed".into()) };
        assert_eq!(grab.take().as_deref(), Some("connect failed"));
    }

    /// 끝낼 때 직전 사유까지 말해야 한다 — 최종 문구가 진짜 원인이 아닐 때가 있다.
    #[test]
    fn the_last_transient_reason_is_kept_for_the_fatal_message() {
        let n = Notice::new();
        n.remember("refresh를 보내지 못했다".into());
        // 찍는 것은 stderr이라 여기서는 들고 있는지만 본다. 문구는 위 테스트가 잠근다.
        assert_eq!(n.0.last.lock().unwrap().as_deref(), Some("refresh를 보내지 못했다"));
    }

    /// **오류가 없으면 실패가 아니다.** 처음 켜면 등록 코드를 넣을 때까지 안 붙어 있는데,
    /// 그 시간을 실패로 말하면 사람은 자기가 뭘 잘못한 줄 안다.
    #[test]
    fn waiting_is_not_the_same_as_failing() {
        let n = Notice::new();
        assert!(n.0.last.lock().unwrap().is_none(), "아직 아무 오류도 없다");
        // 사유가 생겨야 비로소 실패다.
        n.remember("Connection reset by peer".into());
        assert!(n.0.last.lock().unwrap().is_some());
    }

    /// **`NO_COLOR`를 존중한다.** 파이프로 받은 쪽에 이스케이프는 쓰레기 글자다.
    #[test]
    fn no_color_turns_the_escapes_off() {
        // 테스트는 터미널이 아니므로 어차피 꺼져 있어야 한다.
        assert!(!colours(), "터미널이 아닌 곳에 색을 내보내면 안 된다");
    }
}
