//! Typed views of the gateway's tool results. Scripts get the raw JSON
//! (`servers --json`), so fields added gateway-side are never lost there.

use serde::{Deserialize, Serialize};

/// `whoami` result.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Whoami {
    pub authenticated: bool,
    pub auth_path: String,
    /// The OAuth client, not the user.
    pub client_id: Option<String>,
    pub sub: Option<String>,
    pub email: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// One `list_servers` entry.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub url: String,
    pub tools: u64,
    pub status: String,
    pub timeout_ms: u64,
    pub health: String,
    pub last_refresh_at: Option<u64>,
    pub last_error: Option<String>,
    pub call_failures: u64,
    pub last_call_error: Option<String>,
    pub last_call_at: Option<u64>,
    /// `read_only` or `read_write` (gateway migration 0018); absent from an
    /// older gateway, which means read-write.
    #[serde(default)]
    pub access: Option<String>,
    /// Space-separated scopes the gateway mints for this server.
    #[serde(default)]
    pub scopes: Option<String>,
    /// How the gateway authenticates to it (`okta_m2m`, `okta_user`, `none`,
    /// `bearer`, `api_key`); absent from a gateway older than Phase 55.
    #[serde(default)]
    pub auth_mode: Option<String>,
    /// okta_user only, and about the signed-in caller: `connected`,
    /// `needs_connect` or `unavailable` (gateway Phase 57).
    #[serde(default)]
    pub connection: Option<String>,
    /// Present when `list_servers` was asked for `include_tools`.
    #[serde(default)]
    pub tool_classes: Vec<ToolClass>,
    /// The version the server reported at its last probe; absent from an
    /// older gateway.
    #[serde(default)]
    pub server_version: Option<String>,
    /// Who registered it; absent from an older gateway.
    #[serde(default)]
    pub registered_by: Option<String>,
    /// When it was registered (the same unit as `last_refresh_at`); absent
    /// from an older gateway.
    #[serde(default)]
    pub registered_at: Option<u64>,
}

impl Server {
    /// The AUTH column: the mode, and for okta_user the caller's connection.
    pub fn auth_label(&self) -> String {
        match self.auth_mode.as_deref() {
            None => "-".into(),
            Some("okta_m2m") => "m2m".into(),
            Some("api_key") => "key".into(),
            Some("okta_user") => match self.connection.as_deref() {
                Some("connected") => "user:ok".into(),
                Some("needs_connect") => "user:connect".into(),
                Some("unavailable") => "user:n/a".into(),
                _ => "user".into(),
            },
            Some(other) => other.into(),
        }
    }

    pub fn read_only(&self) -> bool {
        self.access.as_deref() == Some("read_only")
    }

    /// Tools the gateway would let through if the server were read-only.
    pub fn read_tools(&self) -> usize {
        self.tool_classes
            .iter()
            .filter(|t| t.classification == "read")
            .count()
    }
}

/// One tool's classification as the gateway computes it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolClass {
    pub name: String,
    /// `read`, `write`, `destructive` or `unknown`.
    pub classification: String,
    /// `policy` (POLICY_CLASSIFY), `admin` (set_tool_class), `annotation`
    /// (destructiveHint) or `default`.
    pub source: String,
}

/// `usage_stats` result.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct UsageStats {
    pub window_days: u64,
    pub total: u64,
    pub failures: u64,
    pub by_tool: Vec<ToolStats>,
    #[serde(default)]
    pub by_failure_kind: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    pub recent_runs: Vec<RecentRun>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ToolStats {
    /// `server.tool`.
    pub tool: String,
    pub calls: u64,
    pub failures: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub avg_result_bytes: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct RecentRun {
    pub run_id: String,
    pub kind: String,
    pub actor: Option<String>,
    pub status: String,
    pub error_kind: Option<String>,
    pub duration_ms: Option<u64>,
    pub started_at: u64,
}

/// `policy_events` result.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct PolicyEvents {
    pub window_days: u64,
    /// The mode in force now; each event carries the mode it was decided under.
    pub mode: String,
    pub policy_version: String,
    pub policy_fingerprint: String,
    #[serde(default)]
    pub config_errors: Option<Vec<String>>,
    pub count: u64,
    pub events: Vec<PolicyEvent>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct PolicyEvent {
    pub ts: u64,
    pub run_id: Option<String>,
    pub actor: Option<String>,
    pub path: Option<String>,
    /// `server.tool`; absent for a run-level event.
    pub target: Option<String>,
    /// `flag` or `deny`.
    pub decision: String,
    /// `observed` or `blocked`.
    pub effect: String,
    pub mode_at_decision: Option<String>,
    #[serde(default)]
    pub rules: Vec<String>,
    pub reason: Option<String>,
    pub args: Option<serde_json::Value>,
}

/// How far back `policy_events` can look (the gateway's POLICY_RETENTION_DAYS).
pub const POLICY_RETENTION_DAYS: u64 = 7;

/// `list_servers` result.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ServerList {
    pub count: u64,
    pub servers: Vec<Server>,
}

/// Who is signed in and what they may do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub whoami: Whoami,
    /// The gateway registers admin tools only for a token carrying the admin
    /// scope whose `sub` is on its admin allowlist, so admin means
    /// both the scope and the admin probe tool in `tools/list`.
    pub admin: bool,
    /// Every tool `tools/list` offered this caller: what the gateway supports.
    pub tools: Vec<String>,
}

