//! Pattern packs: the versioned recipe for building an MCP server.
//!
//! - [`Pack`] loads a pack (`pattern.toml` + `template/` + `skill/`) and
//!   renders its template or skill with an instance's values.
//! - [`Conformance`] compares fleet repos' security files with the rendered
//!   template against the instance's store of blessed diffs, and blesses.
//! - [`lint_repo`] applies the manifest's static rules to a repo checkout.

pub mod conformance;
pub mod difflib;
pub mod lint;
pub mod manifest;
pub mod pack;

use std::path::Path;

use studio_core::check::Check;

pub use conformance::{BlessOptions, BlessOutcome, Conformance, ConformanceError, FileStatus};
pub use lint::lint_repo;
pub use manifest::Manifest;
pub use pack::{FileTree, Pack, PackError, RenderedFile, Values, install_skill};

/// Conformance (`pattern.version`, `pattern.hashes`, `pattern.drift`) plus
/// every static lint rule, for one repo. The repo's name — the store's
/// directory and the diff label — is the checkout's directory name; use
/// [`Conformance`] directly when it differs, or to render the template once
/// for many repos.
pub fn check_repo(
    pack: &Pack,
    values: &Values,
    repo_dir: &Path,
    store_dir: &Path,
) -> Result<Vec<Check>, PackError> {
    let name = repo_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let c = Conformance::new(pack, values, store_dir)?;
    Ok(c.check_and_lint(&name, repo_dir))
}
