//! Zyris 브랜드 팔레트 — 따뜻한 다크.
//!
//! **배경은 한 곳만 칠한다** — 사용자 메시지(`USER_BG`). 화면 전체는 칠하지 않는다 —
//! 터미널이 자기 배경을 쓰게 두는 것이 이 앱의 정책이다. 2026-08-03에 잔상을 막으려고
//! 화면 전체 배경을 켰다가, 사람이 일부러 지운 것임이 드러나 되돌렸다. 잔상은
//! `app::heal_interval`(기본 2초마다 전체 다시 그리기)이 치운다.
//!
//! **색이 없는 텍스트를 만들지 말 것.** 지정하지 않으면 터미널 자체의 기본 전경색이
//! 새어 나온다 — 기본 전경색을 바꿔 둔 터미널에서 "흰 글자여야 할 것"이 전부 그
//! 색으로 보인다.

use ratatui::style::Color;

/// Zyris 웹 팔레트의 `--zyris-bg`(#0f0d0a). **기본으로는 칠하지 않는다** — `page_bg`를 볼 것.
pub const BG: Color = Color::Rgb(0x0f, 0x0d, 0x0a);

/// 화면 전체에 깔 배경. **기본은 없음이다 — 터미널이 자기 배경을 쓰게 둔다.**
///
/// 예전에는 남는 칸 전부에 `BG`를 깔았다. 이유가 있었다: ratatui diff는 전각 글자의 오른쪽
/// 칸을 터미널로 내보내지 않는데(터미널이 두 칸을 다 칠해 준다는 믿음), 전각이 좁은 글자로
/// 바뀔 때 그 칸을 강제로 지우는 보호가 **`previous.bg != Reset`일 때만** 발동한다. 배경이
/// 없으면 원격 터미널에 전각의 오른쪽 반쪽이 잔상으로 남는다.
///
/// **그래도 기본을 끄로 둔다** — 앱이 칠할 수 없는 자리가 있어서다. 터미널 창은 격자에
/// 안 들어맞는 픽셀을 오른쪽·아래에 여백으로 남기고, 창 자체의 패딩도 있다. 그 자리는
/// 터미널 배경 그대로라, 앱이 자기 배경을 칠하는 순간 **가장자리에 색이 다른 띠가 생긴다.**
/// 화면 안을 위해 화면 테두리를 망치는 셈이다. 사람이 고른 터미널 배경을 그냥 쓰는 편이
/// 어디서나 낫다.
///
/// 잔상은 다시 그려 지운다 — `Ctrl+L`과 `app::heal_interval`(기본 2초)이 그 일을 한다.
/// 그걸로 모자란 터미널에서는 **`ZYRIS_CODE_BG`로 되켠다**: `zyris`면 위의 브랜드 색,
/// `#rrggbb`면 그 색이다.
pub fn page_bg() -> Option<Color> {
    static PICKED: std::sync::OnceLock<Option<Color>> = std::sync::OnceLock::new();
    *PICKED.get_or_init(|| page_bg_from(std::env::var("ZYRIS_CODE_BG").ok().as_deref()))
}

/// `$ZYRIS_CODE_BG`를 색으로. **순수** — 판정을 여기 두어야 테스트가 환경변수를 안 흔든다.
pub fn page_bg_from(given: Option<&str>) -> Option<Color> {
    let given = given.map(str::trim).filter(|v| !v.is_empty())?;
    match given.to_ascii_lowercase().as_str() {
        // 끄는 쪽도 명시할 수 있어야 한다 — 어딘가 설정에 켜 두고 잊었을 때 되돌릴 길이다.
        "none" | "off" | "0" | "terminal" => None,
        "zyris" | "on" | "1" | "default" => Some(BG),
        _ => hex(given),
    }
}

/// `#rrggbb` 또는 `rrggbb`. 못 읽으면 `None`이다 — 오타 하나로 앱이 죽을 이유가 없다.
fn hex(text: &str) -> Option<Color> {
    let digits = text.strip_prefix('#').unwrap_or(text);
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some(Color::Rgb(byte(0)?, byte(2)?, byte(4)?))
}

