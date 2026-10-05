//! The demo: the Gateway module against an in-process fake gateway, signed in
//! as a fictional admin through the same browser flow, with an HTTP GET
//! standing in for the browser. Needs no instance.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use studio_fake::{FakeGateway, Options};
use studio_gateway::oauth::ClientCache;
use studio_gateway::tokens::MemoryStore;
use studio_gateway::{Profile, Session, http_client};

use crate::gateway::{GatewayModule, Opener};

/// "Opens" a sign-in page by fetching it and following its redirects: the
/// fake consents at once, so this completes the flow.
pub fn auto_consent() -> Opener {
    Arc::new(|url| {
        let url = url.clone();
        tokio::spawn(async move {
            let _ = studio_gateway::reqwest::get(url).await;
        });
        Ok(())
    })
}

/// A running fake and a session signed in to it. Drop it to stop the fake.
pub struct Demo {
    pub fake: FakeGateway,
    pub session: Arc<Session>,
}

impl Demo {
    /// Starts the fake (its servers kept in `state`, when given, so the demo
    /// keeps its changes between runs) and signs in. `clients` caches the
    /// client registration.
    pub async fn start(state: Option<PathBuf>, clients: PathBuf) -> Result<Demo> {
        let fake = FakeGateway::start(Options {
            state_path: state,
            ..Options::default()
        })
        .await?;
        let profile = Profile::with_defaults("demo", fake.base.as_str())?;
        let session = Session::new(
            http_client(),
            profile,
            Box::new(MemoryStore::default()),
            ClientCache::new(clients),
        )?;
        let open = auto_consent();
        session
            .login(Duration::from_secs(10), move |url| open(url))
            .await?;
        Ok(Demo {
            fake,
            session: Arc::new(session),
        })
    }

    /// The Gateway module over the demo session.
    pub fn module(&self) -> GatewayModule {
        GatewayModule::new(vec![self.session.clone()], auto_consent())
            .with_login_timeout(Duration::from_secs(10))
    }
}
