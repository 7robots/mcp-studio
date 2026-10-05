//! Several `[[gateway]]` entries: one session and one pane each, a picker to
//! switch, and each pane's state kept while the other is shown.

mod common;

use std::sync::Arc;

use common::{Env, gw, load};
use studio_fake::Options;
use studio_tui::gateway::GatewayModule;

const WIDE: (u16, u16) = (150, 40);

#[tokio::test]
async fn the_picker_switches_gateways_and_each_keeps_its_state() {
    let first = Env::named("main", Options::default()).await;
    let second = Env::named(
        "staging",
        Options {
            email: "other@example.org".into(),
            ..Options::default()
        },
    )
    .await;
    let sessions = vec![
        Arc::new(first.signed_in().await),
        Arc::new(second.signed_in().await),
    ];
    let mut h = common::harness_for(sessions, WIDE);
    load(&mut h).await;
    let header = h.row_with("admin@example.org").unwrap();
    assert!(
        header.contains("main") && header.contains("(1/2)"),
        "{header}"
    );
    assert!(h.lines().last().unwrap().contains("p gateway"));
    // Only the gateway on screen is asked anything.
    assert_eq!(second.called("list_servers"), 0);

    h.press("j");
    h.press("p");
    let picker = h
        .app
        .module::<GatewayModule>()
        .unwrap()
        .picker()
        .cloned()
        .unwrap();
    assert_eq!(picker.items.len(), 2);
    assert!(h.text().contains("staging"), "{}", h.text());
    h.press("j");
    h.press("enter");
    assert_eq!(h.app.module::<GatewayModule>().unwrap().active(), 1);
    load(&mut h).await;
    let header = h.row_with("other@example.org").unwrap();
    assert!(
        header.contains("staging") && header.contains("(2/2)"),
        "{header}"
    );
    assert_eq!(gw(&h.app).selected, 0);
    assert_eq!(second.called("list_servers"), 1);

    // Back to the first: its selection survived and nothing was reloaded.
    h.press("p");
    h.press("k");
    h.press("enter");
    assert_eq!(gw(&h.app).selected, 1);
    assert_eq!(first.called("list_servers"), 1);
    assert!(h.text().contains("https://tasks.mcp.example.org/mcp"));

    // Esc closes the picker without switching or quitting.
    h.press("p");
    h.press("esc");
    assert!(h.app.running());
    assert_eq!(h.app.module::<GatewayModule>().unwrap().active(), 0);
}

#[tokio::test]
async fn one_gateway_has_no_picker() {
    let env = Env::new().await;
    let mut h = env.harness(WIDE).await;
    load(&mut h).await;
    h.press("p");
    assert!(h.app.module::<GatewayModule>().unwrap().picker().is_none());
    assert!(!h.lines().last().unwrap().contains("p gateway"));
}

#[tokio::test]
async fn no_gateway_configured_says_where_to_add_one() {
    let module = GatewayModule::new(vec![], studio_tui::demo::auto_consent());
    let (app, rx) = studio_tui::studio_app("Empty", module);
    let mut h = studio_tui::Harness::new(app, rx, (100, 20));
    h.until_text("No gateway is configured").await;
    assert!(h.text().contains("[[gateway]]"));
    h.press("q");
    assert!(!h.app.running());
}
