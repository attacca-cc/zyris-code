//! 로컬 MCP 서버를 이 노드의 캐퍼빌리티로 만든다.
//!
//! 에이전트가 보는 것은 zyris 도구다 — MCP라는 것을 알 필요가 없다. `client`가 서버와
//! 말하고, 그 위에 얹는 브리지가 그것을 캐퍼빌리티 모양으로 바꾼다.

pub mod bridge;
pub mod client;
