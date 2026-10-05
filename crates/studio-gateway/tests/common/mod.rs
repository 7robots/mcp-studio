#![allow(dead_code)]
//! Fixtures: a fake gateway plus a temp directory for the token file and the
//! client registration cache.

use std::time::Duration;

use studio_fake::{FakeGateway, Options};
use studio_gateway::oauth::{self, ClientCache, ClientSpec};
use studio_gateway::tokens::{FileStore, TokenStore, Tokens};
use studio_gateway::{Profile, Session, Url, http_client};

pub struct Env {
    pub fake: FakeGateway,
    pub dir: tempfile::TempDir,
}

/// Stands in for the browser: a redirect-following GET. The fake consents.
pub fn browser(url: &Url) -> anyhow::Result<()> {
    let url = url.clone();
    tokio::spawn(async move {
        let _ = reqwest_get(url).await;
    });
    Ok(())
}

async fn reqwest_get(url: Url) -> Result<(), studio_gateway::reqwest::Error> {
    studio_gateway::reqwest::get(url).await.map(|_| ())
}

impl Env {
    pub async fn new() -> Env {
        Env::with(Options::default()).await
    }

    pub async fn with(opts: Options) -> Env {
        Env {
            fake: FakeGateway::start(opts).await.unwrap(),
            dir: tempfile::tempdir().unwrap(),
        }
    }

    pub fn profile(&self) -> Profile {
        Profile::with_defaults("main", self.fake.base.as_str()).unwrap()
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
        oauth::login(
            &http_client(),
            &self.fake.base,
            &ClientSpec::for_profile(&self.profile()),
            &self.cache(),
            Duration::from_secs(2),
            browser,
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

    /// A session already signed in through the login flow.
    pub async fn signed_in(&self) -> Session {
        let tokens = self.login().await.unwrap();
        self.store().save(&tokens).unwrap();
        self.session()
    }
}
