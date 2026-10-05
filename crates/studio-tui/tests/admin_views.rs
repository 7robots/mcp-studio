//! The newer gateway tools through the TUI against the fake: server versions
//! and registration in the detail pane, the build in the header, the Scopes
//! and Connections screens, revoking a user, and the call_tool console; and
//! the same screens against a fake without those tools (an older gateway).

mod common;

use common::{Env, finish_action, gw, load, select};
use studio_fake::Options;
use studio_tui::Harness;
use studio_tui::framework::overlay::Overlay;
use studio_tui::gateway::Screen;

const WIDE: (u16, u16) = (150, 45);

async fn ready(env: &Env) -> Harness {
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    h
}

fn legacy() -> Options {
    Options {
        legacy: true,
        ..Options::default()
    }
}

async fn to(h: &mut Harness, screen: Screen) {
    for _ in 0..Screen::ALL.len() {
        if gw(&h.app).screen == screen {
            break;
        }
        h.press("tab");
    }
    assert_eq!(gw(&h.app).screen, screen);
    h.settle().await;
}

#[tokio::test]
async fn detail_shows_version_and_registration_and_the_header_the_build() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    h.until_text("build 1.8.0 (3f2a9c1)").await;
    select(&mut h, "notes");
    let text = h.text();
    assert!(text.contains("version      0.3.0"), "{text}");
    assert!(
        text.contains("registered   30d ago by admin@example.org"),
        "{text}"
    );
}

#[tokio::test]
async fn an_older_gateway_shows_no_version_and_no_build() {
    let env = Env::with(legacy()).await;
    let mut h = ready(&env).await;
    h.settle().await;
    select(&mut h, "notes");
    let text = h.text();
    assert!(!text.contains("build "), "{text}");
    assert!(!text.contains("registered "), "{text}");
    assert!(!text.contains("version "), "{text}");
}

#[tokio::test]
async fn scopes_screen_lists_owners() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    to(&mut h, Screen::Scopes).await;
    h.until_text("Scope owners (4)").await;
    let text = h.text();
    assert!(text.contains("4 scopes claimed by 3 servers"), "{text}");
    assert!(h.row_with("notes:write").unwrap().contains("notes"));
    assert!(h.row_with("tasks:all").unwrap().contains("tasks"));
    h.press("G");
    assert_eq!(gw(&h.app).scope_selected, 3);
}

#[tokio::test]
async fn connections_screen_filters_by_kind_and_shift_tab_reaches_it() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    h.press("shift+tab");
    assert_eq!(gw(&h.app).screen, Screen::Connections);
    h.until_text("Connections (3)").await;
    let row = h.row_with("00ufakereader").unwrap();
    assert!(
        row.contains("okta_user") && row.contains("k-2026-09"),
        "{row}"
    );
    assert!(row.contains("in 5d"), "{row}");
    h.press("f");
    h.until_text("kind: api_key").await;
    assert!(h.text().contains("1 of 3 connections"));
    assert!(h.row_with("00ufakereader").is_none());
    h.press("f");
    h.press("f");
    assert!(h.text().contains("kind: all"), "{}", h.text());
}

#[tokio::test]
async fn revoke_a_user_from_the_connections_screen() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    to(&mut h, Screen::Connections).await;
    h.until_text("Connections (3)").await;
    h.press("j");
    h.press("R");
    let text = h.text();
    assert!(
        text.contains("Revoke a user's access") && text.contains("00ufakereader"),
        "{text}"
    );
    h.press("enter");
    assert!(h.text().contains("Revoke 00ufakereader"), "{}", h.text());
    h.press("enter");
    h.type_text("00ufakereade");
    h.press("enter");
    assert!(gw(&h.app).overlay().is_some());
    assert_eq!(env.called("revoke_user_access"), 0);
    h.type_text("r");
    h.press("enter");
    finish_action(&mut h).await;
    h.until_text("Connections (2)").await;
    assert!(
        h.text().contains(
            "Revoked 00ufakereader: 2 grants revoked, 1 clients, 1 login tokens removed, user connection removed"
        ),
        "{}",
        h.text()
    );
    assert!(
        gw(&h.app)
            .shown_connections()
            .iter()
            .all(|c| c.subject != "00ufakereader")
    );
}

