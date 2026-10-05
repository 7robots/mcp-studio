//! Lifecycle operations on a marketplace working tree: add, update,
//! deprecate, reinstate, remove, list.
//!
//! Each operation is planned first as a [`ChangeSet`] (the exact file
//! contents before and after), so `--dry-run` can print it and the real run
//! can apply it, validate, and commit only those paths.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use studio_core::config::{Target, is_slug};

use crate::generate::{self, ENTRY_FILES, FileTree, SERVER_YAML};
use crate::model::{
    Auth, AuthType, ClaudeOverrides, CodexOverrides, Entry, Installation, Kind, Marketplace,
    PluginDir, Transport, normalize_tag, slugify,
};
use crate::ojson::Json;
use crate::tree;
use crate::validate::{self, Finding};
use crate::{CLAUDE_CATALOG, CODEX_CATALOG, git, yaml};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verb {
    Add,
    Update,
    Deprecate,
    Reinstate,
    Remove,
}

impl Verb {
    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Add => "add",
            Verb::Update => "update",
            Verb::Deprecate => "deprecate",
            Verb::Reinstate => "reinstate",
            Verb::Remove => "remove",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileChange {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

impl FileChange {
    pub fn action(&self) -> &'static str {
        match (&self.before, &self.after) {
            (None, Some(_)) => "create",
            (Some(_), None) => "delete",
            _ => "modify",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangeSet {
    pub verb: Verb,
    pub slug: String,
    /// Display name, for the commit title.
    pub name: String,
    pub changes: Vec<FileChange>,
    /// Lines for the commit body and for the operator (e.g. "endpoint changed").
    pub notes: Vec<String>,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn title(&self) -> String {
        format!("{}: {}", self.verb.as_str(), self.name)
    }

    /// `<verb>: <name>`, then the notes as the body.
    pub fn commit_message(&self) -> String {
        if self.notes.is_empty() {
            self.title()
        } else {
            format!("{}\n\n{}\n", self.title(), self.notes.join("\n"))
        }
    }

    /// The branch a PR for this change goes on: `<verb>/<slug>`.
    pub fn branch(&self) -> String {
        format!("{}/{}", self.verb.as_str(), self.slug)
    }

    pub fn paths(&self) -> Vec<&str> {
        self.changes.iter().map(|c| c.path.as_str()).collect()
    }

    /// Every change as a unified diff.
    pub fn diff(&self) -> String {
        self.changes
            .iter()
            .map(|c| {
                crate::reconcile::unified(
                    &c.path,
                    c.before.as_deref().unwrap_or(""),
                    c.after.as_deref().unwrap_or(""),
                )
            })
            .collect()
    }

    /// Write the `after` side to `root`.
    pub fn apply(&self, root: &Path) -> Result<()> {
        for c in &self.changes {
            write_or_delete(root, &c.path, c.after.as_deref())?;
        }
        prune_empty_dirs(root, self.changes.iter().filter(|c| c.after.is_none()));
        Ok(())
    }

    /// Put the `before` side back.
    pub fn revert(&self, root: &Path) -> Result<()> {
        for c in &self.changes {
            write_or_delete(root, &c.path, c.before.as_deref())?;
        }
        prune_empty_dirs(root, self.changes.iter().filter(|c| c.before.is_none()));
        Ok(())
    }
}

fn write_or_delete(root: &Path, rel: &str, content: Option<&str>) -> Result<()> {
    let path = root.join(rel);
    match content {
        Some(text) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, text).with_context(|| format!("writing {rel}"))?;
        }
        None => {
            if path.exists() {
                std::fs::remove_file(&path).with_context(|| format!("deleting {rel}"))?;
            }
        }
    }
    Ok(())
}

/// Remove directories left empty by deleting `deleted` files.
fn prune_empty_dirs<'a>(root: &Path, deleted: impl Iterator<Item = &'a FileChange>) {
    for c in deleted {
        let mut dir = root.join(&c.path);
        while dir.pop() && dir != root {
            if std::fs::remove_dir(&dir).is_err() {
                break; // not empty
            }
        }
    }
}

/// Write a whole file tree under `root`.
pub fn write_tree(root: &Path, files: &FileTree) -> std::io::Result<()> {
    for (rel, text) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
    }
    Ok(())
}

