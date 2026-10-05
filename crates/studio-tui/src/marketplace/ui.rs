//! Drawing the Marketplaces module: the catalog view (marketplaces, the
//! selected one's entries, the selected entry) and the fleet view.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap};
use studio_core::check::Status;

use crate::framework::widgets::{message, slot_placeholder, stale_banner};

use super::data::{ListingState, Market};
use super::{Focus, MarketplaceModule, View, status_mark};

/// Wide enough for the marketplace list beside the entries.
const SIDE_BY_SIDE: u16 = 100;
/// Evidence lines shown per check in the detail pane.
const EVIDENCE_LINES: usize = 8;
/// Wide enough to show the entry and its checks side by side.
const DETAIL_SPLIT: u16 = 90;

pub fn draw(frame: &mut Frame, app: &MarketplaceModule, area: Rect) {
    if slot_placeholder(frame, area, &app.snapshot, "marketplaces") {
        return;
    }
    let area = match stale_banner(&app.snapshot) {
        Some(banner) => {
            let [top, rest] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
            frame.render_widget(banner, top);
            rest
        }
        None => area,
    };
    match app.view {
        View::Catalog => draw_catalog(frame, app, area),
        View::Fleet => draw_fleet(frame, app, area),
    }
}

fn border(title: String, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    Block::bordered().title(title).border_style(style)
}

fn status_color(s: Status) -> Color {
    status_mark(s).1
}

fn draw_catalog(frame: &mut Frame, app: &MarketplaceModule, area: Rect) {
    let markets = app.markets();
    if markets.is_empty() {
        message(
            frame,
            area,
            vec![
                Line::from("No marketplace is configured."),
                Line::from("Add a [[marketplace]] table to the instance's studio.toml.").dim(),
            ],
        );
        return;
    }
    let wide = area.width >= SIDE_BY_SIDE;
    let (list_area, entries_area, detail_area) = if wide {
        let [left, right] =
            Layout::horizontal([Constraint::Length(38), Constraint::Min(0)]).areas(area);
        let [entries, detail] =
            Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(right);
        (left, entries, detail)
    } else {
        let list_height = (markets.len() as u16 + 2).min(6);
        let [list, entries, detail] = Layout::vertical([
            Constraint::Length(list_height),
            Constraint::Percentage(40),
            Constraint::Min(3),
        ])
        .areas(area);
        (list, entries, detail)
    };
    draw_markets(frame, app, list_area, wide);
    if let Some(m) = app.selected_market() {
        draw_entries(frame, app, m, entries_area);
        draw_detail(frame, app, m, detail_area);
    }
}

fn draw_markets(frame: &mut Frame, app: &MarketplaceModule, area: Rect, wide: bool) {
    let mut lines: Vec<Line> = Vec::new();
    let mut selected_line = 0;
    for (i, m) in app.markets().iter().enumerate() {
        let selected = i == app.market;
        if selected {
            selected_line = lines.len();
        }
        let (label, status) = m.validation_label();
        let marker = if selected { "▸ " } else { "  " };
        let mut head = Line::from(vec![
            Span::from(marker),
            Span::from(m.id().to_string()).bold(),
            Span::from("  "),
            Span::from(label).fg(status_color(status)),
        ]);
        if !wide && m.clone.cloned {
            head.push_span(Span::from(format!("  {} entries", m.entries.len())).dim());
            head.push_span(clone_span(m));
        }
        if selected {
            head = head.add_modifier(Modifier::REVERSED);
        }
        lines.push(head);
        if wide {
            lines.push(Line::from(format!("    {}", m.market().repo)).dim());
            lines.push(
                Line::from(format!("    {}  {} entries", m.targets(), m.entries.len())).dim(),
            );
            lines.push(Line::from(vec![Span::from("  "), clone_span(m)]));
        }
    }
    let height = area.height.saturating_sub(2) as usize;
    let per_market = if wide { 4 } else { 1 };
    let scroll = (selected_line + per_market).saturating_sub(height) as u16;
    let block = border(
        format!(" Marketplaces ({}) ", app.markets().len()),
        app.focus == Focus::Markets,
    );
    frame.render_widget(Paragraph::new(lines).block(block).scroll((scroll, 0)), area);
}

fn clone_span(m: &Market) -> Span<'static> {
    let c = &m.clone;
    if !c.cloned {
        return Span::from("  not cloned").fg(Color::DarkGray);
    }
    let mut parts = vec!["cloned".to_string()];
    if c.dirty {
        return Span::from("  cloned, dirty").fg(Color::Yellow);
    }
    if let Some(b) = &c.branch
        && *b != m.market().branch
    {
        parts.push(format!("on {b}"));
    }
    match c.unpushed {
        Some(n) if n > 0 => {
            parts.push(format!("{n} unpushed"));
            Span::from(format!("  {}", parts.join(", "))).fg(Color::Yellow)
        }
        _ => Span::from(format!("  {}", parts.join(", "))).dim(),
    }
}

