//! Reconcile and verify: does the tree say what its `server.yaml`s say?
//!
//! [`reconcile`] compares field by field and reports one [`Check`] per
//! concern (`marketplace.<slug>.version`, `.codex_auth`, `.category`, …), for
//! the TUI and `--json`. [`verify`] regenerates the whole marketplace into a
//! temporary directory and diffs it byte for byte against the real tree.

use std::path::Path;

use serde::Serialize;
use studio_core::check::Check;
use studio_core::config::Target;

use crate::generate::{
    self, CLAUDE_MANIFEST, CODEX_MANIFEST, CODEX_MCP_JSON, ENTRY_FILES, MCP_JSON, README,
};
use crate::model::{Marketplace, PluginDir};
use crate::ojson::Json;
use crate::tree;
use crate::{CLAUDE_CATALOG, CODEX_CATALOG};

fn id(slug: &str, what: &str) -> String {
    format!("marketplace.{slug}.{what}")
}

fn find<'a>(catalog: Option<&'a Json>, name: &str) -> Option<&'a Json> {
    catalog?
        .get("plugins")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(Json::as_str) == Some(name))
}

fn show(v: Option<&Json>) -> String {
    match v {
        None => "absent".into(),
        Some(j) => serde_json::to_string(j).unwrap_or_default(),
    }
}

/// Keys whose values differ between two objects, as `key: have → want`.
fn differing_keys(have: &Json, want: &Json, skip: &[&str]) -> Vec<String> {
    let mut keys: Vec<&str> = Vec::new();
    for j in [have, want] {
        if let Json::Object(kv) = j {
            for (k, _) in kv {
                if !keys.contains(&k.as_str()) && !skip.contains(&k.as_str()) {
                    keys.push(k);
                }
            }
        }
    }
    keys.into_iter()
        .filter_map(|k| {
            let (h, w) = (have.get(k), want.get(k));
            let same = match (h, w) {
                (Some(a), Some(b)) => a.to_value() == b.to_value(),
                (None, None) => true,
                _ => false,
            };
            (!same).then(|| format!("{k}: {} → {}", show(h), show(w)))
        })
        .collect()
}

fn without(j: &Json, key: &str) -> Json {
    match j {
        Json::Object(kv) => Json::Object(kv.iter().filter(|(k, _)| k != key).cloned().collect()),
        other => other.clone(),
    }
}

/// Compare one generated JSON file with the tree, ignoring `skip` keys.
fn json_check(
    root: &Path,
    rel: &str,
    want: Option<&Json>,
    check_id: String,
    label: &str,
    skip: &[&str],
) -> Check {
    let have = tree::read_json(root, rel);
    match (have, want) {
        (Err(e), _) => Check::fail(check_id, format!("{label} unreadable")).with_evidence(e),
        (Ok(None), None) => Check::pass(check_id, format!("{label} correctly absent")),
        (Ok(None), Some(_)) => Check::fail(check_id, format!("{label} missing")),
        (Ok(Some(_)), None) => Check::fail(
            check_id,
            format!("{label} present but server.yaml does not call for it"),
        ),
        (Ok(Some(h)), Some(w)) => {
            let mut h2 = h.clone();
            let mut w2 = w.clone();
            for k in skip {
                h2 = without(&h2, k);
                w2 = without(&w2, k);
            }
            if h2.to_value() == w2.to_value() {
                Check::pass(check_id, format!("{label} matches server.yaml"))
            } else {
                let keys = differing_keys(&h2, &w2, &[]);
                let ev = if keys.is_empty() {
                    "nested values differ".to_string()
                } else {
                    keys.join("\n")
                };
                Check::fail(check_id, format!("{label} differs from server.yaml")).with_evidence(ev)
            }
        }
    }
}

