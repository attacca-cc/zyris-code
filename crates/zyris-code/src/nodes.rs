//! A node of this window's own, so two windows do not take each other's tool calls.
//!
//! **The server keys its registry by node id** (`insert(node_id, connection)`), so two windows
//! dialling with the same credential are one node: the connection that arrived last gets every
//! tool call and the other sits there with a live socket and nothing to do. The way out is for a
//! window past the first to be a *different node* — `register_node` mints one under the same
//! device, and it dials with a static token of its own.
//!
//! Three things make that safe to do in a program a person opens and closes all day:
//!
//! - **The token is cached and reused.** Nodes are permanent — there is no way to delete one from
//!   here yet (zyris PR #21 adds `delete_node`; the server has to implement it). Registering on
//!   every launch would pile up nodes on an account that can only be tidied from the dashboard.
//!   A slot registers once, ever.
//! - **Only the first window registers.** A second window cannot register for itself without
//!   opening a connection on the shared credential first — which is the very displacement this
//!   is meant to prevent. So it leaves a request file, and the window that already holds the
//!   connection services it. Two files, no protocol.
//! - **Failing to get one is not failing to start.** No first window running, or one that is not
//!   connected yet, means no answer inside the wait — so the window carries on sharing, says so,
//!   and asks again next launch.
//!
//! Nothing here talks to the network. `serve_requests` in `main.rs` is the one place that does.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A node registered for one slot, as it is kept on disk.
///
/// **The token is the whole file.** It is the one copy — `register_node` shows it once and no
/// listing ever carries it again, so losing this file means that node can never be dialled and a
/// fresh one has to take its place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Child {
    pub node_id: String,
    pub name: String,
    pub token: String,
}

/// A window asking the first window to register a node for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Which slot wants one. The answer is filed under the same number.
    pub slot: usize,
    /// The name it should carry — the asking window knows its own working directory, and the one
    /// answering does not.
    pub name: String,
    /// Who asked, so a request left behind by a window that died can be told from a live one.
    pub pid: u32,
}

/// Where a slot's node is kept.
pub fn child_path(dir: &Path, slot: usize) -> PathBuf {
    dir.join(format!("node-{slot}.json"))
}

/// Where a slot's request sits until it is answered.
pub fn request_path(dir: &Path, slot: usize) -> PathBuf {
    dir.join(format!("node-{slot}.request.json"))
}

