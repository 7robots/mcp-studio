//! MCP Studio's terminal UI: a small component framework ([`framework`]), the
//! modules built on it ([`gateway`], and placeholders in [`modules`] for the
//! ones not wired yet), and the demo that runs the Gateway module against an
//! in-process fake ([`demo`]).
//!
//! The architecture is described in `docs/tui-architecture.md` in this crate.

pub mod demo;
pub mod fleet;
pub mod framework;
pub mod gateway;
pub mod marketplace;
pub mod modules;
pub mod pattern;

pub use framework::app::App;
pub use framework::component::{Component, Handled, ModuleId};
pub use framework::cx::{Cx, Msg, Sender};
pub use framework::harness::Harness;
pub use framework::keymap::{Hint, Keymap};
pub use framework::run::run;

/// The fake gateway the demo runs on, for dependents' tests.
pub use studio_fake as fake;

/// The modules of the standard shell. `None` keeps a module's place with a
/// placeholder.
#[derive(Default)]
pub struct Modules {
    pub fleet: Option<Box<dyn Component>>,
    pub pattern: Option<Box<dyn Component>>,
    pub gateway: Option<Box<dyn Component>>,
    pub marketplaces: Option<Box<dyn Component>>,
}

/// Assembles the standard Studio shell: Fleet, Pattern, Gateway and
/// Marketplaces, on keys 1-4, with `focus` focused (default: the first real
/// module).
pub fn studio_shell(
    title: impl Into<String>,
    m: Modules,
    focus: Option<ModuleId>,
) -> (App, tokio::sync::mpsc::UnboundedReceiver<Msg>) {
    let first_real = [
        (m.fleet.is_some(), fleet::ID),
        (m.pattern.is_some(), pattern::ID),
        (m.gateway.is_some(), gateway::ID),
        (m.marketplaces.is_some(), marketplace::ID),
    ]
    .into_iter()
    .find_map(|(real, id)| real.then_some(id));
    let modules: Vec<Box<dyn Component>> = vec![
        m.fleet
            .unwrap_or_else(|| Box::new(modules::Placeholder::new(fleet::ID, "Fleet"))),
        m.pattern
            .unwrap_or_else(|| Box::new(modules::Placeholder::new(pattern::ID, "Pattern"))),
        m.gateway
            .unwrap_or_else(|| Box::new(modules::Placeholder::new(gateway::ID, "Gateway"))),
        m.marketplaces.unwrap_or_else(|| {
            Box::new(modules::Placeholder::new(marketplace::ID, "Marketplaces"))
        }),
    ];
    let (mut app, rx) = App::new(title, modules);
    if let Some(id) = focus.or(first_real) {
        app.focus_id(id);
    }
    (app, rx)
}

/// The standard shell with only the Gateway module real (tests and the demo).
pub fn studio_app(
    title: impl Into<String>,
    gateway: gateway::GatewayModule,
) -> (App, tokio::sync::mpsc::UnboundedReceiver<Msg>) {
    studio_shell(
        title,
        Modules {
            gateway: Some(Box::new(gateway)),
            ..Modules::default()
        },
        Some(gateway::ID),
    )
}
