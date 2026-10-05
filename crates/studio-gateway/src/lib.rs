//! The MCP gateway admin client.
//!
//! The gateway has no REST admin API. Every operation is a JSON-RPC
//! `tools/call` to `POST /mcp` under a gateway-issued OAuth token, so this
//! crate is a thin HTTP client ([`client`]), the browser sign-in that obtains
//! the token ([`oauth`]), token storage ([`tokens`]), typed views of the tool
//! results ([`model`]) and the admin actions built on them ([`actions`]).
//!
//! Nothing org-specific lives here: scopes, the admin probe tool, the DCR
//! `client_name` and the gateway origin all come from a [`Profile`], which is
//! built from an instance's `[[gateway]]` table.

pub mod actions;
pub mod client;
pub mod model;
pub mod oauth;
pub mod profile;
pub mod tokens;
pub mod util;

pub use client::{GatewayError, GatewayResult, Session, http_client};
pub use profile::Profile;
pub use tokens::{TokenStore, Tokens};

// Re-exported so dependents (the CLI, the TUI) need not name these crates.
pub use reqwest;
pub use url::{self, Url};
