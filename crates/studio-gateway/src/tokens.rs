//! Token storage.
//!
//! A gateway's pair lives in the macOS Keychain as one JSON secret: service
//! [`KEYCHAIN_SERVICE`], account `<instance name>:<gateway id>`.
//! [`TOKEN_FILE_ENV`] swaps in a 0600 file (scripts, tests), and the demo keeps
//! its pair in memory ([`MemoryStore`]). Each pair records the gateway origin
//! that issued it, and a session for any other gateway ignores it.
//!
//! [`MigratingStore`] carries a sign-in over from the stand-alone
//! `mcpgw-manager` client once: when Studio has no pair of its own and the old
//! Keychain item exists, it is copied (never deleted). The migration sits
//! behind the [`TokenStore`] trait, so tests drive it with in-memory stores and
//! never touch the real Keychain.

use std::fmt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::profile::Profile;
use crate::util::{now_unix, write_atomic};

/// Names the file store in place of the Keychain.
pub const TOKEN_FILE_ENV: &str = "MCP_STUDIO_TOKEN_FILE";
/// Keychain service of Studio's items.
pub const KEYCHAIN_SERVICE: &str = "mcp-studio";
/// Keychain service of the stand-alone client Studio replaces; its account
/// is the gateway host (`host[:port]`).
pub const LEGACY_KEYCHAIN_SERVICE: &str = "mcpgw-manager";

/// A token is refreshed this many seconds before its expiry, so a request is
/// never sent with one that lapses in flight.
pub const EXPIRY_SKEW_SECONDS: u64 = 60;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    /// Rotated on every refresh; the gateway keeps only the previous one valid.
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: u64,
    /// Space-separated, as the token endpoint returned it.
    pub scope: String,
    /// The DCR client the grant belongs to; a refresh must present it.
    pub client_id: String,
    /// The gateway origin that issued the pair. Absent in pairs stored before it
    /// was recorded, which are accepted for the gateway they are keyed under.
    #[serde(default)]
    pub gateway: Option<String>,
}

impl Tokens {
    pub fn is_fresh(&self) -> bool {
        self.expires_at > now_unix() + EXPIRY_SKEW_SECONDS
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.scope.split_whitespace().any(|s| s == scope)
    }
}

/// Never prints the secrets.
impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field("client_id", &self.client_id)
            .field("gateway", &self.gateway)
            .finish()
    }
}

pub trait TokenStore: Send + Sync {
    fn load(&self) -> Result<Option<Tokens>>;
    fn save(&self, tokens: &Tokens) -> Result<()>;
    fn clear(&self) -> Result<()>;
    /// For messages: where the tokens are kept.
    fn describe(&self) -> String;
}

/// The Keychain account of one gateway of one instance.
pub fn keychain_account(instance: &str, gateway_id: &str) -> String {
    format!("{instance}:{gateway_id}")
}

/// The store for one gateway of an instance: the file named by
/// [`TOKEN_FILE_ENV`] when set, else the Keychain with the one-time migration
/// from the legacy item. `cache_dir` (the instance's) holds the migration marker.
pub fn default_store(
    instance: &str,
    profile: &Profile,
    cache_dir: &std::path::Path,
) -> Result<Box<dyn TokenStore>> {
    if let Some(path) = std::env::var_os(TOKEN_FILE_ENV).filter(|v| !v.is_empty()) {
        return Ok(Box::new(FileStore {
            path: PathBuf::from(path),
        }));
    }
    let primary = KeychainStore::new(KEYCHAIN_SERVICE, &keychain_account(instance, &profile.id))?;
    let legacy = KeychainStore::new(LEGACY_KEYCHAIN_SERVICE, &profile.host_key())?;
    Ok(Box::new(MigratingStore {
        primary: Box::new(primary),
        legacy: Box::new(legacy),
        marker: cache_dir.join(format!("migrated-{}", profile.id)),
    }))
}

/// One Keychain item holding the pair as JSON.
pub struct KeychainStore {
    entry: keyring::Entry,
    service: String,
    account: String,
}

