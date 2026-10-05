//! What the Marketplaces module shows, loaded off the UI thread: every
//! configured marketplace with its entries, validation and reconciliation,
//! and the fleet view (which marketplaces list each fleet server).
//!
//! Everything here is blocking file and `git` work; the module runs it in
//! `spawn_blocking`.

use std::path::{Path, PathBuf};

use studio_core::Instance;
use studio_core::check::{Check, Status, rollup};
use studio_core::config::{Target, repo_name};
use studio_marketplace::{
    Configured, Location, Marketplace, PluginDir, ValidationReport, git, tree,
};

/// One configured marketplace as loaded.
#[derive(Debug, Clone)]
pub struct Market {
    pub configured: Configured,
    /// The entries in the clone, sorted by slug (empty when not cloned).
    pub entries: Vec<EntryView>,
    /// `None` when not cloned.
    pub validation: Option<ValidationReport>,
    /// Reconciliation checks (one skip when not cloned).
    pub checks: Vec<Check>,
    pub clone: CloneState,
    /// `server.yaml`s that do not load.
    pub broken: Vec<(String, String)>,
}

/// One entry, with everything the detail pane shows.
#[derive(Debug, Clone)]
pub struct EntryView {
    pub plugin: PluginDir,
    /// Files under the entry's directory, repo-relative.
    pub files: Vec<String>,
    /// Reconcile checks about this entry (`marketplace.<slug>.*`).
    pub checks: Vec<Check>,
}

impl EntryView {
    pub fn slug(&self) -> &str {
        &self.plugin.slug
    }

    /// Worst reconcile status of the entry's checks.
    pub fn status(&self) -> Status {
        rollup(&self.checks)
    }
}

/// The local clone's git state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloneState {
    pub cloned: bool,
    pub dirty: bool,
    pub branch: Option<String>,
    /// Commits ahead of `origin/<branch>`; `None` without a tracking ref.
    pub unpushed: Option<usize>,
}

impl Market {
    pub fn id(&self) -> &str {
        &self.configured.market.id
    }

    pub fn market(&self) -> &Marketplace {
        &self.configured.market
    }

    pub fn dir(&self) -> &Path {
        self.configured.location.path()
    }

    pub fn targets(&self) -> String {
        self.market()
            .targets
            .iter()
            .map(|t| match t {
                Target::Claude => "claude",
                Target::Codex => "codex",
            })
            .collect::<Vec<_>>()
            .join("+")
    }

    /// Worst reconcile status over the whole marketplace.
    pub fn reconcile_status(&self) -> Status {
        rollup(&self.checks)
    }

    /// One word for the list: valid / N problems / not cloned.
    pub fn validation_label(&self) -> (String, Status) {
        match &self.validation {
            None => ("not cloned".into(), Status::Skip),
            Some(v) if v.is_ok() => match self.reconcile_status() {
                Status::Fail => ("drift".into(), Status::Fail),
                Status::Warn => ("valid, warnings".into(), Status::Warn),
                _ => ("valid".into(), Status::Pass),
            },
            Some(v) => (
                format!("{} problem(s)", v.data_errors().count()),
                Status::Fail,
            ),
        }
    }
}

/// Loads every configured marketplace.
pub fn load_markets(instance: &Instance) -> Vec<Market> {
    studio_marketplace::configured(instance)
        .into_iter()
        .map(load_market)
        .collect()
}

