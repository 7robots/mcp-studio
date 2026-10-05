//! A sign-in from the stand-alone client carries over: the legacy pair and
//! its DCR registration are copied once, and the session works with them,
//! refreshes included. Every store here is in memory or a temp file; the real
//! Keychain is never touched.

mod common;

use std::sync::Arc;

use common::Env;
use studio_gateway::oauth::ClientCache;
use studio_gateway::tokens::{MemoryStore, MigratingStore, TokenStore, Tokens};
use studio_gateway::{Session, http_client};

/// Shares one in-memory store between the migrating store and the test.
#[derive(Clone, Default)]
struct Shared(Arc<MemoryStore>);

impl TokenStore for Shared {
    fn load(&self) -> anyhow::Result<Option<Tokens>> {
        self.0.load()
    }
    fn save(&self, t: &Tokens) -> anyhow::Result<()> {
        self.0.save(t)
    }
    fn clear(&self) -> anyhow::Result<()> {
        self.0.clear()
    }
    fn describe(&self) -> String {
        "shared memory".into()
    }
}

#[tokio::test]
async fn a_legacy_sign_in_and_registration_carry_over() {
    let env = Env::new().await;
    // The old client signed in: its pair and its client registration.
    let legacy_cache = env.dir.path().join("legacy/clients.json");
    let old = ClientCache::new(legacy_cache.clone());
    let tokens = studio_gateway::oauth::login(
        &http_client(),
        &env.fake.base,
        &studio_gateway::oauth::ClientSpec::for_profile(&env.profile()),
        &old,
        std::time::Duration::from_secs(2),
        common::browser,
    )
    .await
    .unwrap();
    // Stored before the issuing gateway was recorded, as old pairs were.
    let legacy = Shared::default();
    legacy
        .save(&Tokens {
            gateway: None,
            expires_at: 0,
            ..tokens.clone()
        })
        .unwrap();

    let primary = Shared::default();
    let store = MigratingStore {
        primary: Box::new(primary.clone()),
        legacy: Box::new(legacy.clone()),
        marker: env.dir.path().join("cache/migrated-main"),
    };
    let cache = ClientCache::with_legacy(env.dir.path().join("cache/clients.json"), legacy_cache);
    let session = Session::new(http_client(), env.profile(), Box::new(store), cache).unwrap();

    // The expired access token is refreshed with the migrated refresh token
    // and the same client_id, and the rotation lands in Studio's store only.
    let identity = session.identity().await.unwrap();
    assert!(identity.admin);
    let mine = primary.load().unwrap().unwrap();
    assert!(mine.is_fresh());
    assert_eq!(mine.client_id, tokens.client_id);
    assert_eq!(
        legacy.load().unwrap().unwrap().expires_at,
        0,
        "the old item is untouched"
    );
    // The registration came along, so the next sign-in reuses it.
    assert_eq!(
        session.cache().get(&env.fake.base).unwrap().client_id,
        tokens.client_id
    );
}
