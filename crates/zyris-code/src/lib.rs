//! zyris-code — Attacca 에이전트와 대화하는 터미널 클라이언트.
//!
//! 순수 코어와 I/O를 가른다. 프레임 변환·타임라인·마크다운·레이아웃·스크롤·입력은
//! 전부 순수 함수라 단위 테스트로 덮이고, 네트워크와 터미널은 `conn`과 `app::run()`
//! 두 곳에만 있다.
//!
//! 통합 테스트(`tests/`)는 바이너리를 볼 수 없으므로 모듈은 여기서 공개한다.

pub mod app;
pub mod clipboard;
pub mod command;
pub mod conn;
pub mod enroll;
pub mod event;
pub mod input;
pub mod instructions;
pub mod lang;
pub mod markdown;
pub mod mcp;
pub mod mode;
pub mod notice;
pub mod picker;
pub mod plugin;
pub mod question;
pub mod rows;
pub mod scroll;
pub mod selection;
pub mod sidebar;
pub mod theme;
pub mod timeline;
pub mod tools;
pub mod undo;
pub mod widgets;
