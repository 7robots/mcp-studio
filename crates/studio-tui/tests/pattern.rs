//! The Pattern module against a tiny fixture pack, instance, store and two
//! fleet repos (`tests/fixtures/pattern`, acme values only). Every test runs
//! on a temporary copy, so blessing never touches the checked-in fixtures.
//!
//! The fixture fleet: `weather-mcp-worker` is blessed and current;
//! `news-mcp-worker` is a version behind and carries an unblessed edit to
//! `src/auth.ts` (hash gate and drift both fail).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use studio_core::Instance;
use studio_tui::pattern::{Focus, PatternModule, View};
use studio_tui::{App, Harness};

const WIDE: (u16, u16) = (150, 45);
const TALL: (u16, u16) = (150, 70);
const NARROW: (u16, u16) = (72, 32);
const NEWS: &str = "news-mcp-worker";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pattern")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixtures(), dir.path());
        Fixture { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).unwrap()
    }

    async fn harness(&self, size: (u16, u16)) -> Harness {
        let instance = Instance::load(&self.path("instance")).unwrap();
        let module = PatternModule::new(Arc::new(instance));
        let (app, rx) = App::new("Test", vec![Box::new(module)]);
        let mut h = Harness::new(app, rx, size);
        h.until_text("2026-02-01.1").await;
        h
    }
}

/// The repo's row in the fleet table (not the detail's title).
fn table_row(h: &Harness, repo: &str) -> String {
    h.lines()
        .into_iter()
        .find(|l| {
            ["✓", "!", "✗"]
                .iter()
                .any(|g| l.contains(&format!("{g} {repo} ")))
        })
        .unwrap_or_else(|| panic!("no row for {repo}:\n{}", h.text()))
}

fn pattern(h: &Harness) -> &PatternModule {
    h.app.module::<PatternModule>().unwrap()
}

/// Switches to the Conformance view and waits for the checks.
async fn conformance(h: &mut Harness) {
    h.press("shift+tab");
    assert_eq!(pattern(h).view, View::Conformance);
    h.until(|app| {
        app.module::<PatternModule>()
            .unwrap()
            .reports
            .data
            .is_some()
    })
    .await;
    h.draw();
}

#[tokio::test]
async fn the_overview_shows_the_version_pins_and_variables() {
    let fx = Fixture::new();
    let mut h = fx.harness(WIDE).await;
    let text = h.text();
    for needle in [
        "tiny-ts",
        "A tiny test pack: two security files and a skill",
        "Variables (3)",
        "Pins (dependencies)",
        "hono = 4.13.13",
        "vitest = 4.1.11",
        "miniflare = 5.1.0",
        "zod >= 4",
        "agents",
        "McpAgent",
        "Security files, hashed into conformance.json",
        "src/auth.ts",
    ] {
        assert!(text.contains(needle), "{needle}:\n{text}");
    }
    let row = h.row_with("domain_suffix").unwrap();
    assert!(
        row.contains("mcp.acme.example") && row.contains("Hostname suffix"),
        "{row}"
    );
    let row = h.row_with("gateway_host").unwrap();
    assert!(row.contains("gateway.mcp.acme.example"), "{row}");
    // A credential-looking value is never drawn.
    let row = h.row_with("api_token").unwrap();
    assert!(row.contains("(hidden)"), "{row}");
    assert!(!text.contains("Zx81Kq"), "{text}");
    // The rest is below the fold.
    h.press("G");
    let text = h.text();
    for needle in [
        "ci: starts with \"npm run conformance\"",
        "compatibility_date >= 2026-05-01",
        "KV bindings: OAUTH_KV",
    ] {
        assert!(text.contains(needle), "{needle}:\n{text}");
    }
    // The generated footer.
    let footer = h.lines().last().unwrap().clone();
    assert!(
        footer.contains("Tab views") && footer.contains("j/k scroll"),
        "{footer}"
    );
}

#[tokio::test]
async fn the_overview_scrolls_and_clamps() {
    let fx = Fixture::new();
    let mut h = fx.harness((100, 14)).await;
    assert!(!h.text().contains("Wrangler rules"));
    h.press("G");
    assert!(h.text().contains("Wrangler rules"), "{}", h.text());
    let bottom = h.text();
    h.press("j");
    assert_eq!(h.text(), bottom, "the end of the text is the end");
    h.press("g");
    assert!(h.text().contains("A tiny test pack"));
}

#[tokio::test]
async fn the_changelog_renders_markdown() {
    let fx = Fixture::new();
    let mut h = fx.harness(WIDE).await;
    h.press("tab");
    assert_eq!(pattern(&h).view, View::Changelog);
    let text = h.text();
    assert!(text.contains("Changelog — tiny-ts"), "{text}");
    assert!(text.contains("## 2026-02-01.1") || text.contains("2026-02-01.1"));
    assert!(
        !text.contains("## 2026"),
        "headings lose their markers:\n{text}"
    );
    let bullet = h.row_with("Tightened").unwrap();
    assert!(
        bullet.contains("• Tightened the audience check in src/auth.ts."),
        "inline markup is rendered, not shown: {bullet}"
    );
    assert!(text.contains("  src/auth.ts   audience check"), "{text}");
    assert!(text.contains("  File          Change"), "{text}");
    assert!(!text.contains("| ---"), "{text}");
}

