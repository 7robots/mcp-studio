//! Reading a marketplace working tree.

use std::path::{Path, PathBuf};

use crate::generate::SERVER_YAML;
use crate::model::PluginDir;
use crate::ojson::Json;
use crate::yaml;

/// Top-level directories that hold entries.
pub const ENTRY_ROOTS: &[&str] = &["servers", "plugins"];

/// What a scan of `servers/*/` and `plugins/*/` found.
#[derive(Debug, Default)]
pub struct Scan {
    pub plugins: Vec<PluginDir>,
    /// `(repo-relative server.yaml path, problem)` for files that didn't load.
    pub broken: Vec<(String, String)>,
    /// Entry directories with no `server.yaml` (hand-managed, not Studio's).
    pub unmanaged: Vec<String>,
}

impl Scan {
    pub fn find(&self, slug: &str) -> Option<&PluginDir> {
        self.plugins.iter().find(|p| p.slug == slug)
    }
}

/// Is this directory a marketplace (has either catalog)?
pub fn is_marketplace(dir: &Path) -> bool {
    dir.join(crate::CLAUDE_CATALOG).is_file() || dir.join(crate::CODEX_CATALOG).is_file()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Load one entry directory (`rel_dir` like `servers/acme-search`).
pub fn load_plugin(root: &Path, rel_dir: &str) -> Result<PluginDir, String> {
    let dir = root.join(rel_dir);
    let text = std::fs::read_to_string(dir.join(SERVER_YAML)).map_err(|e| e.to_string())?;
    let entry = yaml::parse_entry(&text).map_err(|e| e.to_string())?;
    let mut p = PluginDir::new(entry, rel_dir);
    p.has_skills = dir.join("skills").is_dir();
    Ok(p)
}

/// Scan every entry directory, sorted by slug.
pub fn scan(root: &Path) -> Scan {
    let mut s = Scan::default();
    for top in ENTRY_ROOTS {
        for d in subdirs(&root.join(top)) {
            let name = d
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let rel = format!("{top}/{name}");
            if !d.join(SERVER_YAML).is_file() {
                s.unmanaged.push(rel);
                continue;
            }
            match load_plugin(root, &rel) {
                Ok(p) => s.plugins.push(p),
                Err(e) => s.broken.push((format!("{rel}/{SERVER_YAML}"), e)),
            }
        }
    }
    s.plugins.sort_by(|a, b| a.slug.cmp(&b.slug));
    s
}

/// Read and parse a JSON file in the tree; `Ok(None)` when it's absent.
pub fn read_json(root: &Path, rel: &str) -> Result<Option<Json>, String> {
    match std::fs::read_to_string(root.join(rel)) {
        Ok(t) => Json::parse(&t)
            .map(Some)
            .map_err(|e| format!("not valid JSON: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Every file under `rel_dir`, repo-relative, sorted.
pub fn files_under(root: &Path, rel_dir: &str) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.filter_map(Result::ok) {
                let p = e.path();
                if p.is_dir() {
                    walk(base, &p, out);
                } else if let Ok(rel) = p.strip_prefix(base) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &root.join(rel_dir), &mut out);
    out.sort();
    out
}
