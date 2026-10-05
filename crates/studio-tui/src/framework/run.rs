//! The real terminal loop: raw mode and the alternate screen, crossterm
//! events, the app's message channel and its timers.

use std::io::stdout;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc::UnboundedReceiver;

use super::app::App;
use super::cx::Msg;

/// Raw mode and the alternate screen, undone on drop and before a panic
/// message prints, so the message is readable.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<TerminalGuard> {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            hook(info);
        }));
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen)?;
        Ok(TerminalGuard)
    }
}

fn restore_terminal() {
    let _ = execute!(stdout(), LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Runs `app` on the terminal until it quits.
pub async fn run(mut app: App, mut rx: UnboundedReceiver<Msg>) -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut events = EventStream::new();
    app.start();
    while app.running() {
        terminal.draw(|frame| app.draw(frame))?;
        let deadline = app
            .next_deadline()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => app.handle_key(key),
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(err.into()),
                None => break,
            },
            msg = rx.recv() => match msg {
                Some(msg) => app.handle_msg(msg),
                None => break,
            },
            _ = tokio::time::sleep_until(deadline.into()) => {}
        }
        while let Ok(msg) = rx.try_recv() {
            app.handle_msg(msg);
        }
        app.tick(Instant::now());
    }
    Ok(())
}
