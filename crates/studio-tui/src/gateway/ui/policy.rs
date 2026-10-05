//! The Policy screen, read-only in v1: the mode and configuration fingerprint in
//! force, any configuration errors, and the `policy_events` feed with the
//! selected event's reason and arguments.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap};

use crate::framework::widgets::{slot_placeholder as placeholder, stale_banner};
use crate::gateway::pane::Pane;
use studio_gateway::util::{ago, now_unix};

pub fn draw(frame: &mut Frame, app: &Pane, area: Rect) {
    if placeholder(frame, area, &app.policy, "policy events") {
        return;
    }
    let Some(policy) = &app.policy.data else {
        return;
    };
    let errors = policy.config_errors.clone().unwrap_or_default();
    let [summary, errors_area, table_area, detail_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(errors.len() as u16),
        Constraint::Percentage(55),
        Constraint::Min(4),
    ])
    .areas(area);

    let mode = Span::from(format!(" mode {} ", policy.mode)).fg(if policy.mode == "enforce" {
        Color::Red
    } else {
        Color::Yellow
    });
    let summary_line = Line::from(vec![
        mode,
        Span::from(format!(
            " version {}  fingerprint {}",
            policy.policy_version, policy.policy_fingerprint
        ))
        .dim(),
        Span::from(format!(
            "  {} event{} in {} day{}  filter: {}",
            policy.count,
            if policy.count == 1 { "" } else { "s" },
            policy.window_days,
            if policy.window_days == 1 { "" } else { "s" },
            app.policy_filter.unwrap_or("all")
        )),
    ]);
    frame.render_widget(stale_banner(&app.policy).unwrap_or(summary_line), summary);
    let error_lines: Vec<Line> = errors
        .iter()
        .map(|e| Line::from(format!(" config error: {e}")).fg(Color::Red))
        .collect();
    frame.render_widget(Paragraph::new(error_lines), errors_area);

    let now = now_unix();
    let rows = policy.events.iter().map(|e| {
        let decision = Span::from(e.decision.clone()).fg(if e.decision == "deny" {
            Color::Red
        } else {
            Color::Yellow
        });
        let effect = Span::from(e.effect.clone());
        Row::new(vec![
            Cell::from(ago(Some(e.ts), now)),
            Cell::from(decision),
            Cell::from(if e.effect == "blocked" {
                effect.bold()
            } else {
                effect.dim()
            }),
            Cell::from(e.target.clone().unwrap_or_else(|| "(run)".into())),
            Cell::from(e.rules.join(", ")),
        ])
    });
    let widths = [
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Length(9),
        Constraint::Min(20),
        Constraint::Min(16),
    ];
    let header = Row::new(["AGE", "DECISION", "EFFECT", "TARGET", "RULES"])
        .style(Style::new().add_modifier(Modifier::BOLD));
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::bordered().title(" Events "))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default()
        .with_selected((!policy.events.is_empty()).then_some(app.event_selected));
    frame.render_stateful_widget(table, table_area, &mut state);

    let lines = match policy.events.get(app.event_selected) {
        Some(e) => {
            let mut lines = vec![Line::from(e.reason.clone().unwrap_or_default())];
            lines.push(Line::from(""));
            let meta = [
                ("actor", e.actor.clone()),
                ("path", e.path.clone()),
                ("run", e.run_id.clone()),
                ("mode then", e.mode_at_decision.clone()),
            ];
            for (label, value) in meta {
                if let Some(value) = value {
                    lines.push(Line::from(vec![
                        Span::from(format!("{label:<10}")).dim(),
                        Span::from(value),
                    ]));
                }
            }
            if let Some(args) = &e.args {
                lines.push(Line::from(vec![
                    Span::from(format!("{:<10}", "args")).dim(),
                    Span::from(args.to_string()),
                ]));
            }
            lines
        }
        None => vec![Line::from("No events in this window.").dim()],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(" Event "))
            .wrap(Wrap { trim: false }),
        detail_area,
    );
}
