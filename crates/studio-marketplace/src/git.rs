//! Thin `git` helpers and the working-clone strategy.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// Run git in `dir`; trimmed stdout.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    Ok(studio_core::exec::run("git", args, Some(dir))?)
}

/// Uncommitted changes (tracked or untracked) in a clone.
pub fn is_dirty(dir: &Path) -> Result<bool> {
    Ok(!run(dir, &["status", "--porcelain"])?.is_empty())
}

pub fn current_branch(dir: &Path) -> Result<String> {
    run(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// `https://github.com/<owner>/<repo>.git`
pub fn github_clone_url(repo: &str) -> String {
    format!("https://github.com/{repo}.git")
}

/// A marketplace working tree to change. A temp clone is deleted on drop.
#[derive(Debug)]
pub struct Workspace {
    pub dir: PathBuf,
    /// Set when this is a throwaway shallow clone rather than the user's.
    pub temp: Option<tempfile::TempDir>,
}

impl Workspace {
    pub fn is_temporary(&self) -> bool {
        self.temp.is_some()
    }
}

/// Where a marketplace's tree is (or would be) on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Cloned(PathBuf),
    NotCloned(PathBuf),
}

impl Location {
    pub fn of(local: &Path) -> Self {
        if local.is_dir() {
            Location::Cloned(local.to_path_buf())
        } else {
            Location::NotCloned(local.to_path_buf())
        }
    }
    pub fn path(&self) -> &Path {
        match self {
            Location::Cloned(p) | Location::NotCloned(p) => p,
        }
    }
}

/// Open a tree to change: the local clone if there is one (refused when it
/// has uncommitted changes, or is on another branch), else — when
/// `allow_temp_clone` — a shallow clone of `clone_url` into a temp dir.
pub fn open_workspace(
    local: &Path,
    clone_url: &str,
    branch: &str,
    allow_temp_clone: bool,
) -> Result<Workspace> {
    if local.is_dir() {
        if !crate::tree::is_marketplace(local) {
            bail!("{} is not a marketplace (no catalogs)", local.display());
        }
        if is_dirty(local)? {
            bail!(
                "{} has uncommitted changes; commit or stash them first",
                local.display()
            );
        }
        let on = current_branch(local)?;
        if on != branch {
            bail!(
                "{} is on branch {on}, not {branch}; switch first",
                local.display()
            );
        }
        return Ok(Workspace {
            dir: local.to_path_buf(),
            temp: None,
        });
    }
    if !allow_temp_clone {
        bail!(
            "not cloned at {} (clone it there, or pass --publish to work in a temporary clone)",
            local.display()
        );
    }
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().join("repo");
    let dir_s = dir.to_string_lossy().to_string();
    run(
        tmp.path(),
        &[
            "clone", "-q", "--depth", "1", "--branch", branch, clone_url, &dir_s,
        ],
    )?;
    Ok(Workspace {
        dir,
        temp: Some(tmp),
    })
}
