//! A pattern pack on disk, and rendering its template and skill with an
//! instance's values.
//!
//! Template syntax is deliberately tiny: `{{name}}` substitutes a declared
//! variable, `{{"{{"}}` is a literal `{{`, and any other `{{` is an error, so a
//! typo or an unescaped brace pair can never ride silently into a generated
//! repo. Files that are not UTF-8 are copied byte for byte.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::manifest::{Manifest, ManifestError};

/// Template values: variable name → value (an instance's `pattern-values.toml`).
pub type Values = BTreeMap<String, String>;

/// Directories never part of a template, whatever is lying around in it.
pub const SKIP_DIRS: &[&str] = &["node_modules", ".wrangler", ".git"];

const LITERAL_OPEN: &str = "{{\"{{\"}}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFile {
    pub bytes: Vec<u8>,
    pub executable: bool,
}

/// A rendered tree: `/`-separated relative path → file.
pub type FileTree = BTreeMap<String, RenderedFile>;

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("{0} does not exist")]
    Missing(String),
    #[error(
        "{file}:{line}: malformed placeholder `{snippet}` (write {{{{\"{{{{\"}}}} for a literal {{{{)"
    )]
    Malformed {
        file: String,
        line: usize,
        snippet: String,
    },
    #[error("{file}:{line}: `{{{{{name}}}}}` is not declared in pattern.toml [variables]")]
    Undeclared {
        file: String,
        line: usize,
        name: String,
    },
    #[error("no value for {names:?} in the instance's pattern values (used by {file})")]
    MissingValues { file: String, names: Vec<String> },
    #[error("{0} already exists and is not empty")]
    NotEmpty(String),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> PackError + '_ {
    move |source| PackError::Io {
        path: path.display().to_string(),
        source,
    }
}

#[derive(Debug, Clone)]
pub struct Pack {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl Pack {
    /// Load `<dir>/pattern.toml` and check the template and skill dirs exist.
    pub fn load(dir: &Path) -> Result<Self, PackError> {
        let manifest = Manifest::load(dir)?;
        let pack = Self {
            dir: dir.to_path_buf(),
            manifest,
        };
        let t = pack.template_dir();
        if !t.is_dir() {
            return Err(PackError::Missing(t.display().to_string()));
        }
        Ok(pack)
    }

    pub fn name(&self) -> &str {
        &self.manifest.pack.name
    }

    pub fn version(&self) -> &str {
        &self.manifest.pack.version
    }

    pub fn template_dir(&self) -> PathBuf {
        self.dir.join(&self.manifest.pack.template)
    }

    pub fn skill_dir(&self) -> PathBuf {
        self.dir.join(&self.manifest.pack.skill)
    }

    /// The security files under conformance.
    pub fn security_files(&self) -> &[String] {
        &self.manifest.security.files
    }

    /// Render the template with `values`, in memory.
    pub fn render(&self, values: &Values) -> Result<FileTree, PackError> {
        render_dir(&self.template_dir(), &self.manifest.variables, values)
    }

    /// Render the template into `out`, which must be absent or empty.
    pub fn render_to(&self, values: &Values, out: &Path) -> Result<FileTree, PackError> {
        let tree = self.render(values)?;
        write_tree(&tree, out)?;
        Ok(tree)
    }

    /// Render the skill (SKILL.md + references/), in memory.
    pub fn render_skill(&self, values: &Values) -> Result<FileTree, PackError> {
        let dir = self.skill_dir();
        if !dir.is_dir() {
            return Err(PackError::Missing(dir.display().to_string()));
        }
        render_dir(&dir, &self.manifest.variables, values)
    }

    /// Render the skill into `dest` (e.g. a skills directory's
    /// `mcp-server-dev/`), replacing the files it renders. Files in `dest` the
    /// skill does not contain are left alone.
    pub fn install_skill(&self, dest: &Path, values: &Values) -> Result<FileTree, PackError> {
        let tree = self.render_skill(values)?;
        write_files(&tree, dest)?;
        Ok(tree)
    }

    /// Every variable used anywhere in the template and skill.
    pub fn used_variables(&self) -> Result<BTreeSet<String>, PackError> {
        let mut used = BTreeSet::new();
        for dir in [self.template_dir(), self.skill_dir()] {
            if !dir.is_dir() {
                continue;
            }
            for (rel, path) in walk(&dir)? {
                let bytes = std::fs::read(&path).map_err(io(&path))?;
                if let Ok(text) = String::from_utf8(bytes) {
                    used.extend(scan(&rel, &text)?.into_iter().map(|(name, _)| name));
                }
            }
        }
        Ok(used)
    }
}

/// Render `install_skill` without a loaded [`Pack`].
pub fn install_skill(pack_dir: &Path, dest: &Path, values: &Values) -> Result<FileTree, PackError> {
    Pack::load(pack_dir)?.install_skill(dest, values)
}

/// Every file under `root`, skipping [`SKIP_DIRS`], as (`/`-relative, path).
pub fn walk(root: &Path) -> Result<Vec<(String, PathBuf)>, PackError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(io(&dir))? {
            let entry = entry.map_err(io(&dir))?;
            let path = entry.path();
            let ft = entry.file_type().map_err(io(&path))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(path);
                }
            } else if ft.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((rel, path));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Placeholders in `text`: (name, line). Literal escapes are skipped.
