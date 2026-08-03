//! `file_io`를 **읽기만** 내준다.
//!
//! capkit의 `LocalFileIo`를 그대로 내주면 `write`·`remove`·`mkdir`까지 딸려 나온다. 파일을
//! 바꾸는 길이 둘이면 에이전트가 통째로 덮어쓰기를 골라 diff가 파일 전체로 번지고, 승인
//! 게이트를 두 곳에 걸어야 한다. 그래서 descriptor의 tool 목록을 걸러 announce한다 —
//! 프로토콜 §5가 "consumers discover tools by descriptor"라고 못 박고 있으므로 합법이다.
//!
//! **파일을 지우는 도구는 이 노드에 아예 없다.** 지우는 것은 사람이 한다.

use std::path::PathBuf;

use async_trait::async_trait;
// `serve` 모듈 자체는 비공개다. 항목은 크레이트 루트에 재수출돼 있으니 그쪽으로 쓴다.
use zyris::{CapabilityDescriptor, IncomingCall, Outgoing, Result, ServeCapability};
use zyris_capkit::LocalFileIo;
use zyris_caps::FileIoServer;

/// 내주는 넷. 나머지는 걸러 낸다.
const READ_ONLY: &[&str] = &["stat", "list", "read", "read_stream"];

pub struct ReadOnlyFileIo(FileIoServer<LocalFileIo>);

impl ReadOnlyFileIo {
    pub fn new(root: PathBuf) -> ReadOnlyFileIo {
        ReadOnlyFileIo(FileIoServer(LocalFileIo::rooted(root)))
    }
}

#[async_trait]
impl ServeCapability for ReadOnlyFileIo {
    fn descriptor(&self) -> CapabilityDescriptor {
        let mut d = self.0.descriptor();
        d.tools.retain(|t| READ_ONLY.contains(&t.name.as_str()));
        d
    }

    async fn dispatch(&self, call: IncomingCall) -> Result<Outgoing> {
        // **announce하지 않은 도구는 부를 수도 없어야 한다.** 목록만 거르고 dispatch를 열어
        // 두면 이름을 아는 쪽이 그냥 부를 수 있다 — 거른 의미가 사라진다.
        if !READ_ONLY.contains(&call.tool.as_str()) {
            return Err(zyris::unknown_tool("file_io", &call.tool));
        }
        self.0.dispatch(call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 쓰기 경로가 둘이면 에이전트가 통째로 덮어쓰기를 골라 diff가 파일 전체로 번진다.
    #[test]
    fn the_announced_file_io_cannot_write() {
        let cap = ReadOnlyFileIo::new(PathBuf::from("/tmp")).descriptor();
        let names: Vec<&str> = cap.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read"), "읽기는 있어야 한다: {names:?}");
        for banned in ["write", "remove", "mkdir"] {
            assert!(!names.contains(&banned), "{banned}이 내줘지면 안 된다: {names:?}");
        }
    }

    /// 이름과 버전이 zyris가 정한 값이어야 붙는다 — 매칭이 (이름, 버전) 쌍이다.
    #[test]
    fn it_still_announces_itself_as_file_io() {
        let cap = ReadOnlyFileIo::new(PathBuf::from("/tmp")).descriptor();
        assert_eq!(cap.name, "file_io");
        assert_eq!(cap.version, zyris_caps::file_io_capability().version);
    }

    /// 목록에서 거른 도구는 불러도 없다고 해야 한다.
    #[tokio::test]
    async fn a_filtered_tool_cannot_be_called_anyway() {
        let cap = ReadOnlyFileIo::new(PathBuf::from("/tmp"));
        let call = IncomingCall {
            tool: "remove".into(),
            params: zyris::Payload::from_json(serde_json::json!({"path": "a"})),
            serialization: zyris::Serialization::Json,
        };
        assert!(cap.dispatch(call).await.is_err(), "거른 도구가 불려서는 안 된다");
    }
}
