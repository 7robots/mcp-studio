//! Claude Code + Codex plugin marketplaces.
//!
//! A marketplace is a git repo: two catalogs, and one directory per entry
//! whose `server.yaml` is the source of truth for everything else in it.
//! This crate reads such a tree ([`tree`]), generates every derived file
//! from the `server.yaml`s ([`generate`]), validates it ([`validate`]),
//! reports drift ([`reconcile`]), changes it ([`ops`]), seeds new ones
//! ([`provision`]) and publishes changes ([`publish`]).
//!
//! Nothing here is org-specific: identity comes from the instance's
//! `[[marketplace]]` config.

use anyhow::{Result, anyhow};
use serde::Serialize;
use studio_core::Instance;
use studio_core::check::Check;

pub mod draft;
pub mod generate;
pub mod git;
pub mod model;
pub mod ojson;
pub mod ops;
pub mod provision;
pub mod publish;
pub mod reconcile;
pub mod schema;
pub mod tree;
pub mod validate;
pub mod yaml;

pub use git::{Location, Workspace};
pub use model::{Entry, Marketplace, PluginDir};
pub use ops::{ChangeSet, EntryPatch, EntrySummary, Verb};
pub use reconcile::{VerifyReport, reconcile, verify};
pub use validate::{ValidationReport, validate_dir};

/// The Claude Code catalog.
pub const CLAUDE_CATALOG: &str = ".claude-plugin/marketplace.json";
/// The Codex catalog.
pub const CODEX_CATALOG: &str = ".agents/plugins/marketplace.json";
/// Where a marketplace repo keeps its copy of the schemas.
pub const SCHEMA_DIR: &str = "schema";

/// A configured marketplace and where its clone is.
#[derive(Debug, Clone)]
pub struct Configured {
    pub market: Marketplace,
    pub location: Location,
}

impl Configured {
    /// Checks for the marketplace: reconciliation when cloned, else one skip.
    pub fn checks(&self) -> Vec<Check> {
        match &self.location {
            Location::Cloned(dir) => reconcile(dir, &self.market),
            Location::NotCloned(dir) => vec![not_cloned(&self.market, dir)],
        }
    }
}

pub fn not_cloned(m: &Marketplace, dir: &std::path::Path) -> Check {
    Check::skip(
        format!("marketplace.{}", m.id),
        format!("{} not cloned at {}", m.repo, dir.display()),
    )
}

/// Every `[[marketplace]]` in the instance.
pub fn configured(instance: &Instance) -> Vec<Configured> {
    let account = instance.config.github.account.as_deref();
    instance
        .config
        .marketplaces
        .iter()
        .map(|c| Configured {
            market: Marketplace::from_config(c, account),
            location: Location::of(&instance.repo_dir(&c.repo)),
        })
        .collect()
}

/// One `[[marketplace]]` by id.
pub fn resolve(instance: &Instance, id: &str) -> Result<Configured> {
    configured(instance)
        .into_iter()
        .find(|c| c.market.id == id)
        .ok_or_else(|| {
            let ids: Vec<String> = instance
                .config
                .marketplaces
                .iter()
                .map(|m| m.id.clone())
                .collect();
            anyhow!(
                "no [[marketplace]] with id {id:?} (have: {})",
                ids.join(", ")
            )
        })
}

/// A marketplace at a glance, for lists and the TUI.
#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    pub id: String,
    pub repo: String,
    pub catalog_name: String,
    pub dir: String,
    pub cloned: bool,
    pub entries: Vec<EntrySummary>,
}

pub fn overview(c: &Configured) -> Overview {
    let (cloned, entries) = match &c.location {
        Location::Cloned(d) => (true, ops::list(d)),
        Location::NotCloned(_) => (false, Vec::new()),
    };
    Overview {
        id: c.market.id.clone(),
        repo: c.market.repo.clone(),
        catalog_name: c.market.catalog_name.clone(),
        dir: c.location.path().display().to_string(),
        cloned,
        entries,
    }
}
