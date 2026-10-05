use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;

use crate::framework::component::{Component, Handled, ModuleId};
use crate::framework::cx::Cx;
use crate::framework::widgets::message;

/// A module slot that says it is not wired yet.
pub struct Placeholder {
    id: ModuleId,
    title: String,
}

impl Placeholder {
    pub fn new(id: ModuleId, title: &str) -> Placeholder {
        Placeholder {
            id,
            title: title.into(),
        }
    }
}

impl Component for Placeholder {
    fn id(&self) -> ModuleId {
        self.id
    }

    fn title(&self) -> String {
        self.title.clone()
    }

    fn handle_key(&mut self, _key: KeyEvent, _cx: &mut Cx<'_>) -> Handled {
        Handled::No
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        message(
            frame,
            area,
            vec![
                Line::from(format!("{}: not wired yet", self.title)).bold(),
                Line::from("This module has not been built into the TUI yet.").dim(),
            ],
        );
    }
}
