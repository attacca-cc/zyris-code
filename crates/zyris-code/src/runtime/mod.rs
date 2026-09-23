//! The program layer: credentials, where they are kept, and the loop that keeps this node dialled.
//!
//! **Upstream deleted all of this when `zyris` became a library.** `zyris::runtime::{Runner,
//! RunConfig, RunError}`, `zyris::runtime::credentials::*` and `zyris::enroll::{CredentialStore,
//! FileCredentialStore, …}` are gone from the dependency, and that is the right call rather than a
//! regression: a library cannot decide a program's supervision story. How long to back off, when a
//! refusal is worth one more rotation, whether a dead credential exits 1 or 2 for a supervisor to
//! read, where on this machine a secret may be written — every one of those is a property of the
//! program, and a library that answers them makes every node that disagrees fight it.
//!
//! So this app owns them now. The code here is a port of what upstream had, kept close to the
//! original on purpose: each of these is a small decision that is easy to get subtly wrong once and
//! then carry forever, and the upstream comments naming the incident behind each one are the most
//! valuable part of what moved. Where the shape had to change, the comment says why.
//!
//! What did **not** move is anything the library still does: the handshake, the reconnect
//! primitives and the device-grant flow. `Node` and `zyris::enroll` are still upstream's, and this
//! layer is only the loop around them.

pub mod credentials;
mod runner;
pub mod store;

pub use credentials::{
    token_prefix, Credentials, CredentialsError, StaticToken, TokenFile, CREDENTIAL_PREFIX,
};
pub use runner::{RunConfig, RunError, Runner};
pub use store::{
    CredentialStore, CredentialStoreError, FileCredentialStore, MemoryCredentialStore,
};
