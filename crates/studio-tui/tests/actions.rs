//! Admin actions through the TUI against the fake gateway: each form, choice
//! and confirmation, the typed-id guard on delete and URL change, and how a
//! refusal, a 403 and an expired sign-in are surfaced.

mod common;

use common::{Env, finish_action, gw, load, select};
use studio_fake::Options;
use studio_tui::Harness;
use studio_tui::framework::overlay::Overlay;

const WIDE: (u16, u16) = (150, 45);

async fn ready(env: &Env) -> Harness {
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    h
}

fn called(env: &Env, tool: &str) -> usize {
    env.called(tool)
}

#[tokio::test]
async fn register_a_server_from_the_form() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    h.press("a");
    assert!(h.text().contains("Register a server"));
    h.type_text("https://newbie.mcp.example.org/mcp");
    h.press("tab");
    h.type_text("newbie");
    h.press("tab");
    h.press("tab");
    h.type_text("60000");
    h.press("enter");
    finish_action(&mut h).await;
    assert!(
        h.text().contains("Registered newbie with 2 tools"),
        "{}",
        h.text()
    );
    assert!(h.text().contains("Servers (5)"));
    select(&mut h, "newbie");
    assert!(h.text().contains("60000 ms"));
}

#[tokio::test]
async fn a_bad_form_says_why_and_sends_nothing() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    h.press("a");
    h.type_text("http://plain.example/mcp");
    h.press("enter");
    assert!(h.text().contains("the URL must start with https://"));
    h.press("esc");
    assert!(gw(&h.app).overlay().is_none());
    h.press("t");
    h.erase(10);
    h.type_text("5");
    h.press("enter");
    assert!(h.text().contains("1000-120000"));
    h.press("esc");
    assert_eq!(
        called(&env, "register_server") + called(&env, "set_server_timeout"),
        0
    );
}

#[tokio::test]
async fn a_refused_registration_shows_the_gateways_reason() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    h.press("a");
    h.type_text("https://refuse.example.com/mcp");
    h.press("enter");
    finish_action(&mut h).await;
    assert!(
        h.text().contains(
            "Refused: Error: refuse.example.com is outside the gateway's trust perimeter"
        ),
        "{}",
        h.text()
    );
}

#[tokio::test]
async fn edit_the_timeout() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    select(&mut h, "tasks");
    h.press("t");
    h.erase(10);
    h.type_text("90000");
    h.press("enter");
    finish_action(&mut h).await;
    assert!(
        h.text().contains("tasks timeout 15000 -> 90000 ms"),
        "{}",
        h.text()
    );
    assert!(h.text().contains("90000 ms"));
}

#[tokio::test]
async fn restore_a_quarantined_server_and_disable_another() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    select(&mut h, "scratch");
    h.press("s");
    assert!(h.text().contains("scratch is quarantined."));
    h.press("a");
    finish_action(&mut h).await;
    assert!(
        h.row_with("│scratch").unwrap().contains("active"),
        "{}",
        h.text()
    );

    select(&mut h, "tasks");
    h.press("s");
    h.press("d");
    finish_action(&mut h).await;
    assert!(h.row_with("│tasks").unwrap().contains("disabled"));
}

#[tokio::test]
async fn read_only_confirms_with_the_callable_count() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    select(&mut h, "tasks");
    h.press("o");
    let text = h.text();
    assert!(text.contains("Make tasks read-only"), "{text}");
    assert!(text.contains("0 of 2 tools remain callable"), "{text}");
    assert!(
        text.contains("every call to this server will be refused"),
        "{text}"
    );
    h.press("n");
    assert!(gw(&h.app).overlay().is_none());
    assert_eq!(called(&env, "set_server_access"), 0);

    h.press("o");
    h.press("y");
    finish_action(&mut h).await;
    assert!(h.row_with("│tasks").unwrap().contains("RO"));

    h.press("o");
    assert!(h.text().contains("becomes callable again"));
    h.press("enter");
    finish_action(&mut h).await;
    assert!(h.row_with("│tasks").unwrap().contains("RW"));
}

#[tokio::test]
async fn classify_a_tool_from_the_picker() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    select(&mut h, "notes");
    assert!(h.text().contains("(2 of 5 tools callable)"));
    h.press("c");
    assert!(matches!(gw(&h.app).overlay(), Some(Overlay::Picker(_))));
    for _ in 0..4 {
        h.press("j");
    }
    h.press("enter");
    assert!(
        h.text().contains("Classify notes.notes_tags"),
        "{}",
        h.text()
    );
    h.press("r");
    finish_action(&mut h).await;
    assert!(
        h.text().contains("notes.notes_tags: read (from admin)"),
        "{}",
        h.text()
    );
    assert!(h.text().contains("(3 of 5 tools callable)"), "{}", h.text());
}