#[tokio::test]
async fn skill_docs_list_and_render_with_the_instance_values() {
    let fx = Fixture::new();
    let mut h = fx.harness(WIDE).await;
    h.press("tab");
    h.press("tab");
    assert_eq!(pattern(&h).view, View::Skill);
    let text = h.text();
    assert!(
        text.contains("SKILL.md") && text.contains("references/guide.md"),
        "{text}"
    );
    assert!(
        text.contains("Servers live at <name>.mcp.acme.example behind the gateway at gateway.mcp.acme.example."),
        "{text}"
    );
    assert!(!text.contains("{{"), "{text}");
    assert!(text.contains("mcp-studio pattern bless"), "the code fence");
    h.press("j");
    let text = h.text();
    assert!(text.contains("Deploy guide"), "{text}");
    assert!(
        text.contains("2. Check the gateway at https://gateway.mcp.acme.example/health."),
        "{text}"
    );
    assert!(h.lines().last().unwrap().contains("j/k document"));
}

#[tokio::test]
async fn conformance_rows_summarise_each_repo() {
    let fx = Fixture::new();
    let mut h = fx.harness(WIDE).await;
    conformance(&mut h).await;
    let weather = table_row(&h, "weather-mcp-worker");
    assert!(
        weather.contains("current") && weather.contains(" ok ") && !weather.contains("DRIFT"),
        "{weather}"
    );
    assert!(weather.contains(" 2 "), "two differing lines: {weather}");
    let news = table_row(&h, NEWS);
    assert!(
        news.contains("behind") && news.contains("FAIL") && news.contains("1 DRIFT"),
        "{news}"
    );
    // Lint counts: weather passes everything; news has no wrangler.toml.
    assert!(weather.contains("0✗"), "{weather}");
    assert!(news.contains("1✗"), "{news}");
    let footer = h.lines().last().unwrap().clone();
    for hint in ["r re-run", "b bless store", "B full bless", "Enter files"] {
        assert!(footer.contains(hint), "{hint}: {footer}");
    }
}

#[tokio::test]
async fn the_detail_lists_checks_and_diffs_a_security_file() {
    let fx = Fixture::new();
    let mut h = fx.harness(TALL).await;
    conformance(&mut h).await;
    h.press("j");
    assert_eq!(pattern(&h).selected_report().unwrap().name, NEWS);
    let text = h.text();
    for needle in [
        "pattern.version",
        "behind: 2026-01-02.1 < pack 2026-02-01.1",
        "pattern.drift",
        "src/auth.ts: diff vs template no longer matches the stored manifest",
        "pattern.wrangler",
        "Security files (2)",
    ] {
        assert!(text.contains(needle), "{needle}:\n{text}");
    }
    let auth = h.row_with("▸ src/auth.ts").unwrap();
    assert!(auth.contains("DRIFT"), "{auth}");
    let index = h.row_with("src/index.ts ").unwrap();
    assert!(index.contains("blessed"), "{index}");
    // The diff of the selected file: current, then the blessed one.
    let text = h.text();
    assert!(
        text.contains("DRIFT: the current diff is not the blessed one."),
        "{text}"
    );
    assert!(
        text.contains("+  return true; // news: unblessed edit"),
        "{text}"
    );
    assert!(text.contains("Blessed diff (instance store)"), "{text}");
    // Into the files: Esc goes back instead of quitting.
    h.press("enter");
    assert_eq!(pattern(&h).focus, Focus::Files);
    h.press("j");
    assert_eq!(pattern(&h).file_selected, 1);
    assert!(
        h.text()
            .contains("Matches the blessed diff (0 differing lines vs the template)."),
        "{}",
        h.text()
    );
    h.press("esc");
    assert_eq!(pattern(&h).focus, Focus::Repos);
    assert!(h.app.running());
}

#[tokio::test]
async fn store_only_bless_writes_the_store_and_refreshes() {
    let fx = Fixture::new();
    let store = "instance/conformance/news-mcp-worker/src__auth.ts.diff";
    let manifest = "repos/news-mcp-worker/conformance.json";
    let before_manifest = fx.read(manifest);
    assert!(!fx.read(store).contains("unblessed edit"));

    let mut h = fx.harness(WIDE).await;
    conformance(&mut h).await;
    h.press("j");
    h.press("b");
    let text = h.text();
    assert!(
        text.contains("Bless news-mcp-worker (store only)"),
        "{text}"
    );
    assert!(text.contains("working tree is not touched"), "{text}");
    h.press("y");
    h.until_text("blessed (store only").await;
    h.until(|app| {
        let m = app.module::<PatternModule>().unwrap();
        m.busy.is_none() && !m.reports.loading
    })
    .await;
    h.draw();

    assert!(
        fx.read(store)
            .contains("+  return true; // news: unblessed edit")
    );
    assert_eq!(
        fx.read(manifest),
        before_manifest,
        "store-only never touches the repo"
    );
    let news = table_row(&h, NEWS);
    assert!(
        !news.contains("DRIFT") && news.contains("FAIL"),
        "drift is blessed; the repo's hash gate still fails: {news}"
    );
    let report = pattern(&h).selected_report().unwrap();
    assert_eq!(report.drifted(), 0);
}

