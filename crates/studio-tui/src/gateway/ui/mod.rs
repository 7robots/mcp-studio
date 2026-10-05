//! Rendering a gateway pane: a line naming the gateway, who is signed in and
//! the screen tabs; then the current screen, or the sign-in state instead of
//! it; then any open overlay.

mod connections;
mod policy;
mod scopes;
mod servers;
mod usage;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::framework::widgets::message;

use super::pane::{Pane, Screen, Viewer};

/// `position` is `(index, count)` of the pane among the module's gateways.
pub fn draw(frame: &mut Frame, pane: &Pane, area: Rect, position: (usize, usize)) {
    let [header, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(2)]).areas(area);
    draw_header(frame, pane, header, position);
    if let Some(login) = &pane.login {
        let mut lines = vec![
            Line::from(format!(
                "Waiting for the browser sign-in to {}...",
                pane.host()
            ))
            .bold(),
            Line::from(""),
        ];
        if let Some(url) = &login.url {
            lines.push(Line::from("If no browser opened, visit:").dim());
            lines.push(Line::from(url.to_string()));
            lines.push(Line::from(""));
        }
        lines.push(Line::from("Esc cancels.").dim());
        message(frame, body, lines);
    } else if let Some(reason) = &pane.signed_out {
        let lines = vec![
            Line::from(reason.clone()).fg(Color::Red),
            Line::from(""),
            Line::from("Press L to sign in through the browser."),
            Line::from("(Or run `mcp-studio gateway login` in a shell.)").dim(),
        ];
        message(frame, body, lines);
    } else if let Some(viewer) = &pane.viewer {
        draw_viewer(frame, viewer, body);
    } else {
        match pane.screen {
            Screen::Servers => servers::draw(frame, pane, body),
            screen if pane.admin() == Some(false) => {
                let tool = screen.tool();
                message(
                    frame,
                    body,
                    vec![
                        Line::from(format!(
                            "{} needs {}.",
                            pane.screen.title(),
                            pane.admin_scope()
                        )),
                        Line::from(format!(
                            "This sign-in is view-only, so {tool} is not offered to it."
                        ))
                        .dim(),
                    ],
                );
            }
            screen if pane.offers(screen.tool()) == Some(false) => message(
                frame,
                body,
                vec![
                    Line::from(format!(
                        "{} is not supported by this gateway.",
                        screen.title()
                    )),
                    Line::from(format!(
                        "It needs the {} tool, which a newer gateway offers.",
                        screen.tool()
                    ))
                    .dim(),
                ],
            ),
            Screen::Usage => usage::draw(frame, pane, body),
            Screen::Policy => policy::draw(frame, pane, body),
            Screen::Scopes => scopes::draw(frame, pane, body),
            Screen::Connections => connections::draw(frame, pane, body),
        }
    }
    if let Some(overlay) = pane.overlay() {
        overlay.draw(frame);
    }
}

fn draw_header(frame: &mut Frame, pane: &Pane, area: Rect, (index, count): (usize, usize)) {
    let mut spans = vec![Span::from(format!(" {} ", pane.id())).bold()];
    if count > 1 {
        spans.push(Span::from(format!("({}/{count}) ", index + 1)).dim());
    }
    spans.push(Span::from(pane.host()).dim());
    if let Some(build) = pane.build() {
        spans.push(Span::from(format!(" build {}", build.label())).dim());
    }
    spans.push(Span::from("  "));
    match &pane.identity.data {
        Some(identity) => {
            let who = identity
                .whoami
                .email
                .clone()
                .or(identity.whoami.sub.clone())
                .unwrap_or_else(|| "?".into());
            spans.push(Span::from(who));
            spans.push(Span::from(" "));
            spans.push(if identity.admin {
                Span::styled(" admin ", Style::new().fg(Color::Black).bg(Color::Green))
            } else {
                Span::styled(
                    " view-only ",
                    Style::new().fg(Color::Black).bg(Color::Yellow),
                )
            });
        }
        None if pane.identity.loading => spans.push(Span::from("signing in...").dim()),
        None => spans.push(Span::from("not signed in").fg(Color::Red)),
    }
    let tabs: Vec<Span> = Screen::ALL
        .iter()
        .flat_map(|screen| {
            let label = format!(" {} ", screen.title());
            let span = if *screen == pane.screen {
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
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(width)]).areas(area);
    frame.render_widget(Line::from(spans), left);
    frame.render_widget(Line::from(tabs), right);
}

/// A console result, scrolled to `viewer.scroll`.
fn draw_viewer(frame: &mut Frame, viewer: &Viewer, area: Rect) {
    let total = viewer.lines.len();
    let lines: Vec<Line> = viewer
        .lines
        .iter()
        .skip(viewer.scroll)
        .map(|l| {
            let line = Line::from(l.clone());
            if viewer.error {
                line.fg(Color::Red)
            } else {
                line
            }
        })
        .collect();
    let title = format!(
        " {}  ({}/{total}) ",
        viewer.title,
        (viewer.scroll + 1).min(total)
    );
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        area,
    );
}