pub fn load_market(configured: Configured) -> Market {
    match configured.location.clone() {
        Location::NotCloned(_) => {
            let checks = configured.checks();
            Market {
                configured,
                entries: Vec::new(),
                validation: None,
                checks,
                clone: CloneState::default(),
                broken: Vec::new(),
            }
        }
        Location::Cloned(dir) => {
            let m = &configured.market;
            let scan = tree::scan(&dir);
            let checks = studio_marketplace::reconcile(&dir, m);
            let validation = studio_marketplace::validate_dir(&dir, m);
            let entries = scan
                .plugins
                .iter()
                .map(|p| {
                    let prefix = format!("marketplace.{}.", p.slug);
                    EntryView {
                        plugin: p.clone(),
                        files: tree::files_under(&dir, &p.rel_dir),
                        checks: checks
                            .iter()
                            .filter(|c| c.id.starts_with(&prefix))
                            .cloned()
                            .collect(),
                    }
                })
                .collect();
            let clone = clone_state(&dir, &m.branch);
            Market {
                configured,
                entries,
                validation: Some(validation),
                checks,
                clone,
                broken: scan.broken,
            }
        }
    }
}

pub fn clone_state(dir: &Path, branch: &str) -> CloneState {
    let tracking = format!("refs/remotes/origin/{branch}");
    let unpushed = git::run(dir, &["rev-parse", "--verify", "-q", &tracking])
        .ok()
        .and_then(|_| {
            git::run(
                dir,
                &["rev-list", "--count", &format!("origin/{branch}..HEAD")],
            )
            .ok()
        })
        .and_then(|n| n.trim().parse().ok());
    CloneState {
        cloned: true,
        dirty: git::is_dirty(dir).unwrap_or(false),
        branch: git::current_branch(dir).ok(),
        unpushed,
    }
}

/// One fleet server in the fleet view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetRow {
    pub repo: String,
    /// `include`, `server`.
    pub origins: Vec<String>,
    /// The slug it is listed under: `marketplace_slug`, else the repo name.
    pub slug: String,
    /// Where the slug came from.
    pub slug_from: &'static str,
    pub local_dir: PathBuf,
    /// `version` in the repo's `package.json`, when the repo is cloned.
    pub repo_version: Option<String>,
    /// Per marketplace (in config order).
    pub listings: Vec<Listing>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub marketplace: String,
    pub state: ListingState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListingState {
    /// The marketplace is not cloned: unknown.
    Unknown,
    NotListed,
    Listed {
        version: String,
        deprecated: bool,
        /// `Some(true)` when it equals package.json's version.
        agrees: Option<bool>,
    },
}

/// Fleet membership from config (include + `[[fleet.server]]`), against the
/// marketplaces already loaded.
pub fn fleet_rows(instance: &Instance, markets: &[Market]) -> Vec<FleetRow> {
    studio_fleet::discover::configured_members(instance)
        .into_iter()
        .map(|(repo, origins)| {
            let configured_slug = instance
                .config
                .fleet_server(&repo)
                .and_then(|s| s.marketplace_slug.clone());
            let (slug, slug_from) = match configured_slug {
                Some(s) => (s, "marketplace_slug"),
                None => (repo_name(&repo).to_string(), "repo name"),
            };
            let local_dir = instance.repo_dir(&repo);
            let repo_version = package_version(&local_dir);
            let listings = markets
                .iter()
                .map(|m| {
                    let state = if !m.clone.cloned {
                        ListingState::Unknown
                    } else {
                        let name = repo_name(&repo);
                        match m
                            .entries
                            .iter()
                            .find(|e| e.slug() == slug)
                            .or_else(|| m.entries.iter().find(|e| e.slug() == name))
                        {
                            None => ListingState::NotListed,
                            Some(e) => {
                                let version = e.plugin.entry.version().to_string();
                                ListingState::Listed {
                                    agrees: repo_version.as_ref().map(|v| *v == version),
                                    deprecated: e.plugin.entry.is_deprecated(),
                                    version,
                                }
                            }
                        }
                    };
                    Listing {
                        marketplace: m.id().to_string(),
                        state,
                    }
                })
                .collect();
            FleetRow {
                repo,
                origins,
                slug,
                slug_from,
                local_dir,
                repo_version,
                listings,
            }
        })
        .collect()
}

fn package_version(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("version")?.as_str().map(str::to_string)
}