#[tokio::test]
async fn full_bless_needs_the_typed_repo_name() {
    let fx = Fixture::new();
    let manifest = "repos/news-mcp-worker/conformance.json";
    let before = fx.read(manifest);
    let mut h = fx.harness(WIDE).await;
    conformance(&mut h).await;
    h.press("j");
    h.press("B");
    let text = h.text();
    assert!(text.contains("Full bless news-mcp-worker"), "{text}");
    assert!(text.contains("changes the repo's checkout"), "{text}");
    assert!(pattern(&h).overlay().is_some());
    // Neither Enter nor y confirms; typing goes to the overlay, not the shell.
    h.press("enter");
    h.press("y");
    h.press("q");
    assert!(pattern(&h).overlay().is_some() && h.app.running());
    h.press("esc");
    assert!(pattern(&h).overlay().is_none());
    h.settle().await;
    assert_eq!(fx.read(manifest), before, "cancelled: nothing written");

    h.press("B");
    h.type_text("news-mcp-worke");
    h.press("enter");
    assert!(pattern(&h).overlay().is_some(), "a prefix is not the name");
    h.type_text("r");
    h.press("enter");
    assert!(pattern(&h).overlay().is_none());
    h.until_text("blessed (full").await;
    h.until(|app| {
        let m = app.module::<PatternModule>().unwrap();
        m.busy.is_none() && !m.reports.loading
    })
    .await;
    h.draw();
    let written = fx.read(manifest);
    assert!(
        written.contains("\"scaffold_version\": \"2026-02-01.1\""),
        "{written}"
    );
    let news = table_row(&h, NEWS);
    assert!(
        news.contains("current") && news.contains(" ok ") && !news.contains("DRIFT"),
        "{news}"
    );
}

#[tokio::test]
async fn a_repo_without_a_checkout_is_reported_not_blessed() {
    let fx = Fixture::new();
    std::fs::remove_dir_all(fx.path("repos/news-mcp-worker")).unwrap();
    let mut h = fx.harness(WIDE).await;
    conformance(&mut h).await;
    let news = table_row(&h, NEWS);
    assert!(news.contains("missing"), "{news}");
    h.press("j");
    h.press("b");
    assert!(pattern(&h).overlay().is_none());
    assert!(h.text().contains("no checkout at"), "{}", h.text());
}

#[tokio::test]
async fn the_narrow_layout_stacks_the_panes() {
    let fx = Fixture::new();
    let mut h = fx.harness(NARROW).await;
    // Overview: variables put their description on the next row.
    let text = h.text();
    assert!(text.contains("domain_suffix  mcp.acme.example"), "{text}");
    assert!(
        h.lines()
            .iter()
            .all(|l| l.chars().count() <= NARROW.0 as usize)
    );
    // Skill docs: the list above the document.
    h.press("tab");
    h.press("tab");
    let lines = h.lines();
    let list = lines.iter().position(|l| l.contains("SKILL.md")).unwrap();
    let doc = lines
        .iter()
        .position(|l| l.contains("tiny-ts skill"))
        .unwrap();
    assert!(list < doc, "{}", h.text());
    // Conformance: the table above the detail.
    h.press("tab");
    h.until(|app| {
        app.module::<PatternModule>()
            .unwrap()
            .reports
            .data
            .is_some()
    })
    .await;
    h.draw();
    let lines = h.lines();
    let table = lines.iter().position(|l| l.contains("REPO")).unwrap();
    let checks = lines
        .iter()
        .position(|l| l.contains("pattern.version"))
        .unwrap();
    assert!(table < checks, "{}", h.text());
    // The detail scrolls by page.
    h.key(KeyCode::PageDown, KeyModifiers::NONE);
    assert!(pattern(&h).view == View::Conformance);
}

#[tokio::test]
async fn refresh_reloads_and_announces() {
    let fx = Fixture::new();
    let mut h = fx.harness(WIDE).await;
    conformance(&mut h).await;
    // Fix news's drift on disk behind the module's back, then re-run.
    std::fs::copy(
        fx.path("repos/weather-mcp-worker/wrangler.toml"),
        fx.path("repos/news-mcp-worker/wrangler.toml"),
    )
    .unwrap();
    h.press("r");
    h.until_text("Checked 2 repo(s)").await;
    let status = h.lines().last().unwrap().clone();
    let news = table_row(&h, NEWS);
    assert!(news.contains("0✗"), "{news}\n{status}");
}
