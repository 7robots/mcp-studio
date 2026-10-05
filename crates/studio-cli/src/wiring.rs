//! Adapters that connect the engine crates to each other. The engines stay
//! independent (fleet doesn't depend on pattern or gateway); the CLI wires
//! them here.

use std::path::{Path, PathBuf};

use studio_core::Instance;
use studio_core::check::Check;
use studio_fleet::RepoChecker;
use studio_pattern::{Pack, Values};

/// Pattern conformance + lint for the fleet's `pattern` source.
pub struct PatternChecker {
    pack: Pack,
    values: Values,
    store: PathBuf,
}

impl PatternChecker {
    /// `None` when the instance has no `[pattern]` (the source then skips).
    pub fn from_instance(inst: &Instance) -> anyhow::Result<Option<Self>> {
        let (Some(dir), Some(store)) = (inst.pattern_dir(), inst.conformance_dir()) else {
            return Ok(None);
        };
        Ok(Some(Self {
            pack: Pack::load(&dir)?,
            values: inst.pattern_values()?,
            store,
        }))
    }
}

impl RepoChecker for PatternChecker {
    fn check(&self, repo_dir: &Path) -> Vec<Check> {
        match studio_pattern::check_repo(&self.pack, &self.values, repo_dir, &self.store) {
            Ok(checks) => checks,
            Err(e) => vec![Check::fail("pattern", format!("pattern pack: {e}"))],
        }
    }
}

/// The gateway registry for the fleet's `gateway` source, through the signed-in
/// sessions. A gateway with no stored sign-in reports an error (the source
/// then skips) rather than starting a browser login.
pub struct GatewayRegistry {
    sessions: Vec<(String, std::sync::Arc<studio_gateway::client::Session>)>,
}

impl GatewayRegistry {
    pub fn new(sessions: Vec<(String, std::sync::Arc<studio_gateway::client::Session>)>) -> Self {
        Self { sessions }
    }
}

impl studio_fleet::GatewaySource for GatewayRegistry {
    fn list_servers<'a>(
        &'a self,
        gateway: &'a studio_core::config::GatewayConfig,
    ) -> futures::future::BoxFuture<'a, Result<Vec<studio_fleet::GatewayServerInfo>, String>> {
        Box::pin(async move {
            let Some((_, session)) = self.sessions.iter().find(|(id, _)| *id == gateway.id) else {
                return Err(format!("no session for gateway {}", gateway.id));
            };
            if session.tokens().await.is_none() {
                return Err(format!(
                    "not signed in to gateway {}: run `mcp-studio gateway login`",
                    gateway.id
                ));
            }
            let servers = session
                .list_servers(true, false)
                .await
                .map_err(|e| e.to_string())?;
            Ok(servers
                .servers
                .into_iter()
                .map(|s| studio_fleet::GatewayServerInfo {
                    id: s.id,
                    url: Some(s.url),
                    status: Some(s.status),
                    health: Some(s.health),
                    access: s.access,
                    auth_mode: s.auth_mode,
                    last_refresh_at: s.last_refresh_at.map(|t| t.to_string()),
                    last_error: s.last_error,
                })
                .collect())
        })
    }
}
