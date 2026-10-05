//! The Marketplaces module end to end, headless: a provisioned acme
//! marketplace in a temp git repo with a local bare remote, a second one
//! that is not cloned, and three fleet server repos. No network.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use studio_core::{Instance, StudioConfig};
use studio_marketplace::model::{AuthType, Kind};
use studio_marketplace::ops::{self, EntryPatch};
use studio_marketplace::provision::{self, ProvisionOptions};
use studio_tui::marketplace::{Focus, MarketplaceModule, View};
use studio_tui::{App, Harness};

const WIDE: (u16, u16) = (150, 45);
const NARROW: (u16, u16) = (72, 32);

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

struct Env {
    _tmp: tempfile::TempDir,
    instance: Arc<Instance>,
    clone: PathBuf,
    remote: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let repos = root.join("repos");
        let text = format!(
            r#"
[instance]
name = "acme-test"

[fleet]
repos_dir = "{repos}"
include = ["acme/acme-search", "acme/acme-notes"]

[[fleet.server]]
repo = "acme/acme-tickets-server"
marketplace_slug = "acme-tickets"

[[marketplace]]
id = "acme"
repo = "acme/acme-plugin-marketplace"
owner_name = "Acme Team"
publish = "push"

[[marketplace]]
id = "beta"
repo = "acme/beta-marketplace"
owner_name = "Acme Beta"
publish = "push"
"#,
            repos = repos.display()
        );
        let config = StudioConfig::parse(&text).unwrap();
        assert!(config.validate().is_empty(), "{:?}", config.validate());
        let instance = Instance {
            root: root.clone(),
            config,
        };

        // The acme marketplace: seeded, two entries, pushed to a bare remote.
        let acme = studio_marketplace::resolve(&instance, "acme").unwrap();
        let clone = repos.join("acme-plugin-marketplace");
        std::fs::create_dir_all(&clone).unwrap();
        git(&clone, &["init", "-q", "-b", "main"]);
        git(&clone, &["config", "user.name", "Acme Tester"]);
        git(&clone, &["config", "user.email", "tester@acme.example"]);
        git(&clone, &["config", "commit.gpgsign", "false"]);
        provision::provision(&clone, &acme.market, ProvisionOptions::default()).unwrap();
        let m = &acme.market;
        for (slug, patch, kind) in [
            (
                "acme-search",
                EntryPatch {
                    name: Some("Acme Search".into()),
                    description: Some("Search Acme's product documentation.".into()),
                    url: Some("https://search.mcp.acme.example/mcp".into()),
                    version: Some("0.2.0".into()),
                    tags: Some(vec!["search".into()]),
                    ..Default::default()
                },
                Kind::McpServer,
            ),
            (
                "acme-tickets",
                EntryPatch {
                    name: Some("Acme Tickets".into()),
                    description: Some("File and track Acme support tickets.".into()),
                    url: Some("https://tickets.mcp.acme.example/mcp".into()),
                    auth: Some(AuthType::Bearer),
                    version: Some("1.0.0".into()),
                    ..Default::default()
                },
                Kind::McpServer,
            ),
        ] {
            let cs = ops::plan_add(&clone, m, patch.into_entry(kind), Some(slug)).unwrap();
            ops::apply_checked(&clone, m, &cs).unwrap();
            ops::commit(&clone, &cs).unwrap();
        }
        let remote = root.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare", "-b", "main"]);
        git(
            &clone,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&clone, &["push", "-q", "-u", "origin", "main"]);

        // Fleet server repos.
        for (name, version, url) in [
            (
                "acme-search",
                "0.3.0",
                "https://search.mcp.acme.example/mcp",
            ),
            ("acme-notes", "0.5.0", "https://notes.mcp.acme.example/mcp"),
            (
                "acme-tickets-server",
                "1.0.0",
                "https://tickets.mcp.acme.example/mcp",
            ),
        ] {
            let dir = repos.join(name);
            write(
                &dir.join("package.json"),
                &format!(
                    r#"{{"name": "{name}", "version": "{version}", "description": "Notes for Acme teams."}}"#
                ),
            );
            write(
                &dir.join("wrangler.toml"),
                &format!("name = \"{name}\"\n\n[vars]\nPUBLIC_MCP_URL = \"{url}\"\n"),
            );
        }

