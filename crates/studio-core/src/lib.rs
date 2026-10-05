//! MCP Studio core: the instance configuration model and the helpers every
//! other crate shares. Nothing org-specific belongs in this crate (or any
//! other): org values come from an instance's `studio.toml`.

pub mod check;
pub mod config;
pub mod exec;
pub mod instance;
pub mod secret;

pub use config::StudioConfig;
pub use instance::Instance;
pub use secret::{Secret, SecretRef};
