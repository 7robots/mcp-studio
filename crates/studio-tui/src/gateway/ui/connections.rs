//! The Connections screen: `list_connections` — the metadata of each stored
//! downstream credential (never the credential), filterable by kind.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Row, Table, TableState};

use crate::framework::widgets::{slot_placeholder as placeholder, stale_banner};
use crate::gateway::pane::Pane;
use studio_gateway::model::{cell, when};
use studio_gateway::util::now_unix;

pub fn draw(frame: &mut Frame, app: &Pane, area: Rect) {
    if placeholder(frame, area, &app.connections, "connections") {
        return;
    }
    let shown = app.shown_connections();
    let total = app.connections.data.as_ref().map_or(0, Vec::len);
    let [summary, table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(2)]).areas(area);
    let line = Line::from(format!(
        " {} of {total} connections  kind: {}",
        shown.len(),
        app.connection_kind.as_deref().unwrap_or("all")
    ));
    frame.render_widget(stale_banner(&app.connections).unwrap_or(line), summary);
    let now = now_unix();
    let rows = shown.iter().map(|c| {
        Row::new(vec![
            Cell::from(c.subject.clone()),
            Cell::from(c.kind.clone().unwrap_or_else(|| "-".into())),
            Cell::from(cell(c.version.as_ref())),
            Cell::from(c.key_id.clone().unwrap_or_else(|| "-".into())),
            Cell::from(when(c.updated_at.as_ref(), now)),
            Cell::from(when(c.expires_at.as_ref(), now)),
            Cell::from(c.issuer.clone().unwrap_or_default()).dim(),
        ])
    });
    let width = shown
        .iter()
        .map(|c| c.subject.len())
        .max()
        .unwrap_or(7)
        .max(7) as u16;
    let table = Table::new(
        rows,
        [
            Constraint::Length(width),
            Constraint::Length(10),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Min(10),
        ],
    )
    .header(
        Row::new([
            "SUBJECT", "KIND", "VERSION", "KEY", "UPDATED", "EXPIRES", "ISSUER",
        ])
        .style(Style::new().add_modifier(Modifier::BOLD)),
    )
    .block(Block::bordered().title(format!(" Connections ({}) ", shown.len())))
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(Some(app.connection_selected));
    frame.render_stateful_widget(table, table_area, &mut state);
}
