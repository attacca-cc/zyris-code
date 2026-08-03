//! 클립보드.
//!
//! **읽기와 쓰기가 대칭이 아니다.** 터미널은 OSC 52로 클립보드에 *쓰는* 것은 대개 허용하지만
//! *읽는* 질의는 막는다(다른 앱이 클립보드를 훔쳐볼 수 있으니 당연하다). 그래서:
//!
//! - 복사: 앱 안에 들고 있으면서 OSC 52로 시스템 클립보드에도 넣는다 — 다른 창에 붙일 수 있다.
//! - 붙여넣기: **이 앱이 복사한 것만** 붙는다. 시스템 클립보드는 읽을 수 없다.
//!
//! 터미널 자체의 붙여넣기(대개 `Ctrl+Shift+V`나 가운데 클릭)는 키 입력으로 들어오므로
//! 그쪽은 그대로 동작한다 — bracketed paste가 켜져 있으면 한 덩어리로 온다.

use std::io::Write;

use base64::Engine;

/// 앱이 들고 있는 클립보드.
#[derive(Debug, Default, Clone)]
pub struct Clipboard {
    text: String,
}

impl Clipboard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 앱 안에 넣는다. 시스템 클립보드로 내보내는 것은 `osc52_sequence`가 만든다.
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }
}

/// 시스템 클립보드에 넣는 OSC 52 시퀀스.
///
/// 터미널이 무시해도 앱 안 클립보드는 이미 채워져 있으므로 손해가 없다.
pub fn osc52_sequence(text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

/// 시스템 클립보드로 내보낸다. 실패는 무시한다 — 앱 안 클립보드가 이미 정본이다.
pub fn export(text: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(osc52_sequence(text).as_bytes());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_goes_in_comes_out() {
        let mut c = Clipboard::new();
        assert!(c.is_empty());
        c.set("안녕하세요");
        assert_eq!(c.get(), "안녕하세요");
        assert!(!c.is_empty());
    }

    /// OSC 52는 `ESC ] 52 ; c ; <base64> BEL` 형태다. 한글이 섞여도 base64라 안전하다.
    #[test]
    fn the_osc52_sequence_is_well_formed() {
        let seq = osc52_sequence("한글 test");
        assert!(seq.starts_with("\x1b]52;c;"), "{seq:?}");
        assert!(seq.ends_with('\x07'), "{seq:?}");

        let body = seq.trim_start_matches("\x1b]52;c;").trim_end_matches('\x07');
        let decoded = base64::engine::general_purpose::STANDARD.decode(body).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "한글 test");
    }
}
