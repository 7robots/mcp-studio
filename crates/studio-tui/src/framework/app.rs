//! The shell: modules on number keys, a tab strip with the instance's name, a
//! footer with key hints, the toast and the module's status, and a help
//! overlay generated from the same keymaps that dispatch the keys.
//!
//! `App` never touches the terminal: it takes key events, [`Msg`]s from the
//! tasks modules spawn, and `tick(now)` for timers; `draw` renders it into any
//! ratatui frame (the real terminal in [`super::run::run`], a test backend in
//! [`super::harness::Harness`]).

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::component::{Component, Handled, ModuleId};
use super::cx::{Cx, Msg};
use super::keymap::{Hint, Keymap, footer_text, help_sections};
use super::overlay::draw_box;
use super::toast::{Toast, Toasts};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellAction {
    Module(usize),
    Help,
    Quit,
}

pub struct App {
    title: String,
    modules: Vec<Box<dyn Component>>,
    started: Vec<bool>,
    active: usize,
    toasts: Toasts,
    help: bool,
    quit: bool,
    tx: UnboundedSender<Msg>,
}

impl App {
    /// An app over `modules`, in tab order (the first is on key `1`). Nothing
    /// starts until [`App::start`].
    pub fn new(
        title: impl Into<String>,
        modules: Vec<Box<dyn Component>>,
    ) -> (App, UnboundedReceiver<Msg>) {
        let (tx, rx) = unbounded_channel();
        let started = vec![false; modules.len()];
        let app = App {
            title: title.into(),
            modules,
            started,
            active: 0,
            toasts: Toasts::default(),
            help: false,
            quit: false,
            tx,
        };
        (app, rx)
    }

    /// Starts the focused module.
    pub fn start(&mut self) {
        self.ensure_started(self.active);
    }

    pub fn running(&self) -> bool {
        !self.quit
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn help_open(&self) -> bool {
        self.help
    }

    pub fn toast(&self) -> Option<&Toast> {
        self.toasts.current()
    }

    pub fn active_id(&self) -> Option<ModuleId> {
        self.modules.get(self.active).map(|m| m.id())
    }

    /// The module of type `T`, for tests and tools that drive one directly.
    pub fn module<T: 'static>(&self) -> Option<&T> {
        self.modules
            .iter()
            .find_map(|m| m.as_any().downcast_ref::<T>())
    }

