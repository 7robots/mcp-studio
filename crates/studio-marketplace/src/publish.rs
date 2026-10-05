//! Publish a committed change: push to the branch, or open a PR.
//!
//! With `publish = "auto"` the decision is made *before* pushing, from the
//! branch's protection status: a protected branch gets a branch + PR even
//! when the pusher could bypass it (`enforce_admins` is often off, and
//! skipping another team's review is not Studio's call). A push rejected as
//! protected also falls back to a PR.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use studio_core::Secret;
use studio_core::config::PublishMode;

use crate::git;
use crate::ops::{ChangeSet, Commit};

pub const GITHUB_API: &str = "https://api.github.com";

/// Read-only GitHub REST access with one account's token.
pub struct GithubApi {
    base: String,
    token: Secret,
    http: reqwest::Client,
}

impl GithubApi {
    pub fn new(token: Secret) -> Self {
        Self::with_base(GITHUB_API, token)
    }

    /// A different API root (GitHub Enterprise, or a test server).
    pub fn with_base(base: &str, token: Secret) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::new(),
        }
    }

    /// Is `branch` of `owner/repo` protected? Uses the branch endpoint, which
    /// needs only read access (the protection endpoint needs admin).
    pub async fn branch_protected(&self, repo: &str, branch: &str) -> Result<bool> {
        let url = format!("{}/repos/{repo}/branches/{branch}", self.base);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(self.token.expose())
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "mcp-studio")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .with_context(|| format!("GET repos/{repo}/branches/{branch}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("GET repos/{repo}/branches/{branch}: HTTP {status}");
        }
        let body: serde_json::Value = resp.json().await?;
        Ok(body["protected"].as_bool().unwrap_or(false))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "route")]
pub enum Route {
    Push,
    PullRequest { reason: String },
}

/// Push or PR, per the marketplace's `publish` mode.
pub async fn decide(
    mode: PublishMode,
    api: Option<&GithubApi>,
    repo: &str,
    branch: &str,
) -> Result<Route> {
    match mode {
        PublishMode::Push => Ok(Route::Push),
        PublishMode::Pr => Ok(Route::PullRequest {
            reason: "publish = \"pr\"".into(),
        }),
        PublishMode::Auto => {
            let api = api
                .context("publish = \"auto\" needs GitHub API access to read branch protection")?;
            if api.branch_protected(repo, branch).await? {
                Ok(Route::PullRequest {
                    reason: format!("{branch} is protected"),
                })
            } else {
                Ok(Route::Push)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "published")]
pub enum Published {
    Pushed { branch: String },
    PullRequest { branch: String, url: String },
}

/// How to run `gh` for PRs.
pub struct Gh {
    pub program: String,
    pub token: Option<Secret>,
}

impl Gh {
    pub fn new(token: Option<Secret>) -> Self {
        Self {
            program: "gh".into(),
            token,
        }
    }

    fn pr_create(
        &self,
        dir: &Path,
        repo: &str,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let mut c = std::process::Command::new(&self.program);
        c.current_dir(dir).args([
            "pr", "create", "--repo", repo, "--base", base, "--head", head, "--title", title,
            "--body", body,
        ]);
        if let Some(t) = &self.token {
            c.env("GH_TOKEN", t.expose());
        }
        let out = c
            .output()
            .with_context(|| format!("`{}` is not installed or not on PATH", self.program))?;
        if !out.status.success() {
            bail!(
                "gh pr create failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

fn looks_protected(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("protected branch") || e.contains("gh006") || e.contains("required status check")
}

/// Publish a commit made by [`crate::ops::commit`] in `dir` (whose `origin`
/// is the marketplace) along `route`.
///
/// For a PR the commit moves to a `<verb>/<slug>` branch and the base branch
/// is put back where it was, so the local clone does not run ahead of origin.
pub fn publish(
    dir: &Path,
    base_branch: &str,
    repo: &str,
    cs: &ChangeSet,
    commit: &Commit,
    route: &Route,
    gh: &Gh,
) -> Result<Published> {
    if *route == Route::Push {
        let refspec = format!("HEAD:refs/heads/{base_branch}");
        match git::run(dir, &["push", "-q", "origin", &refspec]) {
            Ok(_) => {
                return Ok(Published::Pushed {
                    branch: base_branch.into(),
                });
            }
            Err(e) if looks_protected(&e.to_string()) => {} // fall through to a PR
            Err(e) => return Err(e),
        }
    }
    let branch = cs.branch();
    git::run(dir, &["branch", "-f", &branch, &commit.head])?;
    if let Some(base) = &commit.base {
        git::run(dir, &["reset", "-q", "--keep", base])?;
    }
    git::run(
        dir,
        &[
            "push",
            "-q",
            "-u",
            "origin",
            &format!("{branch}:refs/heads/{branch}"),
        ],
    )?;
    let body = if cs.notes.is_empty() {
        format!("{} via mcp-studio.", cs.title())
    } else {
        cs.notes.join("\n\n")
    };
    let url = gh.pr_create(dir, repo, base_branch, &branch, &cs.title(), &body)?;
    Ok(Published::PullRequest { branch, url })
}
