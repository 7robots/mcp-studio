//! The trait every Studio module implements.

use std::any::Any;
use std::time::Instant;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;

use super::cx::{Cx, Payload};
use super::keymap::Hint;

/// A module's stable id (`"gateway"`). Messages are addressed by it.
pub type ModuleId = &'static str;

/// Whether a component used a key. An unused key falls through to the shell
/// (`q`/`Esc` quit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    Yes,
    No,
}

/// Lets the shell hand a concrete module back to tests (`App::module::<T>()`).
/// Implemented for every `'static` type; nothing to write.
pub trait AsAny {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: Any> AsAny for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A module of the Studio TUI (Fleet, Pattern, Gateway, Marketplaces).
///
/// The shell owns the terminal; a component never touches it. It gets:
///
/// - **keys** in [`Component::handle_key`], after the shell has taken its own
///   (`Ctrl-c`, the module numbers `1`-`9`, `?`). A key the component does not
///   use (returns [`Handled::No`]) falls through to `q`/`Esc` = quit. While
///   [`Component::captures_input`] is true (a form or modal is open) every key
///   but `Ctrl-c` goes to the component, so typing `1` or `q` into a form works.
/// - **messages** in [`Component::handle_msg`]: whatever its own async tasks
///   sent back. Spawn work with [`Cx::spawn`] (the future's output is delivered
///   here) or send through a [`Cx::sender`] from inside a task. The payload is
///   the component's own message type, boxed; downcast it:
///   `if let Ok(m) = msg.downcast::<MyMsg>() { ... }`.
/// - **time** in [`Component::tick`], called on every loop turn with `now`; say
///   when it next needs one with [`Component::next_deadline`] (polling).
///
/// It describes itself with [`Component::keymap`] (footer hints and the help
/// overlay are generated from it — build it from the same
/// [`super::keymap::Keymap`] the component dispatches with) and
/// [`Component::status`] (right side of the footer), and draws into the area
/// below the shell's tab strip in [`Component::draw`].
///
/// Register a module by putting it in the `Vec<Box<dyn Component>>` given to
/// [`super::app::App::new`] (or in [`crate::studio_app`]); its position is its
/// number key.
pub trait Component: AsAny + Send {
    /// Stable id; messages are routed by it. Unique within an app.
    fn id(&self) -> ModuleId;

    /// The name in the tab strip.
    fn title(&self) -> String;

    /// Called once, the first time the module is focused: start loads here.
    fn start(&mut self, _cx: &mut Cx<'_>) {}

    /// A key press (see the trait docs for which keys arrive).
    fn handle_key(&mut self, key: KeyEvent, cx: &mut Cx<'_>) -> Handled;

    /// A message one of this module's tasks sent.
    fn handle_msg(&mut self, _msg: Payload, _cx: &mut Cx<'_>) {}

    /// Every loop turn. `cx.focused()` says whether the module is on screen.
    fn tick(&mut self, _now: Instant, _cx: &mut Cx<'_>) {}

    /// When the next `tick` must happen at the latest (a poll, an animation).
    fn next_deadline(&self) -> Option<Instant> {
        None
    }

    /// Draws the module into `area` (everything between the tab strip and
    /// the footer).
    fn draw(&mut self, frame: &mut Frame, area: Rect);

    /// The keys that apply right now: the footer shows those with a footer
    /// label, the help overlay shows all of them under their sections.
    fn keymap(&self) -> Vec<Hint> {
        Vec::new()
    }

    /// True while a modal or text field owns the keyboard.
    fn captures_input(&self) -> bool {
        false
    }

    /// Right side of the footer (e.g. `updated 12s ago`).
    fn status(&self) -> Option<String> {
        None
    }
}
