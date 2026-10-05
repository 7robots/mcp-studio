//! The Gateway module: the gateway's registry (Servers), `usage_stats`
//! (Usage) and `policy_events` (Policy), and the nine admin actions on the
//! Servers screen for an admin sign-in.
//!
//! One [`pane::Pane`] per `[[gateway]]`, each with its own session and state;
//! with more than one, `p` opens a picker. A missing or expired sign-in is
//! fixed in place: `L` runs the browser sign-in in a task, through the
//! [`Opener`] the module was built with.

pub mod gate;
pub mod keys;
pub mod pane;
mod ui;

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Stylize};
use ratatui::text::Line;
use studio_gateway::util::{ago, now_unix};
use studio_gateway::{Session, Url};

use crate::framework::component::{Component, Handled, ModuleId};
use crate::framework::cx::{Cx, Payload};
use crate::framework::keymap::Hint;
use crate::framework::overlay::{Overlay, PickItem, Picker, Step};
use crate::framework::widgets::message;

use keys::Key;
use pane::PaneMsg;
pub use pane::{Pane, Screen};

/// The module's id.
pub const ID: ModuleId = "gateway";

/// Shows the sign-in page to the user: opens the browser in production; in
/// the demo and the tests, a redirect-following GET.
pub type Opener = Arc<dyn Fn(&Url) -> anyhow::Result<()> + Send + Sync>;

/// A message for one pane.
#[derive(Debug)]
pub(crate) struct GwMsg {
    pub pane: usize,
    pub body: PaneMsg,
}

pub struct GatewayModule {
    panes: Vec<Pane>,
    active: usize,
    picker: Option<Picker>,
    opener: Opener,
    login_timeout: Duration,
}

impl GatewayModule {
    /// One pane per session, in order; the first is shown first.
    pub fn new(sessions: Vec<Arc<Session>>, opener: Opener) -> GatewayModule {
        GatewayModule {
            panes: sessions
                .into_iter()
                .enumerate()
                .map(|(i, s)| Pane::new(i, s))
                .collect(),
            active: 0,
            picker: None,
            opener,
            login_timeout: studio_gateway::oauth::LOGIN_TIMEOUT,
        }
    }

    /// How long an in-TUI sign-in waits for the browser.
    pub fn with_login_timeout(mut self, timeout: Duration) -> GatewayModule {
        self.login_timeout = timeout;
        self
    }

    /// The pane on screen.
    pub fn pane(&self) -> Option<&Pane> {
        self.panes.get(self.active)
    }

    pub fn panes(&self) -> &[Pane] {
        &self.panes
    }

    pub fn active(&self) -> usize {
        self.active
    }

    /// The gateway picker, while open.
    pub fn picker(&self) -> Option<&Picker> {
        self.picker.as_ref()
    }

    fn open_picker(&mut self) {
        let items = self
            .panes
            .iter()
            .map(|p| PickItem::tagged(p.id(), p.host(), Color::DarkGray))
            .collect();
        self.picker = Some(Picker::new("Gateways", items, "switch").with_selected(self.active));
    }

    fn switch(&mut self, index: usize, cx: &mut Cx<'_>) {
        if let Some(pane) = self.panes.get_mut(index) {
            self.active = index;
            pane.start(cx);
        }
    }
}

impl Component for GatewayModule {
    fn id(&self) -> ModuleId {
        ID
    }

    fn title(&self) -> String {
        "Gateway".into()
    }

    fn start(&mut self, cx: &mut Cx<'_>) {
        if let Some(pane) = self.panes.get_mut(self.active) {
            pane.start(cx);
        }
    }

    fn handle_key(&mut self, key: KeyEvent, cx: &mut Cx<'_>) -> Handled {
        if let Some(picker) = &mut self.picker {
            match picker.handle_key(key) {
                Step::Open => {}
                Step::Cancel => self.picker = None,
                Step::Done(i) => {
                    self.picker = None;
                    self.switch(i, cx);
                }
            }
            return Handled::Yes;
        }
        let count = self.panes.len();
        let Some(pane) = self.panes.get_mut(self.active) else {
            return Handled::No;
        };
        if pane.overlay.is_none() && pane.keymap(count).resolve(&key) == Some(Key::PickGateway) {
            self.open_picker();
            return Handled::Yes;
        }
        pane.handle_key(key, cx, count, &self.opener, self.login_timeout)
    }

    fn handle_msg(&mut self, msg: Payload, cx: &mut Cx<'_>) {
        if let Ok(msg) = msg.downcast::<GwMsg>()
            && let Some(pane) = self.panes.get_mut(msg.pane)
        {
            pane.handle_msg(msg.body, cx);
        }
    }

    fn tick(&mut self, now: Instant, cx: &mut Cx<'_>) {
        // Only the pane on screen polls.
        if let Some(pane) = self.panes.get_mut(self.active) {
            pane.tick(now, cx);
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.pane().and_then(Pane::next_deadline)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        match self.panes.get(self.active) {
            Some(pane) => ui::draw(frame, pane, area, (self.active, self.panes.len())),
            None => message(
                frame,
                area,
                vec![
                    Line::from("No gateway is configured."),
                    Line::from("Add a [[gateway]] table to the instance's studio.toml.").dim(),
                ],
            ),
        }
        if let Some(picker) = &self.picker {
            Overlay::Picker(picker.clone()).draw(frame);
        }
    }

    fn keymap(&self) -> Vec<Hint> {
        self.pane()
            .map(|p| p.keymap(self.panes.len()).hints())
            .unwrap_or_default()
    }

    fn captures_input(&self) -> bool {
        self.picker.is_some() || self.pane().is_some_and(|p| p.overlay.is_some())
    }

    fn status(&self) -> Option<String> {
        let pane = self.pane()?;
        let at = match pane.screen {
            Screen::Servers => pane.servers.loaded_at,
            Screen::Usage => pane.usage.loaded_at,
            Screen::Policy => pane.policy.loaded_at,
        }?;
        Some(format!("updated {}", ago(Some(at), now_unix())))
    }
}
