#![allow(dead_code)]
//! Fixtures: a fake gateway, a temp directory for the token file and the
//! client registration cache, and the Studio shell around the Gateway module.

use std::sync::Arc;
use std::time::Duration;

use studio_fake::{FakeGateway, Options};
use studio_gateway::oauth::{self, ClientCache, ClientSpec};
use studio_gateway::tokens::{FileStore, TokenStore, Tokens};
use studio_gateway::{Profile, Session, http_client};
use studio_tui::gateway::{GatewayModule, Opener, Pane};
use studio_tui::{App, Harness};

pub const TITLE: &str = "Acme MCP fleet";

pub struct Env {
    pub fake: FakeGateway,
    pub dir: tempfile::TempDir,
    pub id: &'static str,
}

impl Env {
    pub async fn new() -> Env {
        Env::with(Options::default()).await
    }

    pub async fn with(opts: Options) -> Env {
        Env::named("main", opts).await
    }

    pub async fn named(id: &'static str, opts: Options) -> Env {
        Env {
            fake: FakeGateway::start(opts).await.unwrap(),
            dir: tempfile::tempdir().unwrap(),
            id,
        }
    }

    /// Polling off, so tests decide when to reload.
    pub fn profile(&self) -> Profile {
        let mut p = Profile::with_defaults(self.id, self.fake.base.as_str()).unwrap();
        p.refresh_seconds = 0;
        p
    }

    pub fn token_path(&self) -> std::path::PathBuf {
        self.dir.path().join("tokens.json")
    }

    pub fn store(&self) -> FileStore {
        FileStore {
            path: self.token_path(),
        }
    }

    pub fn cache(&self) -> ClientCache {
        ClientCache::new(self.dir.path().join("clients.json"))
    }

    pub async fn login(&self) -> anyhow::Result<Tokens> {
        let open = studio_tui::demo::auto_consent();
        oauth::login(
            &http_client(),
            &self.fake.base,
            &ClientSpec::for_profile(&self.profile()),
            &self.cache(),
            Duration::from_secs(2),
            move |url| open(url),
        )
        .await
    }

    pub fn session(&self) -> Session {
        Session::new(
            http_client(),
            self.profile(),
            Box::new(self.store()),
            self.cache(),
        )
        .unwrap()
    }

    pub async fn signed_in(&self) -> Session {
        let tokens = self.login().await.unwrap();
        self.store().save(&tokens).unwrap();
        self.session()
    }

    /// The TUI, signed in.
    pub async fn harness(&self, size: (u16, u16)) -> Harness {
        harness_for(vec![Arc::new(self.signed_in().await)], size)
    }

    /// The TUI and the session under it, for tests that swap its tokens.
    pub async fn harness_with_session(&self, size: (u16, u16)) -> (Harness, Arc<Session>) {
        let session = Arc::new(self.signed_in().await);
        (harness_for(vec![session.clone()], size), session)
    }

    /// The TUI with no stored sign-in.
    pub fn harness_signed_out(&self) -> Harness {
        harness_for(vec![Arc::new(self.session())], (120, 40))
    }

    pub fn calls(&self, entry: &str) -> usize {
        self.fake.log().iter().filter(|l| *l == entry).count()
    }

    pub fn called(&self, tool: &str) -> usize {
        self.calls(&format!("mcp:tools/call:{tool}"))
    }
}

pub fn harness_for(sessions: Vec<Arc<Session>>, size: (u16, u16)) -> Harness {
    harness_with(sessions, studio_tui::demo::auto_consent(), size)
}

pub fn harness_with(sessions: Vec<Arc<Session>>, opener: Opener, size: (u16, u16)) -> Harness {
    let module = GatewayModule::new(sessions, opener).with_login_timeout(Duration::from_secs(3));
    let (app, rx) = studio_tui::studio_app(TITLE, module);
    Harness::new(app, rx, size)
}

/// The gateway pane on screen.
pub fn gw(app: &App) -> &Pane {
    app.module::<GatewayModule>()
        .expect("a gateway module")
        .pane()
        .expect("a gateway pane")
}

/// Waits for the identity and the server list.
pub async fn load(h: &mut Harness) {
    h.until(|app| gw(app).identity.data.is_some() && gw(app).servers.data.is_some())
        .await;
}

/// Waits for the running admin action to finish and the list to reload.
pub async fn finish_action(h: &mut Harness) {
    h.until(|app| gw(app).idle()).await;
}

/// Selects the table row whose id is `id`.
pub fn select(h: &mut Harness, id: &str) {
    h.press("g");
    for _ in 0..10 {
        if gw(&h.app).selected_server().is_some_and(|s| s.id == id) {
            return;
        }
        h.press("j");
    }
    panic!("no server {id}");
}
