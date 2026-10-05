//! Drawing the Pattern module: a header naming the pack with the view tabs,
//! then the current view.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};
use studio_core::check::Status;
use unicode_width::UnicodeWidthStr;

use crate::framework::widgets::{message, slot_placeholder, stale_banner};

use super::data::{PackData, RepoReport, VersionState, display_value};
use super::markdown::{self, Piece};
use super::{Focus, Pane, PatternModule, Scroll, View};

/// Wide enough for a list and its detail side by side.
const SIDE_BY_SIDE: u16 = 110;
/// The fleet table's width beyond the repo names: the glyph, the other
/// columns, their spacing and the border.
const TABLE_FIXED: u16 = 2 + 8 + 6 + 9 + 11 + 4 + 2;
/// Longest variable value shown before it is cut.
const VALUE_MAX: usize = 40;

pub fn draw(frame: &mut Frame, m: &mut PatternModule, area: Rect) {
    let [header, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    draw_header(frame, m, header);
    if slot_placeholder(frame, body, &m.pack, "the pattern pack") {
        return;
    }
    let body = match stale_banner(&m.pack) {
        Some(banner) => {
            let [top, rest] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(body);
            frame.render_widget(banner, top);
            rest
        }
        None => body,
    };
    let Some(pack) = m.pack.data.clone() else {
        return;
    };
    match m.view {
        View::Overview => {
            let lines = overview(&pack, body.width);
            scrolled(frame, body, lines, &mut m.scroll[Pane::Overview as usize]);
        }
        View::Changelog => match &pack.changelog {
            Ok(text) => {
                let lines = markdown::render(text, body.width.saturating_sub(1));
                scrolled(frame, body, lines, &mut m.scroll[Pane::Changelog as usize]);
            }
            Err(e) => message(
                frame,
                body,
                vec![
                    Line::from("This pack has no changelog.").bold(),
                    Line::from(e.clone()).dim(),
                ],
            ),
        },
        View::Skill => draw_skill(frame, m, &pack, body),
        View::Conformance => draw_conformance(frame, m, &pack, body),
    }
}

fn draw_header(frame: &mut Frame, m: &PatternModule, area: Rect) {
    let spans = match &m.pack.data {
        Some(p) => vec![
            Span::from(format!(" {} ", p.pack.name())).bold(),
            Span::from(p.pack.version().to_string()).fg(Color::Cyan),
        ],
        None if m.pack.loading => vec![Span::from(" loading the pack...").dim()],
        None => vec![Span::from(" no pack").fg(Color::Red)],
    };
    let tabs: Vec<Span> = View::ALL
        .iter()
        .flat_map(|view| {
            let label = format!(" {} ", view.title());
            let span = if *view == m.view {
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
    let [left, right] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(width.min(area.width)),
    ])
    .areas(area);
    frame.render_widget(Line::from(spans), left);
    frame.render_widget(Line::from(tabs), right);
}

/// Renders `lines` into `area` at the pane's offset, clamped to the text.
fn scrolled(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>, s: &mut Scroll) {
    scrolled_in(frame, area, lines, s, None);
}

fn scrolled_in(
    frame: &mut Frame,
    area: Rect,
    lines: Vec<Line<'static>>,
    s: &mut Scroll,
    block: Option<Block<'static>>,
) {
    let inner = block.as_ref().map_or(area, |b| b.inner(area));
    s.content = lines.len();
    s.viewport = inner.height as usize;
    s.offset = s.offset.min(s.content.saturating_sub(s.viewport));
    let mut p = Paragraph::new(lines).scroll((s.offset.min(u16::MAX as usize) as u16, 0));
    if let Some(b) = block {
        let more = s.content > s.viewport;
        let b = if more {
            b.title_bottom(
                Line::from(format!(
                    " {}-{} of {} ",
                    s.offset + 1,
                    (s.offset + s.viewport).min(s.content),
                    s.content
                ))
                .right_aligned()
                .dim(),
            )
        } else {
            b
        };
        p = p.block(b);
    }
    frame.render_widget(p, area);
}

/// Lines wider than `width` are word-wrapped (with a hanging indent);
/// others are kept as they are, alignment included.
fn fit(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    let width = width as usize;
    let mut out = Vec::new();
    for line in lines {
        if line.width() <= width {
            out.push(line);
            continue;
        }
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let indent = text.len() - text.trim_start().len();
        let pieces: Vec<Piece> = line
            .spans
            .iter()
            .map(|s| (s.content.to_string(), line.style.patch(s.style)))
            .collect();
        let lead = vec![(" ".repeat(indent), Style::new())];
        let cont = vec![(" ".repeat(indent + 2), Style::new())];
        out.extend(markdown::wrap(&pieces, width, &lead, &cont));
    }
    out
}

fn section(title: impl Into<String>) -> Line<'static> {
    Line::from(title.into()).bold().fg(Color::Cyan)
}

/// `label  value`, the value wrapped under itself.
fn kv(label: &str, value: Vec<Piece>, label_width: usize, width: u16) -> Vec<Line<'static>> {
    let first = vec![(
        format!("  {label:<label_width$}  "),
        Style::new().add_modifier(Modifier::DIM),
    )];
    let cont = vec![(" ".repeat(label_width + 4), Style::new())];
    markdown::wrap(&value, width as usize, &first, &cont)
}

