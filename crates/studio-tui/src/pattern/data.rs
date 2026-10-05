//! What the Pattern module shows, loaded off the UI thread: the pack with the
//! instance's values, its changelog and rendered skill docs ([`PackData`]),
//! and one conformance report per fleet repo ([`RepoReport`]). Every function
//! here blocks (file reads, rendering, diffing); the module runs them in
//! `spawn_blocking`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use studio_core::Instance;
use studio_core::check::{Check, Status};
use studio_core::config::repo_name;
use studio_pattern::conformance::{RepoManifest, store_path};
use studio_pattern::manifest::compare_versions;
use studio_pattern::pack::{render_text, walk};
use studio_pattern::{BlessOptions, BlessOutcome, Conformance, FileStatus, Pack, Values};

/// The conformance checks proper; everything else a repo report carries is lint.
pub const CONFORMANCE_IDS: [&str; 3] = ["pattern.version", "pattern.hashes", "pattern.drift"];

/// One skill document, rendered with the instance's values.
#[derive(Debug, Clone)]
pub struct SkillDoc {
    /// Relative to the skill directory (`SKILL.md`, `references/x.md`).
    pub path: String,
    pub text: String,
    /// Why substitution failed (the raw text is shown instead).
    pub error: Option<String>,
}

/// The pack and everything derived from it that does not depend on a repo.
#[derive(Debug)]
pub struct PackData {
    pub pack: Pack,
    pub values: Values,
    /// The values file could not be read; the overview says so.
    pub values_error: Option<String>,
    pub changelog: Result<String, String>,
    pub docs: Vec<SkillDoc>,
    pub store_dir: PathBuf,
    /// Fleet repos as `cmd_pattern` lists them: `(name, checkout)`.
    pub repos: Vec<(String, PathBuf)>,
}

/// One security file of one repo.
#[derive(Debug, Clone)]
pub struct FileReport {
    pub status: FileStatus,
    /// The diff of the rendered template against the repo's copy, now.
    pub current: String,
    /// The blessed diff in the instance's store, if any.
    pub stored: Option<String>,
}

/// One fleet repo's conformance and lint.
#[derive(Debug, Clone)]
pub struct RepoReport {
    pub name: String,
    pub dir: PathBuf,
    /// Why the repo could not be checked (no checkout).
    pub missing: Option<String>,
    /// `scaffold_version` from its `conformance.json`.
    pub version: Option<String>,
    pub checks: Vec<Check>,
    pub files: Vec<FileReport>,
}

/// How a repo's blessed version compares with the pack's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionState {
    Current,
    Behind,
    Ahead,
    Unknown,
}

impl RepoReport {
    pub fn check(&self, id: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.id == id)
    }

    pub fn version_state(&self, pack_version: &str) -> VersionState {
        use std::cmp::Ordering;
        match self.version.as_deref() {
            None => VersionState::Unknown,
            Some(v) => match compare_versions(v, pack_version) {
                Ordering::Equal => VersionState::Current,
                Ordering::Less => VersionState::Behind,
                Ordering::Greater => VersionState::Ahead,
            },
        }
    }

    /// Differing lines vs the template, over every security file.
    pub fn drift_lines(&self) -> usize {
        self.files.iter().map(|f| f.status.differing_lines).sum()
    }

    /// Security files whose current diff is not the blessed one (or missing).
    pub fn drifted(&self) -> usize {
        self.files
            .iter()
            .filter(|f| !f.status.matches_store || !f.status.present)
            .count()
    }

    /// `(pass, warn, fail)` over the lint rules (everything but the three
    /// conformance checks).
    pub fn lint_counts(&self) -> (usize, usize, usize) {
        let lint = self
            .checks
            .iter()
            .filter(|c| !CONFORMANCE_IDS.contains(&c.id.as_str()));
        let mut counts = (0, 0, 0);
        for c in lint {
            match c.status {
                Status::Pass => counts.0 += 1,
                Status::Warn => counts.1 += 1,
                Status::Fail => counts.2 += 1,
                Status::Skip => {}
            }
        }
        counts
    }

    pub fn status(&self) -> Status {
        if self.missing.is_some() {
            return Status::Fail;
        }
        studio_core::check::rollup(&self.checks)
    }
}

/// Loads the pack named by the instance's `[pattern]`.
pub fn load_pack(instance: &Instance) -> Result<PackData, String> {
    let dir = instance.pattern_dir().ok_or_else(|| {
        format!(
            "{} has no [pattern] section",
            instance
                .root
                .join(studio_core::instance::CONFIG_FILE)
                .display()
        )
    })?;
    let dir = clean(&dir);
    let pack = Pack::load(&dir).map_err(|e| format!("loading {}: {e}", dir.display()))?;
    let (values, values_error) = match instance.pattern_values() {
        Ok(v) => (v, None),
        Err(e) => (Values::new(), Some(e.to_string())),
    };
    let changelog_path = pack.dir.join("CHANGELOG.md");
    let changelog = std::fs::read_to_string(&changelog_path)
        .map_err(|e| format!("{}: {e}", changelog_path.display()));
    let docs = skill_docs(&pack, &values);
    let store_dir = clean(
        &instance
            .conformance_dir()
            .unwrap_or_else(|| instance.root.join("conformance")),
    );
    Ok(PackData {
        repos: fleet_repos(instance),
        pack,
        values,
        values_error,
        changelog,
        docs,
        store_dir,
    })
}

