//! Read-only Okta Management API lookups through the configured helper tool
//! (`<tool> GET <path> [--org <org>]`). Fetched once per run, never per server.

use std::time::Duration;

use serde_json::Value;
use studio_core::config::OktaConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct OktaData {
    /// The configured policy rules that were found.
    pub rules: Vec<Rule>,
    /// Configured rule ids that no policy on the authorization server holds.
    pub missing_rules: Vec<String>,
    /// The interactive client's redirect URIs; `Err` when it couldn't be read,
    /// `Ok(None)` when no interactive client is configured.
    pub redirect_uris: Result<Option<Vec<String>>, String>,
}

pub async fn load(cfg: &OktaConfig) -> Result<OktaData, String> {
    let Some(api) = &cfg.admin_api else {
        return Err("identity.okta.admin_api not configured".into());
    };
    let call = |path: String| async move { get(&api.tool, api.org.as_deref(), &path).await };
    let as_id = &cfg.authorization_server;

    // Calls run one at a time: each helper call reads its credentials from
    // 1Password, and concurrent reads have been seen to time out.
    let rules_fut = async {
        let mut rules = Vec::new();
        if !cfg.policy_rules.is_empty() {
            let policies = call(format!("/api/v1/authorizationServers/{as_id}/policies")).await?;
            for p in policies.as_array().into_iter().flatten() {
                let Some(pid) = p.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                if cfg
                    .policy_rules
                    .iter()
                    .all(|r| rules.iter().any(|x: &Rule| &x.id == r))
                {
                    break;
                }
                let rs = call(format!(
                    "/api/v1/authorizationServers/{as_id}/policies/{pid}/rules"
                ))
                .await?;
                for r in rs.as_array().into_iter().flatten() {
                    let id = r.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                    if cfg.policy_rules.iter().any(|x| x == id) {
                        rules.push(Rule {
                            id: id.to_string(),
                            name: r
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or(id)
                                .to_string(),
                            active: r.get("status").and_then(|v| v.as_str()) != Some("INACTIVE"),
                            scopes: r
                                .pointer("/conditions/scopes/include")
                                .and_then(|v| v.as_array())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|s| s.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default(),
                        });
                    }
                }
            }
        }
        Ok::<_, String>(rules)
    };
    let app_fut = async {
        match &cfg.interactive_client_id {
            None => Ok(None),
            Some(id) => call(format!("/api/v1/apps/{id}")).await.map(|v| {
                Some(
                    v.pointer("/settings/oauthClient/redirect_uris")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                )
            }),
        }
    };
    let rules = rules_fut.await?;
    let redirect_uris = app_fut.await;
    let missing_rules = cfg
        .policy_rules
        .iter()
        .filter(|r| !rules.iter().any(|x| &x.id == *r))
        .cloned()
        .collect();

    Ok(OktaData {
        rules,
        missing_rules,
        redirect_uris,
    })
}

/// One GET through the helper, retried once (its credential read can be flaky
/// under load). The helper prints JSON on stdout; errors go to stderr.
async fn get(tool: &str, org: Option<&str>, path: &str) -> Result<Value, String> {
    let mut last = String::new();
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let mut c = tokio::process::Command::new(tool);
        c.args(["GET", path]);
        if let Some(o) = org {
            c.args(["--org", o]);
        }
        c.stdin(std::process::Stdio::null()).kill_on_drop(true);
        let out = match tokio::time::timeout(Duration::from_secs(60), c.output()).await {
            Err(_) => return Err(format!("`{tool} GET {path}` timed out")),
            Ok(Err(_)) => return Err(format!("`{tool}` is not installed or not on PATH")),
            Ok(Ok(o)) => o,
        };
        if out.status.success() {
            return serde_json::from_slice(&out.stdout)
                .map_err(|e| format!("`{tool} GET {path}`: not JSON: {e}"));
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("authorization timeout") {
            last = "1Password is locked (authorization timeout); unlock it and retry".into();
            continue;
        }
        last = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("failed")
            .chars()
            .take(200)
            .collect();
    }
    if last.starts_with("1Password") {
        return Err(last);
    }
    Err(format!("`{tool} GET {path}`: {last}"))
}
