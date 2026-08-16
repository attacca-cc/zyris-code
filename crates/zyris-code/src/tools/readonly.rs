//! Exposes `file_io` **read-only**.
//!
//! Handing out capkit's `LocalFileIo` as-is drags `write`·`remove`·`mkdir` along too. If there are
//! two ways to change a file, the agent picks a full overwrite and the diff spreads over the whole
//! file, and the approval gate has to be in two places. So the descriptor's tool list is filtered
//! before announcing — legal, since protocol §5 pins down "consumers discover tools by descriptor".
//!
//! **This node has no file-deleting tool at all.** Deleting is done by a human.

use std::path::PathBuf;

use async_trait::async_trait;
// The `serve` module itself is private. Its items are re-exported at the crate root, so use those.
use zyris::{CapabilityDescriptor, IncomingCall, Outgoing, Result, ServeCapability};
use zyris_capkit::LocalFileIo;
use zyris_caps::FileIoServer;

/// The four that are exposed. The rest are filtered out.
const READ_ONLY: &[&str] = &["stat", "list", "read", "read_stream"];

/// The ones deliberately withheld, written down so upstream cannot grow a writer unnoticed.
///
/// **Every tool capkit offers has to appear in one of these two lists**, and the test below fails
/// the moment that stops being true. Without it, a new upstream tool simply lands on the filtered
/// side by default — silently, with nobody having decided anything. capkit v3 really did add
/// `edit` this way, and the test that was supposed to guard this went on passing.
///
/// It is a test-only list because `READ_ONLY` alone decides what runs; this one exists to force a
/// human to classify what upstream adds, not to gate anything at runtime.
#[cfg(test)]
const WITHHELD: &[&str] = &["write", "edit", "remove", "mkdir"];

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
        // **A tool that wasn't announced must not be callable either.** Filtering only the list while
        // leaving dispatch open lets anyone who knows the name just call it — the filtering is moot.
        if !READ_ONLY.contains(&call.tool.as_str()) {
            return Err(zyris::unknown_tool("file_io", &call.tool));
        }
        self.0.dispatch(call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With two write paths, the agent picks a full overwrite and the diff spreads over the whole file.
    ///
    /// **Every tool capkit offers must be classified here, by hand.**
    ///
    /// This used to name `write`·`remove`·`mkdir` inline, and capkit v3 then added a fourth writer
    /// (`edit`). The allowlist did hold — a new name simply lands on the filtered side — but that
    /// is the problem: it lands there *by default*, with nobody having looked at it. So the test
    /// demands a decision instead of a safe accident, and fails until one is written down.
    #[test]
    fn every_tool_upstream_offers_is_classified() {
        let all = FileIoServer(LocalFileIo::rooted(PathBuf::from("/tmp"))).descriptor();
        let offered: Vec<&str> = all.tools.iter().map(|t| t.name.as_str()).collect();

        for name in &offered {
            assert!(
                READ_ONLY.contains(name) || WITHHELD.contains(name),
                "capkit offers `{name}`, which this node has never decided about. Put it in \
                 READ_ONLY if it only reads, or in WITHHELD if it changes anything."
            );
        }
        for name in READ_ONLY {
            assert!(!WITHHELD.contains(name), "`{name}` is in both lists — decide which it is");
            assert!(offered.contains(name), "`{name}` is announced but upstream no longer has it");
        }
    }

    /// The announced list is exactly `READ_ONLY` — nothing withheld leaks into it.
    #[test]
    fn the_announced_file_io_is_exactly_the_reads() {
        let cap = ReadOnlyFileIo::new(PathBuf::from("/tmp")).descriptor();
        let mut announced: Vec<&str> = cap.tools.iter().map(|t| t.name.as_str()).collect();
        announced.sort_unstable();
        let mut want: Vec<&str> = READ_ONLY.to_vec();
        want.sort_unstable();
        assert_eq!(announced, want);
    }

    /// **Every tool capkit offers that is not a read is refused when called**, not merely hidden.
    /// Filtering the list alone leaves the name callable by anyone who knows it.
    #[tokio::test]
    async fn no_writer_capkit_offers_can_be_called() {
        let all = FileIoServer(LocalFileIo::rooted(PathBuf::from("/tmp"))).descriptor();
        let cap = ReadOnlyFileIo::new(PathBuf::from("/tmp"));
        for tool in all.tools.iter().filter(|t| !READ_ONLY.contains(&t.name.as_str())) {
            let call = IncomingCall {
                tool: tool.name.clone(),
                params: zyris::Payload::from_json(serde_json::json!({"path": "a"})),
                serialization: zyris::Serialization::Json,
                meta: zyris::Payload::default(),
            };
            assert!(
                cap.dispatch(call).await.is_err(),
                "{} is callable — the only way to change a file must stay `code_edit`",
                tool.name
            );
        }
    }

    /// It attaches only when the name and version are the values zyris sets — matching is on the (name, version) pair.
    #[test]
    fn it_still_announces_itself_as_file_io() {
        let cap = ReadOnlyFileIo::new(PathBuf::from("/tmp")).descriptor();
        assert_eq!(cap.name, "file_io");
        assert_eq!(cap.version, zyris_caps::file_io_capability().version);
    }

    /// A tool filtered from the list must be reported as missing even when called.
    #[tokio::test]
    async fn a_filtered_tool_cannot_be_called_anyway() {
        let cap = ReadOnlyFileIo::new(PathBuf::from("/tmp"));
        let call = IncomingCall {
            tool: "remove".into(),
            params: zyris::Payload::from_json(serde_json::json!({"path": "a"})),
            serialization: zyris::Serialization::Json,
            meta: zyris::Payload::default(),
        };
        assert!(cap.dispatch(call).await.is_err(), "a filtered tool must not be callable");
    }
}
