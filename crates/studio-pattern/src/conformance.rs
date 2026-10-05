//! Fleet conformance for the copied security scaffold.
//!
//! Every fleet repo carries hand-copied versions of the template's security
//! files. Copies drift, and drifted security code fails silently, so drift is
//! made loud in two layers:
//!
//! 1. **The store of allowed differences.** For every (repo, security file),
//!    the unified diff between the rendered template's copy and the repo's
//!    copy lives in the instance's conformance store as
//!    `<store>/<repo>/<file with / → __>.diff`; an empty file means
//!    byte-identical. Those diffs are the reviewed manifest of what each repo
//!    may differ in, versioned in the instance repo.
//! 2. **A per-repo hash gate.** Each repo's `conformance.json` names the
//!    sha256 of each security file as last blessed, and the repo's own CI
//!    fails on any change without a re-bless.
//!
//! `bless` regenerates both; `check` fails on any disagreement; `status` counts
//! differing lines per file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};
use studio_core::check::Check;

use crate::difflib::{count_changed_lines, diff_texts};
use crate::pack::{FileTree, Pack, PackError, Values};

/// The bless command, named in a repo's `conformance.json`.
pub const BLESS_COMMAND: &str = "mcp-studio pattern bless";

/// `src/index.ts` → `src__index.ts.diff`.
pub fn store_file_name(rel: &str) -> String {
    format!("{}.diff", rel.replace('/', "__"))
}

pub fn store_path(store_dir: &Path, repo: &str, rel: &str) -> PathBuf {
    store_dir.join(repo).join(store_file_name(rel))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A repo's `conformance.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoManifest {
    pub scaffold_version: Option<String>,
    pub files: BTreeMap<String, String>,
}

impl RepoManifest {
    pub fn read(repo_dir: &Path, file: &str) -> Result<Option<Self>, String> {
        let path = repo_dir.join(file);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let files = v
            .get("files")
            .and_then(|f| f.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(Self {
            scaffold_version: v
                .get("scaffold_version")
                .and_then(|s| s.as_str())
                .map(String::from),
            files,
        }))
    }

    /// The exact text the original tool wrote (`json.dumps(indent=2)` + `\n`),
    /// with keys in the order given.
    pub fn render(version: &str, files: &[(String, String)]) -> String {
        let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
        let comment = format!(
            "Security-scaffold conformance. Do not hand-edit: bless changes with `{BLESS_COMMAND}`. CI fails when a listed file changes without a re-bless."
        );
        let mut out = String::from("{\n");
        out.push_str(&format!("  \"comment\": {},\n", q(&comment)));
        out.push_str(&format!("  \"scaffold_version\": {},\n", q(version)));
        out.push_str("  \"files\": {");
        for (i, (k, v)) in files.iter().enumerate() {
            out.push_str(if i == 0 { "\n" } else { ",\n" });
            out.push_str(&format!("    {}: {}", q(k), q(v)));
        }
        out.push_str(if files.is_empty() {
            "}\n}\n"
        } else {
            "\n  }\n}\n"
        });
        out
    }
}

/// One repo × security file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileStatus {
    pub repo: String,
    pub file: String,
    /// Changed lines in the diff against the template.
    pub differing_lines: usize,
    /// The current diff equals the stored one.
    pub matches_store: bool,
    /// The repo has the file.
    pub present: bool,
}