fn plain(text: impl Into<String>) -> Vec<Piece> {
    vec![(text.into(), Style::new())]
}

fn list(title: &str, items: Vec<String>, width: u16, out: &mut Vec<Line<'static>>) {
    if items.is_empty() {
        return;
    }
    out.push(Line::from(""));
    out.push(section(format!("{title} ({})", items.len())));
    for item in items {
        out.extend(markdown::wrap(
            &plain(item),
            width as usize,
            &[("  ".into(), Style::new())],
            &[("    ".into(), Style::new())],
        ));
    }
}

fn map_list<V: std::fmt::Display>(
    title: &str,
    map: &std::collections::BTreeMap<String, V>,
    sep: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let w = map.keys().map(|k| k.width()).max().unwrap_or(0);
    list(
        title,
        map.iter()
            .map(|(k, v)| format!("{k:<w$} {sep} {v}"))
            .collect(),
        width,
        out,
    );
}

pub(crate) fn overview(p: &PackData, width: u16) -> Vec<Line<'static>> {
    let m = &p.pack.manifest;
    let mut out = vec![Line::from(vec![
        Span::from(m.pack.name.clone()).bold(),
        Span::from("  "),
        Span::from(m.pack.version.clone()).fg(Color::Cyan),
    ])];
    if let Some(d) = &m.pack.description {
        out.extend(markdown::wrap(&plain(d.clone()), width as usize, &[], &[]));
    }
    out.push(Line::from(""));
    let paths = [
        ("pack", p.pack.dir.display().to_string()),
        ("template", p.pack.template_dir().display().to_string()),
        ("skill", p.pack.skill_dir().display().to_string()),
        ("store", p.store_dir.display().to_string()),
    ];
    for (label, value) in paths {
        out.extend(kv(label, plain(value), 8, width));
    }
    if let Some(e) = &p.values_error {
        out.extend(kv(
            "values",
            vec![(e.clone(), Style::new().fg(Color::Red))],
            8,
            width,
        ));
    }

    // Variables, with the instance's values.
    out.push(Line::from(""));
    out.push(section(format!("Variables ({})", m.variables.len())));
    let nw = m.variables.keys().map(|k| k.width()).max().unwrap_or(0);
    let shown: Vec<(String, Style)> = m
        .variables
        .keys()
        .map(|name| match p.values.get(name) {
            Some(v) => {
                let (text, hidden) = display_value(name, v, VALUE_MAX);
                let style = if hidden {
                    Style::new().fg(Color::DarkGray)
                } else {
                    Style::new().fg(Color::Green)
                };
                (text, style)
            }
            None => ("(not set)".into(), Style::new().fg(Color::Red)),
        })
        .collect();
    let vw = shown.iter().map(|(t, _)| t.width()).max().unwrap_or(0);
    let side = (width as usize) >= 2 + nw + 2 + vw + 2 + 24;
    for ((name, desc), (value, style)) in m.variables.iter().zip(shown) {
        let desc_piece = vec![(desc.clone(), Style::new().add_modifier(Modifier::DIM))];
        if side {
            let pad = vw - value.width();
            let first = vec![
                (format!("  {name:<nw$}  "), Style::new().bold()),
                (value, style),
                (" ".repeat(pad + 2), Style::new()),
            ];
            let cont = vec![(" ".repeat(2 + nw + 2 + vw + 2), Style::new())];
            out.extend(markdown::wrap(&desc_piece, width as usize, &first, &cont));
        } else {
            out.push(Line::from(vec![
                Span::styled(format!("  {name:<nw$}  "), Style::new().bold()),
                Span::styled(value, style),
            ]));
            out.extend(markdown::wrap(
                &desc_piece,
                width as usize,
                &[("      ".into(), Style::new())],
                &[("      ".into(), Style::new())],
            ));
        }
    }

    map_list("Pins (dependencies)", &m.pins, "=", width, &mut out);
    map_list(
        "Dev pins (devDependencies)",
        &m.dev_pins,
        "=",
        width,
        &mut out,
    );
    map_list("Overrides", &m.overrides, "=", width, &mut out);
    map_list("Major floors", &m.majors, ">=", width, &mut out);
    list(
        "Forbidden dependencies",
        m.forbidden.dependencies.clone(),
        width,
        &mut out,
    );
    list(
        "Legacy markers (src/*.ts)",
        m.forbidden.legacy_markers.clone(),
        width,
        &mut out,
    );
    list(
        &format!(
            "Security files, hashed into {}",
            m.security.conformance_file
        ),
        m.security.files.clone(),
        width,
        &mut out,
    );
    list("Required files", m.required.files.clone(), width, &mut out);
    list(
        "Required scripts",
        m.required
            .scripts
            .iter()
            .map(|(k, v)| {
                if v.is_empty() {
                    k.clone()
                } else {
                    format!("{k}: starts with \"{v}\"")
                }
            })
            .collect(),
        width,
        &mut out,
    );

    let w = &m.wrangler;
    let mut rules = Vec::new();
    if let Some(main) = &w.main {
        rules.push(format!("main = {main}"));
    }
    if !w.compatibility_flags.is_empty() {
        rules.push(format!(
            "compatibility_flags include {}",
            w.compatibility_flags.join(", ")
        ));
    }
    if let Some(d) = &w.min_compatibility_date {
        rules.push(format!("compatibility_date >= {d}"));
    }
    if !w.required_vars.is_empty() {
        rules.push(format!("vars: {}", w.required_vars.join(", ")));
    }
    if !w.required_kv_bindings.is_empty() {
        rules.push(format!(
            "KV bindings: {}",
            w.required_kv_bindings.join(", ")
        ));
    }
    if let Some(v) = &w.public_url_var {
        rules.push(format!("{v}'s host is a route host"));
    }
    if let Some(v) = &w.scope_var {
        rules.push(format!("{v} scopes share one slug"));
    }
    if w.forbid_durable_objects {
        rules.push("no live Durable Objects".into());
    }
    list("Wrangler rules", rules, width, &mut out);
    map_list(
        "Lockfile entries",
        &m.lockfile.required_prefix_counts,
        "×",
        width,
        &mut out,
    );
    list(
        "Placeholder patterns",
        m.placeholders.patterns.clone(),
        width,
        &mut out,
    );
    out
}