/// Field-by-field reconciliation of every entry with its generated files
/// and both catalogs.
pub fn reconcile(root: &Path, m: &Marketplace) -> Vec<Check> {
    let mut out = Vec::new();
    if !root.is_dir() {
        out.push(Check::skip(
            format!("marketplace.{}", m.id),
            format!("not cloned at {}", root.display()),
        ));
        return out;
    }
    let scan = tree::scan(root);
    let claude = read_catalog(root, CLAUDE_CATALOG, m.has(Target::Claude), &mut out);
    let codex = read_catalog(root, CODEX_CATALOG, m.has(Target::Codex), &mut out);

    if let Some(c) = &claude {
        let want = generate::empty_claude_catalog(m);
        let diffs = differing_keys(c, &want, &["metadata", "plugins"]);
        out.push(identity_check("claude", diffs));
    }
    if let Some(c) = &codex {
        let want = generate::empty_codex_catalog(m);
        let diffs = differing_keys(c, &want, &["plugins"]);
        out.push(identity_check("codex", diffs));
    }

    for (file, problem) in &scan.broken {
        let dir = file.split('/').nth(1).unwrap_or(file);
        out.push(
            Check::fail(id(dir, "server_yaml"), "server.yaml does not load")
                .with_evidence(problem.clone()),
        );
    }

    for p in &scan.plugins {
        reconcile_entry(root, m, p, claude.as_ref(), codex.as_ref(), &mut out);
    }

    // Catalog entries no server.yaml accounts for.
    for (label, cat) in [("Claude", &claude), ("Codex", &codex)] {
        let Some(c) = cat else { continue };
        for e in c
            .get("plugins")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
        {
            let name = e.get("name").and_then(Json::as_str).unwrap_or("?");
            if scan.find(name).is_some() {
                continue;
            }
            let src = e.get("source").and_then(Json::as_str).unwrap_or("");
            let check = if !src.is_empty() && root.join(src).is_dir() {
                Check::warn(
                    id(name, "orphan"),
                    format!(
                        "{label} catalog lists {name}, which has no server.yaml (hand-managed)"
                    ),
                )
            } else {
                Check::fail(
                    id(name, "orphan"),
                    format!("{label} catalog lists {name}, whose source {src:?} does not exist"),
                )
            };
            out.push(check);
        }
    }
    out
}

fn identity_check(target: &str, diffs: Vec<String>) -> Check {
    let cid = format!("marketplace.catalog.{target}");
    if diffs.is_empty() {
        Check::pass(
            cid,
            format!("{target} catalog name and owner match studio.toml"),
        )
    } else {
        Check::fail(
            cid,
            format!("{target} catalog identity differs from studio.toml"),
        )
        .with_evidence(diffs.join("\n"))
    }
}

fn read_catalog(root: &Path, rel: &str, wanted: bool, out: &mut Vec<Check>) -> Option<Json> {
    let which = if rel == CLAUDE_CATALOG {
        "claude"
    } else {
        "codex"
    };
    match tree::read_json(root, rel) {
        Ok(Some(j)) => Some(j),
        Ok(None) if wanted => {
            out.push(Check::fail(
                format!("marketplace.catalog.{which}"),
                format!("{rel} is missing"),
            ));
            None
        }
        Ok(None) => None,
        Err(e) => {
            out.push(
                Check::fail(
                    format!("marketplace.catalog.{which}"),
                    format!("{rel} does not parse"),
                )
                .with_evidence(e),
            );
            None
        }
    }
}