#[tokio::test]
async fn refresh_reports_scope_drift_and_approval_adopts_it() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    select(&mut h, "scratch");
    h.press("A");
    assert!(h.text().contains("No scope drift recorded for scratch"));
    h.press("x");
    finish_action(&mut h).await;
    assert!(
        h.text().contains("scope drift on scratch (A to review)"),
        "{}",
        h.text()
    );
    assert!(h.text().contains("files:write scratch:call"));
    h.press("A");
    let text = h.text();
    assert!(text.contains("approved    scratch:call"), "{text}");
    assert!(
        text.contains("advertised  files:write scratch:call"),
        "{text}"
    );
    h.press("y");
    finish_action(&mut h).await;
    assert!(
        h.text()
            .contains("scratch now minted: files:write scratch:call"),
        "{}",
        h.text()
    );
    assert!(!h.text().contains("scope drift; A to approve"));
    assert!(gw(&h.app).drift.is_empty());
}

#[tokio::test]
async fn refresh_every_server() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    h.press("X");
    finish_action(&mut h).await;
    assert!(h.text().contains("Refreshed 4, 0 failed"), "{}", h.text());
}

#[tokio::test]
async fn delete_needs_the_id_typed_exactly() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    select(&mut h, "weather");
    h.press("D");
    assert!(h.text().contains("Delete weather"));
    h.press("enter");
    h.type_text("weathe");
    h.press("enter");
    assert!(gw(&h.app).overlay().is_some());
    assert_eq!(called(&env, "unregister_server"), 0);
    h.type_text("r");
    h.press("enter");
    finish_action(&mut h).await;
    assert!(
        h.text()
            .contains("Deleted weather (https://weather.example.com/mcp)"),
        "{}",
        h.text()
    );
    assert!(h.row_with("│weather").is_none());
}

fn legacy() -> Options {
    Options {
        legacy: true,
        ..Options::default()
    }
}

#[tokio::test]
async fn change_url_updates_in_place_when_the_gateway_offers_update_server() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    select(&mut h, "notes");
    h.press("u");
    h.erase(100);
    h.type_text("https://notes2.mcp.example.org/mcp");
    h.press("enter");
    let text = h.text();
    assert!(text.contains("Updates it in place"), "{text}");
    h.type_text("notes");
    h.press("enter");
    finish_action(&mut h).await;
    let text = h.text();
    assert!(
        text.contains(
            "notes: url https://notes.mcp.example.org/mcp -> https://notes2.mcp.example.org/mcp; refreshed, 5 tools"
        ),
        "{text}"
    );
    assert_eq!(called(&env, "update_server"), 1);
    assert_eq!(called(&env, "unregister_server"), 0);
    assert_eq!(called(&env, "register_server"), 0);
    select(&mut h, "notes");
    let text = h.text();
    assert!(
        text.contains("https://notes2.mcp.example.org/mcp"),
        "{text}"
    );
    // Nothing else was touched: classifications, access and timeout stay.
    assert!(text.contains("(2 of 5 tools callable)"), "{text}");
    assert!(h.row_with("│notes").unwrap().contains("RO"));
    assert!(text.contains("30000 ms"));
}

#[tokio::test]
async fn a_failed_probe_through_update_server_changes_nothing() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    select(&mut h, "tasks");
    h.press("u");
    h.erase(100);
    h.type_text("https://fail.example.org/mcp");
    h.press("enter");
    h.type_text("tasks");
    h.press("enter");
    finish_action(&mut h).await;
    let text = h.text();
    assert!(
        text.contains("Refused: Error: probe failed: fail.example.org answered HTTP 502"),
        "{text}"
    );
    assert_eq!(called(&env, "unregister_server"), 0);
    select(&mut h, "tasks");
    assert!(h.text().contains("https://tasks.mcp.example.org/mcp"));
}

#[tokio::test]
async fn edit_details_through_update_server() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    select(&mut h, "tasks");
    h.press("e");
    assert!(h.text().contains("Details of tasks"), "{}", h.text());
    h.press("enter");
    assert!(h.text().contains("nothing changed"), "{}", h.text());
    h.erase(20);
    h.type_text("Team tasks");
    h.press("tab");
    h.erase(40);
    h.type_text("Shared task lists");
    h.press("enter");
    finish_action(&mut h).await;
    let text = h.text();
    assert!(
        text.contains(
            "tasks: description Task lists -> Shared task lists; display_name Tasks -> Team tasks"
        ),
        "{text}"
    );
    select(&mut h, "tasks");
    let text = h.text();
    assert!(
        text.contains("Team tasks") && text.contains("Shared task lists"),
        "{text}"
    );
    assert!(h.text().contains("15000 ms"));
}

#[tokio::test]
async fn edit_details_is_refused_up_front_on_an_older_gateway() {
    let env = Env::with(legacy()).await;
    let mut h = ready(&env).await;
    select(&mut h, "tasks");
    h.press("e");
    assert!(gw(&h.app).overlay().is_none());
    assert!(
        h.text()
            .contains("Editing details needs update_server, which this gateway does not offer"),
        "{}",
        h.text()
    );
}

