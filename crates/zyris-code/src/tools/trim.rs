//! announce하는 도구 정의를 **토큰 예산에 맞춘다.**
//!
//! 상류(zyris-caps)의 doc 주석이 그대로 이 노드의 도구 설명이 된다. 그 설명은 세션을
//! 만들 때마다, 그리고 턴마다 에이전트의 컨텍스트에 실린다 — 풍부한 예제·주의·경로 해석
//! 설명이 전부 실리면 file_io 하나가 수백 토큰을 먹는다. 이름과 스키마의 값 해석 부분은
//! 그대로 두고 **설명만** 자르면, 에이전트가 도구를 고르는 데 필요한 것(무엇을 하는
//! 도구인가)은 남고 반복은 빠진다.
//!
//! `dispatch`는 설명을 읽지 않는다 — 여기서 자르는 것이 announce되는 것에만 닿는다.

use serde_json::Value;
use zyris::CapabilityDescriptor;

/// 도구 설명 한 개의 예산(바이트). 첫 문장이 핵심이다.
pub const DESCRIPTION_LIMIT: usize = 200;
/// 스키마 안의 인자 설명 예산.
pub const PARAM_LIMIT: usize = 80;

/// 설명을 예산에 맞춘다. **마침표·줄바꿈에서 끊는다** — 문장 한가운데를 자르면
/// "왜"가 안 보인다. 잘렸다는 것은 `…` 하나로 말한다.
pub fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let cut = text.floor_char_boundary(limit);
    let end = text[..cut]
        .rfind(['.', '\n'])
        .map(|i| i + 1)
        .unwrap_or(cut);
    let mut out = text[..end].trim_end().to_string();
    out.push('…');
    out
}

/// 스키마 JSON 안의 설명을 예산에 맞춘다. `description` 키의 문자열만 자른다 —
/// 타입·기본값·enum 같은 값 해석에 쓰이는 것은 그대로 둔다.
pub fn clip_schema(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get_mut("description") {
                *s = clip(s, PARAM_LIMIT);
            }
            for child in map.values_mut() {
                clip_schema(child);
            }
        }
        Value::Array(items) => {
            for child in items {
                clip_schema(child);
            }
        }
        _ => {}
    }
}

/// 캐퍼빌리티 descriptor 하나를 예산에 맞춘다.
pub fn trim_descriptor(descriptor: &mut CapabilityDescriptor) {
    for tool in &mut descriptor.tools {
        tool.description = clip(&tool.description, DESCRIPTION_LIMIT);
        clip_schema(&mut tool.request_schema);
        if let Some(schema) = &mut tool.response_schema {
            clip_schema(schema);
        }
        if let Some(schema) = &mut tool.item_schema {
            clip_schema(schema);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zyris::ServeCapability;

    #[test]
    fn a_short_description_is_left_alone() {
        assert_eq!(clip("Read a file.", DESCRIPTION_LIMIT), "Read a file.");
        assert_eq!(clip("", 10), "");
    }

    /// 문장 한가운데를 자르지 않는다 — 끊는 자리는 마침표나 줄바꿈 뒤다.
    #[test]
    fn a_long_description_is_cut_at_a_sentence_boundary() {
        let text = "Read a file's text. Large files come back truncated, and you read on \
                    by passing an offset, which is described in more detail further down \
                    this sentence that has to go on for a while.";
        let out = clip(text, 40);
        assert_eq!(out, "Read a file's text.…");
        assert!(out.len() <= DESCRIPTION_LIMIT);
    }

    /// `description`만 자르고 타입은 그대로 — 값을 해석하는 것은 건드리면 안 된다.
    #[test]
    fn schema_descriptions_are_trimmed_but_the_shape_stays() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "A path that goes on and on and on far beyond the budget for a parameter help string, with nothing new to say."
                }
            }
        });
        clip_schema(&mut schema);
        let desc = schema["properties"]["path"]["description"].as_str().unwrap();
        // `…`는 3바이트라 예산 + 3까지 허용한다.
        assert!(desc.len() <= PARAM_LIMIT + 3, "{desc}");
        assert_eq!(schema["properties"]["path"]["type"], "string");
    }

    /// 실제 announce되는 file_io 설명이 예산 안에 들어오는가. Gate가 이 함수를
    /// 부르므로, 여기서 통과하면 에이전트가 받는 것이 통과한 것이다.
    #[test]
    fn the_announced_file_io_fits_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let gate = crate::tools::guard::Gate::new(
            crate::tools::readonly::ReadOnlyFileIo::new(dir.path().to_path_buf()),
            crate::tools::bridge::Bridge::new(),
        );
        for tool in gate.descriptor().tools {
            assert!(
                tool.description.len() <= DESCRIPTION_LIMIT,
                "{}: {}",
                tool.name,
                tool.description
            );
        }
    }
}
