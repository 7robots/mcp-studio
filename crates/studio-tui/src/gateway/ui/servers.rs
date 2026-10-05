//! The Servers screen: the registry as a table, and the selected server in a
//! detail pane beside it (below it on a narrow terminal).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap};

use crate::framework::widgets::{slot_placeholder as placeholder, stale_banner};
use crate::gateway::pane::Pane;
use crate::gateway::pane::{class_color, health_color};
use studio_gateway::actions::ScopeDrift;
use studio_gateway::model::Server;
use studio_gateway::util::{ago, now_unix};

/// Wide enough for the table and the detail side by side.
const SIDE_BY_SIDE: u16 = 110;

pub fn draw(frame: &mut Frame, app: &Pane, area: Rect) {
    if placeholder(frame, area, &app.servers, "servers") {
        return;
    }
    let area = match stale_banner(&app.servers) {
        Some(banner) => {
            let [top, rest] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
            frame.render_widget(banner, top);
            rest
        }
        None => area,
    };
    let (table_area, detail_area) = if area.width >= SIDE_BY_SIDE {
        let [a, b] = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .areas(area);
        (a, b)
    } else {
        let [a, b] =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
        (a, b)
    };
    draw_table(frame, app, table_area);
    if let Some(server) = app.selected_server() {
        draw_detail(frame, server, app.drift.get(&server.id), detail_area);
    }
}

fn draw_table(frame: &mut Frame, app: &Pane, area: Rect) {
    let now = now_unix();
    let servers = app
        .servers
        .data
        .as_ref()
        .map(|l| l.servers.as_slice())
        .unwrap_or(&[]);
    let rows = servers.iter().map(|s| {
        let status_color = match s.status.as_str() {
            "active" => Color::Reset,
            "disabled" => Color::DarkGray,
            _ => Color::Red,
        };
        let access = if s.read_only() {
            Span::from("RO").fg(Color::Yellow)
        } else {
            Span::from("RW").dim()
        };
        let error = s
            .last_call_error
            .as_deref()
            .or(s.last_error.as_deref())
            .unwrap_or("");
        let auth_color = match s.connection.as_deref() {
            Some("connected") => Color::Green,
            Some("needs_connect") => Color::Yellow,
            Some("unavailable") => Color::DarkGray,
            _ => Color::Reset,
        };
        Row::new(vec![
            Cell::from(s.id.clone()),
            Cell::from(Span::from(s.status.clone()).fg(status_color)),
            Cell::from(Span::from(s.health.clone()).fg(health_color(&s.health))),
            Cell::from(access),
            Cell::from(Span::from(s.auth_label()).fg(auth_color)),
            Cell::from(s.tools.to_string()),
            Cell::from(ago(s.last_call_at, now)),
            Cell::from(Span::from(error.to_string()).fg(Color::Red)),
        ])
    });
    let header = Row::new([
        "ID",
        "STATUS",
        "HEALTH",
        "ACCESS",
        "AUTH",
        "TOOLS",
        "LAST CALL",
        "LAST ERROR",
    ])
    .style(Style::new().add_modifier(Modifier::BOLD));
    let widths = [
        Constraint::Length(servers.iter().map(|s| s.id.len()).max().unwrap_or(2).max(2) as u16),
        Constraint::Length(11),
        Constraint::Length(9),
        Constraint::Length(6),
        Constraint::Length(12),
        Constraint::Length(5),
        Constraint::Length(9),
        Constraint::Min(10),
    ];
    let title = format!(" Servers ({}) ", servers.len());
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::bordered().title(title))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn field(label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::from(format!("{label:<13}")).dim(),
        Span::from(value.into()),
    ])
}

fn draw_detail(frame: &mut Frame, s: &Server, drift: Option<&ScopeDrift>, area: Rect) {
    let now = now_unix();
    let mut lines = vec![Line::from(s.name.clone()).bold()];
    if let Some(description) = &s.description {
        lines.push(Line::from(description.clone()).dim());
    }
    lines.push(Line::from(""));
    lines.push(field("url", s.url.clone()));
    lines.push(Line::from(vec![
        Span::from(format!("{:<13}", "status")).dim(),
        Span::from(s.status.clone()),
        Span::from(" / "),
        Span::from(s.health.clone()).fg(health_color(&s.health)),
    ]));
    let access = if s.read_only() {
        let total = s.tool_classes.len();
        Line::from(vec![
            Span::from(format!("{:<13}", "access")).dim(),
            Span::from("read-only").fg(Color::Yellow),
            Span::from(format!(" ({} of {total} tools callable)", s.read_tools())),
        ])
    } else {
        field("access", "read-write")
    };
    lines.push(access);
    if let Some(mode) = &s.auth_mode {
        let value = match s.connection.as_deref() {
            Some("connected") => format!("{mode} (you are connected)"),
            Some("needs_connect") => {
                format!("{mode} (you have not connected; a call returns the link)")
            }
            Some("unavailable") => format!("{mode} (a service caller cannot call it)"),
            _ => mode.clone(),
        };
        lines.push(field("auth", value));
    }
    lines.push(field("timeout", format!("{} ms", s.timeout_ms)));
    lines.push(field(
        "scopes",
        s.scopes.clone().unwrap_or_else(|| "-".into()),
    ));
    if let Some(drift) = drift {
        lines.push(Line::from(vec![
            Span::from(format!("{:<13}", "advertises")).dim(),
            Span::from(drift.advertised.join(" ")).fg(Color::Yellow),
            Span::from("  (scope drift; A to approve)").dim(),
        ]));
    }
    lines.push(field("refreshed", ago(s.last_refresh_at, now)));
    lines.push(field("last call", ago(s.last_call_at, now)));
    if s.call_failures > 0 {
        lines.push(field("call failures", s.call_failures.to_string()));
    }
    for (label, error) in [
        ("call error", &s.last_call_error),
        ("refresh error", &s.last_error),
    ] {
        if let Some(error) = error {
            lines.push(Line::from(vec![
                Span::from(format!("{label:<13}")).dim(),
                Span::from(error.clone()).fg(Color::Red),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(format!("Tools ({})", s.tool_classes.len())).bold());
    let width = s
        .tool_classes
        .iter()
        .map(|t| t.name.len())
        .max()
        .unwrap_or(0);
    for tool in &s.tool_classes {
        let callable = !s.read_only() || tool.classification == "read";
        let name = Span::from(format!("  {:<width$}  ", tool.name));
        lines.push(Line::from(vec![
            if callable {
                name
            } else {
                name.dim().add_modifier(Modifier::CROSSED_OUT)
            },
            Span::from(format!("{:<12}", tool.classification))
                .fg(class_color(&tool.classification)),
            Span::from(tool.source.clone()).dim(),
        ]));
    }
    let block = Block::bordered().title(format!(" {} ", s.id));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}