impl KeychainStore {
    pub fn new(service: &str, account: &str) -> Result<KeychainStore> {
        let entry = keyring::Entry::new(service, account).context("opening the Keychain")?;
        Ok(KeychainStore {
            entry,
            service: service.to_string(),
            account: account.to_string(),
        })
    }
}

impl TokenStore for KeychainStore {
    fn load(&self) -> Result<Option<Tokens>> {
        match self.entry.get_password() {
            Ok(text) => Ok(Some(
                serde_json::from_str(&text).context("decoding the Keychain token item")?,
            )),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err).context("reading the Keychain"),
        }
    }

    fn save(&self, tokens: &Tokens) -> Result<()> {
        let text = serde_json::to_string(tokens)?;
        self.entry
            .set_password(&text)
            .context("writing the Keychain")
    }

    fn clear(&self) -> Result<()> {
        match self.entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err).context("deleting the Keychain item"),
        }
    }

    fn describe(&self) -> String {
        format!("Keychain item {} / {}", self.service, self.account)
    }
}

/// JSON file readable only by its owner.
pub struct FileStore {
    pub path: PathBuf,
}

impl TokenStore for FileStore {
    fn load(&self) -> Result<Option<Tokens>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => {
                Ok(Some(serde_json::from_str(&text).with_context(|| {
                    format!("decoding {}", self.path.display())
                })?))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("reading {}", self.path.display())),
        }
    }

    fn save(&self, tokens: &Tokens) -> Result<()> {
        write_atomic(&self.path, &serde_json::to_string_pretty(tokens)?, true)
            .with_context(|| format!("writing {}", self.path.display()))
    }

    fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).with_context(|| format!("removing {}", self.path.display())),
        }
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// Tokens held only for the life of the process (the demo, whose fake gateway
/// forgets every grant when it exits; tests).
#[derive(Default)]
pub struct MemoryStore {
    tokens: std::sync::Mutex<Option<Tokens>>,
}

impl MemoryStore {
    pub fn with(tokens: Tokens) -> MemoryStore {
        MemoryStore {
            tokens: std::sync::Mutex::new(Some(tokens)),
        }
    }
}

impl TokenStore for MemoryStore {
    fn load(&self) -> Result<Option<Tokens>> {
        Ok(self.tokens.lock().unwrap().clone())
    }

    fn save(&self, tokens: &Tokens) -> Result<()> {
        *self.tokens.lock().unwrap() = Some(tokens.clone());
        Ok(())
    }

    fn clear(&self) -> Result<()> {
        *self.tokens.lock().unwrap() = None;
        Ok(())
    }

    fn describe(&self) -> String {
        "memory".into()
    }
}

/// `primary`, seeded once from `legacy`.
///
/// On a load that finds `primary` empty, the `legacy` pair (if any) is copied
/// into `primary` and returned. `legacy` is only ever read. `marker` is a file
/// recording that the migration happened (or that Studio has stored a pair of
/// its own), so a later sign-out is not undone by migrating the old item again.
pub struct MigratingStore {
    pub primary: Box<dyn TokenStore>,
    pub legacy: Box<dyn TokenStore>,
    pub marker: PathBuf,
}

impl MigratingStore {
    fn mark(&self) -> Result<()> {
        if !self.marker.exists() {
            write_atomic(&self.marker, "", false)
                .with_context(|| format!("writing {}", self.marker.display()))?;
        }
        Ok(())
    }
}

impl TokenStore for MigratingStore {
    fn load(&self) -> Result<Option<Tokens>> {
        if let Some(tokens) = self.primary.load()? {
            return Ok(Some(tokens));
        }
        if self.marker.exists() {
            return Ok(None);
        }
        // A legacy item that cannot be read is no reason to fail a sign-in check.
        let Ok(Some(tokens)) = self.legacy.load() else {
            return Ok(None);
        };
        self.primary.save(&tokens)?;
        self.mark()?;
        Ok(Some(tokens))
    }

    fn save(&self, tokens: &Tokens) -> Result<()> {
        self.primary.save(tokens)?;
        self.mark()
    }

    fn clear(&self) -> Result<()> {
        self.primary.clear()?;
        self.mark()
    }