#[tokio::test]
async fn older_gateway_screens_say_not_supported_and_revoke_still_works() {
    let env = Env::with(legacy()).await;
    let mut h = ready(&env).await;
    to(&mut h, Screen::Scopes).await;
    h.until_text("Scopes is not supported by this gateway.")
        .await;
    to(&mut h, Screen::Connections).await;
    h.until_text("Connections is not supported by this gateway.")
        .await;
    assert!(h.text().contains("list_connections"));
    assert_eq!(env.called("list_scope_owners"), 0);
    assert_eq!(env.called("list_connections"), 0);
    h.press("R");
    h.type_text("00ufakereader");
    h.press("enter");
    h.type_text("00ufakereader");
    h.press("enter");
    finish_action(&mut h).await;
    assert!(
        h.text().contains("Revoked 00ufakereader: 2 grants revoked"),
        "{}",
        h.text()
    );
}

#[tokio::test]
async fn console_calls_a_read_tool_and_shows_the_result() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    h.press("C");
    assert!(
        h.text().contains("Call a tool: which server"),
        "{}",
        h.text()
    );
    h.press("enter");
    let text = h.text();
    assert!(text.contains("Call a tool on notes"), "{text}");
    assert!(
        text.contains("notes_search") && text.contains("notes_read"),
        "{text}"
    );
    assert!(text.contains("3 more not classified read"), "{text}");
    assert!(!text.contains("notes_trash"), "{text}");
    h.press("enter");
    assert!(h.text().contains("Call notes.notes_search (read)"));
    h.erase(2);
    h.type_text("{\"q\": ");
    h.press("enter");
    assert!(h.text().contains("not JSON"), "{}", h.text());
    h.erase(10);
    h.type_text("[1]");
    h.press("enter");
    assert!(h.text().contains("args must be a JSON object"));
    h.erase(10);
    h.type_text("{\"q\": \"milk\"}");
    h.press("enter");
    h.until(|app| gw(app).viewer.is_some()).await;
    let text = h.text();
    assert!(text.contains("notes.notes_search"), "{text}");
    assert!(text.contains("\"q\": \"milk\""), "{text}");
    assert!(text.contains("returned (j/k scroll, Esc closes)"), "{text}");
    let before = gw(&h.app).viewer.as_ref().unwrap().scroll;
    h.press("j");
    assert_eq!(gw(&h.app).viewer.as_ref().unwrap().scroll, before + 1);
    h.press("q");
    assert!(gw(&h.app).viewer.is_none());
    assert!(h.app.running(), "q closed the result, it did not quit");
    assert_eq!(env.called("call_tool"), 1);
}

#[tokio::test]
async fn console_asks_before_a_tool_not_classified_read() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    select(&mut h, "tasks");
    h.press("C");
    h.press("enter");
    // tasks has no read tool: only the entry that lists the rest.
    let text = h.text();
    assert!(text.contains("2 more not classified read"), "{text}");
    h.press("enter");
    assert!(h.text().contains("tasks_add"), "{}", h.text());
    h.press("j");
    h.press("enter");
    h.press("enter");
    let text = h.text();
    assert!(
        text.contains("tasks.tasks_add is classified unknown"),
        "{text}"
    );
    h.press("n");
    assert!(gw(&h.app).overlay().is_none());
    assert_eq!(env.called("call_tool"), 0);

    h.press("C");
    h.press("enter");
    h.press("enter");
    h.press("j");
    h.press("enter");
    h.press("enter");
    h.press("y");
    h.until(|app| gw(app).viewer.is_some()).await;
    assert!(h.text().contains("\"tool\": \"tasks_add\""), "{}", h.text());
    assert_eq!(env.called("call_tool"), 1);
}

#[tokio::test]
async fn a_destructive_tool_needs_its_name_typed_and_a_refusal_shows() {
    let env = Env::new().await;
    let mut h = ready(&env).await;
    h.press("C");
    h.press("enter");
    h.press("j");
    h.press("j");
    h.press("enter");
    // The full list: notes_trash is the destructive one.
    let tools: Vec<String> = match gw(&h.app).overlay() {
        Some(Overlay::Picker(p)) => p.items.iter().map(|i| i.label.clone()).collect(),
        other => panic!("{other:?}"),
    };
    let at = tools.iter().position(|t| t == "notes_trash").unwrap();
    for _ in 0..at {
        h.press("j");
    }
    h.press("enter");
    h.press("enter");
    assert!(h.text().contains("Type the tool name"), "{}", h.text());
    h.press("y");
    assert!(gw(&h.app).overlay().is_some(), "y alone does not confirm");
    h.erase(1);
    h.type_text("notes_trash");
    h.press("enter");
    h.until(|app| gw(app).viewer.is_some()).await;
    let text = h.text();
    assert!(text.contains("notes.notes_trash: failed"), "{text}");
    assert!(text.contains("policy denied notes.notes_trash"), "{text}");
}
