//! The read screens, driven through the headless harness against the fake
//! gateway: what is drawn, what is requested, and what a view-only or
//! signed-out session sees instead.

mod common;

use common::{Env, gw, load};
use studio_fake::Options;
use studio_gateway::tokens::{TokenStore, Tokens};
use studio_tui::gateway::Screen;

const WIDE: (u16, u16) = (150, 40);

#[tokio::test]
async fn header_names_the_admin_and_the_gateway() {
    let env = Env::new().await;
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    let top = h.row_with("admin@example.org").unwrap();
    assert!(top.contains(" admin "), "{top}");
    assert!(top.contains("127.0.0.1") && top.contains(" main "), "{top}");
    assert!(top.contains("Servers") && top.contains("Policy"), "{top}");
}

#[tokio::test]
async fn servers_table_lists_every_server_with_status_health_and_access() {
    let env = Env::new().await;
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    let text = h.text();
    assert!(text.contains("Servers (4)"), "{text}");
    let notes = h.row_with("│notes").unwrap_or_default();
    assert!(
        notes.contains("active")
            && notes.contains(" ok ")
            && notes.contains("RO")
            && notes.contains("m2m"),
        "{notes}"
    );
    let tasks = h.row_with("│tasks").unwrap();
    assert!(
        tasks.contains("degraded") && tasks.contains("RW"),
        "{tasks}"
    );
    assert!(tasks.contains("refresh: upstream"), "{tasks}");
    // okta_user reports the signed-in caller's connection.
    assert!(tasks.contains("user:connect"), "{tasks}");
    assert!(h.row_with("│weather").unwrap().contains("disabled"));
    assert!(h.row_with("│scratch").unwrap().contains("quarantined"));
    // The list is asked for once, with inactive servers and classifications.
    assert_eq!(env.called("list_servers"), 1);
}

#[tokio::test]
async fn detail_shows_the_selected_server_and_its_classified_tools() {
    let env = Env::new().await;
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    let text = h.text();
    assert!(text.contains("https://notes.mcp.example.org/mcp"), "{text}");
    assert!(text.contains("read-only (2 of 5 tools callable)"), "{text}");
    assert!(text.contains("notes:read notes:write"), "{text}");
    assert!(text.contains("okta_m2m"), "{text}");
    let tool = h.row_with("notes_trash").unwrap();
    assert!(
        tool.contains("destructive") && tool.contains("annotation"),
        "{tool}"
    );

    h.press("j");
    let text = h.text();
    assert!(text.contains("https://tasks.mcp.example.org/mcp"), "{text}");
    assert!(text.contains("read-write"), "{text}");
    assert!(text.contains("refresh error"), "{text}");

    h.press("G");
    assert!(h.text().contains("timeout after 30000ms"));
    h.press("g");
    assert!(h.text().contains("https://notes.mcp.example.org/mcp"));
}

#[tokio::test]
async fn a_narrow_terminal_stacks_the_detail_under_the_table() {
    let env = Env::new().await;
    let mut h = env.harness((90, 40)).await;
    load(&mut h).await;
    let lines = h.lines();
    let table_row = lines
        .iter()
        .position(|l| l.contains("Servers (4)"))
        .unwrap();
    let detail_row = lines
        .iter()
        .position(|l| l.contains("https://notes.mcp.example.org/mcp"))
        .unwrap();
    assert!(detail_row > table_row + 5, "{}", h.text());
}

#[tokio::test]
async fn usage_screen_shows_per_tool_stats_and_recent_runs() {
    let env = Env::new().await;
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    h.press("tab");
    h.until_text("By tool").await;
    let text = h.text();
    assert!(text.contains("57 calls, 5 failed, last 7 days"), "{text}");
    assert!(text.contains("timeout 3"), "{text}");
    let row = h.row_with("tasks.tasks_list").unwrap();
    assert!(row.contains("18") && row.contains("1900"), "{row}");
    assert!(text.contains("Recent runs (12)"), "{text}");

    h.press("d");
    h.until_text("last 30 days").await;
}

