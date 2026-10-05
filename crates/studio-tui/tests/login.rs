//! Signing in from inside the TUI: `L` runs the browser flow in a task, shows
//! where the browser was sent, and fixes a missing or expired sign-in in place.

mod common;

use std::sync::Arc;

use common::{Env, gw};
use studio_fake::Options;
use studio_gateway::tokens::{TokenStore, Tokens};

const WIDE: (u16, u16) = (150, 40);

#[tokio::test]
async fn l_signs_in_from_the_not_signed_in_screen() {
    let env = Env::new().await;
    let mut h = env.harness_signed_out();
    h.until_text("Press L to sign in").await;
    h.press("L");
    h.until(|app| gw(app).identity.data.is_some()).await;
    h.until_text("Servers (4)").await;
    let text = h.text();
    assert!(
        text.contains("admin@example.org") && text.contains(" admin "),
        "{text}"
    );
    assert!(env.store().load().unwrap().is_some(), "the pair was stored");
    assert!(gw(&h.app).signed_out.is_none() && gw(&h.app).login.is_none());
}

#[tokio::test]
async fn l_recovers_an_expired_sign_in() {
    let env = Env::new().await;
    let tokens = env.login().await.unwrap();
    env.store()
        .save(&Tokens {
            expires_at: 0,
            ..tokens
        })
        .unwrap();
    env.fake.revoke_all_grants();
    let mut h = common::harness_for(vec![Arc::new(env.session())], WIDE);
    h.until_text("sign-in has expired").await;
    h.press("L");
    h.until_text("Servers (4)").await;
    assert!(h.text().contains("Signed in to 127.0.0.1"), "{}", h.text());
}

#[tokio::test]
async fn the_sign_in_page_is_shown_and_esc_cancels_without_quitting() {
    let env = Env::new().await;
    // A browser that never comes back.
    let opener: studio_tui::gateway::Opener = Arc::new(|_| Ok(()));
    let mut h = common::harness_with(vec![Arc::new(env.session())], opener, WIDE);
    h.until_text("Press L").await;
    h.press("L");
    h.until_text("If no browser opened, visit:").await;
    assert!(h.text().contains("/authorize?"), "{}", h.text());
    // The toast holds the status line now; the footer it replaced offers Esc.
    let footer = studio_tui::framework::keymap::footer_text(&h.app.hints());
    assert!(footer.contains("Esc cancel sign-in"), "{footer}");
    h.press("esc");
    assert!(
        h.app.running(),
        "Esc cancelled the sign-in, it did not quit"
    );
    h.until_text("Press L").await;
    assert!(gw(&h.app).login.is_none());
    assert!(h.text().contains("Sign-in cancelled"));
}

#[tokio::test]
async fn a_refused_sign_in_says_so_and_stays_signed_out() {
    let env = Env::with(Options {
        idp_grants_admin: false,
        ..Options::default()
    })
    .await;
    let mut h = env.harness_signed_out();
    h.until_text("Press L").await;
    h.press("L");
    h.until_text("Sign-in failed").await;
    assert!(h.text().contains("browser page"), "{}", h.text());
    assert!(gw(&h.app).signed_out.is_some());
    assert!(env.store().load().unwrap().is_none());
}

#[tokio::test]
async fn a_second_l_while_waiting_is_refused() {
    let env = Env::new().await;
    let opener: studio_tui::gateway::Opener = Arc::new(|_| Ok(()));
    let mut h = common::harness_with(vec![Arc::new(env.session())], opener, WIDE);
    h.until_text("Press L").await;
    h.press("L");
    h.press("L");
    assert!(h.text().contains("Already waiting for the browser sign-in"));
}
