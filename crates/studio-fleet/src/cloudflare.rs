//! Read-only Cloudflare API calls: accounts, Workers scripts, Workers custom
//! domains, and Workers Builds.
//!
//! Endpoints (all GET, base `https://api.cloudflare.com/client/v4`):
//! - `/accounts` — to resolve the account id when config says `"resolve"`
//! - `/accounts/{account}/workers/scripts` — script names (`id`) and tags
//! - `/accounts/{account}/workers/domains` — custom domain → service mapping
//! - `/accounts/{account}/builds/workers/{tag}/builds` — Workers Builds for
//!   a script, keyed by its tag (`external_script_id`)

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use studio_core::Secret;

use crate::github::short_err;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Script {
    /// The script name.
    pub id: String,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub modified_on: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Domain {
    pub hostname: String,
    pub service: String,
    #[serde(default)]
    pub environment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Build {
    #[serde(default)]
    pub build_uuid: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub build_outcome: Option<String>,
    #[serde(default)]
    pub created_on: Option<String>,
    #[serde(default)]
    pub stopped_on: Option<String>,
    #[serde(default)]
    pub build_trigger_metadata: Option<TriggerMeta>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct TriggerMeta {
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub commit_hash: Option<String>,
}

#[derive(Clone)]
pub struct CloudflareApi {
    base: String,
    token: Secret,
    client: reqwest::Client,
}

/// Everything fetched once per run.
#[derive(Debug, Clone)]
pub struct CfAccount {
    pub account_id: String,
    pub scripts: BTreeMap<String, Script>,
    pub domains: Vec<Domain>,
}

impl CloudflareApi {
    pub fn new(base: &str, token: Secret, client: reqwest::Client) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            client,
        }
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        let resp = self
            .client
            .get(format!("{}{path}", self.base))
            .bearer_auth(self.token.expose())
            .send()
            .await
            .map_err(|e| format!("GET {path}: {}", short_err(&e)))?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let ok = body
            .get("success")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);
        if !status.is_success() || !ok {
            let msg = body
                .pointer("/errors/0/message")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            return Err(format!("GET {path}: HTTP {} {msg}", status.as_u16())
                .trim()
                .to_string());
        }
        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn load(&self, account_id: &str) -> Result<CfAccount, String> {
        let account_id = if account_id == "resolve" {
            let v = self.get("/accounts?per_page=50").await?;
            let ids: Vec<String> = v
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.get("id")?.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            match ids.as_slice() {
                [one] => one.clone(),
                [] => return Err("the token can see no accounts".into()),
                _ => {
                    return Err(format!(
                        "the token can see {} accounts; set cloudflare.account_id",
                        ids.len()
                    ));
                }
            }
        } else {
            account_id.to_string()
        };
        let scripts: Vec<Script> = serde_json::from_value(
            self.get(&format!("/accounts/{account_id}/workers/scripts"))
                .await?,
        )
        .map_err(|e| format!("workers/scripts: {e}"))?;
        let domains: Vec<Domain> = serde_json::from_value(
            self.get(&format!("/accounts/{account_id}/workers/domains"))
                .await?,
        )
        .map_err(|e| format!("workers/domains: {e}"))?;
        Ok(CfAccount {
            account_id,
            scripts: scripts.into_iter().map(|s| (s.id.clone(), s)).collect(),
            domains,
        })
    }

    /// Recent builds for a script tag, newest first.
    pub async fn builds(&self, account_id: &str, tag: &str) -> Result<Vec<Build>, String> {
        let v = self
            .get(&format!(
                "/accounts/{account_id}/builds/workers/{tag}/builds?page=1&per_page=10"
            ))
            .await?;
        let mut b: Vec<Build> = serde_json::from_value(v).map_err(|e| format!("builds: {e}"))?;
        b.sort_by_key(|x| {
            std::cmp::Reverse(
                x.created_on
                    .as_deref()
                    .and_then(crate::time::parse_rfc3339)
                    .unwrap_or(0),
            )
        });
        Ok(b)
    }
}
