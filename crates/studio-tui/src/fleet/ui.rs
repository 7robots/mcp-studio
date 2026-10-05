//! Rendering the Fleet module: a summary line, then the status matrix and the
//! selected server's detail beside it (below it on a narrow terminal).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};
use studio_core::check::{Check, Status};
use studio_fleet::time::{age, now_unix};
use studio_fleet::{FleetReport, ServerStatus, Source};
use unicode_width::UnicodeWidthStr;

use crate::framework::widgets::{message, stale_banner};

use super::{FleetModule, Focus};

/// Wide enough for the matrix and the detail side by side.
const SIDE_BY_SIDE: u16 = 140;
/// The widest a matrix cell gets.
const CELL: usize = 18;
/// Notes shown under the matrix at most.
const NOTES: usize = 3;

pub fn glyph(status: Status) -> &'static str {
    match status {
        Status::Pass => "✓",
        Status::Warn => "!",
        Status::Fail => "✗",
        Status::Skip => "–",
    }
}

/// The same palette as the Gateway module's health colours.
pub fn color(status: Status) -> Color {
    match status {
        Status::Pass => Color::Green,
        Status::Warn => Color::Yellow,
        Status::Fail => Color::Red,
        Status::Skip => Color::DarkGray,
    }
}

fn short(source: Source) -> &'static str {
    match source {
        Source::Git => "git",
        Source::Github => "gh",
        Source::Cloudflare => "cf",
        Source::Http => "http",
        Source::Okta => "okta",
        Source::Gateway => "gw",
        Source::Marketplace => "mp",
        Source::Pattern => "pat",
    }
}

pub fn draw(frame: &mut Frame, m: &mut FleetModule, area: Rect) {
    let Some(report) = m.report.data.clone() else {
        draw_empty(frame, m, area);
        return;
    };
    let [header, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    frame.render_widget(header_line(m, &report), header);
    let body = match stale_banner(&m.report) {
        Some(banner) => {
            let [top, body] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(rest);
            frame.render_widget(banner, top);
            body
        }
        None => rest,
    };
    if report.servers.is_empty() {
        let mut lines = vec![
            Line::from("No fleet servers."),
            Line::from("Add [fleet] include or [[fleet.server]] to the instance's studio.toml.")
                .dim(),
        ];
        lines.extend(
            report
                .notes
                .iter()
                .map(|n| Line::from(format!("note: {n}")).dim()),
        );
        message(frame, body, lines);
        return;
    }
    let (matrix_area, detail_area) = if body.width >= SIDE_BY_SIDE {
        let [a, b] = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
            .areas(body);
        (a, b)
    } else {
        let [a, b] =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body);
        (a, b)
    };
    draw_matrix(frame, m, &report, matrix_area);
    match m.selected_server().cloned() {
        Some(server) => draw_detail(frame, m, &server, detail_area),
        None => message(
            frame,
            detail_area,
            vec![Line::from("No server matches the filters.").dim()],
        ),
    }
}

fn draw_empty(frame: &mut Frame, m: &FleetModule, area: Rect) {
    let lines = match (&m.report.error, m.loading_since) {
        (_, Some(since)) => vec![
            Line::from(format!(
                "Probing the fleet{}... {}s",
                m.instance
                    .as_deref()
                    .map(|n| format!(" of {n}"))
                    .unwrap_or_default(),
                since.elapsed().as_secs()
            ))
            .bold(),
            Line::from("A full probe can take half a minute.").dim(),
        ],
        (Some(error), None) => vec![
            Line::from(format!("Could not load fleet status: {error}")).fg(Color::Red),
            Line::from(""),
            Line::from("r retries, R re-probes ignoring the cache.").dim(),
        ],
        (None, None) => vec![Line::from("Fleet status not loaded yet. r loads it.").dim()],
    };
    message(frame, area, lines);
}

