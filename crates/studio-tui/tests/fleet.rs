//! The Fleet module, driven headlessly over a scripted loader: the matrix,
//! selection and detail, the filters, refreshes, loading and error states,
//! opening URLs, and the narrow layout.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use studio_core::check::{Check, Status};
use studio_fleet::{Column, FleetReport, ServerInfo, ServerStatus, Source};
use studio_tui::fleet::{FleetModule, Focus, Loader, Opener};
use studio_tui::{App, Component, Harness};

const WIDE: (u16, u16) = (180, 40);
const NARROW: (u16, u16) = (100, 40);

fn info(name: &str) -> ServerInfo {
    ServerInfo {
        repo: format!("acme/{name}"),
        name: name.into(),
        local_dir: PathBuf::from(format!("/srv/acme/{name}")),
        local_exists: true,
        origins: vec!["include".into()],
        worker: Some(format!("{name}-worker")),
        hosts: vec![format!("{name}.mcp.acme.example")],
        url: Some(format!("https://{name}.mcp.acme.example/mcp")),
        url_source: Some("config".into()),
        scopes: vec!["acme:read".into()],
        version: Some("1.2.3".into()),
        gateway: Some("main".into()),
        gateway_id: Some(name.into()),
        marketplace_slug: format!("acme-{name}"),
        environments: Vec::new(),
        problems: Vec::new(),
    }
}

fn server(name: &str, cells: &[(Source, Status, &str)], checks: Vec<Check>) -> ServerStatus {
    let info = info(name);
    ServerStatus {
        repo: info.repo.clone(),
        name: name.into(),
        url: info.url.clone(),
        gateway_id: info.gateway_id.clone(),
        local_dir: info.local_dir.clone(),
        checks,
        columns: cells
            .iter()
            .map(|(source, status, text)| Column {
                source: *source,
                status: *status,
                text: (*text).into(),
            })
            .collect(),
        facts: BTreeMap::from([("git.head".to_string(), "abc1234".to_string())]),
        info,
    }
}

const SOURCES: [Source; 4] = [Source::Git, Source::Http, Source::Okta, Source::Pattern];

const EVIDENCE: &str = "GET https://tides.mcp.acme.example/mcp answered 200 without a token; \
expected 401 with a WWW-Authenticate header naming the protected resource metadata";

fn report() -> FleetReport {
    use Source::*;
    use Status::*;
    let weather = server(
        "weather",
        &[
            (Git, Pass, "main"),
            (Http, Pass, "401 ok"),
            (Okta, Pass, "ok"),
            (Pattern, Skip, "no clone"),
        ],
        vec![
            Check::pass("git.clean", "working tree clean"),
            Check::pass("http.unauth_401", "401 without a token"),
            Check::pass("okta.scopes", "scopes granted"),
            Check::skip("pattern", "no local clone"),
        ],
    );
    let tides = server(
        "tides",
        &[
            (Git, Pass, "main"),
            (Http, Fail, "open to anyone"),
            (Okta, Warn, "scope unused"),
            (Pattern, Pass, "conforms"),
        ],
        vec![
            Check::pass("git.clean", "working tree clean"),
            Check::fail("http.unauth_401", "answers without a token").with_evidence(EVIDENCE),
            Check::warn("okta.scopes", "acme:write is granted but unused"),
            Check::pass("pattern.pins", "pins match"),
        ],
    );
    let almanac = server(
        "almanac",
        &[
            (Git, Warn, "2 behind"),
            (Http, Pass, "401 ok"),
            (Okta, Pass, "ok"),
            (Pattern, Pass, "conforms"),
        ],
        vec![
            Check::warn("git.behind", "2 commits behind origin/main"),
            Check::pass("http.unauth_401", "401 without a token"),
        ],
    );
    let now = studio_fleet::time::now_unix();
    FleetReport {
        instance: "acme-test".into(),
        generated_at: studio_fleet::time::format_rfc3339(now - 120),
        generated_at_unix: now - 120,
        sources: SOURCES.to_vec(),
        servers: vec![weather, tides, almanac],
        notes: vec!["GitHub token: not signed in to gh".into()],
        from_cache: true,
    }
}

/// A scripted loader: what it returns next, how long it takes, and every
/// `refresh` flag it was called with.
#[derive(Clone)]
struct Script {
    next: Arc<Mutex<Result<FleetReport, String>>>,
    delays: Arc<Mutex<Vec<Duration>>>,
    calls: Arc<Mutex<Vec<bool>>>,
    hang: Arc<Mutex<bool>>,
}

impl Script {
    fn new(result: Result<FleetReport, String>) -> Script {
        Script {
            next: Arc::new(Mutex::new(result)),
            delays: Arc::default(),
            calls: Arc::default(),
            hang: Arc::default(),
        }
    }