fn reconcile_entry(
    root: &Path,
    m: &Marketplace,
    p: &PluginDir,
    claude: Option<&Json>,
    codex: Option<&Json>,
    out: &mut Vec<Check>,
) {
    let s = &p.slug;
    let e = &p.entry;
    let want_version = e.version();

    // Version agreement across manifests and the Claude catalog.
    let mut seen = Vec::new();
    if m.has(Target::Claude) {
        if let Ok(Some(j)) = tree::read_json(root, &p.path(CLAUDE_MANIFEST)) {
            seen.push((CLAUDE_MANIFEST, show(j.get("version"))));
        }
        if let Some(ce) = find(claude, s) {
            seen.push(("Claude catalog", show(ce.get("version"))));
        }
    }
    if m.has(Target::Codex)
        && let Ok(Some(j)) = tree::read_json(root, &p.path(CODEX_MANIFEST))
    {
        seen.push((CODEX_MANIFEST, show(j.get("version"))));
    }
    let want_shown = show(Some(&Json::str(want_version)));
    let bad: Vec<String> = seen
        .iter()
        .filter(|(_, v)| *v != want_shown)
        .map(|(f, v)| format!("{f}: {v}"))
        .collect();
    out.push(if bad.is_empty() {
        Check::pass(
            id(s, "version"),
            format!("version {want_version} everywhere"),
        )
    } else {
        Check::fail(
            id(s, "version"),
            format!("server.yaml says {want_version}; others disagree"),
        )
        .with_evidence(bad.join("\n"))
    });

    if m.has(Target::Claude) {
        let want = generate::claude_entry(p);
        match find(claude, s) {
            None => out.push(Check::fail(
                id(s, "claude_entry"),
                "not listed in the Claude catalog",
            )),
            Some(have) => {
                let (h, w) = (have.get("category"), want.get("category"));
                let same = h.map(Json::to_value) == w.map(Json::to_value);
                out.push(if same {
                    Check::pass(id(s, "category"), "category matches server.yaml")
                } else {
                    Check::fail(
                        id(s, "category"),
                        format!(
                            "Claude catalog category is {}, server.yaml says {}",
                            show(h),
                            show(w)
                        ),
                    )
                });
                let diffs = differing_keys(have, &want, &["version", "category"]);
                out.push(if diffs.is_empty() {
                    Check::pass(
                        id(s, "claude_entry"),
                        "Claude catalog entry matches server.yaml",
                    )
                } else {
                    Check::fail(
                        id(s, "claude_entry"),
                        "Claude catalog entry differs from server.yaml",
                    )
                    .with_evidence(diffs.join("\n"))
                });
            }
        }
        out.push(json_check(
            root,
            &p.path(CLAUDE_MANIFEST),
            Some(&generate::claude_manifest(m, p)),
            id(s, "claude_manifest"),
            CLAUDE_MANIFEST,
            &["version"],
        ));
    }

    if m.has(Target::Codex) {
        let want = generate::codex_entry(p);
        match find(codex, s) {
            None => out.push(Check::fail(
                id(s, "codex_entry"),
                "not listed in the Codex catalog",
            )),
            Some(have) => {
                let h = have.get("policy").and_then(|p| p.get("authentication"));
                let w = want.get("policy").and_then(|p| p.get("authentication"));
                let same = h.map(Json::to_value) == w.map(Json::to_value);
                let auth = e.auth_type().as_str();
                out.push(if same {
                    Check::pass(
                        id(s, "codex_auth"),
                        format!("policy.authentication right for auth {auth}"),
                    )
                } else {
                    Check::fail(
                        id(s, "codex_auth"),
                        format!(
                            "Codex policy.authentication is {}, auth {auth} needs {}",
                            show(h),
                            show(w)
                        ),
                    )
                });
                let mut h2 = have.clone();
                let mut w2 = want.clone();
                for j in [&mut h2, &mut w2] {
                    if let Some(pol) = j.get("policy").map(|p| without(p, "authentication")) {
                        j.set("policy", pol);
                    }
                }
                let diffs = differing_keys(&h2, &w2, &[]);
                out.push(if diffs.is_empty() {
                    Check::pass(
                        id(s, "codex_entry"),
                        "Codex catalog entry matches server.yaml",
                    )
                } else {
                    Check::fail(
                        id(s, "codex_entry"),
                        "Codex catalog entry differs from server.yaml",
                    )
                    .with_evidence(diffs.join("\n"))
                });
            }
        }
        out.push(json_check(
            root,
            &p.path(CODEX_MANIFEST),
            Some(&generate::codex_manifest(m, p)),
            id(s, "codex_manifest"),
            CODEX_MANIFEST,
            &["version"],
        ));
        out.push(json_check(
            root,
            &p.path(CODEX_MCP_JSON),
            generate::codex_mcp_json(m, p).as_ref(),
            id(s, "codex_mcp"),
            CODEX_MCP_JSON,
            &[],
        ));
    }

    out.push(json_check(
        root,
        &p.path(MCP_JSON),
        generate::mcp_json(m, p).as_ref(),
        id(s, "mcp"),
        MCP_JSON,
        &[],
    ));

    let want_readme = generate::readme(m, p);
    out.push(match std::fs::read_to_string(root.join(p.path(README))) {
        Ok(have) if have == want_readme => {
            Check::pass(id(s, "readme"), "README.md matches server.yaml")
        }
        Ok(have) => Check::warn(
            id(s, "readme"),
            "README.md differs from what server.yaml generates",
        )
        .with_evidence(unified(&p.path(README), &have, &want_readme)),
        Err(_) => Check::warn(id(s, "readme"), "README.md missing"),
    });
}