    pub fn module_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.modules
            .iter_mut()
            .find_map(|m| m.as_any_mut().downcast_mut::<T>())
    }

    /// Focuses the module with `id` (starting it the first time).
    pub fn focus_id(&mut self, id: ModuleId) {
        if let Some(i) = self.modules.iter().position(|m| m.id() == id) {
            self.focus(i);
        }
    }

    fn focus(&mut self, index: usize) {
        if index >= self.modules.len() {
            return;
        }
        self.active = index;
        // Before `start()`, focusing only chooses the first module.
        if self.started.iter().any(|s| *s) {
            self.ensure_started(index);
        }
    }

    fn ensure_started(&mut self, index: usize) {
        if self.started.get(index) == Some(&false) {
            self.started[index] = true;
            self.with_module(index, |m, cx| m.start(cx));
        }
    }

    /// Runs `f` on module `index` with its context.
    fn with_module<R>(
        &mut self,
        index: usize,
        f: impl FnOnce(&mut dyn Component, &mut Cx<'_>) -> R,
    ) -> Option<R> {
        let focused = index == self.active;
        let module = self.modules.get_mut(index)?;
        let mut cx = Cx {
            id: module.id(),
            tx: &self.tx,
            toasts: &mut self.toasts,
            quit: &mut self.quit,
            focused,
        };
        Some(f(module.as_mut(), &mut cx))
    }

    fn keymap(&self) -> Keymap<ShellAction> {
        let mut km = Keymap::new("Studio");
        for (i, m) in self.modules.iter().enumerate().take(9) {
            let key = char::from(b'1' + i as u8).to_string();
            km = km.bind(
                &[key.as_str()],
                format!("{} module", m.title()),
                ShellAction::Module(i),
            );
        }
        let n = self.modules.len().min(9);
        if n > 1 {
            km = km.footer(&format!("1-{n}"), "modules");
        }
        km.bind(&["?"], "this help", ShellAction::Help)
            .footer("?", "help")
            .bind(&["q", "esc"], "quit", ShellAction::Quit)
            .footer("q", "quit")
            .bind(&["ctrl+c"], "quit, from anywhere", ShellAction::Quit)
    }

    /// Every key that applies now: the shell's, then the focused module's.
    pub fn hints(&self) -> Vec<Hint> {
        let mut hints = self
            .modules
            .get(self.active)
            .map(|m| m.keymap())
            .unwrap_or_default();
        hints.extend(self.keymap().hints());
        hints
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.help {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
            ) {
                self.help = false;
            }
            return;
        }
        let captured = self
            .modules
            .get(self.active)
            .is_some_and(|m| m.captures_input());
        let shell = self.keymap().resolve(&key);
        if !captured {
            match shell {
                Some(ShellAction::Module(i)) => return self.focus(i),
                Some(ShellAction::Help) => {
                    self.help = true;
                    return;
                }
                _ => {}
            }
        }
        let handled = self
            .with_module(self.active, |m, cx| m.handle_key(key, cx))
            .unwrap_or(Handled::No);
        if handled == Handled::No && !captured && shell == Some(ShellAction::Quit) {
            self.quit = true;
        }
    }

    /// Delivers a message to the module it is addressed to.
    pub fn handle_msg(&mut self, msg: Msg) {
        if let Some(i) = self.modules.iter().position(|m| m.id() == msg.to) {
            self.with_module(i, |m, cx| m.handle_msg(msg.payload, cx));
        }
    }

    pub fn tick(&mut self, now: Instant) {
        self.toasts.expire(now);
        for i in 0..self.modules.len() {
            if self.started[i] {
                self.with_module(i, |m, cx| m.tick(now, cx));
            }
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.modules
            .iter()
            .zip(&self.started)
            .filter(|(_, started)| **started)
            .filter_map(|(m, _)| m.next_deadline())
            .chain(self.toasts.next_deadline())
            .min()
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        self.draw_header(frame, header);
        if let Some(m) = self.modules.get_mut(self.active) {
            m.draw(frame, body);
        }
        self.draw_footer(frame, footer);
        if self.help {
            self.draw_help(frame);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let left = Line::from(vec![
            Span::from(" MCP Studio ").bold(),
            Span::from(self.title.clone()).dim(),
        ]);
        let tabs: Vec<Span> = self
            .modules
            .iter()
            .enumerate()
            .flat_map(|(i, m)| {
                let label = format!(" {} {} ", i + 1, m.title());
                let span = if i == self.active {
                    Span::styled(
                        label,
                        Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
                    )
                } else {
                    Span::from(label)
                };
                [span, Span::from(" ")]
            })
            .collect();
        let width: u16 = tabs.iter().map(|s| s.width() as u16).sum();
        let [l, r] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(width)]).areas(area);
        frame.render_widget(left, l);
        frame.render_widget(Line::from(tabs), r);
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        // A toast takes the whole line: an error must be readable in full.
        if let Some(toast) = self.toasts.current() {
            let line = Line::from(format!(" {}", toast.text));
            frame.render_widget(
                if toast.error {
                    line.fg(Color::Red).bold()
                } else {
                    line
                },
                area,
            );
            return;
        }
        let status = self
            .modules
            .get(self.active)
            .and_then(|m| m.status())
            .unwrap_or_default();
        let width = status.chars().count() as u16 + 1;
        let [l, r] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(width)]).areas(area);
        frame.render_widget(
            Line::from(format!(" {}", footer_text(&self.hints()))).dim(),
            l,
        );
        frame.render_widget(Line::from(status), r);
    }

    fn draw_help(&self, frame: &mut Frame) {
        let mut lines = Vec::new();
        for (section, entries) in help_sections(&self.hints()) {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(section).bold().underlined());
            for (keys, help) in entries {
                lines.push(Line::from(vec![
                    Span::from(format!("{keys:>12}  ")).bold(),
                    Span::from(help),
                ]));
            }
        }
        draw_box(frame, "Keys", lines);
    }
}
