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
}
