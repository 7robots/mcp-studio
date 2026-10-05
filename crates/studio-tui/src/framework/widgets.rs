//! Small drawing helpers shared by modules.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::slot::Slot;

/// A `width` x `height` rectangle in the middle of `area` (clamped to it).
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    cell
}

/// A short message in the middle of `area`.
pub fn message(frame: &mut Frame, area: Rect, lines: Vec<Line>) {
    let height = (lines.len() as u16).min(area.height);
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    frame.render_widget(
        Paragraph::new(lines).centered().wrap(Wrap { trim: true }),
        row,
    );
}

/// Loading and error lines for a slot with no data yet. Returns whether it
/// drew (so the caller draws nothing else).
pub fn slot_placeholder<T>(frame: &mut Frame, area: Rect, slot: &Slot<T>, what: &str) -> bool {
    if slot.data.is_some() {
        return false;
    }
    let line = match &slot.error {
        Some(error) => Line::from(format!("Could not load {what}: {error}")).fg(Color::Red),
        None => Line::from(format!("Loading {what}...")).dim(),
    };
    message(frame, area, vec![line]);
    true
}

/// The error of a failed reload, shown above data that is now stale.
pub fn stale_banner<T>(slot: &Slot<T>) -> Option<Line<'static>> {
    slot.error
        .as_ref()
        .map(|e| Line::from(format!(" Refresh failed, showing earlier data: {e}")).fg(Color::Red))
}