    fn set(&self, result: Result<FleetReport, String>) {
        *self.next.lock().unwrap() = result;
    }

    fn calls(&self) -> Vec<bool> {
        self.calls.lock().unwrap().clone()
    }

    fn loader(&self) -> Loader {
        let s = self.clone();
        Arc::new(move |refresh| {
            s.calls.lock().unwrap().push(refresh);
            let result = s.next.lock().unwrap().clone();
            let delay = {
                let mut d = s.delays.lock().unwrap();
                if d.is_empty() {
                    Duration::ZERO
                } else {
                    d.remove(0)
                }
            };
            let hang = *s.hang.lock().unwrap();
            Box::pin(async move {
                if hang {
                    futures::future::pending::<()>().await;
                }
                tokio::time::sleep(delay).await;
                result
            })
        })
    }
}

fn harness_with(module: FleetModule, size: (u16, u16)) -> Harness {
    let (app, rx) = App::new("Acme MCP fleet", vec![Box::new(module)]);
    Harness::new(app, rx, size)
}

async fn loaded(script: &Script, size: (u16, u16)) -> Harness {
    let mut h = harness_with(FleetModule::with_loader(script.loader()), size);
    h.until_text("Servers (3)").await;
    h
}

fn fleet(h: &Harness) -> &FleetModule {
    h.app.module::<FleetModule>().unwrap()
}

/// The matrix row for `name`: a rollup glyph right after the border, then the name.
fn matrix_row(h: &Harness, name: &str) -> String {
    h.lines()
        .into_iter()
        .find(|l| {
            let mut chars = l.chars();
            chars.next() == Some('│')
                && chars.next().is_some_and(|c| "✓!✗–".contains(c))
                && chars.as_str().trim_start().starts_with(name)
        })
        .unwrap_or_else(|| panic!("no matrix row for {name}:\n{}", h.text()))
}

fn row_index(h: &Harness, needle: &str) -> usize {
    h.lines()
        .iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{}", h.text()))
}

#[tokio::test]
async fn the_matrix_has_a_row_per_server_and_a_column_per_source() {
    let script = Script::new(Ok(report()));
    let h = loaded(&script, WIDE).await;
    let text = h.text();
    let header = h.row_with("SERVER").unwrap();
    for col in ["GIT", "HTTP", "OKTA", "PATTERN"] {
        assert!(header.contains(col), "{col}: {header}");
    }
    assert!(!header.contains("CLOUDFLARE"), "only sources that ran");
    let weather = matrix_row(&h, "weather");
    assert!(
        weather.contains("✓ main") && weather.contains("– no clone"),
        "{weather}"
    );
    let tides = matrix_row(&h, "tides");
    assert!(
        tides.starts_with("│✗") || tides.contains("✗ tides"),
        "{tides}"
    );
    assert!(
        tides.contains("✗ open to") && tides.contains("! scope"),
        "{tides}"
    );
    // The summary line: instance, age, cache marker, counts.
    let summary = &h.lines()[1];
    for part in [
        "acme-test",
        "probed 2m ago",
        "(cached)",
        "6 pass",
        "2 warn",
        "1 fail",
        "1 skip",
    ] {
        assert!(summary.contains(part), "{part}: {summary}");
    }
    assert!(text.contains("note: GitHub token"), "{text}");
    let footer = h.lines().last().unwrap().clone();
    for hint in [
        "j/k select",
        "r refresh",
        "R re-probe",
        "f problems",
        "s source",
        "o open",
    ] {
        assert!(footer.contains(hint), "{hint}: {footer}");
    }
    assert_eq!(script.calls(), vec![false], "start loads through the cache");
}

#[tokio::test]
async fn selection_moves_and_the_detail_shows_facts_checks_and_evidence() {
    let script = Script::new(Ok(report()));
    let mut h = loaded(&script, WIDE).await;
    // The first server is selected and described beside the matrix.
    assert!(h.text().contains("weather-worker"));
    assert!(
        h.row_with("SERVER").unwrap().contains("acme/weather"),
        "side by side"
    );
    h.press("j");
    let text = h.text();
    for part in [
        "tides-worker",
        "tides.mcp.acme.example",
        "acme:read",
        "1.2.3",
        "tides on main",
        "acme-tides",
        "/srv/acme/tides",
        "git.head",
        "abc1234",
        "http.unauth_401",
        "answers without a token",
        "okta.scopes",
        "acme:write is granted but unused",
        "pattern.pins",
    ] {
        assert!(text.contains(part), "{part}:\n{text}");
    }
    // Evidence is wrapped inside the pane, all of it readable.
    assert!(text.contains("WWW-Authenticate"), "{text}");
    assert!(text.contains("metadata"), "{text}");
    // Checks are grouped under their source, in source order.
    let git = row_index(&h, "git.clean");
    let http = row_index(&h, "http.unauth_401");
    let okta = row_index(&h, "okta.scopes");
    assert!(git < http && http < okta);
    h.press("G");
    assert_eq!(fleet(&h).selected_server().unwrap().name, "almanac");
    h.press("j");
    assert_eq!(
        fleet(&h).selected_server().unwrap().name,
        "almanac",
        "clamped"
    );
    h.press("g");
    assert_eq!(fleet(&h).selected_server().unwrap().name, "weather");
    h.press("k");
    assert_eq!(fleet(&h).selected_server().unwrap().name, "weather");
}