fn draw_skill(frame: &mut Frame, m: &mut PatternModule, pack: &PackData, area: Rect) {
    if pack.docs.is_empty() {
        message(
            frame,
            area,
            vec![
                Line::from("The pack's skill has no Markdown documents.").bold(),
                Line::from(pack.pack.skill_dir().display().to_string()).dim(),
            ],
        );
        return;
    }
    let names: Vec<String> = pack.docs.iter().map(|d| d.path.clone()).collect();
    let longest = names.iter().map(|n| n.width()).max().unwrap_or(0) as u16;
    let (list_area, doc_area) = if area.width >= 100 {
        let [a, b] = Layout::horizontal([
            Constraint::Length((longest + 4).clamp(14, 40)),
            Constraint::Min(20),
        ])
        .areas(area);
        (a, b)
    } else {
        let height = (names.len() as u16 + 2).min(7).min(area.height / 2);
        let [a, b] = Layout::vertical([Constraint::Length(height), Constraint::Min(3)]).areas(area);
        (a, b)
    };
    let rows = names.iter().map(|n| Row::new([Cell::from(n.clone())]));
    let table = Table::new(rows, [Constraint::Percentage(100)])
        .block(Block::bordered().title(format!(" Skill docs ({}) ", names.len())))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(Some(m.doc_selected));
    frame.render_stateful_widget(table, list_area, &mut state);

    let doc = &pack.docs[m.doc_selected.min(pack.docs.len() - 1)];
    let block = Block::bordered().title(format!(" {} ", doc.path));
    let inner = block.inner(doc_area);
    let mut lines = Vec::new();
    if let Some(e) = &doc.error {
        lines.extend(fit(
            vec![Line::from(format!("Values not substituted: {e}")).fg(Color::Red)],
            inner.width,
        ));
        lines.push(Line::from(""));
    }
    lines.extend(markdown::render(&doc.text, inner.width.saturating_sub(1)));
    scrolled_in(
        frame,
        doc_area,
        lines,
        &mut m.scroll[Pane::Doc as usize],
        Some(block),
    );
}

