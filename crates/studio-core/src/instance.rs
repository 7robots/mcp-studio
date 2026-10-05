//! Finding and loading an instance directory.
//!
//! An instance is a directory holding `studio.toml` (and, optionally, the
//! pattern values file and blessed conformance diffs). It is chosen by, in
//! order: `--instance PATH`, `$MCP_STUDIO_INSTANCE`, or `default_instance` in
//! the user config `${XDG_CONFIG_HOME:-~/.config}/mcp-studio/config.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::{Problem, StudioConfig, expand_tilde, repo_name, resolve_path};

pub const APP_NAME: &str = "mcp-studio";
pub const CONFIG_FILE: &str = "studio.toml";
pub const INSTANCE_ENV: &str = "MCP_STUDIO_INSTANCE";

#[derive(Debug, thiserror::Error)]
pub enum InstanceError {
    #[error(
        "no instance selected: pass --instance PATH, set {INSTANCE_ENV}, or set default_instance in {0}"
    )]
    NotSelected(String),
    #[error("{path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("{path}:{line}:{col}: {message}")]
    Parse {
        path: String,
        line: usize,
        col: usize,
        message: String,
    },
    #[error("{path} is invalid:\n{}", problems.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n"))]
    Invalid {
        path: String,
        problems: Vec<Problem>,
    },
}

/// The user-level config: which instance to use when none is named.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    pub default_instance: Option<PathBuf>,
}

pub fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| expand_tilde(Path::new("~/.config")))
        .join(APP_NAME)
}

pub fn cache_home() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| expand_tilde(Path::new("~/.cache")))
        .join(APP_NAME)
}

pub fn user_config_path() -> PathBuf {
    config_home().join("config.toml")
}

pub fn load_user_config() -> Result<UserConfig, InstanceError> {
    let path = user_config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_toml(&path, &text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserConfig::default()),
        Err(source) => Err(InstanceError::Read {
            path: path.display().to_string(),
            source,
        }),
    }
}

/// Resolve which instance directory to use.
pub fn locate(cli: Option<&Path>) -> Result<PathBuf, InstanceError> {
    if let Some(p) = cli {
        return Ok(expand_tilde(p));
    }
    if let Some(p) = std::env::var_os(INSTANCE_ENV).filter(|v| !v.is_empty()) {
        return Ok(expand_tilde(Path::new(&p)));
    }
    load_user_config()?
        .default_instance
        .map(|p| expand_tilde(&p))
        .ok_or_else(|| InstanceError::NotSelected(user_config_path().display().to_string()))
}

#[derive(Debug, Clone)]
pub struct Instance {
    pub root: PathBuf,
    pub config: StudioConfig,
}

impl Instance {
    /// Load and validate `<dir>/studio.toml`.
    pub fn load(dir: &Path) -> Result<Self, InstanceError> {
        let root = dir.to_path_buf();
        let path = root.join(CONFIG_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| InstanceError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let config: StudioConfig = parse_toml(&path, &text)?;
        let problems = config.validate();
        if !problems.is_empty() {
            return Err(InstanceError::Invalid {
                path: path.display().to_string(),
                problems,
            });
        }
        Ok(Self { root, config })
    }

    pub fn name(&self) -> &str {
        &self.config.instance.name
    }

    pub fn display_name(&self) -> &str {
        self.config
            .instance
            .display_name
            .as_deref()
            .unwrap_or(&self.config.instance.name)
    }

    /// A path from config, resolved against the instance directory.
    pub fn path(&self, p: &Path) -> PathBuf {
        resolve_path(&self.root, p)
    }

    /// Per-instance cache directory (not created).
    pub fn cache_dir(&self) -> PathBuf {
        cache_home().join(self.name())
    }

    /// Local checkout of `owner/repo`: an explicit override, else `<repos_dir>/<name>`.
    pub fn repo_dir(&self, repo: &str) -> PathBuf {
        let explicit = self
            .config
            .fleet_server(repo)
            .and_then(|s| s.local_path.clone())
            .or_else(|| {
                self.config
                    .marketplaces
                    .iter()
                    .find(|m| m.repo.eq_ignore_ascii_case(repo))
                    .and_then(|m| m.local_path.clone())
            });
        match explicit {
            Some(p) => self.path(&p),
            None => self
                .path(&self.config.fleet.repos_dir)
                .join(repo_name(repo)),
        }
    }

    pub fn pattern_dir(&self) -> Option<PathBuf> {
        self.config.pattern.as_ref().map(|p| self.path(&p.source))
    }

    pub fn conformance_dir(&self) -> Option<PathBuf> {
        self.config
            .pattern
            .as_ref()
            .map(|p| self.path(&p.conformance_dir))
    }

    /// The template values (`pattern-values.toml`): a flat string table.
    pub fn pattern_values(&self) -> Result<BTreeMap<String, String>, InstanceError> {
        let Some(p) = &self.config.pattern else {
            return Ok(BTreeMap::new());
        };
        let path = self.path(&p.values);
        let text = std::fs::read_to_string(&path).map_err(|source| InstanceError::Read {
            path: path.display().to_string(),
            source,
        })?;
        parse_toml(&path, &text)
    }
}

fn parse_toml<T: serde::de::DeserializeOwned>(path: &Path, text: &str) -> Result<T, InstanceError> {
    toml::from_str(text).map_err(|e| {
        let (line, col) = e.span().map(|s| line_col(text, s.start)).unwrap_or((0, 0));
        InstanceError::Parse {
            path: path.display().to_string(),
            line,
            col,
            message: e.message().to_string(),
        }
    })
}

fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn loads_the_example_instance() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/instance");
        let inst = Instance::load(&dir).unwrap();
        assert_eq!(inst.name(), "acme");
        let values = inst.pattern_values().unwrap();
        assert!(values.contains_key("okta_domain"));
        assert_eq!(
            inst.repo_dir("acme/weather-mcp-worker"),
            inst.path(Path::new("../repos")).join("weather-mcp-worker")
        );
    }

    #[test]
    fn parse_errors_carry_line_and_column() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            CONFIG_FILE,
            "[instance]\nname = \"x\"\n\n[github]\nacount = \"x\"\n",
        );
        let e = Instance::load(dir.path()).unwrap_err().to_string();
        assert!(e.contains("studio.toml:5:1"), "{e}");
        assert!(e.contains("acount"), "{e}");
    }

    #[test]
    fn invalid_configs_list_every_problem() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            CONFIG_FILE,
            "[instance]\nname = \"X Y\"\n[[gateway]]\nid = \"g\"\nurl = \"ftp://x\"\n",
        );
        let e = Instance::load(dir.path()).unwrap_err().to_string();
        assert!(
            e.contains("instance.name") && e.contains("gateway[0].url"),
            "{e}"
        );
    }

    #[test]
    fn cli_path_wins() {
        assert_eq!(
            locate(Some(Path::new("/x/y"))).unwrap(),
            PathBuf::from("/x/y")
        );
    }

    #[test]
    fn line_col_counts_from_one() {
        assert_eq!(line_col("ab\ncd", 0), (1, 1));
        assert_eq!(line_col("ab\ncd", 4), (2, 2));
    }
}