fn draw_entries(frame: &mut Frame, app: &MarketplaceModule, m: &Market, area: Rect) {
    let block = border(
        format!(" {} entries ({}) ", m.id(), m.entries.len()),
        app.focus == Focus::Entries,
    );
    if !m.clone.cloned {
        let text = vec![
            Line::from(format!("Not cloned at {}", m.dir().display())),
            Line::from(format!(
                "Clone {} there to see and change its entries.",
                m.market().repo
            ))
            .dim(),
        ];
        frame.render_widget(
            Paragraph::new(text).block(block).wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let rows = m.entries.iter().map(|e| {
        let entry = &e.plugin.entry;
        let (mark, color) = status_mark(e.status());
        let dep = if entry.is_deprecated() {
            Span::from("yes").fg(Color::Yellow)
        } else {
            Span::from("")
        };
        Row::new(vec![
            Cell::from(e.slug().to_string()),
            Cell::from(entry.kind().as_str()),
            Cell::from(entry.auth_type().as_str()),
            Cell::from(entry.version().to_string()),
            Cell::from(dep),
            Cell::from(Span::from(mark).fg(color)),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Min(14),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(9),
            Constraint::Length(4),
            Constraint::Length(5),
        ],
    )
    .header(
        Row::new(vec!["slug", "kind", "auth", "version", "dep", "sync"])
            .style(Style::new().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .block(block);
    let mut state = TableState::default().with_selected(Some(app.entry));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_detail(frame: &mut Frame, app: &MarketplaceModule, m: &Market, area: Rect) {
    let Some(e) = app.selected_entry() else {
        let mut lines =
            vec![Line::from(format!("{} ({})", m.market().catalog_name, m.market().repo)).bold()];
        lines.push(Line::from(format!("clone: {}", m.dir().display())).dim());
        lines.push(
            Line::from(format!(
                "branch {}, publish {:?}, targets {}",
                m.market().branch,
                m.market().publish,
                m.targets()
            ))
            .dim(),
        );
        for c in m.checks.iter().filter(|c| c.status != Status::Pass) {
            let (mark, color) = status_mark(c.status);
            lines.push(Line::from(format!("{mark:<4}  {}", c.summary)).fg(color));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(border(" Marketplace ".into(), false))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    };
    let entry = &e.plugin.entry;
    let mut info: Vec<Line> = Vec::new();
    let mut title = vec![Span::from(entry.name.clone()).bold()];
    if entry.is_deprecated() {
        title.push(Span::from("  deprecated").fg(Color::Yellow));
        if let Some(r) = &entry.deprecated_reason {
            title.push(Span::from(format!(": {r}")).fg(Color::Yellow));
        }
    }
    let title = Line::from(title);
    info.push(Line::from(format!("{}/server.yaml", e.plugin.rel_dir)).dim());
    for l in studio_marketplace::yaml::emit(entry).lines() {
        if l.trim_start().starts_with('#') {
            continue;
        }
        info.push(yaml_line(l));
    }
    info.push(Line::from(""));
    info.push(Line::from(format!("Files ({})", e.files.len())).bold());
    for f in &e.files {
        let rel = f
            .strip_prefix(&format!("{}/", e.plugin.rel_dir))
            .unwrap_or(f);
        info.push(Line::from(format!("  {rel}")));
    }

    let wide = area.width >= DETAIL_SPLIT;
    let mut checks: Vec<Line> = Vec::new();
    let (mark, color) = status_mark(e.status());
    checks.push(Line::from(vec![
        Span::from("Reconcile  ").bold(),
        Span::from(mark).fg(color),
    ]));
    // Problems first, with their evidence.
    let mut sorted: Vec<_> = e.checks.iter().collect();
    sorted.sort_by_key(|c| c.status == Status::Pass);
    let passing = sorted.iter().filter(|c| c.status == Status::Pass).count();
    for c in sorted {
        if !wide && c.status == Status::Pass {
            continue;
        }
        let (mark, color) = status_mark(c.status);
        let what =
            c.id.strip_prefix(&format!("marketplace.{}.", e.slug()))
                .unwrap_or(&c.id);
        checks.push(Line::from(vec![
            Span::from(format!("  {mark:<4} ")).fg(color),
            Span::from(format!("{what}: ")).dim(),
            Span::from(c.summary.clone()),
        ]));
        if c.status != Status::Pass
            && let Some(ev) = &c.evidence
        {
            let all: Vec<&str> = ev.lines().collect();
            for l in all.iter().take(EVIDENCE_LINES) {
                checks.push(evidence_line(l));
            }
            if all.len() > EVIDENCE_LINES {
                checks.push(
                    Line::from(format!(
                        "         ... {} more (R shows everything)",
                        all.len() - EVIDENCE_LINES
                    ))
                    .dim(),
                );
            }
        }
    }

    if !wide && passing > 0 {
        checks.push(Line::from(format!("  ok   {passing} more check(s) pass")).dim());
    }

    let block = border(format!(" {} ", e.slug()), false);
    if wide {
        info.insert(0, title);
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
                .areas(area);
        frame.render_widget(
            Paragraph::new(info).block(block).wrap(Wrap { trim: false }),
            left,
        );
        frame.render_widget(
            Paragraph::new(checks)
                .block(border(" Reconcile ".into(), false))
                .wrap(Wrap { trim: false }),
            right,
        );
    } else {
        // Narrow: the problems first, they are what needs attention.
        let mut lines = vec![title];
        lines.extend(checks);
        lines.push(Line::from(""));
        lines.extend(info);
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}

fn yaml_line(l: &str) -> Line<'static> {
    match l.split_once(':') {
        Some((k, v)) if !k.trim().is_empty() && !k.trim().contains(' ') => Line::from(vec![
            Span::from(format!("  {k}:")).fg(Color::Cyan),
            Span::from(v.to_string()),
        ]),
        _ => Line::from(format!("  {l}")),
    }
}

fn evidence_line(l: &str) -> Line<'static> {
    let text = format!("         {l}");
    if l.starts_with('+') && !l.starts_with("+++") {
        Line::from(text).fg(Color::Green)
    } else if l.starts_with('-') && !l.starts_with("---") {
        Line::from(text).fg(Color::Red)
    } else {
        Line::from(text).dim()
    }
}

fn draw_fleet(frame: &mut Frame, app: &MarketplaceModule, area: Rect) {
    let rows = app.fleet();
    if rows.is_empty() {
        message(
            frame,
            area,
            vec![
                Line::from("No fleet servers are configured."),
                Line::from("fleet.include and [[fleet.server]] in studio.toml list them.").dim(),
            ],
        );
        return;
    }
    let ids: Vec<String> = app.markets().iter().map(|m| m.id().to_string()).collect();
    let [table_area, detail_area] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(7)]).areas(area);
    let mut header = vec![
        "fleet server".to_string(),
        "slug".to_string(),
        "package.json".to_string(),
    ];
    header.extend(ids.iter().cloned());
    let table_rows = rows.iter().map(|r| {
        let mut cells = vec![
            Cell::from(r.repo.clone()),
            Cell::from(r.slug.clone()),
            Cell::from(
                r.repo_version
                    .clone()
                    .map(Span::from)
                    .unwrap_or_else(|| Span::from("-").dim()),
            ),
        ];
        for l in &r.listings {
            cells.push(Cell::from(listing_span(&l.state)));
        }
        Row::new(cells)
    });
    let mut widths = vec![
        Constraint::Min(18),
        Constraint::Length(18),
        Constraint::Length(12),
    ];
    widths.extend(ids.iter().map(|_| Constraint::Length(18)));
    let table = Table::new(table_rows, widths)
        .header(Row::new(header).style(Style::new().add_modifier(Modifier::BOLD)))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .block(border(format!(" Fleet servers ({}) ", rows.len()), true));
    let mut state = TableState::default().with_selected(Some(app.fleet_row));
    frame.render_stateful_widget(table, table_area, &mut state);

    if let Some(r) = rows.get(app.fleet_row) {
        let mut lines = vec![
            Line::from(r.repo.clone()).bold(),
            Line::from(format!(
                "from {}; listed as {} ({})",
                r.origins.join(" + "),
                r.slug,
                r.slug_from
            ))
            .dim(),
            Line::from(format!("local: {}", r.local_dir.display())).dim(),
        ];
        for l in &r.listings {
            let text = match &l.state {
                ListingState::Unknown => "marketplace not cloned".to_string(),
                ListingState::NotListed => "not listed".to_string(),
                ListingState::Listed {
                    version,
                    deprecated,
                    agrees,
                } => {
                    let dep = if *deprecated { ", deprecated" } else { "" };
                    let agree = match agrees {
                        Some(true) => "matches package.json",
                        Some(false) => "differs from package.json",
                        None => "no package.json to compare",
                    };
                    format!("listed at {version} ({agree}{dep})")
                }
            };
            lines.push(Line::from(vec![
                Span::from(format!("{}: ", l.marketplace)).bold(),
                listing_span(&l.state),
                Span::from(format!("  {text}")).dim(),
            ]));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(border(" Server ".into(), false))
                .wrap(Wrap { trim: true }),
            detail_area,
        );
    }
}

fn listing_span(s: &ListingState) -> Span<'static> {
    match s {
        ListingState::Unknown => Span::from("?").dim(),
        ListingState::NotListed => Span::from("-").dim(),
        ListingState::Listed {
            version, agrees, ..
        } => match agrees {
            Some(true) => Span::from(format!("{version} ok")).fg(Color::Green),
            Some(false) => Span::from(format!("{version} differs")).fg(Color::Yellow),
            None => Span::from(version.clone()),
        },
    }
}