fn header_line(m: &FleetModule, r: &FleetReport) -> Line<'static> {
    let mut spans = vec![
        Span::from(format!(" {} ", r.instance)).bold(),
        Span::from(format!(
            "probed {} ago",
            age(now_unix() - r.generated_at_unix)
        ))
        .dim(),
    ];
    if r.from_cache {
        spans.push(Span::from(" (cached)").fg(Color::Yellow));
    }
    spans.push(Span::from("  "));
    for status in [Status::Pass, Status::Warn, Status::Fail, Status::Skip] {
        let label = match status {
            Status::Pass => "pass",
            Status::Warn => "warn",
            Status::Fail => "fail",
            Status::Skip => "skip",
        };
        spans.push(
            Span::from(format!("{} {} {label}  ", glyph(status), r.count(status)))
                .fg(color(status)),
        );
    }
    if m.problems_only {
        spans.push(Span::styled(
            " problems only ",
            Style::new().fg(Color::Black).bg(Color::Yellow),
        ));
        spans.push(Span::from(" "));
    }
    if let Some(src) = m.source {
        spans.push(Span::styled(
            format!(" source: {src} "),
            Style::new().fg(Color::Black).bg(Color::Cyan),
        ));
        spans.push(Span::from(" "));
    }
    if let Some(since) = m.loading_since {
        spans.push(
            Span::from(format!("refreshing... {}s", since.elapsed().as_secs())).fg(Color::Yellow),
        );
    }
    Line::from(spans)
}

fn truncate(s: &str, n: usize) -> String {
    if s.width() <= n {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        if out.width() + 1 >= n {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

fn draw_matrix(frame: &mut Frame, m: &FleetModule, r: &FleetReport, area: Rect) {
    let visible = m.visible();
    let sources: Vec<Source> = match m.source {
        Some(s) => vec![s],
        None => r.sources.clone(),
    };
    let notes: Vec<&String> = r.notes.iter().take(NOTES).collect();
    let (table_area, notes_area) = if notes.is_empty() {
        (area, None)
    } else {
        let [a, b] = Layout::vertical([Constraint::Min(3), Constraint::Length(notes.len() as u16)])
            .areas(area);
        (a, Some(b))
    };

    let name_width = r
        .servers
        .iter()
        .map(|s| s.name.width())
        .max()
        .unwrap_or(6)
        .clamp(6, 28);
    // Inner width, less the rollup column, the name and the column gaps.
    let fixed = 2 + 1 + name_width + sources.len() + 1;
    let room = usize::from(table_area.width).saturating_sub(fixed);
    let cell = (room / sources.len().max(1)).clamp(1, CELL);
    let with_text = cell >= 5;

    let rows = visible.iter().map(|&i| {
        let s = &r.servers[i];
        let rollup = s.rollup();
        let mut cells = vec![
            Cell::from(Span::from(glyph(rollup)).fg(color(rollup))),
            Cell::from(truncate(&s.name, name_width)),
        ];
        for src in &sources {
            cells.push(match s.column(*src) {
                Some(c) => {
                    let text = if with_text {
                        truncate(&format!("{} {}", glyph(c.status), c.text), cell)
                    } else {
                        glyph(c.status).to_string()
                    };
                    Cell::from(Span::from(text).fg(color(c.status)))
                }
                None => Cell::from(Span::from("–").dim()),
            });
        }
        Row::new(cells)
    });
    let mut header = vec![Cell::from(""), Cell::from("SERVER")];
    for src in &sources {
        let name = if src.as_str().len() <= cell {
            src.as_str()
        } else {
            short(*src)
        };
        header.push(Cell::from(truncate(&name.to_uppercase(), cell)));
    }
    let header = Row::new(header).style(Style::new().add_modifier(Modifier::BOLD));
    let mut widths = vec![Constraint::Length(1), Constraint::Length(name_width as u16)];
    widths.extend(sources.iter().map(|_| Constraint::Length(cell as u16)));
    let total = r.servers.len();
    let title = if visible.len() == total {
        format!(" Servers ({total}) ")
    } else {
        format!(" Servers ({} of {total}) ", visible.len())
    };
    let highlight = if m.focus == Focus::Matrix {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    };
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::bordered().title(title))
        .row_highlight_style(highlight);
    let mut state =
        TableState::default().with_selected((!visible.is_empty()).then_some(m.selected));
    frame.render_stateful_widget(table, table_area, &mut state);
    if visible.is_empty() {
        let inner = table_area.inner(ratatui::layout::Margin::new(1, 2));
        message(
            frame,
            inner,
            vec![Line::from("No server matches the filters (f, s).").dim()],
        );
    }
    if let Some(notes_area) = notes_area {
        let lines: Vec<Line> = notes
            .iter()
            .map(|n| Line::from(format!(" note: {n}")).dim())
            .collect();
        frame.render_widget(Paragraph::new(lines), notes_area);
    }
}

fn field(label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::from(format!("{label:<16}")).dim(),
        Span::from(value.into()),
    ])
}

