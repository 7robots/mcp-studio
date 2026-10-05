//! The Usage screen: `usage_stats` — calls, failures and latency per tool, and
//! the most recent runs.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Row, Table};

use crate::framework::widgets::{slot_placeholder as placeholder, stale_banner};
use crate::gateway::pane::Pane;
use studio_gateway::util::{ago, now_unix};

pub fn draw(frame: &mut Frame, app: &Pane, area: Rect) {
    if placeholder(frame, area, &app.usage, "usage") {
        return;
    }
    let Some(usage) = &app.usage.data else { return };
    let [summary, tools, runs] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Percentage(55),
        Constraint::Min(4),
    ])
    .areas(area);

    let mut spans = vec![Span::from(format!(
        " {} calls, {} failed, last {} day{}",
        usage.total,
        usage.failures,
        usage.window_days,
        if usage.window_days == 1 { "" } else { "s" }
    ))];
    for (kind, n) in &usage.by_failure_kind {
        spans.push(Span::from("  "));
        spans.push(Span::from(format!("{kind} {n}")).fg(Color::Red));
    }
    frame.render_widget(
        stale_banner(&app.usage).unwrap_or(Line::from(spans)),
        summary,
    );

    let header = |cols: &[&'static str]| {
        Row::new(cols.to_vec()).style(Style::new().add_modifier(Modifier::BOLD))
    };
    let rows = usage.by_tool.iter().map(|t| {
        let failures = Span::from(t.failures.to_string());
        Row::new(vec![
            Cell::from(t.tool.clone()),
            Cell::from(t.calls.to_string()),
            Cell::from(if t.failures > 0 {
                failures.fg(Color::Red)
            } else {
                failures.dim()
            }),
            Cell::from(format!("{:.0}", t.p50_ms)),
            Cell::from(format!("{:.0}", t.p95_ms)),
            Cell::from(t.avg_result_bytes.map_or("-".into(), |b| format!("{b:.0}"))),
        ])
    });
    let widths = [
        Constraint::Min(24),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(10),
    ];
    let table = Table::new(rows, widths)
        .header(header(&[
            "TOOL",
            "CALLS",
            "FAIL",
            "P50 MS",
            "P95 MS",
            "AVG BYTES",
        ]))
        .block(Block::bordered().title(" By tool "));
    frame.render_widget(table, tools);

    let now = now_unix();
    let rows = usage.recent_runs.iter().map(|r| {
        let status = match &r.error_kind {
            Some(kind) => Span::from(format!("{} ({kind})", r.status)).fg(Color::Red),
            None => Span::from(r.status.clone()),
        };
        Row::new(vec![
            Cell::from(ago(Some(r.started_at), now)),
            Cell::from(r.kind.clone()),
            Cell::from(status),
            Cell::from(r.duration_ms.map_or("-".into(), |ms| ms.to_string())),
            Cell::from(r.actor.clone().unwrap_or_default()).dim(),
            Cell::from(r.run_id.clone()).dim(),
        ])
    });
    let widths = [
        Constraint::Length(9),
        Constraint::Length(10),
        Constraint::Length(18),
        Constraint::Length(7),
        Constraint::Length(22),
        Constraint::Min(10),
    ];
    let table = Table::new(rows, widths)
        .header(header(&["STARTED", "KIND", "STATUS", "MS", "ACTOR", "RUN"]))
        .block(Block::bordered().title(format!(" Recent runs ({}) ", usage.recent_runs.len())));
    frame.render_widget(table, runs);
}
