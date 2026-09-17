//! Exposes `file_io` **read-only**.
//!
//! Handing out `zyris-fs`'s `LocalFileIo` as-is drags `write`·`remove`·`mkdir` along too. If there are
//! two ways to change a file, the agent picks a full overwrite and the diff spreads over the whole
//! file, and the approval gate has to be in two places. So the descriptor's tool list is filtered
//! before announcing — legal, since protocol §5 pins down "consumers discover tools by descriptor".
//!
//! **This node has no file-deleting tool at all.** Deleting is done by a human.

use std::path::PathBuf;

use async_trait::async_trait;
// The `serve` module itself is private. Its items are re-exported at the crate root, so use those.
use serde_json::Value;
use zyris::{CapabilityDescriptor, IncomingCall, Outgoing, Result, ServeCapability};
use zyris_caps::FileIoServer;
use zyris_fs::LocalFileIo;

/// The four that are exposed. The rest are filtered out.
const READ_ONLY: &[&str] = &["stat", "list", "read", "read_stream"];

/// The ones deliberately withheld, written down so upstream cannot grow a writer unnoticed.
///
/// **Every tool the implementation offers has to appear in one of these two lists**, and the test
/// below fails the moment that stops being true. Without it, a new upstream tool simply lands on
/// the filtered side by default — silently, with nobody having decided anything. capkit v3 really
/// did add `edit` this way, and the test that was supposed to guard this went on passing. That
/// implementation is `zyris-fs` now rather than `zyris-capkit`, which changes where the tools come
/// from and nothing at all about how a new one gets classified.
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
        let out = self.0.dispatch(call).await?;
        Ok(note_how_to_read_on(out))
    }
}

/// Adds the two things a caller would otherwise put together by hand to a `read` answer: the
/// file's **version token** and, when the file was cut short, the **offset that reads on**.
///
/// **The token is spelled out here because spelling it wrong looks like something else.**
/// `code_edit` wants `"mtime_ms:size"`, and upstream hands those out as two separate numbers under
/// `stat` — joining them with a colon is exactly the step that goes wrong in a way that reads as a
/// stale file, costing a re-read and a retry. `offset + len` is the same kind of arithmetic, and it
/// was written down in a description that trimming then cut shorter than the sentence carrying it
/// (`tools/trim.rs`).
///
/// Only `read` is touched: it is the one answer with bytes in it. `read_stream`'s head is a stat and
/// its bytes are a stream, and `stat`·`list` have nothing to add.
fn note_how_to_read_on(out: Outgoing) -> Outgoing {
    let Outgoing::Response(payload) = out else { return out };
    let Ok(mut body) = payload.to_json() else { return Outgoing::Response(payload) };
    let Some(map) = body.as_object_mut() else { return Outgoing::Response(payload) };
    if map.get("content").is_none() {
        return Outgoing::Response(payload);
    }
    let stat = map.get("stat");
    let ms = stat.and_then(|s| s.get("modified_unix_ms")).and_then(Value::as_u64);
    let size = stat.and_then(|s| s.get("size")).and_then(Value::as_u64);
    if let (Some(ms), Some(size)) = (ms, size) {
        map.insert("version".into(), Value::from(format!("{ms}:{size}")));
    }
    if map.get("truncated").and_then(Value::as_bool) == Some(true) {
        let offset = map.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let len = map.get("len").and_then(Value::as_u64).unwrap_or(0);
        map.insert("next_offset".into(), Value::from(offset + len));
    }
    Outgoing::Response(zyris::Payload::from_json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With two write paths, the agent picks a full overwrite and the diff spreads over the whole file.
    ///
    /// **Every tool `zyris-fs` offers must be classified here, by hand.**
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
                "zyris-fs offers `{name}`, which this node has never decided about. Put it in \
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

    /// **Every tool `zyris-fs` offers that is not a read is refused when called**, not merely
    /// hidden. Filtering the list alone leaves the name callable by anyone who knows it.
    #[tokio::test]
    async fn no_writer_the_implementation_offers_can_be_called() {
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

    /// **The version token and the way on are in the answer**, rather than assembled by the caller:
    /// `code_edit` wants `mtime_ms:size`, and `offset + len` is what reads on.
    #[tokio::test]
    async fn a_read_answer_carries_the_version_token_and_the_next_offset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let cap = ReadOnlyFileIo::new(dir.path().to_path_buf());
        let call = IncomingCall {
            tool: "read".into(),
            params: zyris::Payload::from_json(serde_json::json!({ "path": "a.txt", "len": 2 })),
            serialization: zyris::Serialization::Json,
            meta: zyris::Payload::default(),
        };
        let out = cap.dispatch(call).await.unwrap();
        let Outgoing::Response(payload) = &out else { panic!("a read answers in one response") };
        let body = payload.to_json().unwrap();
        let md = std::fs::metadata(dir.path().join("a.txt")).unwrap();
        let ms = md.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        assert_eq!(body["content"], serde_json::json!("he"));
        assert_eq!(body["truncated"], serde_json::json!(true));
        assert_eq!(body["version"], serde_json::json!(format!("{ms}:6")));
        assert_eq!(body["next_offset"], serde_json::json!(2));
    }

    /// A whole file is not cut short: there is no offset to read on to, and saying nothing beats
    /// pointing past the end of the file.
    #[tokio::test]
    async fn a_whole_read_has_no_next_offset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let cap = ReadOnlyFileIo::new(dir.path().to_path_buf());
        let call = IncomingCall {
            tool: "read".into(),
            params: zyris::Payload::from_json(serde_json::json!({ "path": "a.txt" })),
            serialization: zyris::Serialization::Json,
            meta: zyris::Payload::default(),
        };
        let out = cap.dispatch(call).await.unwrap();
        let Outgoing::Response(payload) = &out else { panic!("a read answers in one response") };
        let body = payload.to_json().unwrap();
        assert!(body.get("next_offset").is_none(), "{body}");
        assert!(body.get("version").is_some(), "{body}");
    }
}
