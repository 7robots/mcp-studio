//! A headless driver for [`App`]: injected keys, a test backend to draw into,
//! and a wait loop that runs the message and timer plumbing the real loop
//! would. Tests read the drawn buffer as text. In the library rather than
//! under `tests/` so tools (the gateway's acceptance gate) can drive the real
//! app the same way.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc::UnboundedReceiver;

use super::app::App;
use super::cx::Msg;

pub struct Harness {
    pub app: App,
    rx: UnboundedReceiver<Msg>,
    terminal: Terminal<TestBackend>,
}

impl Harness {
    /// Starts `app` and draws it once into a `size` (columns, rows) backend.
    pub fn new(mut app: App, rx: UnboundedReceiver<Msg>, size: (u16, u16)) -> Harness {
        app.start();
        let terminal = Terminal::new(TestBackend::new(size.0, size.1)).expect("test backend");
        let mut harness = Harness { app, rx, terminal };
        harness.draw();
        harness
    }

    pub fn draw(&mut self) {
        let app = &mut self.app;
        self.terminal.draw(|frame| app.draw(frame)).expect("draw");
    }

    fn pump(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.app.handle_msg(msg);
        }
        self.app.tick(Instant::now());
        self.draw();
    }

    /// Runs the plumbing until `pred` holds or `timeout` passes.
    pub async fn wait_until(
        &mut self,
        pred: impl Fn(&App) -> bool,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump();
            if pred(&self.app) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "condition not met within {timeout:?}\n{}",
                    self.text()
                ));
            }
            let next = self
                .app
                .next_deadline()
                .unwrap_or(Instant::now() + Duration::from_millis(25));
            let step = next
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(25));
            tokio::select! {
                msg = self.rx.recv() => {
                    if let Some(msg) = msg { self.app.handle_msg(msg); }
                }
                _ = tokio::time::sleep(step) => {}
            }
        }
    }

    /// `wait_until` with five seconds, panicking with the screen on timeout.
    pub async fn until(&mut self, pred: impl Fn(&App) -> bool) {
        if let Err(err) = self.wait_until(pred, Duration::from_secs(5)).await {
            panic!("{err}");
        }
    }

    /// Waits until the drawn screen contains `needle`.
    pub async fn until_text(&mut self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.text().contains(needle) {
            if Instant::now() >= deadline {
                panic!("{needle:?} never appeared:\n{}", self.text());
            }
            let _ = self.wait_until(|_| false, Duration::from_millis(20)).await;
        }
    }

    /// Lets queued work drain for a moment.
    pub async fn settle(&mut self) {
        let _ = self.wait_until(|_| false, Duration::from_millis(60)).await;
    }

    pub fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        self.app.handle_key(KeyEvent::new(code, modifiers));
        self.draw();
    }

    /// Presses a key by name (`enter`, `esc`, `tab`, `shift+tab`, `down`, `up`,
    /// `backspace`, `ctrl+c`) or by its character; an uppercase character
    /// carries SHIFT.
    pub fn press(&mut self, name: &str) {
        let (code, modifiers) = match name {
            "enter" => (KeyCode::Enter, KeyModifiers::NONE),
            "esc" => (KeyCode::Esc, KeyModifiers::NONE),
            "tab" => (KeyCode::Tab, KeyModifiers::NONE),
            "shift+tab" => (KeyCode::BackTab, KeyModifiers::SHIFT),
            "down" => (KeyCode::Down, KeyModifiers::NONE),
            "up" => (KeyCode::Up, KeyModifiers::NONE),
            "backspace" => (KeyCode::Backspace, KeyModifiers::NONE),
            "ctrl+c" => (KeyCode::Char('c'), KeyModifiers::CONTROL),
            other => {
                let ch = other.chars().next().expect("a key name");
                (
                    KeyCode::Char(ch),
                    if ch.is_uppercase() {
                        KeyModifiers::SHIFT
                    } else {
                        KeyModifiers::NONE
                    },
                )
            }
        };
        self.key(code, modifiers);
    }

    /// Types each character as a key press.
    pub fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            let modifiers = if ch.is_uppercase() {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };
            self.key(KeyCode::Char(ch), modifiers);
        }
    }

    /// Presses backspace `n` times.
    pub fn erase(&mut self, n: usize) {
        for _ in 0..n {
            self.press("backspace");
        }
    }

    /// The screen as text, one string per row, trailing spaces trimmed.
    pub fn lines(&self) -> Vec<String> {
        let buffer = self.terminal.backend().buffer();
        let area = buffer.area;
        (0..area.height)
            .map(|y| {
                let mut row = String::new();
                for x in 0..area.width {
                    if !self.is_continuation(x, y) {
                        row.push_str(buffer[(x, y)].symbol());
                    }
                }
                row.trim_end().to_string()
            })
            .collect()
    }

    pub fn text(&self) -> String {
        self.lines().join("\n")
    }

    /// The first row containing `needle`.
    pub fn row_with(&self, needle: &str) -> Option<String> {
        self.lines().into_iter().find(|l| l.contains(needle))
    }

    /// Is `(x, y)` the cell hidden under a wide glyph to its left? The test
    /// backend keeps stale content there; a real terminal paints over it.
    fn is_continuation(&self, x: u16, y: u16) -> bool {
        if x == 0 {
            return false;
        }
        let left = self.terminal.backend().buffer()[(x - 1, y)].symbol();
        unicode_width::UnicodeWidthStr::width(left) == 2
    }
}