fn scan(file: &str, text: &str) -> Result<Vec<(String, usize)>, PackError> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut offset = 0;
    while let Some(i) = rest.find("{{") {
        let at = offset + i;
        let line = text[..at].matches('\n').count() + 1;
        let tail = &rest[i..];
        if tail.starts_with(LITERAL_OPEN) {
            offset = at + LITERAL_OPEN.len();
            rest = &text[offset..];
            continue;
        }
        let name = tail[2..]
            .split("}}")
            .next()
            .filter(|_| tail[2..].contains("}}"));
        match name {
            Some(n) if is_var_name(n) => {
                out.push((n.to_string(), line));
                offset = at + 2 + n.len() + 2;
            }
            _ => {
                let snippet: String = tail.chars().take(24).take_while(|c| *c != '\n').collect();
                return Err(PackError::Malformed {
                    file: file.to_string(),
                    line,
                    snippet,
                });
            }
        }
        rest = &text[offset..];
    }
    Ok(out)
}

fn is_var_name(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_lowercase())
        && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Substitute placeholders in one file's text.
pub fn render_text(
    file: &str,
    text: &str,
    declared: &BTreeMap<String, String>,
    values: &Values,
) -> Result<String, PackError> {
    let used = scan(file, text)?;
    let mut missing = BTreeSet::new();
    for (name, line) in &used {
        if !declared.contains_key(name) {
            return Err(PackError::Undeclared {
                file: file.to_string(),
                line: *line,
                name: name.clone(),
            });
        }
        if !values.contains_key(name) {
            missing.insert(name.clone());
        }
    }
    if !missing.is_empty() {
        return Err(PackError::MissingValues {
            file: file.to_string(),
            names: missing.into_iter().collect(),
        });
    }
    if used.is_empty() && !text.contains(LITERAL_OPEN) {
        return Ok(text.to_string());
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(after) = tail.strip_prefix(LITERAL_OPEN) {
            out.push_str("{{");
            rest = after;
            continue;
        }
        // scan() validated every placeholder, so this split cannot fail.
        let name = tail[2..].split("}}").next().unwrap_or_default();
        out.push_str(&values[name]);
        rest = &tail[2 + name.len() + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn render_dir(
    dir: &Path,
    declared: &BTreeMap<String, String>,
    values: &Values,
) -> Result<FileTree, PackError> {
    let mut tree = FileTree::new();
    for (rel, path) in walk(dir)? {
        let bytes = std::fs::read(&path).map_err(io(&path))?;
        let executable = is_executable(&path);
        let bytes = match String::from_utf8(bytes) {
            Ok(text) => render_text(&rel, &text, declared, values)?.into_bytes(),
            Err(e) => e.into_bytes(),
        };
        tree.insert(rel, RenderedFile { bytes, executable });
    }
    Ok(tree)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_: &Path) -> bool {
    false
}

/// Write `tree` into `out`, which must be absent or empty.
pub fn write_tree(tree: &FileTree, out: &Path) -> Result<(), PackError> {
    if out.exists() {
        let mut entries = std::fs::read_dir(out).map_err(io(out))?;
        if entries.next().is_some() {
            return Err(PackError::NotEmpty(out.display().to_string()));
        }
    }
    write_files(tree, out)
}

fn write_files(tree: &FileTree, out: &Path) -> Result<(), PackError> {
    for (rel, f) in tree {
        let path = out.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io(parent))?;
        }
        std::fs::write(&path, &f.bytes).map_err(io(&path))?;
        #[cfg(unix)]
        if f.executable {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&path).map_err(io(&path))?.permissions();
            p.set_mode(p.mode() | 0o111);
            std::fs::set_permissions(&path, p).map_err(io(&path))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(names: &[&str]) -> BTreeMap<String, String> {
        names
            .iter()
            .map(|n| (n.to_string(), String::new()))
            .collect()
    }
    fn vals(pairs: &[(&str, &str)]) -> Values {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn substitutes_declared_variables() {
        let out = render_text(
            "f",
            "host = \"x.{{domain_suffix}}\"\n{{domain_suffix}}",
            &decl(&["domain_suffix"]),
            &vals(&[("domain_suffix", "mcp.acme.example")]),
        )
        .unwrap();
        assert_eq!(out, "host = \"x.mcp.acme.example\"\nmcp.acme.example");
    }

    #[test]
    fn literal_braces_are_escaped() {
        let out = render_text("f", "a {{\"{{\"}}b}} c", &decl(&[]), &vals(&[])).unwrap();
        assert_eq!(out, "a {{b}} c");
    }

    #[test]
    fn undeclared_and_missing_variables_fail() {
        let e = render_text("f", "\n{{nope}}", &decl(&[]), &vals(&[("nope", "x")])).unwrap_err();
        assert!(matches!(e, PackError::Undeclared { line: 2, .. }), "{e}");
        let e =
            render_text("f", "{{a}}{{b}}", &decl(&["a", "b"]), &vals(&[("a", "x")])).unwrap_err();
        assert!(
            matches!(&e, PackError::MissingValues { names, .. } if names == &["b"]),
            "{e}"
        );
    }

    #[test]
    fn malformed_placeholders_fail_loudly() {
        for t in ["{{ spaced }}", "{{Upper}}", "{{unterminated", "{{}}"] {
            let e = render_text(
                "f",
                t,
                &decl(&["spaced", "Upper", "unterminated"]),
                &vals(&[]),
            )
            .unwrap_err();
            assert!(matches!(e, PackError::Malformed { .. }), "{t}: {e}");
        }
    }

    #[test]
    fn text_without_placeholders_is_untouched() {
        let t = "{ a: { b: 1 } }\n";
        assert_eq!(render_text("f", t, &decl(&[]), &vals(&[])).unwrap(), t);
    }
}
