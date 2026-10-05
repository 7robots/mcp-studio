//! MCP Studio's terminal UI: a small component framework ([`framework`]), the
//! modules built on it ([`gateway`], and placeholders in [`modules`] for the
//! ones not wired yet), and the demo that runs the Gateway module against an
//! in-process fake ([`demo`]).
//!
//! The architecture is described in `docs/tui-architecture.md` in this crate.

pub mod demo;
pub mod framework;
pub mod gateway;
pub mod modules;

pub use framework::app::App;
pub use framework::component::{Component, Handled, ModuleId};
pub use framework::cx::{Cx, Msg, Sender};
pub use framework::harness::Harness;
pub use framework::keymap::{Hint, Keymap};
pub use framework::run::run;

/// The fake gateway the demo runs on, for dependents' tests.
pub use studio_fake as fake;

/// Assembles the standard Studio shell: Fleet, Pattern, Gateway and
/// Marketplaces, on keys 1-4, with the Gateway module focused. Modules not
/// implemented yet are placeholders.
pub fn studio_app(
    title: impl Into<String>,
    gateway: gateway::GatewayModule,
) -> (App, tokio::sync::mpsc::UnboundedReceiver<Msg>) {
    let modules: Vec<Box<dyn Component>> = vec![
        Box::new(modules::Placeholder::new("fleet", "Fleet")),
        Box::new(modules::Placeholder::new("pattern", "Pattern")),
        Box::new(gateway),
        Box::new(modules::Placeholder::new("marketplaces", "Marketplaces")),
    ];
    let (mut app, rx) = App::new(title, modules);
    app.focus_id(gateway::ID);
    (app, rx)
}
