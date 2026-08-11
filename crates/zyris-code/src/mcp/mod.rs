//! Makes a local MCP server into a capability of this node.
//!
//! What the agent sees is a zyris tool — it needn't know it's MCP. `client` talks to the server,
//! and a bridge on top of it reshapes that into a capability.

pub mod bridge;
pub mod client;
/// What other coding clients already have set up. **Listed, never started on its own.**
pub mod discovery;
/// Remote servers (`"type": "http"`). Same three calls, a different pipe.
pub mod http;
