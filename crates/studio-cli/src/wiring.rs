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
