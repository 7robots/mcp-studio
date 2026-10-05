//! `pattern.toml`: what a pattern pack promises, as data.
//!
//! The manifest is the single source of the pattern's version, its exact
//! dependency pins, the security files under conformance, and the static rules
//! a conforming repo must follow. Fleet probes read it; nothing about the
//! pattern is hardcoded in Rust.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MANIFEST_FILE: &str = "pattern.toml";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub pack: PackMeta,
    /// Exact `dependencies` versions (security load-bearing).
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    /// Exact `devDependencies` versions.
    #[serde(default)]
    pub dev_pins: BTreeMap<String, String>,
    /// Exact `overrides` values.
    #[serde(default)]
    pub overrides: BTreeMap<String, String>,
    /// Major-version floors for ranged deps (`zod = 4`).
    #[serde(default)]
    pub majors: BTreeMap<String, u64>,
    #[serde(default)]
    pub forbidden: Forbidden,
    pub security: Security,
    #[serde(default)]
    pub required: Required,
    #[serde(default)]
    pub wrangler: WranglerRules,
    #[serde(default)]
    pub lockfile: LockfileRules,
    #[serde(default)]
    pub placeholders: Placeholders,
    /// Template variables: name → description. Every `{{name}}` in the
    /// template must be declared here and supplied by the instance.
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PackMeta {
    pub name: String,
    /// `YYYY-MM-DD.N`. A repo whose conformance.json names an older version is behind.
    pub version: String,
    pub description: Option<String>,
    /// Template directory, relative to the pack.
    #[serde(default = "default_template")]
    pub template: PathBuf,
    /// Skill directory (SKILL.md + references/), relative to the pack.
    #[serde(default = "default_skill")]
    pub skill: PathBuf,
}

fn default_template() -> PathBuf {
    "template".into()
}
fn default_skill() -> PathBuf {
    "skill".into()
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Forbidden {
    /// Packages that must not appear in dependencies or devDependencies.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Strings that mark the retired pattern when found in `src/*.ts`.
    #[serde(default)]
    pub legacy_markers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Security {
    /// Files hashed into each repo's conformance.json and diffed against the template.
    pub files: Vec<String>,
    /// The per-repo manifest file name.
    #[serde(default = "default_conformance_file")]
    pub conformance_file: String,
}

fn default_conformance_file() -> String {
    "conformance.json".into()
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Required {
    /// Files that must exist in every repo.
    #[serde(default)]
    pub files: Vec<String>,
    /// `package.json` scripts that must exist, with a required prefix (may be empty).
    #[serde(default)]
    pub scripts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WranglerRules {
    #[serde(default)]
    pub main: Option<String>,
    #[serde(default)]
    pub compatibility_flags: Vec<String>,
    /// Lowest acceptable `compatibility_date` (ISO date; string compare).
    pub min_compatibility_date: Option<String>,
    #[serde(default)]
    pub required_vars: Vec<String>,
    #[serde(default)]
    pub required_kv_bindings: Vec<String>,
    /// The var holding the public MCP URL; its host must equal a route host.
    pub public_url_var: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LockfileRules {
    /// `node_modules/<prefix>*` entries the lockfile must carry, and how many.
    #[serde(default)]
    pub required_prefix_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Placeholders {
    /// Regexes that must not match anywhere in a scaffolded repo.
    #[serde(default)]
    pub patterns: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("{path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Load `<pack_dir>/pattern.toml`.
    pub fn load(pack_dir: &Path) -> Result<Self, ManifestError> {
        let path = pack_dir.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| ManifestError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text).map_err(|source| ManifestError::Parse {
            path: path.display().to_string(),
            source,
        })
    }
}

/// Order pattern versions (`YYYY-MM-DD.N`). Unparseable versions sort first.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn key(v: &str) -> (String, u64) {
        match v.rsplit_once('.') {
            Some((d, n)) => (d.to_string(), n.parse().unwrap_or(0)),
            None => (v.to_string(), 0),
        }
    }
    key(a).cmp(&key(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn versions_order_by_date_then_serial() {
        assert_eq!(compare_versions("2026-08-24.1", "2026-10-05.1"), Ordering::Less);
        assert_eq!(compare_versions("2026-10-05.2", "2026-10-05.10"), Ordering::Less);
        assert_eq!(compare_versions("2026-10-05.1", "2026-10-05.1"), Ordering::Equal);
    }

    #[test]
    fn minimal_manifest_parses() {
        let m = Manifest::parse(
            "[pack]\nname = \"p\"\nversion = \"2026-01-01.1\"\n[security]\nfiles = [\"src/a.ts\"]\n",
        )
        .unwrap();
        assert_eq!(m.security.conformance_file, "conformance.json");
        assert_eq!(m.pack.template, PathBuf::from("template"));
    }
}
