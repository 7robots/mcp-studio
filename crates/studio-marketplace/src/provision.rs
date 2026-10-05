//! Seed a new marketplace repo, or adopt an existing one.

use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;
use studio_core::Secret;
use studio_core::config::Target;

use crate::generate::{self, FileTree};
use crate::model::Marketplace;
use crate::ops::write_tree;
use crate::schema::SchemaId;
use crate::{CLAUDE_CATALOG, CODEX_CATALOG, SCHEMA_DIR, git};

pub const VALIDATE_WORKFLOW: &str = ".github/workflows/validate.yml";
pub const HEALTH_WORKFLOW: &str = ".github/workflows/health.yml";

const VALIDATE_YML: &str = include_str!("../../../marketplace/ci/validate.yml");
const HEALTH_YML: &str = include_str!("../../../marketplace/ci/health.yml");

#[derive(Debug, Clone, Copy, Default)]
pub struct ProvisionOptions {
    /// Also seed the weekly endpoint probe (`health.yml`).
    pub health: bool,
}

/// The CI and schema files every marketplace carries.
pub fn tooling_files(opts: ProvisionOptions) -> FileTree {
    let mut out = FileTree::new();
    for id in SchemaId::ALL {
        out.insert(
            format!("{SCHEMA_DIR}/{}", id.file_name()),
            id.source().to_string(),
        );
    }
    out.insert(VALIDATE_WORKFLOW.into(), VALIDATE_YML.into());
    if opts.health {
        out.insert(HEALTH_WORKFLOW.into(), HEALTH_YML.into());
    }
    out
}

/// Everything a fresh marketplace repo starts with.
pub fn seed_files(m: &Marketplace, opts: ProvisionOptions) -> FileTree {
    let mut out = tooling_files(opts);
    if m.has(Target::Claude) {
        out.insert(
            CLAUDE_CATALOG.into(),
            generate::empty_claude_catalog(m).to_pretty(),
        );
    }
    if m.has(Target::Codex) {
        out.insert(
            CODEX_CATALOG.into(),
            generate::empty_codex_catalog(m).to_pretty(),
        );
    }
    out.insert("servers/.gitkeep".into(), String::new());
    out.insert("plugins/.gitkeep".into(), String::new());
    out.insert("README.md".into(), marketplace_readme(m));
    out
}

pub fn marketplace_readme(m: &Marketplace) -> String {
    let mut s = format!(
        "# {owner} plugin marketplace\n\nA plugin marketplace on GitHub. It holds MCP servers and skill plugins.\n\n## Install from this marketplace\n",
        owner = m.owner_name
    );
    if m.has(Target::Claude) {
        s.push_str(&format!(
            "\n**Claude Code**\n```\n/plugin marketplace add {repo}\n/plugin install <name>@{cat}\n```\n",
            repo = m.repo,
            cat = m.catalog_name
        ));
    }
    if m.has(Target::Codex) {
        s.push_str(&format!(
            "\n**Codex**\n```\ncodex plugin marketplace add {repo}\n```\n",
            repo = m.repo
        ));
    }
    s.push_str(
        "\nFor a private repo, each user needs read access to it and working git credentials.\n\n\
## Layout\n\n```\n\
.claude-plugin/marketplace.json      # Claude catalog\n\
.agents/plugins/marketplace.json     # Codex catalog\n\
servers/<name>/                      # an entry providing an MCP server\n\
plugins/<name>/                      # an entry providing skills only\n\
schema/                              # JSON Schemas the CI gate validates against\n\
```\n\n\
## Managing entries\n\n\
Each entry's `server.yaml` is its source of truth; every other file in its\n\
directory and both catalogs are generated from it. Manage entries with\n\
`mcp-studio marketplace add|update|deprecate|reinstate|remove`, which\n\
validates before committing. **Write access to this repo is the access\n\
control:** whoever can push can publish.\n",
    );
    s
}

