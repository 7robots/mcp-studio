//! The demo starts its own fake, signs in through the browser flow, and shows
//! the seeded registry as an admin.

mod common;

use common::{gw, load};

#[tokio::test]
async fn the_demo_signs_in_and_shows_the_registry() {
    let dir = tempfile::tempdir().unwrap();
    let demo = studio_tui::demo::Demo::start(
        Some(dir.path().join("state.json")),
        dir.path().join("clients.json"),
    )
    .await
    .unwrap();
    let (app, rx) = studio_tui::studio_app("Demo", demo.module());
    let mut h = studio_tui::Harness::new(app, rx, (150, 40));
    load(&mut h).await;
    assert_eq!(gw(&h.app).admin(), Some(true));
    assert!(h.text().contains("Servers (4)"));
    // The fake keeps its servers in the state file.
    assert!(dir.path().join("state.json").exists());
}