#[tokio::test]
async fn problems_only_hides_healthy_servers() {
    let script = Script::new(Ok(report()));
    let mut h = loaded(&script, WIDE).await;
    h.press("j"); // tides
    h.press("f");
    assert!(fleet(&h).problems_only());
    assert!(h.text().contains("Servers (2 of 3)"), "{}", h.text());
    assert!(h.lines()[1].contains("problems only"));
    assert!(h.row_with("weather").is_none(), "{}", h.text());
    assert_eq!(
        fleet(&h).selected_server().unwrap().name,
        "tides",
        "the selection survives the filter"
    );
    h.press("f");
    assert!(h.text().contains("Servers (3)"));
}

#[tokio::test]
async fn the_source_filter_cycles_through_the_sources_that_ran() {
    let script = Script::new(Ok(report()));
    let mut h = loaded(&script, WIDE).await;
    h.press("s");
    assert_eq!(fleet(&h).source_filter(), Some(Source::Git));
    h.press("s");
    assert_eq!(fleet(&h).source_filter(), Some(Source::Http));
    assert!(h.lines()[1].contains("source: http"));
    let header = h.row_with("SERVER").unwrap();
    assert!(
        header.contains("HTTP") && !header.contains("OKTA"),
        "{header}"
    );
    // One column: the whole cell text fits.
    assert!(matrix_row(&h, "tides").contains("✗ open to anyone"));
    // The detail keeps only that source's checks.
    h.press("j");
    let text = h.text();
    assert!(
        text.contains("http.unauth_401") && !text.contains("okta.scopes"),
        "{text}"
    );
    // With problems only, the source decides: almanac is fine on http.
    h.press("f");
    assert!(h.text().contains("Servers (1 of 3)"), "{}", h.text());
    assert!(h.row_with("almanac").is_none());
    h.press("f");
    for _ in 0..3 {
        h.press("s");
    }
    assert_eq!(fleet(&h).source_filter(), None, "back to every source");
}

#[tokio::test]
async fn r_reloads_through_the_cache_and_capital_r_re_probes() {
    let script = Script::new(Ok(report()));
    let mut h = loaded(&script, WIDE).await;
    h.press("j");
    let mut fresh = report();
    fresh.from_cache = false;
    fresh.servers[1].columns[1].status = Status::Pass;
    fresh.servers[1].columns[1].text = "401 ok".into();
    script.set(Ok(fresh));
    h.press("R");
    h.until(|app| {
        app.module::<FleetModule>()
            .unwrap()
            .report
            .data
            .as_ref()
            .is_some_and(|r| !r.from_cache)
    })
    .await;
    assert_eq!(script.calls(), vec![false, true]);
    assert_eq!(fleet(&h).full_refreshes(), 1);
    assert!(!h.lines()[1].contains("(cached)"));
    assert_eq!(
        fleet(&h).selected_server().unwrap().name,
        "tides",
        "the selection survives a reload"
    );
    h.press("r");
    h.until(|_| script.calls().len() == 3).await;
    assert_eq!(script.calls(), vec![false, true, false]);
    h.until_text("Fleet status updated").await;
}

#[tokio::test]
async fn loading_shows_elapsed_time_and_a_superseded_load_is_dropped() {
    let script = Script::new(Ok(report()));
    *script.hang.lock().unwrap() = true;
    let mut h = harness_with(FleetModule::with_loader(script.loader()), WIDE);
    assert!(h.text().contains("Probing the fleet... 0s"), "{}", h.text());
    assert!(h.lines().last().unwrap().contains("probing 0s"));
    assert!(
        h.app.next_deadline().is_some(),
        "ticks while loading, for the clock"
    );
    // A second load supersedes the first, which never answers anyway; and a
    // slow third load's answer must lose to the fourth's.
    *script.hang.lock().unwrap() = false;
    let mut slow = report();
    slow.instance = "slow-answer".into();
    script.set(Ok(slow));
    script
        .delays
        .lock()
        .unwrap()
        .push(Duration::from_millis(300));
    h.press("r");
    script.set(Ok(report()));
    h.press("r");
    h.until_text("Servers (3)").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    h.settle().await;
    assert!(h.lines()[1].contains("acme-test"), "{}", h.lines()[1]);
    assert!(!h.text().contains("slow-answer"));
    assert!(fleet(&h).next_deadline().is_none(), "no clock once loaded");
}