fn glyph(status: Status) -> Span<'static> {
    match status {
        Status::Pass => Span::from("✓").fg(Color::Green),
        Status::Warn => Span::from("!").fg(Color::Yellow),
        Status::Fail => Span::from("✗").fg(Color::Red),
        Status::Skip => Span::from("-").dim(),
    }
}

fn draw_conformance(frame: &mut Frame, m: &mut PatternModule, pack: &PackData, area: Rect) {
    if pack.repos.is_empty() {
        message(
            frame,
            area,
            vec![
                Line::from("The instance's fleet lists no repos.").bold(),
                Line::from("Add [fleet] include or [[fleet.server]] entries to studio.toml.").dim(),
            ],
        );
        return;
    }
    if slot_placeholder(frame, area, &m.reports, "the conformance checks") {
        return;
    }
    let area = match stale_banner(&m.reports) {
        Some(banner) => {
            let [top, rest] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
            frame.render_widget(banner, top);
            rest
        }
        None => area,
    };
    let count = m.reports.data.as_ref().map_or(0, Vec::len) as u16;
    let (table_area, detail_area) = if area.width >= SIDE_BY_SIDE {
        let names = m
            .reports
            .data
            .iter()
            .flatten()
            .map(|r| r.name.width())
            .max()
            .unwrap_or(4) as u16;
        let table = (names + TABLE_FIXED).min(area.width * 11 / 20);
        let [a, b] =
            Layout::horizontal([Constraint::Length(table), Constraint::Min(30)]).areas(area);
        (a, b)
    } else {
        let height = (count + 3).min(area.height * 2 / 5).max(4);
        let [a, b] = Layout::vertical([Constraint::Length(height), Constraint::Min(4)]).areas(area);
        (a, b)
    };
    draw_table(frame, m, pack, table_area);
    let Some(report) = m.selected_report().cloned() else {
        return;
    };
    // The focused half gets the room.
    let checks_share = if m.focus == Focus::Repos { 60 } else { 40 };
    let [checks_area, diff_area] = Layout::vertical([
        Constraint::Percentage(checks_share),
        Constraint::Percentage(100 - checks_share),
    ])
    .areas(detail_area);
    draw_checks(frame, m, pack, &report, checks_area);
    draw_diff(frame, m, &report, diff_area);
}

