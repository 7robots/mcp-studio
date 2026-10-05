//! Read-only GitHub REST calls: repo metadata, branch heads, org listings.

use serde::Deserialize;
use serde_json::Value;
use studio_core::Secret;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RepoInfo {
    pub full_name: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub default_branch: String,
    pub pushed_at: Option<String>,
}

#[derive(Clone)]
pub struct GithubApi {
    base: String,
    token: Option<Secret>,
    client: reqwest::Client,
}

impl GithubApi {
    pub fn new(base: &str, token: Option<Secret>, client: reqwest::Client) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            client,
        }
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// GET a path; `Ok(None)` on 404.
    async fn get(&self, path: &str) -> Result<Option<Value>, String> {
        let url = format!("{}{path}", self.base);
        let mut req = self
            .client
            .get(&url)
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28");
        if let Some(t) = &self.token {
            req = req.bearer_auth(t.expose());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("GET {path}: {}", short_err(&e)))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
                .unwrap_or_default();
            return Err(format!("GET {path}: HTTP {} {msg}", status.as_u16())
                .trim()
                .to_string());
        }
        resp.json::<Value>()
            .await
            .map(Some)
            .map_err(|e| format!("GET {path}: bad JSON: {e}"))
    }

    pub async fn repo(&self, repo: &str) -> Result<Option<RepoInfo>, String> {
        match self.get(&format!("/repos/{repo}")).await? {
            None => Ok(None),
            Some(v) => serde_json::from_value(v)
                .map(Some)
                .map_err(|e| format!("repo {repo}: {e}")),
        }
    }

    /// The commit sha at the tip of `branch`.
    pub async fn branch_head(&self, repo: &str, branch: &str) -> Result<Option<String>, String> {
        Ok(self
            .get(&format!("/repos/{repo}/branches/{branch}"))
            .await?
            .and_then(|v| v.pointer("/commit/sha")?.as_str().map(String::from)))
    }

    pub async fn has_file(&self, repo: &str, path: &str, git_ref: &str) -> Result<bool, String> {
        Ok(self
            .get(&format!("/repos/{repo}/contents/{path}?ref={git_ref}"))
            .await?
            .is_some())
    }

    /// Every repo owned by `owner`: an org, the authenticated user, or another user.
    pub async fn owner_repos(&self, owner: &str) -> Result<Vec<RepoInfo>, String> {
        if let Some(r) = self.paged(&format!("/orgs/{owner}/repos?type=all")).await? {
            return Ok(r);
        }
        let me = self
            .get("/user")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.get("login")?.as_str().map(String::from));
        let path = if me.is_some_and(|m| m.eq_ignore_ascii_case(owner)) {
            "/user/repos?affiliation=owner".to_string()
        } else {
            format!("/users/{owner}/repos?type=owner")
        };
        Ok(self.paged(&path).await?.unwrap_or_default())
    }

    async fn paged(&self, path: &str) -> Result<Option<Vec<RepoInfo>>, String> {
        let mut out = Vec::new();
        for page in 1..=20 {
            let Some(v) = self
                .get(&format!("{path}&per_page=100&page={page}"))
                .await?
            else {
                return Ok(if page == 1 { None } else { Some(out) });
            };
            let batch: Vec<RepoInfo> =
                serde_json::from_value(v).map_err(|e| format!("{path}: {e}"))?;
            let n = batch.len();
            out.extend(batch);
            if n < 100 {
                break;
            }
        }
        Ok(Some(out))
    }
}

/// A reqwest error without its URL query (which never carries secrets here,
/// but keeps messages short).
pub(crate) fn short_err(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "connection failed".into()
    } else {
        let s = e.to_string();
        // drop the URL reqwest appends
        s.split(" for url (").next().unwrap_or(&s).to_string()
    }
}