/// What a bless did to one repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlessOutcome {
    pub repo: String,
    pub files: usize,
    /// Store diffs whose content changed (or were created).
    pub diffs_changed: Vec<String>,
    /// `conformance.json` was (re)written.
    pub manifest_written: bool,
    /// `conformance.json` would change but `--store-only` kept it as is.
    pub manifest_stale: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BlessOptions {
    /// Write only the instance's store; never touch the repo.
    pub store_only: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ConformanceError {
    #[error(transparent)]
    Pack(#[from] PackError),
    #[error("{repo}: {file} is missing — a missing security file is not blessable")]
    MissingFile { repo: String, file: String },
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

/// The template rendered once for an instance, plus where its store lives.
pub struct Conformance<'p> {
    pub pack: &'p Pack,
    pub store_dir: PathBuf,
    template: FileTree,
}

impl<'p> Conformance<'p> {
    pub fn new(pack: &'p Pack, values: &Values, store_dir: &Path) -> Result<Self, PackError> {
        Ok(Self {
            pack,
            store_dir: store_dir.to_path_buf(),
            template: pack.render(values)?,
        })
    }

    fn template_text(&self, rel: &str) -> String {
        self.template
            .get(rel)
            .map(|f| String::from_utf8_lossy(&f.bytes).into_owned())
            .unwrap_or_default()
    }

    /// The diff between the rendered template and the repo's copy of `rel`,
    /// labelled `template/<rel>` and `<repo>/<rel>`.
    pub fn current_diff(&self, repo: &str, repo_dir: &Path, rel: &str) -> String {
        let theirs = std::fs::read(repo_dir.join(rel))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        diff_texts(
            &self.template_text(rel),
            &theirs,
            &format!("template/{rel}"),
            &format!("{repo}/{rel}"),
        )
    }

    fn stored_diff(&self, repo: &str, rel: &str) -> Option<String> {
        std::fs::read(store_path(&self.store_dir, repo, rel))
            .ok()
            .map(|b| crate::difflib::universal_newlines(&String::from_utf8_lossy(&b)))
    }

    /// Per-file drift for one repo.
    pub fn status(&self, repo: &str, repo_dir: &Path) -> Vec<FileStatus> {
        self.pack
            .security_files()
            .iter()
            .map(|rel| {
                let current = self.current_diff(repo, repo_dir, rel);
                FileStatus {
                    repo: repo.to_string(),
                    file: rel.clone(),
                    differing_lines: count_changed_lines(&current),
                    matches_store: self.stored_diff(repo, rel).as_deref() == Some(current.as_str()),
                    present: repo_dir.join(rel).is_file(),
                }
            })
            .collect()
    }

    /// `pattern.version`, `pattern.hashes` and `pattern.drift` for one repo —
    /// the full conformance check.
    pub fn check(&self, repo: &str, repo_dir: &Path) -> Vec<Check> {
        let mut out = version_and_hashes(self.pack, repo_dir);
        out.push(self.drift_check(repo, repo_dir));
        out
    }

    /// [`Self::check`]'s drift plus every [`crate::lint_repo`] rule (which
    /// carries `pattern.version` and `pattern.hashes`).
    pub fn check_and_lint(&self, repo: &str, repo_dir: &Path) -> Vec<Check> {
        let mut out = crate::lint::lint_repo(self.pack, repo_dir);
        let at = out
            .iter()
            .position(|c| c.id == "pattern.hashes")
            .map_or(0, |i| i + 1);
        out.insert(at, self.drift_check(repo, repo_dir));
        out
    }

    /// `pattern.drift`: every stored diff still equals the current one.
    pub fn drift_check(&self, repo: &str, repo_dir: &Path) -> Check {
        let mut problems = Vec::new();
        let mut total = 0;
        for s in self.status(repo, repo_dir) {
            total += s.differing_lines;
            if !s.present {
                problems.push(format!("{}: MISSING", s.file));
            } else if !s.matches_store {
                problems.push(format!(
                    "{}: diff vs template no longer matches the stored manifest",
                    s.file
                ));
            }
        }
        let n = self.pack.security_files().len();
        if problems.is_empty() {
            Check::pass(
                "pattern.drift",
                format!(
                    "{n} security files match their blessed diffs ({total} differing lines vs template)"
                ),
            )
        } else {
            Check::fail(
                "pattern.drift",
                format!(
                    "{} of {n} security files drifted from their blessed diffs",
                    problems.len()
                ),
            )
            .with_evidence(problems.join("\n"))
        }
    }

    /// Regenerate the stored diffs for `repo` and, unless `store_only`, its
    /// `conformance.json` — which is left untouched when its version and
    /// hashes are already current.
    pub fn bless(
        &self,
        repo: &str,
        repo_dir: &Path,
        opts: BlessOptions,
    ) -> Result<BlessOutcome, ConformanceError> {
        let mut hashes = Vec::new();
        let mut diffs = Vec::new();
        for rel in self.pack.security_files() {
            let path = repo_dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|_| ConformanceError::MissingFile {
                repo: repo.to_string(),
                file: rel.clone(),
            })?;
            hashes.push((rel.clone(), sha256_hex(&bytes)));
            diffs.push((rel.clone(), self.current_diff(repo, repo_dir, rel)));
        }
        let dir = self.store_dir.join(repo);
        std::fs::create_dir_all(&dir).map_err(|source| ConformanceError::Io {
            path: dir.display().to_string(),
            source,
        })?;
        let mut diffs_changed = Vec::new();
        for (rel, diff) in &diffs {
            let path = store_path(&self.store_dir, repo, rel);
            if std::fs::read(&path).ok().as_deref() != Some(diff.as_bytes()) {
                std::fs::write(&path, diff).map_err(|source| ConformanceError::Io {
                    path: path.display().to_string(),
                    source,
                })?;
                diffs_changed.push(rel.clone());
            }
        }

        let file = &self.pack.manifest.security.conformance_file;
        let wanted: BTreeMap<String, String> = hashes.iter().cloned().collect();
        let current = RepoManifest::read(repo_dir, file).ok().flatten();
        let up_to_date = current.as_ref().is_some_and(|m| {
            m.scaffold_version.as_deref() == Some(self.pack.version()) && m.files == wanted
        });
        let mut manifest_written = false;
        if !up_to_date && !opts.store_only {
            let path = repo_dir.join(file);
            std::fs::write(&path, RepoManifest::render(self.pack.version(), &hashes)).map_err(
                |source| ConformanceError::Io {
                    path: path.display().to_string(),
                    source,
                },
            )?;
            manifest_written = true;
        }
        Ok(BlessOutcome {
            repo: repo.to_string(),
            files: hashes.len(),
            diffs_changed,
            manifest_written,
            manifest_stale: !up_to_date && opts.store_only,
        })
    }
}

/// `pattern.version` and `pattern.hashes` from a repo's `conformance.json`.
pub fn version_and_hashes(pack: &Pack, repo_dir: &Path) -> Vec<Check> {
    use std::cmp::Ordering;
    let file = &pack.manifest.security.conformance_file;
    let manifest = match RepoManifest::read(repo_dir, file) {
        Ok(Some(m)) => m,
        Ok(None) => {
            let msg = format!("no {file} — run `{BLESS_COMMAND}`");
            return vec![
                Check::fail("pattern.version", msg.clone()),
                Check::fail("pattern.hashes", msg),
            ];
        }
        Err(e) => {
            return vec![
                Check::fail("pattern.version", format!("{file} is unreadable"))
                    .with_evidence(e.clone()),
                Check::fail("pattern.hashes", format!("{file} is unreadable")).with_evidence(e),
            ];
        }
    };
    let want = pack.version();
    let version = match manifest.scaffold_version.as_deref() {
        None => Check::fail(
            "pattern.version",
            format!("{file} names no scaffold_version"),
        ),
        Some(v) => match crate::manifest::compare_versions(v, want) {
            Ordering::Equal => Check::pass("pattern.version", format!("current ({v})")),
            Ordering::Less => Check::fail("pattern.version", format!("behind: {v} < pack {want}")),
            Ordering::Greater => Check::fail(
                "pattern.version",
                format!("ahead of the pack: {v} > {want}"),
            ),
        },
    };

    let mut problems = Vec::new();
    for rel in pack.security_files() {
        match std::fs::read(repo_dir.join(rel)) {
            Err(_) => problems.push(format!("{rel}: MISSING")),
            Ok(bytes) => match manifest.files.get(rel) {
                None => problems.push(format!("{rel}: not listed in {file}")),
                Some(h) if *h != sha256_hex(&bytes) => {
                    problems.push(format!("{rel}: drifted from its blessed hash"))
                }
                Some(_) => {}
            },
        }
    }
    for listed in manifest.files.keys() {
        if !pack.security_files().contains(listed) {
            problems.push(format!(
                "{listed}: listed in {file} but not a security file of the pack"
            ));
        }
    }
    let n = pack.security_files().len();
    let hashes = if problems.is_empty() {
        Check::pass(
            "pattern.hashes",
            format!("{n} security files match their blessed hashes"),
        )
    } else {
        Check::fail(
            "pattern.hashes",
            format!("{} security file problem(s) against {file}", problems.len()),
        )
        .with_evidence(problems.join("\n"))
    };
    vec![version, hashes]
}