fn visible_entries(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n != ".git" && n != ".DS_Store")
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Seed `dir` (absent or empty) as a new marketplace and make the first
/// commit on the marketplace's branch. Local only: no remote is touched.
pub fn provision(dir: &Path, m: &Marketplace, opts: ProvisionOptions) -> Result<Vec<String>> {
    if dir.exists() && !visible_entries(dir).is_empty() {
        bail!(
            "{} is not empty; use `adopt` for an existing repo",
            dir.display()
        );
    }
    std::fs::create_dir_all(dir)?;
    let files = seed_files(m, opts);
    write_tree(dir, &files)?;
    if !dir.join(".git").exists() {
        git::run(dir, &["init", "-q", "-b", &m.branch])?;
    }
    git::run(dir, &["add", "-A"])?;
    git::run(dir, &["commit", "-q", "-m", "seed marketplace"])?;
    Ok(files.into_keys().collect())
}

/// Create the GitHub repo (private unless `public`), add it as `origin`,
/// and push. Runs `gh` with the marketplace's account token.
pub fn create_repo(dir: &Path, m: &Marketplace, token: &Secret, public: bool) -> Result<String> {
    let visibility = if public { "--public" } else { "--private" };
    let description = format!("{} plugin marketplace (Claude Code + Codex)", m.owner_name);
    let out = std::process::Command::new("gh")
        .args([
            "repo",
            "create",
            &m.repo,
            visibility,
            "--description",
            &description,
        ])
        .env("GH_TOKEN", token.expose())
        .output()
        .map_err(|_| anyhow::anyhow!("`gh` is not installed or not on PATH"))?;
    if !out.status.success() {
        bail!(
            "gh repo create {} failed: {}",
            m.repo,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let url = git::github_clone_url(&m.repo);
    git::run(dir, &["remote", "add", "origin", &url])?;
    git::run(dir, &["push", "-q", "-u", "origin", &m.branch])?;
    Ok(url)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum AdoptOutcome {
    /// Empty (or README/LICENSE only): seeded like a new marketplace.
    Seeded { files: Vec<String> },
    /// Already a marketplace: missing schemas/CI added, entries untouched.
    AddedTooling { files: Vec<String> },
    /// Already complete; nothing to do.
    NothingToDo,
}

/// Adopt an existing clone. Refuses a repo whose content it doesn't
/// recognize. Commits locally; never pushes.
pub fn adopt(dir: &Path, m: &Marketplace, opts: ProvisionOptions) -> Result<AdoptOutcome> {
    if !dir.join(".git").exists() {
        bail!("{} is not a git clone", dir.display());
    }
    let entries = visible_entries(dir);
    let trivial = entries.iter().all(|n| {
        let l = n.to_ascii_lowercase();
        l == "readme.md" || l.starts_with("license") || l == ".gitignore"
    });
    let has_catalogs = dir.join(CLAUDE_CATALOG).is_file() || dir.join(CODEX_CATALOG).is_file();
    if trivial {
        let mut files = seed_files(m, opts);
        if dir.join("README.md").exists() {
            files.remove("README.md");
        }
        write_tree(dir, &files)?;
        git::run(dir, &["add", "-A", "--"])?;
        git::run(dir, &["commit", "-q", "-m", "seed marketplace"])?;
        return Ok(AdoptOutcome::Seeded {
            files: files.into_keys().collect(),
        });
    }
    if !has_catalogs {
        bail!(
            "{} holds content that is not a marketplace ({}); not touching it",
            dir.display(),
            entries.join(", ")
        );
    }
    let missing: FileTree = tooling_files(opts)
        .into_iter()
        .filter(|(p, _)| !dir.join(p).exists())
        .collect();
    if missing.is_empty() {
        return Ok(AdoptOutcome::NothingToDo);
    }
    write_tree(dir, &missing)?;
    let paths: Vec<String> = missing.into_keys().collect();
    let mut args = vec!["add", "--"];
    args.extend(paths.iter().map(String::as_str));
    git::run(dir, &args)?;
    git::run(
        dir,
        &[
            "commit",
            "-q",
            "-m",
            "adopt: add marketplace schemas and CI gate",
        ],
    )?;
    Ok(AdoptOutcome::AddedTooling { files: paths })
}