fn draw_table(frame: &mut Frame, m: &PatternModule, pack: &PackData, area: Rect) {
    let reports = m.reports.data.as_deref().unwrap_or(&[]);
    let version = pack.pack.version();
    let rows = reports.iter().map(|r| {
        if r.missing.is_some() {
            return Row::new(vec![
                Cell::from(Line::from(vec![
                    glyph(Status::Fail),
                    Span::from(" "),
                    Span::from(r.name.clone()),
                ])),
                Cell::from(Span::from("missing").fg(Color::Red)),
            ]);
        }
        let v = match r.version_state(version) {
            VersionState::Current => Span::from("current").fg(Color::Green),
            VersionState::Behind => Span::from("behind").fg(Color::Red),
            VersionState::Ahead => Span::from("ahead").fg(Color::Yellow),
            VersionState::Unknown => Span::from("none").fg(Color::Red),
        };
        let hashes = match r.check("pattern.hashes").map(|c| c.status) {
            Some(Status::Pass) => Span::from("ok").fg(Color::Green),
            Some(_) => Span::from("FAIL").fg(Color::Red),
            None => Span::from("-").dim(),
        };
        let drifted = r.drifted();
        let drift = if drifted > 0 {
            Line::from(vec![
                Span::from(format!("{} ", r.drift_lines())),
                Span::from(format!("{drifted} DRIFT")).fg(Color::Red),
            ])
        } else {
            Line::from(r.drift_lines().to_string())
        };
        let (p, w, f) = r.lint_counts();
        let lint = Line::from(vec![
            Span::from(format!("{p}✓ ")).fg(Color::Green),
            Span::from(format!("{w}! ")).fg(if w > 0 {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
            Span::from(format!("{f}✗")).fg(if f > 0 { Color::Red } else { Color::DarkGray }),
        ]);
        Row::new(vec![
            Cell::from(Line::from(vec![
                glyph(r.status()),
                Span::from(" "),
                Span::from(r.name.clone()),
            ])),
            Cell::from(v),
            Cell::from(hashes),
            Cell::from(drift),
            Cell::from(lint),
        ])
    });
    let header = Row::new(["REPO", "VERSION", "HASHES", "DRIFT", "LINT"])
        .style(Style::new().add_modifier(Modifier::BOLD));
    let name_width = reports.iter().map(|r| r.name.width()).max().unwrap_or(4) as u16 + 2;
    let widths = [
        Constraint::Length(name_width.max(6)),
        Constraint::Length(8),
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Min(9),
    ];
    let border = if m.focus == Focus::Repos {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new()
    };
    let loading = if m.reports.loading {
        " checking..."
    } else {
        ""
    };
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::bordered()
                .border_style(border)
                .title(format!(" Fleet ({}){loading} ", reports.len())),
        )
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(Some(m.repo_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_checks(
    frame: &mut Frame,
    m: &mut PatternModule,
    pack: &PackData,
    r: &RepoReport,
    area: Rect,
) {
    let block = Block::bordered()
        .title(format!(" {} ", r.name))
        .border_style(if m.focus == Focus::Repos {
            Style::new().fg(Color::Cyan)
        } else {
            Style::new()
        });
    let inner = block.inner(area);
    let mut lines = vec![Line::from(r.dir.display().to_string()).dim()];
    if let Some(why) = &r.missing {
        lines.push(Line::from(why.clone()).fg(Color::Red));
    } else {
        lines.push(Line::from(vec![
            Span::from("blessed at ").dim(),
            Span::from(r.version.clone().unwrap_or_else(|| "-".into())),
            Span::from(format!(", pack {}", pack.pack.version())).dim(),
        ]));
        if let Some(busy) = &m.busy
            && *busy == r.name
        {
            lines.push(Line::from("blessing...").fg(Color::Yellow));
        }
        lines.push(Line::from(""));
        let idw = r.checks.iter().map(|c| c.id.width()).max().unwrap_or(0);
        for c in &r.checks {
            let first: Vec<Piece> = vec![
                (" ".into(), Style::new()),
                (glyph(c.status).content.to_string(), glyph(c.status).style),
                (format!(" {:<idw$}  ", c.id), Style::new()),
            ];
            let cont = vec![(" ".repeat(idw + 5), Style::new())];
            lines.extend(markdown::wrap(
                &plain(c.summary.clone()),
                inner.width as usize,
                &first,
                &cont,
            ));
            if c.status != Status::Pass
                && let Some(e) = &c.evidence
            {
                for l in e.lines() {
                    lines.push(Line::from(format!("      {l}")).dim());
                }
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(format!("Security files ({})", r.files.len())).bold());
        let fw = r
            .files
            .iter()
            .map(|f| f.status.file.width())
            .max()
            .unwrap_or(0);
        for (i, f) in r.files.iter().enumerate() {
            let s = &f.status;
            let (state, color) = if !s.present {
                ("MISSING", Color::Red)
            } else if s.matches_store {
                ("blessed", Color::Green)
            } else {
                ("DRIFT", Color::Red)
            };
            let mut line = Line::from(vec![
                Span::from(if i == m.file_selected { " ▸ " } else { "   " }),
                Span::from(format!("{:<fw$}  ", s.file)),
                Span::from(format!("{:>4} lines  ", s.differing_lines)).dim(),
                Span::from(state).fg(color),
            ]);
            if i == m.file_selected && m.focus == Focus::Files {
                line = line.add_modifier(Modifier::REVERSED);
            }
            lines.push(line);
        }
    }
    let lines = fit(lines, inner.width);
    scrolled_in(
        frame,
        area,
        lines,
        &mut m.scroll[Pane::Checks as usize],
        Some(block),
    );
}

fn diff_lines(diff: &str) -> Vec<Line<'static>> {
    diff.lines()
        .map(|l| {
            let line = Line::from(l.replace('\t', "    "));
            if l.starts_with("+++") || l.starts_with("---") {
                line.bold()
            } else if l.starts_with('+') {
                line.fg(Color::Green)
            } else if l.starts_with('-') {
                line.fg(Color::Red)
            } else if l.starts_with("@@") {
                line.fg(Color::Cyan)
            } else {
                line
            }
        })
        .collect()
}

fn draw_diff(frame: &mut Frame, m: &mut PatternModule, r: &RepoReport, area: Rect) {
    let Some(f) = r.files.get(m.file_selected) else {
        frame.render_widget(Block::bordered().title(" diff "), area);
        return;
    };
    let s = &f.status;
    let block = Block::bordered()
        .title(format!(
            " {}  ({}/{}) ",
            s.file,
            m.file_selected + 1,
            r.files.len()
        ))
        .border_style(if m.focus == Focus::Files {
            Style::new().fg(Color::Cyan)
        } else {
            Style::new()
        });
    let mut lines = Vec::new();
    if !s.present {
        lines.push(Line::from(format!("{} is MISSING from the repo.", s.file)).fg(Color::Red));
    } else if s.matches_store {
        lines.push(
            Line::from(format!(
                "Matches the blessed diff ({} differing lines vs the template).",
                s.differing_lines
            ))
            .fg(Color::Green),
        );
    } else {
        lines.push(
            Line::from("DRIFT: the current diff is not the blessed one.")
                .fg(Color::Red)
                .bold(),
        );
    }
    lines.push(Line::from(""));
    lines.push(
        Line::from("Current diff (template → repo)")
            .bold()
            .underlined(),
    );
    if f.current.is_empty() {
        lines.push(Line::from("identical to the template").dim());
    } else {
        lines.extend(diff_lines(&f.current));
    }
    if !s.matches_store {
        lines.push(Line::from(""));
        lines.push(
            Line::from("Blessed diff (instance store)")
                .bold()
                .underlined(),
        );
        match &f.stored {
            None => lines.push(Line::from("nothing stored: never blessed").fg(Color::Red)),
            Some(d) if d.is_empty() => {
                lines.push(Line::from("empty: blessed identical to the template").dim())
            }
            Some(d) => lines.extend(diff_lines(d)),
        }
    }
    scrolled_in(
        frame,
        area,
        lines,
        &mut m.scroll[Pane::Diff as usize],
        Some(block),
    );
}
