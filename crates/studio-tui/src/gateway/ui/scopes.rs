//! The Scopes screen: `list_scope_owners` — which server claims each scope.
//! A scope claimed by more than one server is marked: the gateway mints it
//! for both.

use std::collections::BTreeMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Row, Table, TableState};

use crate::framework::widgets::{slot_placeholder as placeholder, stale_banner};
use crate::gateway::pane::Pane;

pub fn draw(frame: &mut Frame, app: &Pane, area: Rect) {
    if placeholder(frame, area, &app.scopes, "scope owners") {
        return;
    }
    let Some(owners) = &app.scopes.data else {
        return;
    };
    let mut claims: BTreeMap<&str, usize> = BTreeMap::new();
    for o in owners {
        *claims.entry(o.scope.as_str()).or_default() += 1;
    }
    let shared = claims.values().filter(|n| **n > 1).count();
    let [summary, table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(2)]).areas(area);
    let mut spans = vec![Span::from(format!(
        " {} scopes claimed by {} servers",
        claims.len(),
        owners
            .iter()
            .map(|o| o.server.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    ))];
    if shared > 0 {
        spans.push(
            Span::from(format!("  {shared} claimed by more than one server")).fg(Color::Yellow),
        );
    }
    frame.render_widget(
        stale_banner(&app.scopes).unwrap_or(Line::from(spans)),
        summary,
    );
    let rows = owners.iter().map(|o| {
        let many = claims.get(o.scope.as_str()).copied().unwrap_or(0) > 1;
        Row::new(vec![
            Cell::from(if many {
                Span::from(o.scope.clone()).fg(Color::Yellow)
            } else {
                Span::from(o.scope.clone())
            }),
            Cell::from(o.server.clone()),
            Cell::from(if many { "shared" } else { "" }).dim(),
        ])
    });
    let width = owners
        .iter()
        .map(|o| o.scope.len())
        .max()
        .unwrap_or(5)
        .max(5) as u16;
    let table = Table::new(
        rows,
        [
            Constraint::Length(width),
            Constraint::Min(10),
            Constraint::Length(8),
        ],
    )
    .header(Row::new(["SCOPE", "SERVER", ""]).style(Style::new().add_modifier(Modifier::BOLD)))
    .block(Block::bordered().title(format!(" Scope owners ({}) ", owners.len())))
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(Some(app.scope_selected));
    frame.render_stateful_widget(table, table_area, &mut state);
}