#[tokio::test]
async fn errors_show_inline_and_as_a_toast_and_keep_earlier_data() {
    let script = Script::new(Err("gh: not signed in".into()));
    let mut h = harness_with(FleetModule::with_loader(script.loader()), WIDE);
    h.until_text("Could not load fleet status: gh: not signed in")
        .await;
    assert!(
        h.lines().last().unwrap().contains("Fleet status failed"),
        "{}",
        h.text()
    );
    script.set(Ok(report()));
    h.press("r");
    h.until_text("Servers (3)").await;
    script.set(Err("okta helper timed out".into()));
    h.press("r");
    h.until_text("Refresh failed, showing earlier data: okta helper timed out")
        .await;
    assert!(h.text().contains("Servers (3)"), "the matrix stays");
}

#[tokio::test]
async fn the_detail_pane_takes_focus_and_scrolls() {
    let script = Script::new(Ok(report()));
    let mut h = loaded(&script, (180, 16)).await;
    h.press("j"); // tides: more lines than fit
    let title = "tides  acme/tides";
    assert!(h.text().contains(title));
    h.press("enter");
    assert_eq!(fleet(&h).focus(), Focus::Detail);
    assert!(h.text().contains("Esc back"));
    assert!(h.lines().last().unwrap().contains("Esc matrix"));
    h.press("j");
    h.press("j");
    assert_eq!(fleet(&h).scroll(), 2);
    assert!(!h.text().contains(title), "scrolled past the title");
    assert_eq!(
        fleet(&h).selected_server().unwrap().name,
        "tides",
        "j scrolls, not selects"
    );
    h.press("G");
    let bottom = fleet(&h).scroll();
    assert!(h.text().contains("pattern.pins"), "the end is in view");
    h.press("j");
    assert_eq!(fleet(&h).scroll(), bottom, "no scrolling past the end");
    h.press("g");
    assert_eq!(fleet(&h).scroll(), 0);
    h.press("esc");
    assert_eq!(fleet(&h).focus(), Focus::Matrix);
    assert!(h.app.running(), "Esc left the detail, it did not quit");
    // J/K scroll from the matrix too; a new selection starts at the top.
    h.press("J");
    assert_eq!(fleet(&h).scroll(), 1);
    h.press("j");
    assert_eq!(fleet(&h).scroll(), 0);
    h.press("esc");
    assert!(!h.app.running(), "Esc in the matrix quits");
}

#[tokio::test]
async fn o_opens_the_url_and_capital_o_the_repo() {
    let script = Script::new(Ok(report()));
    let opened: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = opened.clone();
    let opener: Opener = Arc::new(move |url: &str| {
        seen.lock().unwrap().push(url.to_string());
        Ok(())
    });
    let mut h = harness_with(
        FleetModule::with_loader(script.loader()).with_opener(opener),
        WIDE,
    );
    h.until_text("Servers (3)").await;
    h.press("j");
    h.press("o");
    h.press("O");
    assert_eq!(
        *opened.lock().unwrap(),
        vec![
            "https://tides.mcp.acme.example/mcp".to_string(),
            "https://github.com/acme/tides".to_string(),
        ]
    );
    assert!(
        h.lines()
            .last()
            .unwrap()
            .contains("Opened https://github.com/acme/tides")
    );
}

#[tokio::test]
async fn a_narrow_terminal_stacks_the_detail_below_the_matrix() {
    let script = Script::new(Ok(report()));
    let h = loaded(&script, NARROW).await;
    let header = row_index(&h, "SERVER");
    let detail = row_index(&h, "weather-worker");
    assert!(detail > header + 3, "{}", h.text());
    assert!(!h.row_with("SERVER").unwrap().contains("acme/weather"));
    // Cells still say something at this width.
    assert!(matrix_row(&h, "tides").contains("✗ open"));
}

#[tokio::test]
async fn an_empty_fleet_says_how_to_add_servers() {
    let mut empty = report();
    empty.servers.clear();
    let script = Script::new(Ok(empty));
    let mut h = harness_with(FleetModule::with_loader(script.loader()), WIDE);
    h.until_text("No fleet servers.").await;
    h.press("j");
    h.press("enter");
    h.press("o");
    assert_eq!(fleet(&h).focus(), Focus::Matrix);
}