/// A unified diff, `have` → `want`.
pub fn unified(path: &str, have: &str, want: &str) -> String {
    similar::TextDiff::from_lines(have, want)
        .unified_diff()
        .context_radius(2)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    /// Generated content differs from the tree.
    Changed,
    /// Generated, but not in the tree.
    Missing,
    /// In the tree where Studio generates files, but not generated.
    Extra,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDiff {
    pub path: String,
    pub kind: DiffKind,
    pub diff: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    /// Files regenerated.
    pub generated: usize,
    /// Of those, byte-identical to the tree.
    pub identical: usize,
    pub diffs: Vec<FileDiff>,
    /// `server.yaml`s that did not load (nothing was generated for them).
    pub broken: Vec<(String, String)>,
}

impl VerifyReport {
    pub fn is_clean(&self) -> bool {
        self.diffs.is_empty() && self.broken.is_empty()
    }
}

/// Regenerate the whole marketplace into a temp dir and diff it against the
/// tree. Read-only on `root`.
pub fn verify(root: &Path, m: &Marketplace) -> anyhow::Result<VerifyReport> {
    let scan = tree::scan(root);
    let claude = tree::read_json(root, CLAUDE_CATALOG).ok().flatten();
    let codex = tree::read_json(root, CODEX_CATALOG).ok().flatten();
    let files = generate::generate(m, &scan.plugins, claude.as_ref(), codex.as_ref());

    let tmp = tempfile::tempdir()?;
    crate::ops::write_tree(tmp.path(), &files)?;

    let mut report = VerifyReport {
        generated: files.len(),
        identical: 0,
        diffs: Vec::new(),
        broken: scan.broken.clone(),
    };
    for path in files.keys() {
        let want = std::fs::read(tmp.path().join(path))?;
        match std::fs::read(root.join(path)) {
            Ok(have) if have == want => report.identical += 1,
            Ok(have) => report.diffs.push(FileDiff {
                path: path.clone(),
                kind: DiffKind::Changed,
                diff: unified(
                    path,
                    &String::from_utf8_lossy(&have),
                    &String::from_utf8_lossy(&want),
                ),
            }),
            Err(_) => report.diffs.push(FileDiff {
                path: path.clone(),
                kind: DiffKind::Missing,
                diff: unified(path, "", &String::from_utf8_lossy(&want)),
            }),
        }
    }
    for p in &scan.plugins {
        for f in ENTRY_FILES {
            let rel = p.path(f);
            if !files.contains_key(&rel) && root.join(&rel).is_file() {
                report.diffs.push(FileDiff {
                    path: rel,
                    kind: DiffKind::Extra,
                    diff: String::new(),
                });
            }
        }
    }
    report.diffs.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(report)
}