fn or_dash(v: Option<&str>) -> String {
    v.filter(|v| !v.is_empty()).unwrap_or("–").to_string()
}

fn list(v: &[String]) -> String {
    if v.is_empty() {
        "–".into()
    } else {
        v.join(" ")
    }
}

/// The source a check belongs to, from its id's first segment.
fn check_source(check: &Check) -> Option<Source> {
    studio_fleet::cache::check_source(&check.id)
}

/// The detail pane's lines, before wrapping.
pub fn detail_lines(s: &ServerStatus, filter: Option<Source>) -> Vec<Line<'static>> {
    let info = &s.info;
    let rollup = s.rollup();
    let mut lines = vec![
        Line::from(vec![
            Span::from(format!("{} ", glyph(rollup))).fg(color(rollup)),
            Span::from(s.name.clone()).bold(),
            Span::from(format!("  {}", s.repo)).dim(),
        ]),
        Line::from(""),
        field("url", or_dash(s.url.as_deref())),
        field("worker", or_dash(info.worker.as_deref())),
        field("hosts", list(&info.hosts)),
        field("scopes", list(&info.scopes)),
        field("version", or_dash(info.version.as_deref())),
        field(
            "gateway",
            match (&info.gateway, &s.gateway_id) {
                (Some(g), Some(id)) => format!("{id} on {g}"),
                (None, Some(id)) => id.clone(),
                (Some(g), None) => format!("– on {g}"),
                (None, None) => "–".into(),
            },
        ),
        field("marketplace", info.marketplace_slug.clone()),
    ];
    let envs: Vec<String> = info
        .environments
        .iter()
        .map(|e| {
            let url = e
                .url
                .as_deref()
                .map(|u| format!(" {u}"))
                .unwrap_or_default();
            format!(
                "{}{}{url}",
                e.name,
                if e.deployed { " (probed)" } else { "" }
            )
        })
        .collect();
    lines.push(field("environments", list(&envs)));
    lines.push(field(
        "local dir",
        format!(
            "{}{}",
            s.local_dir.display(),
            if info.local_exists { "" } else { " (no clone)" }
        ),
    ));
    for (key, value) in &s.facts {
        lines.push(field(key, value.clone()));
    }
    for problem in &info.problems {
        lines.push(Line::from(format!("! {problem}")).fg(Color::Yellow));
    }

    let mut groups: Vec<(Option<Source>, Vec<&Check>)> = Vec::new();
    for check in &s.checks {
        let src = check_source(check);
        if filter.is_some() && src != filter {
            continue;
        }
        match groups.iter_mut().find(|(g, _)| *g == src) {
            Some((_, checks)) => checks.push(check),
            None => groups.push((src, vec![check])),
        }
    }
    groups.sort_by_key(|(src, _)| src.map_or(usize::MAX, |s| s as usize));
    let id_width = s
        .checks
        .iter()
        .map(|c| c.id.width())
        .max()
        .unwrap_or(0)
        .min(28);
    for (src, checks) in groups {
        lines.push(Line::from(""));
        let name = src.map_or("other", Source::as_str);
        let mut heading = vec![Span::from(name.to_string()).bold()];
        if let Some(column) = src.and_then(|x| s.column(x)) {
            heading.push(
                Span::from(format!("  {} {}", glyph(column.status), column.text))
                    .fg(color(column.status)),
            );
        }
        lines.push(Line::from(heading));
        for c in checks {
            lines.push(Line::from(vec![
                Span::from(format!("  {} ", glyph(c.status))).fg(color(c.status)),
                Span::from(format!("{:<id_width$}  ", c.id)).dim(),
                Span::from(c.summary.clone()),
            ]));
            if let Some(evidence) = &c.evidence {
                for part in evidence.lines() {
                    lines.push(Line::from(format!("      {part}")).dim());
                }
            }
        }
    }
    if s.checks.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from("No checks ran.").dim());
    }
    lines
}

