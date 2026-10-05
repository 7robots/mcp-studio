//! The component framework every Studio module is built on.
//!
//! - [`component::Component`]: the trait a module implements.
//! - [`cx::Cx`]: what a module gets while handling an event: a typed message
//!   channel back to itself, `spawn` for async loads, toasts, quit.
//! - [`keymap::Keymap`]: one table per context that both dispatches keys and
//!   generates the footer hints and the help overlay.
//! - [`overlay`]: reusable modal widgets: form, one-key choice, list picker,
//!   confirmation (optionally guarded by a typed id).
//! - [`slot::Slot`]: one async-loaded value with a generation, so a stale
//!   result is dropped instead of cancelled.
//! - [`toast::Toasts`]: the status-line message.
//! - [`app::App`]: the shell: module tab strip, header, footer, help, routing.
//! - [`harness::Harness`]: the headless driver tests (and the gateway's
//!   acceptance gate) use.
//! - [`run::run`]: the real terminal loop.

pub mod app;
pub mod component;
pub mod cx;
pub mod harness;
pub mod keymap;
pub mod overlay;
pub mod run;
pub mod slot;
pub mod toast;
pub mod widgets;