        Env {
            _tmp: tmp,
            instance: Arc::new(instance),
            clone,
            remote,
        }
    }

    async fn harness(&self, size: (u16, u16)) -> Harness {
        let module = MarketplaceModule::new(self.instance.clone());
        let (app, rx) = App::new("Test", vec![Box::new(module)]);
        let mut h = Harness::new(app, rx, size);
        idle(&mut h).await;
        h
    }

    fn subject(&self) -> String {
        git(&self.clone, &["log", "-1", "--format=%s"])
    }

    fn commits(&self) -> usize {
        git(&self.clone, &["rev-list", "--count", "HEAD"])
            .parse()
            .unwrap()
    }
}

fn module(app: &App) -> &MarketplaceModule {
    app.module::<MarketplaceModule>().expect("the module")
}

/// Loaded, and nothing running.
async fn idle(h: &mut Harness) {
    h.until(|a| {
        let m = module(a);
        m.snapshot.data.is_some() && m.idle()
    })
    .await;
}

async fn preview(h: &mut Harness) {
    h.until(|a| module(a).pager().is_some()).await;
}

/// Everything in the open preview or report, scrolled or not.
fn pager_text(h: &Harness) -> String {
    module(&h.app)
        .pager()
        .expect("a pager")
        .lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn toast(h: &Harness) -> String {
    h.app.toast().map(|t| t.text.clone()).unwrap_or_default()
}

/// Focus the entries table and select `slug`.
fn select_entry(h: &mut Harness, slug: &str) {
    if module(&h.app).focus != Focus::Entries {
        h.press("tab");
    }
    for _ in 0..10 {
        if module(&h.app)
            .selected_entry()
            .is_some_and(|e| e.slug() == slug)
        {
            return;
        }
        h.press("j");
    }
    panic!("{slug} not found:\n{}", h.text());
}

/// Confirms the open preview and waits for the commit and reload.
async fn apply(h: &mut Harness) {
    h.press("enter");
    h.until(|a| module(a).busy.is_none() && module(a).idle() && module(a).pager().is_none())
        .await;
}

#[tokio::test]
async fn lists_marketplaces_entries_and_the_selected_entry() {
    let env = Env::new();
    let mut h = env.harness(WIDE).await;
    let text = h.text();
    for needle in [
        "Marketplaces (2)",
        "acme/acme-plugin-marketplace",
        "claude+codex",
        "2 entries",
        "beta",
        "not cloned",
        "acme entries (2)",
        "acme-search",
        "acme-tickets",
    ] {
        assert!(text.contains(needle), "{needle}:\n{text}");
    }
    let row = h.row_with("acme-tickets").unwrap();
    assert!(
        row.contains("bearer") && row.contains("1.0.0") && row.contains("ok"),
        "{row}"
    );
    // The list's rollup: valid and in sync.
    assert!(h.row_with("acme  valid").is_some(), "{text}");

    select_entry(&mut h, "acme-tickets");
    let text = h.text();
    for needle in [
        "Acme Tickets",
        "servers/acme-tickets/server.yaml",
        "url:",
        "https://tickets.mcp.acme.example/mcp",
        "Files (",
        ".mcp.json",
        ".claude-plugin/plugin.json",
        "Reconcile",
    ] {
        assert!(text.contains(needle), "{needle}:\n{text}");
    }
    // The footer is generated from the keymap.
    let footer = h.lines().last().unwrap().clone();
    assert!(
        footer.contains("a add") && footer.contains("P publish"),
        "{footer}"
    );

    // The not-cloned marketplace says how to fix it.
    h.press("tab");
    h.press("j");
    assert!(h.text().contains("Not cloned at"), "{}", h.text());
    assert!(h.text().contains("acme/beta-marketplace"));
}

#[tokio::test]
async fn reconcile_shows_planted_drift_and_regenerate_fixes_it() {
    let env = Env::new();
    let mcp = env.clone.join("servers/acme-search/.mcp.json");
    let planted = std::fs::read_to_string(&mcp)
        .unwrap()
        .replace("search.mcp.acme.example", "old-search.acme.example");
    std::fs::write(&mcp, planted).unwrap();
    git(&env.clone, &["commit", "-qam", "hand edit"]);
    let mut h = env.harness(WIDE).await;

    let row = h.row_with("acme-search").unwrap();
    assert!(row.contains("FAIL") || row.contains("warn"), "{row}");
    assert!(h.row_with("acme  drift").is_some(), "{}", h.text());

    h.press("R");
    h.until(|a| module(a).pager().is_some()).await;
    assert!(h.text().contains("Reconcile acme"), "{}", h.text());
    let text = pager_text(&h);
    assert!(text.contains("Verify DIFFERS"), "{text}");
    assert!(
        text.contains("changed  servers/acme-search/.mcp.json"),
        "{text}"
    );
    assert!(
        text.contains("+++ b/servers/acme-search/.mcp.json"),
        "{text}"
    );
    h.press("esc");
    assert!(module(&h.app).pager().is_none());

    select_entry(&mut h, "acme-search");
    h.press("g");
    preview(&mut h).await;
    assert!(h.text().contains("update: Acme Search"), "{}", h.text());
    let text = pager_text(&h);
    assert!(
        text.contains("modify  servers/acme-search/.mcp.json"),
        "{text}"
    );
    assert!(text.contains("old-search.acme.example"), "{text}");
    apply(&mut h).await;
    assert!(toast(&h).contains("Committed"), "{}", toast(&h));
    assert_eq!(env.subject(), "update: Acme Search");
    let acme = studio_marketplace::resolve(&env.instance, "acme").unwrap();
    assert!(
        studio_marketplace::verify(&env.clone, &acme.market)
            .unwrap()
            .is_clean()
    );
    let row = h.row_with("acme-search").unwrap();
    assert!(row.contains("ok"), "{row}");
    // Regenerating a clean entry has nothing to do.
    h.press("g");
    h.until(|a| module(a).idle()).await;
    assert!(toast(&h).contains("nothing to change"), "{}", toast(&h));
}

#[tokio::test]
async fn add_through_the_form_previews_then_commits() {
    let env = Env::new();
    let mut h = env.harness(WIDE).await;
    let before = env.commits();

    // A slug that exists: the plan fails and the form comes back with why.
    h.press("a");
    assert!(module(&h.app).overlay().is_some());
    h.type_text("acme-search");
    h.press("tab");
    h.type_text("Acme Search Again");
    h.press("tab");
    h.press("tab");
    h.type_text("Again.");
    h.press("tab");
    h.type_text("https://search2.mcp.acme.example/mcp");
    h.press("enter");
    h.until_text("already exists").await;
    assert!(module(&h.app).overlay().is_some());
    h.press("esc");

    // A bad enum is caught in the form.
    h.press("a");
    for _ in 0..6 {
        h.press("tab");
    }
    h.type_text("magic");
    h.press("enter");
    assert!(h.text().contains("auth \"magic\""), "{}", h.text());
    h.press("esc");

    h.press("a");
    h.type_text("acme-maps");
    h.press("tab");
    h.type_text("Acme Maps");
    h.press("tab"); // kind: mcp-server
    h.press("tab");
    h.type_text("Maps of Acme sites.");
    h.press("tab");
    h.type_text("https://maps.mcp.acme.example/mcp");
    h.press("enter");
    preview(&mut h).await;
    assert!(
        h.text().contains("Preview acme: add: Acme Maps"),
        "{}",
        h.text()
    );
    let text = pager_text(&h);
    for needle in [
        "create  servers/acme-maps/server.yaml",
        "modify  .claude-plugin/marketplace.json",
        "+name: Acme Maps",
    ] {
        assert!(text.contains(needle), "{needle}:\n{text}");
    }
    // Scrolling works and does not close it.
    h.press("j");
    h.press(" ");
    assert!(module(&h.app).pager().unwrap().scroll > 0);
    apply(&mut h).await;
    assert_eq!(env.subject(), "add: Acme Maps");
    assert_eq!(env.commits(), before + 1);
    assert!(env.clone.join("servers/acme-maps/.mcp.json").is_file());
    let m = module(&h.app);
    assert_eq!(m.selected_entry().unwrap().slug(), "acme-maps");
    assert!(h.text().contains("acme entries (3)"));
    assert_eq!(git(&env.clone, &["status", "--porcelain"]), "");
    assert!(h.text().contains("1 unpushed"), "{}", h.text());
}

#[tokio::test]
async fn deprecate_reinstate_and_remove() {
    let env = Env::new();
    let mut h = env.harness(WIDE).await;
    select_entry(&mut h, "acme-tickets");

    // Esc on a preview writes nothing.
    h.press("d");
    h.type_text("Use the Acme Desk server instead.");
    h.press("enter");
    preview(&mut h).await;
    assert!(pager_text(&h).contains("Reason: Use the Acme Desk server instead."));
    h.press("esc");
    assert!(toast(&h).contains("nothing was written"));
    assert_eq!(git(&env.clone, &["status", "--porcelain"]), "");

    h.press("d");
    h.press("enter");
    assert!(h.text().contains("needs a reason"), "{}", h.text());
    h.type_text("Use the Acme Desk server instead.");
    h.press("enter");
    preview(&mut h).await;
    apply(&mut h).await;
    assert_eq!(env.subject(), "deprecate: Acme Tickets");
    let row = h.row_with("acme-tickets").unwrap();
    assert!(row.contains("yes"), "{row}");
    assert!(
        h.text().contains("deprecated: Use the Acme Desk"),
        "{}",
        h.text()
    );

    h.press("u");
    preview(&mut h).await;
    assert!(h.text().contains("reinstate: Acme Tickets"));
    apply(&mut h).await;
    assert_eq!(env.subject(), "reinstate: Acme Tickets");
    assert!(!h.row_with("acme-tickets").unwrap().contains("yes"));

    // Reinstating what is not deprecated is refused.
    h.press("u");
    h.until(|a| module(a).idle()).await;
    assert!(toast(&h).contains("not deprecated"), "{}", toast(&h));

    h.press("D");
    h.press("enter"); // nothing typed: stays open
    assert!(module(&h.app).overlay().is_some());
    h.type_text("acme-tickets");
    h.press("enter");
    preview(&mut h).await;
    assert!(pager_text(&h).contains("delete  servers/acme-tickets/server.yaml"));
    apply(&mut h).await;
    assert_eq!(env.subject(), "remove: Acme Tickets");
    assert!(!env.clone.join("servers/acme-tickets").exists());
    assert!(h.row_with("acme-tickets").is_none(), "{}", h.text());
    assert!(h.text().contains("acme entries (1)"));
}

#[tokio::test]
async fn publish_pushes_unpushed_commits_to_the_remote() {
    let env = Env::new();
    let mut h = env.harness(WIDE).await;

    // Nothing to publish yet.
    h.press("P");
    h.until(|a| module(a).idle()).await;
    assert!(toast(&h).contains("nothing to publish"), "{}", toast(&h));

    select_entry(&mut h, "acme-tickets");
    h.press("d");
    h.type_text("Superseded.");
    h.press("enter");
    preview(&mut h).await;
    apply(&mut h).await;
    let head = git(&env.clone, &["rev-parse", "HEAD"]);
    assert_ne!(git(&env.remote, &["rev-parse", "main"]), head);

    h.press("P");
    h.until(|a| module(a).overlay().is_some()).await;
    let text = h.text();
    assert!(text.contains("Publish 1 commit(s)"), "{text}");
    assert!(text.contains("deprecate: Acme Tickets"), "{text}");
    assert!(text.contains("Route: push to main"), "{text}");
    h.press("y");
    h.until(|a| module(a).idle() && module(a).overlay().is_none())
        .await;
    assert!(
        toast(&h).contains("Pushed to acme/acme-plugin-marketplace:main"),
        "{}",
        toast(&h)
    );
    assert_eq!(git(&env.remote, &["rev-parse", "main"]), head);
    assert!(!h.text().contains("unpushed"), "{}", h.text());
}

#[tokio::test]
async fn refuses_changes_to_a_missing_or_dirty_clone() {
    let env = Env::new();
    let mut h = env.harness(WIDE).await;

    // beta is not cloned.
    h.press("j");
    assert_eq!(module(&h.app).selected_market().unwrap().id(), "beta");
    for key in ["a", "P", "V", "R"] {
        h.press(key);
        assert!(module(&h.app).overlay().is_none(), "{key}");
        assert!(toast(&h).contains("not cloned"), "{key}: {}", toast(&h));
    }
    h.press("k");

    // Dirty after loading: the job's own check refuses and the form says so.
    write(&env.clone.join("notes.txt"), "scratch");
    select_entry(&mut h, "acme-search");
    h.press("d");
    h.type_text("Old.");
    h.press("enter");
    h.until_text("uncommitted changes").await;
    assert!(module(&h.app).pager().is_none());
    h.press("esc");
    // Regenerate has no form: the refusal is a toast.
    h.press("g");
    h.until(|a| module(a).idle()).await;
    assert!(toast(&h).contains("uncommitted changes"), "{}", toast(&h));

    // Reloaded, the list shows it and actions are refused up front.
    h.press("r");
    idle(&mut h).await;
    assert!(h.text().contains("cloned, dirty"), "{}", h.text());
    h.press("e");
    assert!(module(&h.app).overlay().is_none());
    assert!(toast(&h).contains("uncommitted changes"), "{}", toast(&h));
    h.press("P");
    assert!(toast(&h).contains("uncommitted changes"), "{}", toast(&h));
    assert_eq!(env.subject(), "add: Acme Tickets");
}

#[tokio::test]
async fn edit_prefills_the_form_and_changes_only_what_changed() {
    let env = Env::new();
    let mut h = env.harness(WIDE).await;
    select_entry(&mut h, "acme-search");
    h.press("e");
    let text = h.text();
    assert!(text.contains("Edit acme-search in acme"), "{text}");
    assert!(
        text.contains("Search Acme's product documentation."),
        "{text}"
    );
    // Unchanged: nothing to do.
    h.press("enter");
    assert!(toast(&h).contains("Nothing changed"));
    h.press("e");
    // name, description, url, transport, auth, header, tags, category, homepage, version
    for _ in 0..9 {
        h.press("tab");
    }
    h.erase(5);
    h.type_text("0.3.0");
    h.press("enter");
    preview(&mut h).await;
    assert!(
        pager_text(&h).contains("+version: 0.3.0"),
        "{}",
        pager_text(&h)
    );
    apply(&mut h).await;
    assert_eq!(env.subject(), "update: Acme Search");
    assert!(h.row_with("acme-search").unwrap().contains("0.3.0"));
}

#[tokio::test]
async fn validate_reports_in_a_scrollable_overlay() {
    let env = Env::new();
    let mut h = env.harness(WIDE).await;
    h.press("V");
    h.until(|a| module(a).pager().is_some()).await;
    assert!(h.text().contains("Validate acme"), "{}", h.text());
    assert!(h.text().contains("valid:"), "{}", h.text());
    h.press("q");
    assert!(module(&h.app).pager().is_none());
    // q closed the report; it did not quit.
    assert!(h.app.running());
}

#[tokio::test]
async fn fleet_view_shows_which_marketplaces_list_each_server() {
    let env = Env::new();
    let mut h = env.harness(WIDE).await;
    h.press("v");
    assert_eq!(module(&h.app).view, View::Fleet);
    let text = h.text();
    assert!(text.contains("Fleet servers (3)"), "{text}");
    let search = h.row_with("acme/acme-search ").unwrap();
    assert!(
        search.contains("0.3.0") && search.contains("0.2.0 differs"),
        "{search}"
    );
    let tickets = h.row_with("acme/acme-tickets-server").unwrap();
    assert!(
        tickets.contains("acme-tickets") && tickets.contains("1.0.0 ok"),
        "{tickets}"
    );
    assert!(tickets.contains('?'), "beta is not cloned: {tickets}");
    // The detail says where the slug came from.
    h.press("j");
    h.press("j");
    assert!(h.text().contains("marketplace_slug"), "{}", h.text());
    assert!(h.text().contains("matches package.json"), "{}", h.text());
    h.press("v");
    assert_eq!(module(&h.app).view, View::Catalog);
}

#[tokio::test]
async fn draft_from_a_fleet_repo_then_add() {
    let env = Env::new();
    let mut h = env.harness(WIDE).await;
    h.press("A");
    assert!(h.text().contains("acme/acme-notes"), "{}", h.text());
    h.press("j");
    h.press("enter");
    h.until(|a| module(a).overlay().is_some() && module(a).idle())
        .await;
    let text = h.text();
    assert!(text.contains("Add a drafted entry to acme"), "{text}");
    assert!(
        text.contains("https://notes.mcp.acme.example/mcp"),
        "{text}"
    );
    assert!(text.contains("Acme Notes"), "{text}");
    h.press("enter");
    preview(&mut h).await;
    assert!(h.text().contains("add: Acme Notes"));
    apply(&mut h).await;
    assert_eq!(env.subject(), "add: Acme Notes");
    let yaml = std::fs::read_to_string(env.clone.join("servers/acme-notes/server.yaml")).unwrap();
    assert!(
        yaml.contains("version: 0.5.0") && yaml.contains("type: oauth"),
        "{yaml}"
    );
}

#[tokio::test]
async fn narrow_layout_stacks_the_panes() {
    let env = Env::new();
    let mut h = env.harness(NARROW).await;
    let text = h.text();
    assert!(
        text.contains("acme") && text.contains("acme-search"),
        "{text}"
    );
    select_entry(&mut h, "acme-tickets");
    assert!(h.text().contains("Acme Tickets"), "{}", h.text());
    h.press("d");
    h.type_text("Narrow.");
    h.press("enter");
    preview(&mut h).await;
    assert!(h.text().contains("deprecate: Acme Tickets"), "{}", h.text());
    h.press("esc");
    h.press("v");
    assert!(h.text().contains("Fleet servers (3)"), "{}", h.text());
}