fn draw_detail(frame: &mut Frame, m: &mut FleetModule, s: &ServerStatus, area: Rect) {
    let focused = m.focus == Focus::Detail;
    let title = if focused {
        format!(" {} (j/k scroll, Esc back) ", s.name)
    } else {
        format!(" {} ", s.name)
    };
    let block = if focused {
        Block::bordered()
            .title(title)
            .border_style(Style::new().fg(Color::Cyan))
    } else {
        Block::bordered().title(title)
    };
    let inner = block.inner(area);
    let lines = wrap(detail_lines(s, m.source), usize::from(inner.width).max(1));
    m.max_scroll = (lines.len() as u16).saturating_sub(inner.height);
    m.scroll = m.scroll.min(m.max_scroll);
    frame.render_widget(
        Paragraph::new(lines).block(block).scroll((m.scroll, 0)),
        area,
    );
}

/// Wraps styled lines to `width` columns at spaces (hard-breaking words that
/// do not fit), indenting continuations to the line's own leading spaces plus
/// two, so the line count is exact and scrolling can stop at the end.
pub fn wrap(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for line in lines {
        if line.width() <= width {
            out.push(line);
            continue;
        }
        let lead = line
            .spans
            .first()
            .map(|s| s.content.len() - s.content.trim_start().len())
            .unwrap_or(0);
        let indent = (lead + 2).min(width / 2);
        // Words with their trailing spaces, each with its span's style.
        let mut words: Vec<(String, Style)> = Vec::new();
        for span in &line.spans {
            let style = line.style.patch(span.style);
            let mut word = String::new();
            let mut in_space = false;
            for ch in span.content.chars() {
                if ch != ' ' && in_space {
                    words.push((std::mem::take(&mut word), style));
                    in_space = false;
                }
                in_space |= ch == ' ';
                word.push(ch);
            }
            if !word.is_empty() {
                words.push((word, style));
            }
        }
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut used = 0;
        let flush = |current: &mut Vec<Span<'static>>, used: &mut usize, out: &mut Vec<Line>| {
            out.push(Line::from(std::mem::take(current)));
            current.push(Span::from(" ".repeat(indent)));
            *used = indent;
        };
        for (word, style) in words {
            let w = word.trim_end().width();
            if used + w > width && used > indent {
                flush(&mut current, &mut used, &mut out);
            }
            if indent + w > width {
                // Too long for any line: break it by columns.
                let mut piece = String::new();
                for ch in word.chars() {
                    let cw = ch.to_string().width();
                    if used + piece.width() + cw > width {
                        current.push(Span::styled(std::mem::take(&mut piece), style));
                        flush(&mut current, &mut used, &mut out);
                    }
                    piece.push(ch);
                }
                used += piece.width();
                current.push(Span::styled(piece, style));
                continue;
            }
            used += word.width();
            current.push(Span::styled(word, style));
        }
        if current.iter().any(|s| !s.content.trim().is_empty()) {
            out.push(Line::from(current));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn wrapping_breaks_at_spaces_indents_and_fits() {
        let lines = wrap(
            vec![Line::from("    alpha beta gamma delta epsilon zeta")],
            16,
        );
        let texts: Vec<String> = lines.iter().map(text).collect();
        assert!(lines.iter().all(|l| l.width() <= 16), "{texts:?}");
        assert!(texts[0].starts_with("    alpha"), "{texts:?}");
        assert!(texts[1].starts_with("      "), "{texts:?}");
        let joined: String = texts.join(" ");
        for w in ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"] {
            assert!(joined.contains(w), "{w}: {texts:?}");
        }
    }

    #[test]
    fn a_long_word_is_broken_by_columns() {
        let lines = wrap(vec![Line::from("x".repeat(40))], 10);
        assert!(lines.iter().all(|l| l.width() <= 10));
        let total: usize = lines.iter().map(|l| text(l).trim().len()).sum();
        assert_eq!(total, 40);
    }

    #[test]
    fn check_ids_map_to_sources() {
        let src = |id: &str| check_source(&Check::pass(id, ""));
        assert_eq!(src("cf.build"), Some(Source::Cloudflare));
        assert_eq!(src("github.topic"), Some(Source::Github));
        assert_eq!(src("pattern"), Some(Source::Pattern));
        assert_eq!(src("weird.thing"), None);
    }
}