/// Desired content per path (`None` = must not exist) → the changes needed.
fn diff_against(root: &Path, desired: Vec<(String, Option<String>)>) -> Vec<FileChange> {
    let mut out = Vec::new();
    for (path, after) in desired {
        let before = std::fs::read_to_string(root.join(&path)).ok();
        if before != after {
            out.push(FileChange {
                path,
                before,
                after,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn catalog_or_empty(root: &Path, rel: &str, empty: Json) -> Result<Json> {
    Ok(tree::read_json(root, rel)
        .map_err(|e| anyhow::anyhow!("{rel}: {e}"))?
        .unwrap_or(empty))
}

/// Everything an upsert of `p` should leave on disk.
fn upsert_plan(
    root: &Path,
    m: &Marketplace,
    p: &PluginDir,
) -> Result<Vec<(String, Option<String>)>> {
    let previous = std::fs::read_to_string(root.join(p.path(SERVER_YAML))).ok();
    let mut desired: Vec<(String, Option<String>)> = vec![(
        p.path(SERVER_YAML),
        Some(yaml::rewrite(&p.entry, previous.as_deref())),
    )];
    let files = generate::entry_files(m, p);
    for f in ENTRY_FILES {
        let rel = p.path(f);
        desired.push((rel.clone(), files.get(&rel).cloned()));
    }
    if m.has(Target::Claude) {
        let c = catalog_or_empty(root, CLAUDE_CATALOG, generate::empty_claude_catalog(m))?;
        desired.push((
            CLAUDE_CATALOG.into(),
            Some(generate::upsert(&c, generate::claude_entry(p)).to_pretty()),
        ));
    }
    if m.has(Target::Codex) {
        let c = catalog_or_empty(root, CODEX_CATALOG, generate::empty_codex_catalog(m))?;
        desired.push((
            CODEX_CATALOG.into(),
            Some(generate::upsert(&c, generate::codex_entry(p)).to_pretty()),
        ));
    }
    Ok(desired)
}

/// Check a slug is usable for a new entry.
pub fn check_slug(m: &Marketplace, slug: &str) -> Result<()> {
    if !is_slug(slug) {
        bail!(
            "slug {slug:?} must be lowercase alphanumeric with hyphens (no leading/trailing hyphen)"
        );
    }
    if m.is_reserved(slug) {
        bail!("slug {slug:?} is reserved");
    }
    Ok(())
}

fn find(root: &Path, slug: &str) -> Result<PluginDir> {
    let scan = tree::scan(root);
    if let Some(p) = scan.find(slug) {
        return Ok(p.clone());
    }
    if let Some((f, e)) = scan
        .broken
        .iter()
        .find(|(f, _)| f.contains(&format!("/{slug}/")))
    {
        bail!("{f} does not load: {e}");
    }
    bail!("no entry {slug:?} in this marketplace (see `list`)")
}

/// Plan adding a new entry. The slug comes from `slug`, else `entry.slug`,
/// else the name; it is written into `server.yaml` explicitly.
pub fn plan_add(
    root: &Path,
    m: &Marketplace,
    mut entry: Entry,
    slug: Option<&str>,
) -> Result<ChangeSet> {
    let slug = slug
        .map(str::to_string)
        .or_else(|| entry.slug.clone())
        .unwrap_or_else(|| slugify(&entry.name));
    check_slug(m, &slug)?;
    let scan = tree::scan(root);
    let kind = entry.kind();
    for top in tree::ENTRY_ROOTS {
        if root.join(top).join(&slug).exists() {
            bail!("{top}/{slug} already exists: this is an update, not an add");
        }
    }
    if scan.find(&slug).is_some() {
        bail!("an entry with slug {slug:?} already exists: use update");
    }
    if entry.name.trim().is_empty() || entry.description.trim().is_empty() {
        bail!("name and description are required");
    }
    if kind == Kind::McpServer && entry.url.as_deref().unwrap_or("").is_empty() {
        bail!("an mcp-server entry needs an https:// url");
    }
    entry.slug = Some(slug.clone());
    normalize(&mut entry);
    let p = PluginDir::new(entry, format!("{}/{slug}", kind.dir()));
    let changes = diff_against(root, upsert_plan(root, m, &p)?);
    Ok(ChangeSet {
        verb: Verb::Add,
        slug,
        name: p.entry.name.clone(),
        changes,
        notes: Vec::new(),
    })
}

fn normalize(e: &mut Entry) {
    if let Some(tags) = &mut e.tags {
        let mut out: Vec<String> = Vec::new();
        for t in tags.iter().map(|t| normalize_tag(t)) {
            if !t.is_empty() && !out.contains(&t) {
                out.push(t);
            }
        }
        *tags = out;
    }
    e.name = e.name.trim().to_string();
    e.description = e.description.trim().to_string();
}

/// A partial change to an entry: `Some` fields replace, `None` leave alone.
#[derive(Debug, Clone, Default)]
pub struct EntryPatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub transport: Option<Transport>,
    pub auth: Option<AuthType>,
    pub header_name: Option<String>,
    pub tags: Option<Vec<String>>,
    pub category: Option<String>,
    pub homepage: Option<String>,
    pub version: Option<String>,
    pub owner: Option<String>,
    pub installation: Option<Installation>,
    pub strict: Option<bool>,
    pub license: Option<String>,
    pub repository: Option<String>,
}

impl EntryPatch {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.url.is_none()
            && self.transport.is_none()
            && self.auth.is_none()
            && self.header_name.is_none()
            && self.tags.is_none()
            && self.category.is_none()
            && self.homepage.is_none()
            && self.version.is_none()
            && self.owner.is_none()
            && self.installation.is_none()
            && self.strict.is_none()
            && self.license.is_none()
            && self.repository.is_none()
    }

    pub fn apply_to(&self, e: &mut Entry) {
        let set = |slot: &mut Option<String>, v: &Option<String>| {
            if let Some(v) = v {
                *slot = (!v.is_empty()).then(|| v.clone());
            }
        };
        if let Some(v) = &self.name {
            e.name = v.clone();
        }
        if let Some(v) = &self.description {
            e.description = v.clone();
        }
        set(&mut e.url, &self.url);
        if let Some(t) = self.transport {
            e.transport = Some(t);
        }
        if self.auth.is_some() || self.header_name.is_some() {
            let a = e.auth.get_or_insert_with(Auth::default);
            if let Some(k) = self.auth {
                a.kind = Some(k);
            }
            set(&mut a.header_name, &self.header_name);
        }
        if let Some(t) = &self.tags {
            e.tags = Some(t.clone());
        }
        set(&mut e.category, &self.category);
        set(&mut e.homepage, &self.homepage);
        set(&mut e.version, &self.version);
        set(&mut e.owner, &self.owner);
        if let Some(i) = self.installation {
            e.codex
                .get_or_insert_with(CodexOverrides::default)
                .installation = Some(i);
        }
        if self.strict.is_some() || self.license.is_some() || self.repository.is_some() {
            let c = e.claude.get_or_insert_with(ClaudeOverrides::default);
            if let Some(s) = self.strict {
                c.strict = Some(s);
            }
            set(&mut c.license, &self.license);
            set(&mut c.repository, &self.repository);
        }
    }

    /// Build a new entry from the patch (for `add`).
    pub fn into_entry(self, kind: Kind) -> Entry {
        let mut e = Entry {
            kind: (kind == Kind::Skill).then_some(kind),
            transport: (kind == Kind::McpServer).then_some(Transport::Http),
            auth: (kind == Kind::McpServer).then_some(Auth {
                kind: Some(AuthType::None),
                header_name: None,
            }),
            ..Default::default()
        };
        self.apply_to(&mut e);
        e
    }
}

/// Plan an update of an existing entry.
pub fn plan_update(
    root: &Path,
    m: &Marketplace,
    slug: &str,
    patch: &EntryPatch,
) -> Result<ChangeSet> {
    let old = find(root, slug)?;
    let mut p = old.clone();
    patch.apply_to(&mut p.entry);
    normalize(&mut p.entry);
    let mut notes = Vec::new();
    if p.entry.url != old.entry.url {
        notes.push(format!(
            "Endpoint changed: {} -> {} (existing installs are affected).",
            old.entry.url(),
            p.entry.url()
        ));
    }
    if p.entry.auth_type() != old.entry.auth_type()
        || p.entry.header_name() != old.entry.header_name()
    {
        notes.push(format!(
            "Auth changed: {} -> {} (existing installs are affected).",
            old.entry.auth_type().as_str(),
            p.entry.auth_type().as_str()
        ));
    }
    let changes = diff_against(root, upsert_plan(root, m, &p)?);
    if !patch.is_empty() && !changes.is_empty() && p.entry.version() == old.entry.version() {
        notes.push(format!(
            "Version left at {}; consider bumping it so installers see the change.",
            p.entry.version()
        ));
    }
    Ok(ChangeSet {
        verb: Verb::Update,
        slug: p.slug.clone(),
        name: p.entry.name.clone(),
        changes,
        notes,
    })
}

/// Regenerate an entry's files from its `server.yaml` as it stands (no
/// field changes): the fix for drift `reconcile` reports.
pub fn plan_regenerate(root: &Path, m: &Marketplace, slug: &str) -> Result<ChangeSet> {
    plan_update(root, m, slug, &EntryPatch::default())
}

/// Plan deprecating an entry: it stays listed and installable.
pub fn plan_deprecate(root: &Path, m: &Marketplace, slug: &str, reason: &str) -> Result<ChangeSet> {
    let mut p = find(root, slug)?;
    if reason.trim().is_empty() {
        bail!("a deprecation needs a reason");
    }
    p.entry.deprecated = Some(true);
    p.entry.deprecated_reason = Some(reason.trim().to_string());
    let changes = diff_against(root, upsert_plan(root, m, &p)?);
    Ok(ChangeSet {
        verb: Verb::Deprecate,
        slug: p.slug.clone(),
        name: p.entry.name.clone(),
        changes,
        notes: vec![format!("Reason: {}", reason.trim())],
    })
}

/// Plan reinstating a deprecated entry.
pub fn plan_reinstate(root: &Path, m: &Marketplace, slug: &str) -> Result<ChangeSet> {
    let mut p = find(root, slug)?;
    if !p.entry.is_deprecated() {
        bail!("{slug} is not deprecated");
    }
    p.entry.deprecated = None;
    p.entry.deprecated_reason = None;
    let changes = diff_against(root, upsert_plan(root, m, &p)?);
    Ok(ChangeSet {
        verb: Verb::Reinstate,
        slug: p.slug.clone(),
        name: p.entry.name.clone(),
        changes,
        notes: Vec::new(),
    })
}

/// Plan removing an entry: its directory and both catalog entries.
pub fn plan_remove(root: &Path, m: &Marketplace, slug: &str) -> Result<ChangeSet> {
    let p = find(root, slug)?;
    let mut desired: Vec<(String, Option<String>)> = tree::files_under(root, &p.rel_dir)
        .into_iter()
        .map(|f| (f, None))
        .collect();
    for (rel, target) in [
        (CLAUDE_CATALOG, Target::Claude),
        (CODEX_CATALOG, Target::Codex),
    ] {
        if !m.has(target) {
            continue;
        }
        if let Some(c) = tree::read_json(root, rel).map_err(|e| anyhow::anyhow!("{rel}: {e}"))? {
            desired.push((
                rel.into(),
                Some(generate::remove_from(&c, &p.slug).to_pretty()),
            ));
        }
    }
    Ok(ChangeSet {
        verb: Verb::Remove,
        slug: p.slug.clone(),
        name: p.entry.name.clone(),
        changes: diff_against(root, desired),
        notes: vec!["Removal breaks existing installs.".into()],
    })
}

/// Apply a change set, then validate. Any finding the tree did not already
/// have puts everything back and fails.
pub fn apply_checked(root: &Path, m: &Marketplace, cs: &ChangeSet) -> Result<()> {
    let baseline = validate::validate_dir(root, m).findings;
    cs.apply(root)?;
    let after = validate::validate_dir(root, m).findings;
    let new: Vec<&Finding> = after.iter().filter(|f| !baseline.contains(f)).collect();
    if !new.is_empty() {
        cs.revert(root)?;
        let lines: Vec<String> = new
            .iter()
            .map(|f| format!("  {}: {}", f.file, f.message))
            .collect();
        bail!(
            "the change would make the marketplace invalid, so nothing was written:\n{}",
            lines.join("\n")
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Commit {
    /// HEAD before the commit (`None` in an empty repo).
    pub base: Option<String>,
    pub head: String,
}

/// Stage exactly the change set's paths and commit with its message.
pub fn commit(root: &Path, cs: &ChangeSet) -> Result<Commit> {
    let base = git::run(root, &["rev-parse", "--verify", "-q", "HEAD"]).ok();
    let mut args = vec!["add", "-A", "--"];
    args.extend(cs.paths());
    git::run(root, &args)?;
    git::run(root, &["commit", "-q", "-m", &cs.commit_message()])?;
    let head = git::run(root, &["rev-parse", "HEAD"])?;
    Ok(Commit { base, head })
}

/// One row of `list`.
#[derive(Debug, Clone, Serialize)]
pub struct EntrySummary {
    pub slug: String,
    pub name: String,
    pub kind: Kind,
    pub version: String,
    pub auth: AuthType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub deprecated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated_reason: Option<String>,
    pub dir: String,
}

impl From<&PluginDir> for EntrySummary {
    fn from(p: &PluginDir) -> Self {
        let e = &p.entry;
        Self {
            slug: p.slug.clone(),
            name: e.name.clone(),
            kind: e.kind(),
            version: e.version().to_string(),
            auth: e.auth_type(),
            url: e.url.clone(),
            category: e.category.clone(),
            tags: e.tags().to_vec(),
            deprecated: e.is_deprecated(),
            deprecated_reason: e.deprecated_reason.clone(),
            dir: p.rel_dir.clone(),
        }
    }
}

/// The entries in a working tree, sorted by slug.
pub fn list(root: &Path) -> Vec<EntrySummary> {
    tree::scan(root)
        .plugins
        .iter()
        .map(EntrySummary::from)
        .collect()
}