/// `SKILL.md` first, then every other Markdown file, each rendered on its
/// own so one bad placeholder does not hide the rest.
fn skill_docs(pack: &Pack, values: &Values) -> Vec<SkillDoc> {
    let dir = pack.skill_dir();
    let Ok(files) = walk(&dir) else {
        return Vec::new();
    };
    let mut docs: Vec<SkillDoc> = files
        .into_iter()
        .filter(|(rel, _)| rel.ends_with(".md"))
        .map(|(rel, path)| {
            let raw = std::fs::read_to_string(&path).unwrap_or_default();
            match render_text(&rel, &raw, &pack.manifest.variables, values) {
                Ok(text) => SkillDoc {
                    path: rel,
                    text,
                    error: None,
                },
                Err(e) => SkillDoc {
                    path: rel,
                    text: raw,
                    error: Some(e.to_string()),
                },
            }
        })
        .collect();
    docs.sort_by_key(|d| (d.path != "SKILL.md", d.path.clone()));
    docs
}

/// The fleet's repos as `mcp-studio pattern` lists them: `[fleet] include`,
/// then `[[fleet.server]]`, de-duplicated without regard to case.
pub fn fleet_repos(instance: &Instance) -> Vec<(String, PathBuf)> {
    let fleet = &instance.config.fleet;
    let mut all: Vec<&str> = Vec::new();
    for r in fleet
        .include
        .iter()
        .chain(fleet.servers.iter().map(|s| &s.repo))
    {
        if !all.iter().any(|a| a.eq_ignore_ascii_case(r)) {
            all.push(r);
        }
    }
    all.into_iter()
        .map(|r| (repo_name(r).to_string(), clean(&instance.repo_dir(r))))
        .collect()
}

/// `a/b/../c` → `a/c`, lexically (for display; nothing is resolved).
pub fn clean(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Conformance and lint for every fleet repo.
pub fn run_checks(data: &PackData) -> Result<Vec<RepoReport>, String> {
    let c = Conformance::new(&data.pack, &data.values, &data.store_dir)
        .map_err(|e| format!("rendering the template: {e}"))?;
    let conformance_file = &data.pack.manifest.security.conformance_file;
    Ok(data
        .repos
        .iter()
        .map(|(name, dir)| {
            if !dir.is_dir() {
                return RepoReport {
                    name: name.clone(),
                    dir: dir.clone(),
                    missing: Some(format!("no checkout at {}", dir.display())),
                    version: None,
                    checks: Vec::new(),
                    files: Vec::new(),
                };
            }
            let files = c
                .status(name, dir)
                .into_iter()
                .map(|status| {
                    let current = c.current_diff(name, dir, &status.file);
                    let stored = std::fs::read(store_path(&data.store_dir, name, &status.file))
                        .ok()
                        .map(|b| String::from_utf8_lossy(&b).into_owned());
                    FileReport {
                        status,
                        current,
                        stored,
                    }
                })
                .collect();
            RepoReport {
                name: name.clone(),
                dir: dir.clone(),
                missing: None,
                version: RepoManifest::read(dir, conformance_file)
                    .ok()
                    .flatten()
                    .and_then(|m| m.scaffold_version),
                checks: c.check_and_lint(name, dir),
                files,
            }
        })
        .collect())
}

/// Blesses one repo: always the store; the repo's `conformance.json` too
/// unless `store_only`.
pub fn bless(data: &Arc<PackData>, repo: &str, store_only: bool) -> Result<BlessOutcome, String> {
    let (name, dir) = data
        .repos
        .iter()
        .find(|(n, _)| n == repo)
        .ok_or_else(|| format!("{repo} is not in the fleet"))?;
    let c = Conformance::new(&data.pack, &data.values, &data.store_dir)
        .map_err(|e| format!("rendering the template: {e}"))?;
    c.bless(name, dir, BlessOptions { store_only })
        .map_err(|e| e.to_string())
}

/// Hides values that look like credentials, and shortens long ones.
pub fn display_value(name: &str, value: &str, max: usize) -> (String, bool) {
    if looks_secret(name, value) {
        return ("•••••• (hidden)".into(), true);
    }
    let value = value.replace(['\n', '\r'], " ");
    if value.chars().count() > max {
        let cut: String = value.chars().take(max.saturating_sub(1)).collect();
        (format!("{cut}…"), false)
    } else {
        (value, false)
    }
}

/// A name or value that suggests a credential. The values file is not meant
/// to hold any, but the overview never shows one if it does.
pub fn looks_secret(name: &str, value: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if [
        "secret",
        "token",
        "password",
        "passwd",
        "private",
        "credential",
        "api_key",
        "apikey",
    ]
    .iter()
    .any(|k| n.contains(k))
    {
        return true;
    }
    let v = value.trim();
    if [
        "eyJ",
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "sk-",
        "xox",
        "AKIA",
        "-----BEGIN",
    ]
    .iter()
    .any(|p| v.starts_with(p))
    {
        return true;
    }
    // A long unbroken token mixing letters and digits, with no URL or
    // hostname punctuation, reads as a key.
    v.len() >= 32
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '='))
        && !v.contains('/')
        && v.chars().any(|c| c.is_ascii_digit())
        && v.chars().any(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_hidden_and_long_values_cut() {
        assert!(looks_secret("api_token", "x"));
        assert!(looks_secret("anything", "ghp_abc"));
        assert!(looks_secret(
            "anything",
            "Zx81Kq0LmNp4Rs7TuVw2YzAb3Cd5Ef6Gh9Ij"
        ));
        assert!(!looks_secret("okta_domain", "https://acme.okta.example"));
        assert!(!looks_secret("okta_client_id", "0oaEXAMPLEINTERACTIVE"));
        let (v, hidden) = display_value("domain", &"a".repeat(60), 20);
        assert!(!hidden);
        assert_eq!(v.chars().count(), 20);
        assert!(v.ends_with('…'));
    }
}