/// 영역을 가르는 선. 터미널 배경이 무엇이든 은은하게 보이도록 중간 밝기로 둔다.
pub const BORDER: Color = Color::Rgb(0x3a, 0x30, 0x29);
pub const BORDER_LIGHT: Color = Color::Rgb(0x4a, 0x3e, 0x36);
pub const TEXT: Color = Color::Rgb(0xe8, 0xe2, 0xdc);
pub const TEXT_MUTED: Color = Color::Rgb(0x9c, 0x94, 0x8d);
pub const TEXT_HEADING: Color = Color::Rgb(0xf1, 0xed, 0xe8);
pub const ACCENT: Color = Color::Rgb(0xc9, 0x73, 0x4d);
pub const ACCENT_HOVER: Color = Color::Rgb(0xb5, 0x62, 0x3e);
pub const ACCENT_MUTED: Color = Color::Rgb(0xa3, 0x53, 0x32);
pub const SUCCESS: Color = Color::Rgb(0x8f, 0xae, 0x5c);
pub const WARNING: Color = Color::Rgb(0xd9, 0xa4, 0x41);
pub const DANGER: Color = Color::Rgb(0xc1, 0x50, 0x3f);

/// 사용자가 말한 자리의 배경. ACCENT(0xc9734d)를 배경으로 쓸 수 있을 만큼 낮춘 것이다.
///
/// **배경을 쓰는 곳은 여기 하나뿐이다.** 파일 맨 위의 "배경은 칠하지 않는다"를 여기서만
/// 뒤집는다 — 화면 전체에 깔면 배경을 바꿔 둔 터미널에서 얼룩처럼 튀고, 무엇보다
/// 다 칠하면 아무것도 구별되지 않는다. 이 한 줄이 "내가 말한 자리"라는 신호다.
pub const USER_BG: Color = Color::Rgb(0x2a, 0x20, 0x1a);

/// 도구 줄의 이름. **추론과 같은 흐린 색이면 안 된다.**
///
/// 펼친 카드 안에서 추론이 화면을 채우는데 도구까지 `TEXT_MUTED`면, 정작 "무엇을 했는가"가
/// 생각 더미에 묻힌다. 읽는 사람이 훑는 것은 도구 줄이므로 그쪽이 떠 보여야 한다.
pub const TOOL: Color = Color::Rgb(0x7f, 0xb0, 0xd4);
/// 도구 줄의 인자 요약. 이름보다 한 단계 낮춘다.
pub const TOOL_ARG: Color = Color::Rgb(0x6b, 0x8a, 0xa0);

/// diff에서 더해진 줄. 초록.
///
/// `SUCCESS`·`DANGER`를 그대로 쓰지 않는다. 그 둘은 "도구가 됐다/안 됐다"를 말하는
/// 색이라, 성공한 편집의 삭제 줄이 실패한 도구와 같은 빨강이 되면 눈이 잘못 읽는다.
pub const DIFF_ADD: Color = Color::Rgb(0x7e, 0xc0, 0x50);
/// diff에서 지워진 줄. 빨강.
pub const DIFF_DEL: Color = Color::Rgb(0xe0, 0x6c, 0x75);

#[cfg(test)]
mod tests {
    use super::*;

    /// 팔레트는 Zyris 웹과 값이 같아야 한다. 어긋나면 같은 제품이 다른 색으로 보인다.
    #[test]
    fn the_palette_matches_the_brand_values() {
        assert_eq!(BG, Color::Rgb(0x0f, 0x0d, 0x0a));
        assert_eq!(ACCENT, Color::Rgb(0xc9, 0x73, 0x4d));
        assert_eq!(TEXT, Color::Rgb(0xe8, 0xe2, 0xdc));
        assert_eq!(TEXT_MUTED, Color::Rgb(0x9c, 0x94, 0x8d));
        assert_eq!(TEXT_HEADING, Color::Rgb(0xf1, 0xed, 0xe8));
        assert_eq!(BORDER, Color::Rgb(0x3a, 0x30, 0x29));
        assert_eq!(DANGER, Color::Rgb(0xc1, 0x50, 0x3f));
    }
}