impl Identity {
    /// Whether the gateway offered this caller `tool`.
    pub fn offers(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }
}

/// The gateway's own build, from `health`; absent from an older gateway.
#[derive(Clone, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct BuildInfo {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub built_at: Option<String>,
}

impl BuildInfo {
    /// `1.4.0 (3f2a9c1)`, or as much of it as is known.
    pub fn label(&self) -> String {
        let sha = self.sha.as_deref().map(|s| &s[..s.len().min(7)]);
        match (self.version.is_empty(), sha) {
            (false, Some(sha)) => format!("{} ({sha})", self.version),
            (false, None) => self.version.clone(),
            (true, Some(sha)) => sha.to_string(),
            (true, None) => "?".into(),
        }
    }
}

/// `health` result. Every field is optional: the shape has grown over time.
#[derive(Clone, Debug, PartialEq, Default, Deserialize, Serialize)]
pub struct Health {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub build: Option<BuildInfo>,
}

/// One `list_scope_owners` entry: the server that claims a scope.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ScopeOwner {
    pub scope: String,
    pub server: String,
}

/// One `list_connections` entry: metadata about a stored downstream
/// credential (never the credential itself).
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Connection {
    pub subject: String,
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub version: Option<serde_json::Value>,
    #[serde(default)]
    pub key_id: Option<String>,
    #[serde(default)]
    pub updated_at: Option<serde_json::Value>,
    #[serde(default)]
    pub expires_at: Option<serde_json::Value>,
}

/// A JSON value for a table cell: strings bare, null as `-`.
pub fn cell(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => "-".into(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// A timestamp field as an age (`5m ago`, `in 2h`) when it is epoch seconds
/// or milliseconds, else as given.
pub fn when(value: Option<&serde_json::Value>, now: u64) -> String {
    let Some(n) = value.and_then(serde_json::Value::as_u64) else {
        return cell(value);
    };
    // Milliseconds are 1000x anything plausible in seconds.
    let secs = if n > 100_000_000_000 { n / 1000 } else { n };
    if secs > now {
        format!("in {}", crate::util::span(secs - now))
    } else {
        format!("{} ago", crate::util::span(now - secs))
    }
}

/// A tool result for a person: the text content (pretty-printed when it is
/// JSON), else the whole result pretty-printed.
pub fn render_result(result: &serde_json::Value) -> String {
    let texts: Vec<String> = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|c| c.get("text").and_then(serde_json::Value::as_str))
                .map(
                    |text| match serde_json::from_str::<serde_json::Value>(text) {
                        Ok(v @ (serde_json::Value::Object(_) | serde_json::Value::Array(_))) => {
                            serde_json::to_string_pretty(&v).unwrap_or_else(|_| text.to_string())
                        }
                        _ => text.to_string(),
                    },
                )
                .collect()
        })
        .unwrap_or_default();
    if !texts.is_empty() {
        return texts.join("\n\n");
    }
    let shown = result.get("structuredContent").unwrap_or(result);
    serde_json::to_string_pretty(shown).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn older_servers_decode_without_the_new_fields() {
        let s: Server = serde_json::from_value(json!({
            "id": "a", "name": "A", "description": null, "url": "https://a.example/mcp", "tools": 1,
            "status": "active", "timeout_ms": 1000, "health": "ok", "last_refresh_at": null,
            "last_error": null, "call_failures": 0, "last_call_error": null, "last_call_at": null
        }))
        .unwrap();
        assert_eq!(
            (s.server_version, s.registered_by, s.registered_at),
            (None, None, None)
        );
    }

    #[test]
    fn health_and_build_labels() {
        let h: Health = serde_json::from_value(json!({"status": "ok", "servers": 3})).unwrap();
        assert!(h.build.is_none());
        let h: Health = serde_json::from_value(
            json!({"build": {"version": "1.2.0", "sha": "abcdef0123", "built_at": null}}),
        )
        .unwrap();
        assert_eq!(h.build.unwrap().label(), "1.2.0 (abcdef0)");
        let b = BuildInfo {
            version: String::new(),
            sha: None,
            built_at: None,
        };
        assert_eq!(b.label(), "?");
    }

    #[test]
    fn when_reads_seconds_milliseconds_and_text() {
        let now = 1_800_000_000;
        assert_eq!(when(Some(&json!(now - 120)), now), "2m ago");
        assert_eq!(when(Some(&json!((now + 7200) * 1000)), now), "in 2h");
        assert_eq!(when(Some(&json!("2026-10-01")), now), "2026-10-01");
        assert_eq!(when(None, now), "-");
        assert_eq!(cell(Some(&json!(3))), "3");
    }

    #[test]
    fn results_render_their_text_pretty() {
        let r = json!({"content": [{"type": "text", "text": "{\"a\":1}"}, {"type": "text", "text": "plain"}]});
        assert_eq!(render_result(&r), "{\n  \"a\": 1\n}\n\nplain");
        let r = json!({"structuredContent": {"b": 2}, "content": []});
        assert_eq!(render_result(&r), "{\n  \"b\": 2\n}");
    }
}