    fn describe(&self) -> String {
        self.primary.describe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    pub(crate) fn sample() -> Tokens {
        Tokens {
            access_token: "secret-access".into(),
            refresh_token: Some("secret-refresh".into()),
            expires_at: now_unix() + 3600,
            scope: "mcp-access gateway:admin".into(),
            client_id: "c1".into(),
            gateway: Some("https://gw.example.org/".into()),
        }
    }

    #[test]
    fn debug_redacts_secrets() {
        let text = format!("{:?}", sample());
        assert!(!text.contains("secret"), "{text}");
        assert!(text.contains("gateway:admin"));
    }

    #[test]
    fn freshness_honors_skew() {
        let mut tokens = sample();
        assert!(tokens.is_fresh());
        tokens.expires_at = now_unix() + EXPIRY_SKEW_SECONDS - 1;
        assert!(!tokens.is_fresh());
    }

    #[test]
    fn scope_match_is_whole_word() {
        let tokens = sample();
        assert!(tokens.has_scope("gateway:admin"));
        assert!(!tokens.has_scope("gateway"));
    }

    #[test]
    fn file_store_round_trips_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore {
            path: dir.path().join("t.json"),
        };
        assert!(store.load().unwrap().is_none());
        let tokens = sample();
        store.save(&tokens).unwrap();
        assert_eq!(store.load().unwrap(), Some(tokens));
        store.clear().unwrap();
        store.clear().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn keychain_accounts_name_the_instance_and_gateway() {
        assert_eq!(keychain_account("acme", "main"), "acme:main");
    }

    /// A store shared between the migrating store and the test.
    #[derive(Clone, Default)]
    struct Shared(Arc<MemoryStore>);
    impl TokenStore for Shared {
        fn load(&self) -> Result<Option<Tokens>> {
            self.0.load()
        }
        fn save(&self, t: &Tokens) -> Result<()> {
            self.0.save(t)
        }
        fn clear(&self) -> Result<()> {
            self.0.clear()
        }
        fn describe(&self) -> String {
            "shared".into()
        }
    }

    fn migrating(dir: &std::path::Path) -> (MigratingStore, Shared, Shared) {
        let (primary, legacy) = (Shared::default(), Shared::default());
        let store = MigratingStore {
            primary: Box::new(primary.clone()),
            legacy: Box::new(legacy.clone()),
            marker: dir.join("migrated-main"),
        };
        (store, primary, legacy)
    }

    #[test]
    fn a_legacy_pair_is_copied_once_and_left_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let (store, primary, legacy) = migrating(dir.path());
        legacy.save(&sample()).unwrap();
        assert_eq!(store.load().unwrap(), Some(sample()));
        assert_eq!(primary.load().unwrap(), Some(sample()));
        assert_eq!(legacy.load().unwrap(), Some(sample()), "never deleted");
        // A sign-out is not undone by migrating again.
        store.clear().unwrap();
        assert!(store.load().unwrap().is_none());
        assert!(legacy.load().unwrap().is_some());
    }

    #[test]
    fn studio_pairs_win_over_legacy_ones() {
        let dir = tempfile::tempdir().unwrap();
        let (store, primary, legacy) = migrating(dir.path());
        let mine = Tokens {
            client_id: "studio".into(),
            ..sample()
        };
        primary.save(&mine).unwrap();
        legacy.save(&sample()).unwrap();
        assert_eq!(store.load().unwrap(), Some(mine));
    }

    #[test]
    fn nothing_to_migrate_leaves_the_door_open() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _primary, legacy) = migrating(dir.path());
        assert!(store.load().unwrap().is_none());
        // The old client signs in later; Studio still picks it up.
        legacy.save(&sample()).unwrap();
        assert_eq!(store.load().unwrap(), Some(sample()));
    }

    #[test]
    fn a_studio_sign_in_closes_the_migration() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _primary, legacy) = migrating(dir.path());
        store.save(&sample()).unwrap();
        store.clear().unwrap();
        legacy.save(&sample()).unwrap();
        assert!(store.load().unwrap().is_none());
    }
}