#[tokio::test]
async fn legacy_change_url_re_registers_and_restores_access() {
    let env = Env::with(legacy()).await;
    let mut h = ready(&env).await;
    select(&mut h, "notes");
    h.press("u");
    h.erase(100);
    h.type_text("https://notes2.mcp.example.org/mcp");
    h.press("enter");
    let text = h.text();
    assert!(
        text.contains("Move notes") && text.contains("to    https://notes2.mcp.example.org/mcp"),
        "{text}"
    );
    h.press("enter");
    assert_eq!(called(&env, "unregister_server"), 0);
    h.type_text("notes");
    h.press("enter");
    finish_action(&mut h).await;
    let text = h.text();
    assert!(
        text.contains("notes moved to https://notes2.mcp.example.org/mcp"),
        "{text}"
    );
    // The fake's re-registered catalogue has new tool names, so the old
    // classifications cannot all come back, and the toast says which.
    assert!(
        text.contains("could not restore notes_search=read"),
        "{text}"
    );
    select(&mut h, "notes");
    assert!(h.text().contains("https://notes2.mcp.example.org/mcp"));
    assert!(h.row_with("│notes").unwrap().contains("RO"), "{}", h.text());
    assert!(h.text().contains("30000 ms"));
}

#[tokio::test]
async fn legacy_a_refused_url_change_puts_the_old_registration_back() {
    let env = Env::with(legacy()).await;
    let mut h = ready(&env).await;
    select(&mut h, "tasks");
    h.press("u");
    h.erase(100);
    h.type_text("https://fail.example.org/mcp");
    h.press("enter");
    h.type_text("tasks");
    h.press("enter");
    finish_action(&mut h).await;
    let text = h.text();
    assert!(text.contains("the new URL was refused"), "{text}");
    assert!(
        text.contains("tasks is registered at its old URL again"),
        "{text}"
    );
    select(&mut h, "tasks");
    assert!(h.text().contains("https://tasks.mcp.example.org/mcp"));
    assert!(h.text().contains("15000 ms"));
}

#[tokio::test]
async fn view_only_gets_no_forms() {
    let env = Env::with(Options {
        admin_subs: vec![],
        ..Options::default()
    })
    .await;
    let mut h = ready(&env).await;
    for key in ["a", "D", "o", "s"] {
        h.press(key);
        assert!(gw(&h.app).overlay().is_none(), "{key}");
    }
    assert!(
        h.text()
            .contains("That needs gateway:admin; this sign-in is view-only")
    );
}

#[tokio::test]
async fn a_403_says_to_sign_in_with_the_admin_scope() {
    let env = Env::new().await;
    let (mut h, session) = env.harness_with_session(WIDE).await;
    load(&mut h).await;
    let client_id = session.tokens().await.unwrap().client_id;
    session
        .install(env.fake.mint("mcp-access", &client_id))
        .await
        .unwrap();
    select(&mut h, "weather");
    h.press("s");
    h.press("a");
    finish_action(&mut h).await;
    let text = h.text();
    assert!(
        text.contains("Refused: Tool 'set_server_status' requires the 'gateway:admin' scope."),
        "{text}"
    );
    assert!(
        text.contains("Sign in again with gateway:admin (L)"),
        "{text}"
    );
}

#[tokio::test]
async fn an_expired_sign_in_during_an_action_shows_the_fix() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    env.fake.revoke_all_grants();
    h.press("X");
    h.until_text("sign-in has expired").await;
    assert!(h.text().contains("mcp-studio gateway login"));
}

#[tokio::test]
async fn digits_and_q_typed_into_a_form_stay_in_the_form() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    select(&mut h, "tasks");
    h.press("t");
    h.erase(10);
    h.type_text("1q2000");
    // Neither the module keys (1-4) nor quit (q) fired.
    assert!(h.app.running());
    assert_eq!(h.app.active_id(), Some("gateway"));
    assert!(h.text().contains("1q2000"), "{}", h.text());
    h.press("esc");
    assert!(gw(&h.app).overlay().is_none());
    assert!(h.app.running(), "Esc closed the form, it did not quit");
}

#[tokio::test]
async fn the_register_form_shows_the_profiles_url_hint() {
    let env = Env::new().await;
    let mut profile = env.profile();
    profile.server_url_hint = Some("https://name.mcp.example.org/mcp".into());
    let tokens = env.login().await.unwrap();
    let session = studio_gateway::Session::new(
        studio_gateway::http_client(),
        profile,
        Box::new(studio_gateway::tokens::MemoryStore::with(tokens)),
        env.cache(),
    )
    .unwrap();
    let mut h = common::harness_for(vec![std::sync::Arc::new(session)], WIDE);
    load(&mut h).await;
    h.press("a");
    h.press("tab");
    assert!(
        h.text().contains("https://name.mcp.example.org/mcp"),
        "{}",
        h.text()
    );
}