pub fn load_child(dir: &Path, slot: usize) -> Option<Child> {
    let text = std::fs::read_to_string(child_path(dir, slot)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes a slot's node down. **Readable only by its owner** — this file is a credential, and the
/// config directory is not private on every system.
pub fn save_child(dir: &Path, slot: usize, child: &Child) -> std::io::Result<()> {
    let path = child_path(dir, slot);
    let text = serde_json::to_string_pretty(child)?;
    std::fs::write(&path, text)?;
    restrict(&path);
    Ok(())
}

pub fn forget_child(dir: &Path, slot: usize) {
    let _ = std::fs::remove_file(child_path(dir, slot));
}

/// Leaves the request. **Overwrites whatever was there** — a request from a window that has since
/// died says nothing about what this one needs.
pub fn ask_for(dir: &Path, slot: usize, name: &str) -> std::io::Result<()> {
    let request = Request { slot, name: name.to_string(), pid: std::process::id() };
    std::fs::write(request_path(dir, slot), serde_json::to_string_pretty(&request)?)
}

pub fn clear_request(dir: &Path, slot: usize) {
    let _ = std::fs::remove_file(request_path(dir, slot));
}

/// Every request waiting in this directory, lowest slot first.
///
/// **A request whose slot already has a node is dropped**, not answered again — a window that
/// asked and then found its file before the answer landed would otherwise get a second node
/// nobody dials.
pub fn pending(dir: &Path) -> Vec<Request> {
    let mut found: Vec<Request> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return found };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("node-") || !name.ends_with(".request.json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let Ok(request) = serde_json::from_str::<Request>(&text) else {
            // Unreadable: nobody can answer it, and leaving it means reading it again forever.
            let _ = std::fs::remove_file(entry.path());
            continue;
        };
        if load_child(dir, request.slot).is_some() {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        found.push(request);
    }
    found.sort_by_key(|r| r.slot);
    found
}

/// Drops cached nodes the server no longer knows about, so the slot asks for a fresh one.
///
/// **Without this a node deleted from the dashboard is a window that can never connect again.**
/// Its token stops working, the dial fails, and nothing on disk says why — the file looks fine.
/// Returns the slots that were forgotten.
pub fn drop_unknown(dir: &Path, known: &[String]) -> Vec<usize> {
    let mut dropped = Vec::new();
    for slot in 2..=crate::conn::MAX_SLOTS {
        let Some(child) = load_child(dir, slot) else { continue };
        if !known.contains(&child.node_id) {
            forget_child(dir, slot);
            dropped.push(slot);
        }
    }
    dropped
}

/// How long a window past the first waits for its node before carrying on without one.
///
/// **Short on purpose.** The first window answers in one poll when it is connected, and when it is
/// not there is nothing to wait for — the only thing a longer wait buys is a longer stare at
/// "connecting…" before the same fallback.
pub const WAIT: std::time::Duration = std::time::Duration::from_secs(8);

/// This slot's node: the one it already has, or one asked for and waited on.
///
/// `None` means carry on sharing the first window's identity and say so. **That is a normal
/// outcome**, not an error — it is what happens when this is the only window and the lock it holds
/// is simply a slot the previous window left behind.
pub async fn obtain(dir: &Path, slot: usize, name: &str) -> Option<Child> {
    if let Some(child) = load_child(dir, slot) {
        return Some(child);
    }
    if let Err(e) = ask_for(dir, slot, name) {
        tracing::warn!(error = %e, slot, "could not leave a request for a node of this window's own");
        return None;
    }
    let deadline = tokio::time::Instant::now() + WAIT;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if let Some(child) = load_child(dir, slot) {
            return Some(child);
        }
    }
    // **The request is left where it is.** The first window may still be starting up, and an
    // answer that lands after this gives the next launch a node without asking again.
    tracing::info!(slot, "no node arrived in time; sharing the first window's identity");
    None
}

/// The first window's side: register what the others asked for, and keep the cache honest.
///
/// **Only the window holding the connection can do this.** A window that registered for itself
/// would have to open a second connection on the shared credential — displacing the first, which
/// is the whole problem this exists to avoid.
pub async fn serve_requests(
    api: &zyris_attacca::AttaccaApiClient,
    dir: &Path,
    scopes: &[&str],
) -> usize {
    use zyris_attacca::{AttaccaApi, ZNewNode};

    // **The sweep runs first.** A slot whose node was deleted from the dashboard has to forget it
    // before its request is looked at, or the stale file answers the request itself.
    match api.list_nodes().await {
        Ok(nodes) => {
            let known: Vec<String> = nodes.into_iter().map(|n| n.node_id).collect();
            for slot in drop_unknown(dir, &known) {
                tracing::info!(slot, "the server no longer knows this slot's node; asking again");
            }
        }
        // Listing is only for tidying. Not being allowed to, or the call failing, must not stop a
        // window from getting the node it is waiting on.
        Err(e) => tracing::debug!(error = %e, "could not list nodes to check the cached ones"),
    }

    let mut served = 0;
    for request in pending(dir) {
        let new = ZNewNode {
            name: request.name.clone(),
            platform: None,
            scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
        };
        match api.register_node(new).await {
            Ok(node) => {
                let Some(token) = node.token else {
                    // The register answer is the one chance to see it. Without it the node exists
                    // and can never be dialled — say so rather than write a file that cannot work.
                    tracing::error!(slot = request.slot, "the server registered a node without a token");
                    clear_request(dir, request.slot);
                    continue;
                };
                let child = Child { node_id: node.node_id, name: node.name, token };
                match save_child(dir, request.slot, &child) {
                    // **Written before the request is cleared.** The other way round loses the
                    // token if the write fails, and it can never be asked for again.
                    Ok(()) => {
                        clear_request(dir, request.slot);
                        served += 1;
                        tracing::info!(slot = request.slot, node = %child.node_id, "registered a node for another window");
                    }
                    Err(e) => tracing::error!(error = %e, slot = request.slot, "could not save the node that was just registered"),
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, slot = request.slot, "could not register a node for another window");
                // Leave the request: the scope may be missing now and granted at the next
                // enrolment, and the window that asked will look again when it next starts.
                break;
            }
        }
    }
    served
}