#[tokio::test]
async fn policy_screen_shows_mode_events_and_the_selected_event() {
    let env = Env::new().await;
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    h.press("shift+tab");
    assert_eq!(gw(&h.app).screen, Screen::Policy);
    h.until_text("Events").await;
    let text = h.text();
    assert!(text.contains("mode observe"), "{text}");
    assert!(text.contains("fingerprint 5f3a9c1e"), "{text}");
    assert!(text.contains("3 events in 1 day"), "{text}");
    assert!(
        h.row_with("notes.notes_create")
            .unwrap()
            .contains("blocked")
    );
    assert!(
        text.contains("notes is read-only and notes_create is classified write"),
        "{text}"
    );
    assert!(text.contains("{\"title\":\"Groceries\"}"), "{text}");

    h.press("j");
    assert!(h.text().contains("is not in the registry's catalogue"));

    h.press("f");
    h.until_text("filter: deny").await;
    h.until_text("2 events").await;
    assert!(h.row_with("tasks.tasks_nuke").is_none());
    h.press("d");
    h.until_text("in 7 days").await;
}

#[tokio::test]
async fn view_only_never_asks_for_admin_tools() {
    let env = Env::with(Options {
        admin_subs: vec![],
        ..Options::default()
    })
    .await;
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    assert!(
        h.row_with("admin@example.org")
            .unwrap()
            .contains("view-only")
    );
    h.press("tab");
    h.until_text("Usage needs gateway:admin").await;
    h.press("tab");
    h.until_text("Policy needs gateway:admin").await;
    h.settle().await;
    assert_eq!(env.called("usage_stats"), 0);
    assert_eq!(env.called("policy_events"), 0);
    // The servers screen still works.
    h.press("tab");
    assert!(h.text().contains("Servers (4)"));
}

#[tokio::test]
async fn an_admin_screen_opened_before_whoami_answers_loads_once_it_does() {
    let env = Env::new().await;
    let mut h = env.harness(WIDE).await;
    h.press("tab");
    h.until_text("By tool").await;
    assert_eq!(gw(&h.app).screen, Screen::Usage);
    assert_eq!(env.called("usage_stats"), 1);
}

#[tokio::test]
async fn refresh_reloads_and_the_help_overlay_opens_and_closes() {
    let env = Env::new().await;
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    h.press("r");
    h.until(|app| !gw(app).servers.loading).await;
    assert_eq!(env.called("list_servers"), 2);
    assert!(h.text().contains("Refreshing"));
    h.until_text("updated").await;

    h.press("?");
    assert!(h.text().contains("refresh from the gateway"));
    h.press("esc");
    assert!(!h.text().contains("refresh from the gateway"));
    assert!(h.app.running());
    h.press("q");
    assert!(!h.app.running());
}

#[tokio::test]
async fn not_signed_in_says_how_to_fix_it() {
    let env = Env::new().await;
    let mut h = env.harness_signed_out();
    h.until_text("mcp-studio gateway login").await;
    assert!(h.text().contains("not signed in"));
    assert!(h.text().contains("Press L to sign in"));
    assert!(h.lines().last().unwrap().contains("L sign in"));
}

#[tokio::test]
async fn an_expired_sign_in_replaces_the_screen_with_the_fix() {
    let env = Env::new().await;
    let tokens = env.login().await.unwrap();
    env.store()
        .save(&Tokens {
            expires_at: 0,
            ..tokens
        })
        .unwrap();
    env.fake.revoke_all_grants();
    let mut h = common::harness_for(vec![std::sync::Arc::new(env.session())], WIDE);
    h.until_text("sign-in has expired").await;
    assert!(h.text().contains("mcp-studio gateway login"));
    assert!(h.text().contains("Press L"));
}