#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(id: &str) -> Child {
        Child { node_id: id.into(), name: "arch zyris-code 2".into(), token: "znt_x".into() }
    }

    /// The round trip that matters: what a slot registered once is what it dials with forever
    /// after. Registering per launch would pile up nodes nothing can delete.
    #[test]
    fn a_slot_keeps_the_node_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_child(dir.path(), 2), None);
        save_child(dir.path(), 2, &child("n2")).unwrap();
        assert_eq!(load_child(dir.path(), 2).unwrap().token, "znt_x");
        // Slots do not read each other's.
        assert_eq!(load_child(dir.path(), 3), None);
    }

    /// **The token is not world readable.** The config directory is not private everywhere, and
    /// this file is the one copy of a credential that dials as this account.
    #[cfg(unix)]
    #[test]
    fn the_token_file_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        save_child(dir.path(), 2, &child("n2")).unwrap();
        let mode = std::fs::metadata(child_path(dir.path(), 2)).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }

    /// A request is answered once. **Answering a slot that already has a node** would mint one
    /// nobody ever dials, and nodes cannot be deleted from here.
    #[test]
    fn a_slot_that_already_has_a_node_is_not_answered_again() {
        let dir = tempfile::tempdir().unwrap();
        ask_for(dir.path(), 2, "arch zyris-code 2").unwrap();
        ask_for(dir.path(), 3, "arch zyris-code 3").unwrap();
        assert_eq!(pending(dir.path()).iter().map(|r| r.slot).collect::<Vec<_>>(), vec![2, 3]);

        save_child(dir.path(), 2, &child("n2")).unwrap();
        assert_eq!(pending(dir.path()).iter().map(|r| r.slot).collect::<Vec<_>>(), vec![3]);
        assert!(!request_path(dir.path(), 2).exists(), "the answered request was left behind");
    }

    /// Nonsense in the directory must not stop the requests that are readable, and must not be
    /// read again on the next pass.
    #[test]
    fn an_unreadable_request_is_thrown_away_rather_than_read_forever() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(request_path(dir.path(), 4), "not json").unwrap();
        ask_for(dir.path(), 5, "arch zyris-code 5").unwrap();
        assert_eq!(pending(dir.path()).iter().map(|r| r.slot).collect::<Vec<_>>(), vec![5]);
        assert!(!request_path(dir.path(), 4).exists());
    }

    /// **A node deleted from the dashboard has to be forgotten here too**, or its window dials a
    /// token the server no longer knows and fails with nothing on disk to explain it.
    #[test]
    fn a_node_the_server_forgot_is_dropped_so_the_slot_asks_again() {
        let dir = tempfile::tempdir().unwrap();
        save_child(dir.path(), 2, &child("still-there")).unwrap();
        save_child(dir.path(), 3, &child("deleted")).unwrap();

        let dropped = drop_unknown(dir.path(), &["still-there".to_string()]);
        assert_eq!(dropped, vec![3]);
        assert!(load_child(dir.path(), 2).is_some(), "a node the server still knows was dropped");
        assert!(load_child(dir.path(), 3).is_none());
    }

    /// **Slot 1 is never dropped.** It does not dial with a registered node at all — it holds the
    /// device credential — so it has no file here and nothing to compare against a listing.
    #[test]
    fn the_first_window_is_left_alone_by_the_sweep() {
        let dir = tempfile::tempdir().unwrap();
        save_child(dir.path(), 1, &child("would-not-be-listed")).unwrap();
        assert!(drop_unknown(dir.path(), &[]).is_empty());
        assert!(load_child(dir.path(), 1).is_some());
    }
}
