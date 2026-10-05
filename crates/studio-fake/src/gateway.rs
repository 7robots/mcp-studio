//! An in-process stand-in for the gateway, for the tests and the demo.
//!
//! It keeps the real gateway's contract where the client depends on it: RFC
//! 8414 metadata, DCR for public clients with loopback any-port redirect
//! matching, `/authorize` with mandatory PKCE S256 (consent is automatic; a
//! refused admin login is a text page, as the real one shows), `/token` with code
//! and rotating refresh grants plus revocation, and `POST /mcp` with the Accept
//! check, 401 `invalid_token`, the 403 `insufficient_scope` step-up driven by
//! `Mcp-Method`/`Mcp-Name`, SSE framing, `isError` tool results, and admin
//! tools listed only for an allowlisted `sub` holding `gateway:admin`.
//! Servers persist in an optional JSON state file; OAuth state is in memory.
//!
//! [`Options::legacy`] makes it a gateway from before `update_server`,
//! `list_scope_owners`, `list_connections`, the servers' version and
//! registration fields and `health`'s build, for compatibility tests.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use url::Url;

use studio_gateway::oauth::challenge_for;
use studio_gateway::util::{now_unix, random_token, write_atomic};

/// The default access and admin scopes of a `[[gateway]]` table.
pub const ACCESS_SCOPE: &str = "mcp-access";
pub const ADMIN_SCOPE: &str = "gateway:admin";

pub const PUBLIC_TOOLS: &[&str] = &[
    "whoami",
    "health",
    "list_servers",
    "search_tools",
    "get_api",
    "call_tool",
];
pub const ADMIN_TOOLS: &[&str] = &[
    "policy_events",
    "refresh_server",
    "usage_stats",
    "register_server",
    "unregister_server",
    "set_server_timeout",
    "set_server_status",
    "set_server_access",
    "set_tool_class",
    "revoke_user_access",
];
/// Admin tools a [`Options::legacy`] gateway does not have.
pub const NEW_ADMIN_TOOLS: &[&str] = &["update_server", "list_scope_owners", "list_connections"];

/// The fake gateway's build, as `health` reports it.
pub const BUILD_VERSION: &str = "1.8.0";
pub const BUILD_SHA: &str = "3f2a9c1e7d";

/// A seeded user other than the signed-in one, with grants and a connection
/// for `revoke_user_access` to remove.
pub const OTHER_SUB: &str = "00ufakereader";

#[derive(Clone, Debug)]
pub struct Options {
    pub sub: String,
    pub email: String,
    pub name: String,
    /// The identity provider's policy: whether this user may be granted the
    /// admin scope. When false, a login requesting it ends on an error page,
    /// as for a user outside the admin group.
    pub idp_grants_admin: bool,
    /// The gateway's admin subject allowlist.
    pub admin_subs: Vec<String>,
    pub access_ttl: u64,
    /// Frame `/mcp` answers as `text/event-stream` (the gateway's usual reply).
    pub sse: bool,
    pub state_path: Option<PathBuf>,
    /// Behave like a gateway from before `update_server` and friends: those
    /// tools are absent and the newer fields are left out.
    pub legacy: bool,
}

impl Default for Options {
    fn default() -> Self {
        let sub = "00ufakeadmin".to_string();
        Options {
            admin_subs: vec![sub.clone()],
            sub,
            email: "admin@example.org".into(),
            name: "Fake Admin".into(),
            idp_grants_admin: true,
            access_ttl: 3600,
            sse: true,
            state_path: None,
            legacy: false,
        }
    }
}

struct Access {
    scope: String,
    client_id: String,
    expires_at: u64,
    grant: usize,
}

struct Grant {
    scope: String,
    client_id: String,
    current: String,
    previous: Option<String>,
    revoked: bool,
}

struct CodeGrant {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    scope: String,
}

struct Inner {
    opts: Options,
    base: Url,
    clients: HashMap<String, Vec<String>>,
    codes: HashMap<String, CodeGrant>,
    access: HashMap<String, Access>,
    grants: Vec<Grant>,
    servers: Vec<Value>,
    /// `list_connections` rows.
    connections: Vec<Value>,
    /// Per-user OAuth state `revoke_user_access` removes, beyond the real
    /// grants of the signed-in user: (grants, clients, login tokens).
    users: HashMap<String, (u64, u64, u64)>,
    log: Vec<String>,
}

impl Inner {
    fn offers(&self, tool: &str) -> bool {
        PUBLIC_TOOLS.contains(&tool)
            || ADMIN_TOOLS.contains(&tool)
            || (!self.opts.legacy && NEW_ADMIN_TOOLS.contains(&tool))
    }

    fn admin_tool(&self, tool: &str) -> bool {
        ADMIN_TOOLS.contains(&tool) || (!self.opts.legacy && NEW_ADMIN_TOOLS.contains(&tool))
    }
}

type Shared = Arc<Mutex<Inner>>;

/// A running fake on `127.0.0.1` at an OS-chosen port.
pub struct FakeGateway {
    pub base: Url,
    shared: Shared,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeGateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeGateway {
    pub async fn start(opts: Options) -> Result<FakeGateway> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let (base, shared, app) = build(&listener, opts)?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(FakeGateway { base, shared, task })
    }

    /// Makes every access token expired at the gateway (not in the client's
    /// view), forcing the 401-refresh-retry path.
    pub fn expire_access_tokens(&self) {
        for access in self.shared.lock().unwrap().access.values_mut() {
            access.expires_at = 0;
        }
    }

    /// Revokes every grant, as a new login from elsewhere or a deleted client would.
    pub fn revoke_all_grants(&self) {
        let mut inner = self.shared.lock().unwrap();
        inner.grants.iter_mut().for_each(|g| g.revoked = true);
        inner.access.clear();
    }

    /// Issues a pair directly, for tests that need a token the login would not
    /// produce (a reader token without `gateway:admin`).
    pub fn mint(&self, scope: &str, client_id: &str) -> studio_gateway::Tokens {
        let mut inner = self.shared.lock().unwrap();
        let (access, refresh, expires_in) = issue(&mut inner, scope, client_id);
        studio_gateway::Tokens {
            access_token: access,
            refresh_token: Some(refresh),
            expires_at: now_unix() + expires_in,
            scope: scope.to_string(),
            client_id: client_id.to_string(),
            gateway: Some(self.base.to_string()),
        }
    }

    /// Requests seen, in order: `token:<grant_type>`, `mcp:<method>[:<tool>]`,
    /// `mcp:401` for a rejected bearer.
    pub fn log(&self) -> Vec<String> {
        self.shared.lock().unwrap().log.clone()
    }
}

/// Serves on `listener` until the process ends (the `studio-fake-gateway` binary).
pub async fn serve(listener: TcpListener, opts: Options) -> Result<()> {
    let (_, _, app) = build(&listener, opts)?;
    axum::serve(listener, app).await.context("serving")
}

fn build(listener: &TcpListener, opts: Options) -> Result<(Url, Shared, Router)> {
    let addr: SocketAddr = listener.local_addr()?;
    let base = Url::parse(&format!("http://{addr}/"))?;
    let servers = load_servers(opts.state_path.as_ref())?;
    let shared = Arc::new(Mutex::new(Inner {
        opts,
        base: base.clone(),
        clients: HashMap::new(),
        codes: HashMap::new(),
        access: HashMap::new(),
        grants: Vec::new(),
        servers,
        connections: seed_connections(),
        users: HashMap::from([(OTHER_SUB.to_string(), (2, 1, 1))]),
        log: Vec::new(),
    }));
    let app = Router::new()
        .route("/.well-known/oauth-authorization-server", get(metadata))
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(resource_metadata),
        )
        .route("/register", post(register))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .route("/mcp", post(mcp))
        .with_state(shared.clone());
    Ok((base, shared, app))
}

fn load_servers(path: Option<&PathBuf>) -> Result<Vec<Value>> {
    if let Some(path) = path
        && path.exists()
    {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let state: Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        return Ok(state
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default());
    }
    let servers = seed_servers();
    if let Some(path) = path {
        write_atomic(
            path,
            &serde_json::to_string_pretty(&json!({ "servers": servers }))?,
            false,
        )?;
    }
    Ok(servers)
}

/// A spread of states the screens have to render: healthy, degraded,
/// disabled, quarantined; one read-only server with a partly classified
/// catalogue.
pub fn seed_servers() -> Vec<Value> {
    let now = now_unix();
    let tools = |list: &[(&str, &str, &str)]| -> Value {
        list.iter().map(|(name, class, source)| json!({"name": name, "classification": class, "source": source})).collect()
    };
    let mut servers = vec![
        json!({"id": "notes", "name": "Notes", "description": "Read and write notes", "url": "https://notes.mcp.example.org/mcp",
               "status": "active", "timeout_ms": 30000, "health": "ok", "last_refresh_at": now - 3600,
               "last_error": null, "call_failures": 0, "last_call_error": null, "last_call_at": now - 120,
               "access": "read_only", "scopes": "notes:read notes:write", "auth_mode": "okta_m2m",
               "tool_classes": tools(&[("notes_search", "read", "admin"), ("notes_read", "read", "admin"),
                                       ("notes_create", "write", "admin"), ("notes_trash", "destructive", "annotation"),
                                       ("notes_tags", "unknown", "default")])}),
        json!({"id": "tasks", "name": "Tasks", "description": "Task lists", "url": "https://tasks.mcp.example.org/mcp",
               "status": "active", "timeout_ms": 15000, "health": "degraded", "last_refresh_at": now - 7200,
               "last_error": "refresh: upstream answered HTTP 502", "call_failures": 0, "last_call_error": null, "last_call_at": now - 900,
               "access": "read_write", "scopes": "tasks:all", "auth_mode": "okta_user", "connection": "needs_connect",
               "tool_classes": tools(&[("tasks_list", "unknown", "default"), ("tasks_add", "unknown", "default")])}),
        json!({"id": "weather", "name": "Weather", "description": null, "url": "https://weather.example.com/mcp",
               "status": "disabled", "timeout_ms": 10000, "health": "failing", "last_refresh_at": now - 86400 * 3,
               "last_error": null, "call_failures": 0, "last_call_error": null, "last_call_at": null,
               "access": "read_write", "scopes": null,
               "tool_classes": tools(&[("forecast", "read", "policy")])}),
        json!({"id": "scratch", "name": "Scratch", "description": "Experimental", "url": "https://scratch.example.com/mcp",
               "status": "quarantined", "timeout_ms": 30000, "health": "failing", "last_refresh_at": now - 600,
               "last_error": "scope drift: new scope files:write", "call_failures": 4, "last_call_error": "timeout after 30000ms", "last_call_at": now - 650,
               "access": "read_write", "scopes": "scratch:call", "_advertised": "files:write scratch:call",
               "tool_classes": tools(&[("scratch_echo", "unknown", "default"), ("scratch_wipe", "destructive", "annotation")])}),
    ];
    for (i, server) in servers.iter_mut().enumerate() {
        server["tools"] = json!(server["tool_classes"].as_array().map_or(0, Vec::len));
        server["server_version"] = if i == 2 {
            Value::Null
        } else {
            json!(format!("0.{}.0", i + 3))
        };
        server["registered_by"] = json!("admin@example.org");
        server["registered_at"] = json!(now - 86400 * (30 - i as u64));
    }
    servers
}

/// `list_connections` rows: metadata only, never a credential.
pub fn seed_connections() -> Vec<Value> {
    let now = now_unix();
    vec![
        json!({"subject": "00ufakeadmin", "issuer": "https://idp.example.org/oauth2/default", "kind": "okta_user",
               "version": 3, "key_id": "k-2026-09", "updated_at": now - 3600, "expires_at": now + 86400 * 30}),
        json!({"subject": OTHER_SUB, "issuer": "https://idp.example.org/oauth2/default", "kind": "okta_user",
               "version": 1, "key_id": "k-2026-09", "updated_at": now - 86400 * 2, "expires_at": now + 86400 * 5}),
        json!({"subject": "tasks", "issuer": "https://tasks.mcp.example.org", "kind": "api_key",
               "version": 2, "key_id": "k-2026-08", "updated_at": now - 86400 * 20, "expires_at": null}),
    ]
}

/// `usage_stats`, in the gateway's shape.
fn usage(days: u64, recent: usize) -> Value {
    let now = now_unix();
    let runs: Vec<Value> = (0..12u64)
        .map(|i| {
            let failed = i % 5 == 3;
            json!({"run_id": format!("run-{i:04}"), "kind": if i % 3 == 0 { "run" } else { "call_tool" },
                   "actor": "00ufakeadmin", "status": if failed { "error" } else { "ok" },
                   "error_kind": if failed { json!("timeout") } else { Value::Null },
                   "duration_ms": 120 + i * 37, "started_at": now - 300 * (i + 1)})
        })
        .take(recent)
        .collect();
    json!({
        "window_days": days, "total": 57, "failures": 5,
        "by_tool": [
            {"tool": "notes.notes_search", "calls": 31, "failures": 0, "p50_ms": 180, "p95_ms": 420, "avg_result_bytes": 2048},
            {"tool": "tasks.tasks_list", "calls": 18, "failures": 2, "p50_ms": 260, "p95_ms": 1900, "avg_result_bytes": 900},
            {"tool": "scratch.scratch_echo", "calls": 8, "failures": 3, "p50_ms": 30000, "p95_ms": 30000, "avg_result_bytes": null},
        ],
        "by_failure_kind": {"timeout": 3, "http": 2},
        "recent_runs": runs,
        "note": "Gateway traffic only.",
    })
}

/// `policy_events`, in the gateway's shape.
fn policy_events(days: u64, limit: usize, decision: Option<&str>) -> Value {
    let now = now_unix();
    let all = [
        json!({"ts": now - 60, "run_id": "run-0001", "actor": "00ufakeadmin", "path": "call_tool", "target": "notes.notes_create",
               "decision": "deny", "effect": "blocked", "mode_at_decision": "observe", "rules": ["server-read-only"],
               "reason": "notes is read-only and notes_create is classified write; only its tools classified read may be called: notes_read, notes_search",
               "args": {"title": "Groceries"}}),
        json!({"ts": now - 3600, "run_id": "run-0007", "actor": "0oaReader", "path": "bridge", "target": "tasks.tasks_nuke",
               "decision": "flag", "effect": "observed", "mode_at_decision": "observe", "rules": ["unknown-tool"],
               "reason": "tasks.tasks_nuke is not in the registry's catalogue", "args": {}}),
        json!({"ts": now - 7200, "run_id": "run-0009", "actor": "0oaReader", "path": "call_tool", "target": "scratch.scratch_wipe",
               "decision": "deny", "effect": "observed", "mode_at_decision": "observe", "rules": ["tool-denylist"],
               "reason": "scratch.scratch_wipe is denied by policy", "args": {"all": true}}),
    ];
    let events: Vec<&Value> = all
        .iter()
        .filter(|e| decision.is_none_or(|d| e["decision"] == d))
        .take(limit)
        .collect();
    json!({
        "window_days": days, "mode": "observe", "policy_version": "2026-09-25.1", "policy_fingerprint": "5f3a9c1e",
        "count": events.len(), "events": events,
    })
}

fn text_page(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        message.to_string(),
    )
        .into_response()
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        axum::Json(json!({"error": error, "error_description": description})),
    )
        .into_response()
}

async fn metadata(State(shared): State<Shared>) -> Response {
    let base = shared.lock().unwrap().base.clone();
    let at = |path: &str| base.join(path).unwrap().to_string();
    axum::Json(json!({
        "issuer": base.as_str().trim_end_matches('/'),
        "authorization_endpoint": at("/authorize"),
        "token_endpoint": at("/token"),
        "registration_endpoint": at("/register"),
        "revocation_endpoint": at("/token"),
        "scopes_supported": [ACCESS_SCOPE],
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
    }))
    .into_response()
}

async fn resource_metadata(State(shared): State<Shared>) -> Response {
    let base = shared.lock().unwrap().base.clone();
    axum::Json(json!({
        "resource": base.join("/mcp").unwrap().to_string(),
        "authorization_servers": [base.as_str().trim_end_matches('/')],
        "scopes_supported": [ACCESS_SCOPE],
        "bearer_methods_supported": ["header"],
    }))
    .into_response()
}

async fn register(State(shared): State<Shared>, axum::Json(body): axum::Json<Value>) -> Response {
    let redirects: Vec<String> = body
        .get("redirect_uris")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if redirects.is_empty() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "redirect_uris is required",
        );
    }
    if body
        .get("token_endpoint_auth_method")
        .and_then(Value::as_str)
        != Some("none")
    {
        // The real gateway would issue a secret; the fake only supports what the client sends.
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "fake-gateway registers public clients only",
        );
    }
    let client_id = random_token(12);
    shared
        .lock()
        .unwrap()
        .clients
        .insert(client_id.clone(), redirects.clone());
    (
        StatusCode::CREATED,
        axum::Json(json!({
            "client_id": client_id,
            "redirect_uris": redirects,
            "token_endpoint_auth_method": "none",
            "client_id_issued_at": now_unix(),
        })),
    )
        .into_response()
}

fn is_loopback(host: Option<&str>) -> bool {
    matches!(host, Some(h) if h == "localhost" || h == "[::1]" || h == "::1" || h.starts_with("127."))
}

/// The gateway's rule: loopback redirects match on any port, everything else exactly.
fn redirect_matches(registered: &str, requested: &str) -> bool {
    let (Ok(reg), Ok(req)) = (Url::parse(registered), Url::parse(requested)) else {
        return false;
    };
    if is_loopback(reg.host_str()) {
        reg.scheme() == req.scheme()
            && reg.host_str() == req.host_str()
            && reg.path() == req.path()
            && reg.query() == req.query()
    } else {
        registered == requested
    }
}

async fn authorize(
    State(shared): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let mut inner = shared.lock().unwrap();
    let Some(client_id) = q.get("client_id") else {
        return text_page(StatusCode::BAD_REQUEST, "Invalid client_id");
    };
    let Some(registered) = inner.clients.get(client_id) else {
        return text_page(StatusCode::BAD_REQUEST, "Invalid client_id");
    };
    let redirect_uri = q.get("redirect_uri").cloned().unwrap_or_default();
    if !registered
        .iter()
        .any(|r| redirect_matches(r, &redirect_uri))
    {
        return text_page(StatusCode::BAD_REQUEST, "Invalid redirect URI");
    }
    let state = q.get("state").cloned().unwrap_or_default();
    let back = |params: &[(&str, &str)]| -> Response {
        let mut url = Url::parse(&redirect_uri).unwrap();
        url.query_pairs_mut()
            .extend_pairs(params)
            .append_pair("state", &state);
        Redirect::to(url.as_str()).into_response()
    };
    if q.get("response_type").map(String::as_str) != Some("code") {
        return back(&[("error", "unsupported_response_type")]);
    }
    let challenge = match (
        q.get("code_challenge"),
        q.get("code_challenge_method").map(String::as_str),
    ) {
        (Some(c), Some("S256")) => c.clone(),
        _ => {
            return back(&[
                ("error", "invalid_request"),
                ("error_description", "PKCE S256 is required"),
            ]);
        }
    };
    let requested = q.get("scope").map(String::as_str).unwrap_or("").trim();
    let requested = if requested.is_empty() {
        ACCESS_SCOPE
    } else {
        requested
    };
    if requested
        .split_whitespace()
        .any(|s| s != ACCESS_SCOPE && s != ADMIN_SCOPE)
    {
        return back(&[("error", "invalid_scope")]);
    }
    let resource = inner.base.join("/mcp").unwrap().to_string();
    if q.get("resource").is_some_and(|r| *r != resource) {
        return back(&[("error", "invalid_target")]);
    }
    if requested.split_whitespace().any(|s| s == ADMIN_SCOPE) && !inner.opts.idp_grants_admin {
        return text_page(
            StatusCode::BAD_REQUEST,
            "The identity provider returned an error: access_denied",
        );
    }
    let code = random_token(24);
    inner.codes.insert(
        code.clone(),
        CodeGrant {
            client_id: client_id.clone(),
            redirect_uri: redirect_uri.clone(),
            challenge,
            scope: requested.to_string(),
        },
    );
    let iss = inner.base.as_str().trim_end_matches('/').to_string();
    drop(inner);
    back(&[("code", &code), ("iss", &iss)])
}

/// Returns (access, refresh, expires_in) for a new grant.
fn issue(inner: &mut Inner, scope: &str, client_id: &str) -> (String, String, u64) {
    let refresh = random_token(32);
    inner.grants.push(Grant {
        scope: scope.into(),
        client_id: client_id.into(),
        current: refresh.clone(),
        previous: None,
        revoked: false,
    });
    let grant = inner.grants.len() - 1;
    let access = mint_access(inner, grant);
    (access, refresh, inner.opts.access_ttl)
}

fn mint_access(inner: &mut Inner, grant: usize) -> String {
    let access = random_token(32);
    let g = &inner.grants[grant];
    let entry = Access {
        scope: g.scope.clone(),
        client_id: g.client_id.clone(),
        expires_at: now_unix() + inner.opts.access_ttl,
        grant,
    };
    inner.access.insert(access.clone(), entry);
    access
}

fn token_json(access: &str, refresh: &str, expires_in: u64, scope: &str) -> Response {
    axum::Json(json!({
        "access_token": access, "token_type": "bearer", "expires_in": expires_in,
        "scope": scope, "refresh_token": refresh,
    }))
    .into_response()
}

async fn token(
    State(shared): State<Shared>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let mut inner = shared.lock().unwrap();
    let grant_type = form.get("grant_type").cloned().unwrap_or_default();
    inner.log.push(format!(
        "token:{}",
        if grant_type.is_empty() {
            "revoke"
        } else {
            &grant_type
        }
    ));
    let client_id = form.get("client_id").cloned().unwrap_or_default();
    if !inner.clients.contains_key(&client_id) {
        return oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "Client not found",
        );
    }
    match grant_type.as_str() {
        "authorization_code" => {
            let Some(code) = form.get("code").and_then(|c| inner.codes.remove(c)) else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "Invalid or expired code",
                );
            };
            if code.client_id != client_id {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "Client mismatch");
            }
            if form
                .get("redirect_uri")
                .is_some_and(|r| *r != code.redirect_uri)
            {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "redirect_uri mismatch",
                );
            }
            if form.get("code_verifier").map(|v| challenge_for(v)) != Some(code.challenge) {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "Invalid PKCE code_verifier",
                );
            }
            let (access, refresh, expires_in) = issue(&mut inner, &code.scope, &client_id);
            token_json(&access, &refresh, expires_in, &code.scope)
        }
        "refresh_token" => {
            let presented = form.get("refresh_token").cloned().unwrap_or_default();
            let found = inner.grants.iter().position(|g| {
                !g.revoked
                    && g.client_id == client_id
                    && (g.current == presented || g.previous.as_deref() == Some(presented.as_str()))
            });
            let Some(index) = found else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "Invalid refresh token",
                );
            };
            let refresh = random_token(32);
            let grant = &mut inner.grants[index];
            grant.previous = Some(std::mem::replace(&mut grant.current, refresh.clone()));
            let scope = grant.scope.clone();
            let access = mint_access(&mut inner, index);
            token_json(&access, &refresh, inner.opts.access_ttl, &scope)
        }
        "" => {
            let presented = form.get("token").cloned().unwrap_or_default();
            if let Some(index) = inner.grants.iter().position(|g| {
                g.current == presented || g.previous.as_deref() == Some(presented.as_str())
            }) {
                inner.grants[index].revoked = true;
                inner.access.retain(|_, a| a.grant != index);
            }
            StatusCode::OK.into_response()
        }
        other => oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type", other),
    }
}

fn www_authenticate(inner: &Inner, extra: &str) -> HeaderValue {
    let meta = inner
        .base
        .join("/.well-known/oauth-protected-resource/mcp")
        .unwrap();
    HeaderValue::from_str(&format!(
        "Bearer realm=\"OAuth\", resource_metadata=\"{meta}\"{extra}"
    ))
    .unwrap()
}

fn rpc_reply(sse: bool, message: Value) -> Response {
    if sse {
        let body = format!("event: message\ndata: {message}\n\n");
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            body,
        )
            .into_response()
    } else {
        axum::Json(message).into_response()
    }
}

fn tool_text(value: &Value) -> Value {
    json!({"content": [{"type": "text", "text": value.to_string()}]})
}

fn tool_error(message: &str) -> Value {
    json!({"content": [{"type": "text", "text": message}], "isError": true})
}

fn descriptor(name: &str) -> Value {
    json!({"name": name, "description": format!("fake {name}"), "inputSchema": {"type": "object"}})
}

async fn mcp(State(shared): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let mut inner = shared.lock().unwrap();
    let header_str = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let accept = header_str("accept");
    if !(accept.contains("application/json") && accept.contains("text/event-stream")) {
        return (StatusCode::NOT_ACCEPTABLE, axum::Json(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32000, "message": "Not Acceptable"}}))).into_response();
    }
    if !header_str("content-type").starts_with("application/json") {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let bearer = header_str("authorization")
        .strip_prefix("Bearer ")
        .map(str::to_string);
    let Some(bearer) = bearer else {
        let value = www_authenticate(&inner, ", scope=\"mcp-access\"");
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, value)],
            "",
        )
            .into_response();
    };
    let (scope, client_id) = match inner.access.get(&bearer) {
        Some(a) if a.expires_at > now_unix() && !inner.grants[a.grant].revoked => {
            (a.scope.clone(), a.client_id.clone())
        }
        _ => {
            inner.log.push("mcp:401".into());
            let value = www_authenticate(
                &inner,
                ", scope=\"mcp-access\", error=\"invalid_token\", error_description=\"Invalid access token\"",
            );
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, value)],
                axum::Json(json!({"error": "invalid_token"})),
            )
                .into_response();
        }
    };
    let has_admin_scope = scope.split_whitespace().any(|s| s == ADMIN_SCOPE);
    let mcp_name = header_str("mcp-name");
    if header_str("mcp-method") == "tools/call" && inner.admin_tool(&mcp_name) && !has_admin_scope {
        let description = format!("Tool '{mcp_name}' requires the 'gateway:admin' scope.");
        let value = www_authenticate(
            &inner,
            &format!(
                ", error=\"insufficient_scope\", scope=\"gateway:admin\", error_description=\"{description}\""
            ),
        );
        return (
            StatusCode::FORBIDDEN,
            [(header::WWW_AUTHENTICATE, value)],
            axum::Json(json!({"error": "insufficient_scope", "error_description": description})),
        )
            .into_response();
    }
    let Ok(request) = serde_json::from_str::<Value>(&body) else {
        return rpc_reply(
            inner.opts.sse,
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}}),
        );
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let params = request.get("params").cloned().unwrap_or(json!({}));
    let admin = has_admin_scope && inner.opts.admin_subs.contains(&inner.opts.sub);
    let sse = inner.opts.sse;
    let tool = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    inner.log.push(if method == "tools/call" {
        format!("mcp:tools/call:{tool}")
    } else {
        format!("mcp:{method}")
    });
    let outcome = match method.as_str() {
        "tools/list" => {
            let mut tools: Vec<Value> = PUBLIC_TOOLS.iter().map(|t| descriptor(t)).collect();
            if admin {
                tools.extend(ADMIN_TOOLS.iter().map(|t| descriptor(t)));
                if !inner.opts.legacy {
                    tools.extend(NEW_ADMIN_TOOLS.iter().map(|t| descriptor(t)));
                }
            }
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => {
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if inner.admin_tool(&tool) && !admin || !inner.offers(&tool) {
                Err((-32602, format!("Tool {tool} not found")))
            } else {
                Ok(call(&mut inner, &tool, &args, &scope, &client_id))
            }
        }
        _ => Err((-32601, format!("Method not found: {method}"))),
    };
    let message = match outcome {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    };
    rpc_reply(sse, message)
}

fn server_index(inner: &Inner, args: &Value) -> Option<usize> {
    let id = args["server"].as_str()?;
    inner.servers.iter().position(|s| s["id"] == id)
}

/// Runs `f` on the named server and persists the result; an unknown server is
/// the gateway's tool error.
fn with_server(
    inner: &mut Inner,
    args: &Value,
    f: impl FnOnce(&mut Inner, usize) -> Value,
) -> Value {
    let Some(i) = server_index(inner, args) else {
        let id = args["server"].as_str().unwrap_or_default();
        return tool_error(&format!(
            "Error: unknown server \"{id}\" — call list_servers for valid ids"
        ));
    };
    let result = f(inner, i);
    save_state(inner);
    tool_text(&result)
}

fn save_state(inner: &Inner) {
    if let Some(path) = &inner.opts.state_path {
        let text =
            serde_json::to_string_pretty(&json!({ "servers": inner.servers })).unwrap_or_default();
        let _ = write_atomic(path, &text, false);
    }
}

/// The probe, simplified: an https URL is accepted unless its host says
/// otherwise (`refuse` is outside the perimeter, `fail` does not answer).
/// Returns the host, or the tool error.
fn probe(url: &str) -> Result<String, Value> {
    let Some(host) = Url::parse(url)
        .ok()
        .filter(|u| u.scheme() == "https")
        .and_then(|u| u.host_str().map(str::to_string))
    else {
        return Err(tool_error("Error: url must be an https URL"));
    };
    if host.contains("refuse") {
        return Err(tool_error(&format!(
            "Error: {host} is outside the gateway's trust perimeter"
        )));
    }
    if host.contains("fail") {
        return Err(tool_error(&format!(
            "Error: probe failed: {host} answered HTTP 502"
        )));
    }
    Ok(host)
}

/// `update_server`: only the fields given change; a new URL is probed first
/// and on failure nothing changes. Id, status, access, timeout, auth, tool
/// classes and approved scopes are kept.
fn admin_update(inner: &mut Inner, args: &Value) -> Value {
    let Some(i) = server_index(inner, args) else {
        let id = args["server"].as_str().unwrap_or_default();
        return tool_error(&format!(
            "Error: unknown server \"{id}\" — call list_servers for valid ids"
        ));
    };
    let url = args["url"].as_str();
    let name = args["display_name"].as_str();
    let description = args["description"].as_str();
    if url.is_none() && name.is_none() && description.is_none() {
        return tool_error("Error: give at least one of url, display_name, description");
    }
    if let Some(url) = url
        && let Err(refused) = probe(url)
    {
        return refused;
    }
    let now = now_unix();
    let server = &mut inner.servers[i];
    let mut changed = serde_json::Map::new();
    for (field, key, value) in [
        ("url", "url", url),
        ("display_name", "name", name),
        ("description", "description", description),
    ] {
        if let Some(value) = value
            && server[key] != value
        {
            changed.insert(field.into(), json!({"was": server[key], "now": value}));
            server[key] = json!(value);
        }
    }
    let mut result = json!({"server": server["id"], "changed": changed});
    if url.is_some() && changed.contains_key("url") {
        server["last_refresh_at"] = json!(now);
        server["last_error"] = Value::Null;
        result["refresh"] = json!({"server": server["id"], "ok": true, "tools": server["tools"]});
    }
    save_state(inner);
    tool_text(&result)
}

/// `call_tool`: the downstream call, simulated. The tool must be in the
/// server's catalogue; a read-only server refuses a tool not classified read.
/// The result echoes the call.
fn proxy_call(inner: &mut Inner, args: &Value) -> Value {
    let server = args["server"].as_str().unwrap_or_default();
    let tool = args["tool"].as_str().unwrap_or_default();
    let Some(entry) = inner.servers.iter_mut().find(|s| s["id"] == server) else {
        return tool_error(&format!("Error: unknown server '{server}'"));
    };
    if entry["status"] != "active" {
        return tool_error(&format!("Error: {server} is {}", cell(&entry["status"])));
    }
    let Some(class) = entry["tool_classes"]
        .as_array()
        .and_then(|list| list.iter().find(|t| t["name"] == tool))
        .map(|t| t["classification"].clone())
    else {
        return tool_error(&format!("Error: {server} has no tool '{tool}'"));
    };
    if entry["access"] == "read_only" && class != "read" {
        return tool_error(&format!(
            "Error: policy denied {server}.{tool}: {server} is read-only"
        ));
    }
    entry["last_call_at"] = json!(now_unix());
    let payload =
        json!({"server": server, "tool": tool, "classification": class, "args": args["args"]});
    json!({"content": [{"type": "text", "text": payload.to_string()}]})
}

fn cell(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn admin_register(inner: &mut Inner, args: &Value) -> Value {
    let url = args["url"].as_str().unwrap_or_default().to_string();
    let host = match probe(&url) {
        Ok(host) => host,
        Err(refused) => return refused,
    };
    let id = args["id"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| host.split('.').next().unwrap_or_default().to_string());
    if inner.servers.iter().any(|s| s["id"] == id.as_str()) {
        return tool_error(&format!(
            "Error: a server with id \"{id}\" is already registered"
        ));
    }
    let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(10_000);
    let now = now_unix();
    let tools = json!([
        {"name": format!("{id}_get"), "classification": "unknown", "source": "default"},
        {"name": format!("{id}_put"), "classification": "unknown", "source": "default"},
    ]);
    inner.servers.push(json!({
        "id": id, "name": id, "description": args["description"], "url": url, "tools": 2, "status": "active",
        "timeout_ms": timeout_ms, "health": "ok", "last_refresh_at": now, "last_error": null, "call_failures": 0,
        "last_call_error": null, "last_call_at": null, "access": "read_write", "scopes": format!("{id}:call"),
        "tool_classes": tools, "server_version": "1.0.0", "registered_by": inner.opts.email, "registered_at": now,
    }));
    inner
        .servers
        .sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    save_state(inner);
    tool_text(
        &json!({"registered": id, "url": url, "tools": 2, "timeout_ms": timeout_ms, "note": "Call get_api to see its declarations."}),
    )
}

/// A server whose state holds `_advertised` reports it as scope drift until
/// `approve_scopes` adopts it.
fn admin_refresh(inner: &mut Inner, args: &Value) -> Value {
    let approve = args["approve_scopes"].as_bool().unwrap_or(false);
    let only = args["server"].as_str();
    if approve && only.is_none() {
        return tool_error("approve_scopes needs `server`: scope approval is one server at a time");
    }
    if let Some(id) = only
        && !inner.servers.iter().any(|s| s["id"] == id)
    {
        return tool_error(&format!(
            "unknown server \"{id}\" — call list_servers for valid ids"
        ));
    }
    let now = now_unix();
    let mut outcomes = Vec::new();
    for server in inner
        .servers
        .iter_mut()
        .filter(|s| only.is_none_or(|id| s["id"] == id))
    {
        let mut outcome = json!({"server": server["id"], "ok": true, "tools": server["tools"]});
        server["last_refresh_at"] = json!(now);
        if let Some(advertised) = server["_advertised"].as_str().map(str::to_string) {
            let split = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
            if approve {
                server["scopes"] = json!(advertised);
                outcome["scopes_approved"] = json!(split(&advertised));
                if let Some(map) = server.as_object_mut() {
                    map.remove("_advertised");
                }
                server["last_error"] = Value::Null;
            } else {
                let approved = split(server["scopes"].as_str().unwrap_or_default());
                outcome["scope_drift"] =
                    json!({"approved": approved, "advertised": split(&advertised)});
            }
        }
        outcomes.push(outcome);
    }
    save_state(inner);
    let refreshed = outcomes.len();
    tool_text(&json!({"refreshed": refreshed, "failed": 0, "outcomes": outcomes}))
}

fn call(inner: &mut Inner, tool: &str, args: &Value, scope: &str, client_id: &str) -> Value {
    match tool {
        "whoami" => tool_text(&json!({
            "authenticated": true, "auth_path": "interactive", "client_id": client_id,
            "sub": inner.opts.sub, "email": inner.opts.email, "name": inner.opts.name,
            "scopes": scope.split_whitespace().collect::<Vec<_>>(),
        })),
        "health" => {
            let mut health = json!({"status": "ok", "servers": inner.servers.len()});
            if !inner.opts.legacy {
                health["build"] = json!({"version": BUILD_VERSION, "sha": BUILD_SHA, "built_at": "2026-10-01T12:00:00Z"});
            }
            tool_text(&health)
        }
        "list_servers" => {
            let all = args
                .get("include_inactive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let with_tools = args
                .get("include_tools")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let legacy = inner.opts.legacy;
            let servers: Vec<Value> = inner
                .servers
                .iter()
                .filter(|s| all || s["status"] == "active")
                .map(|s| {
                    let mut s = s.clone();
                    if let Some(map) = s.as_object_mut() {
                        // Fake-only bookkeeping (`_advertised`) is not the gateway's.
                        map.retain(|k, _| !k.starts_with('_'));
                        if !with_tools {
                            map.remove("tool_classes");
                        }
                        if legacy {
                            for key in ["server_version", "registered_by", "registered_at"] {
                                map.remove(key);
                            }
                        }
                    }
                    s
                })
                .collect();
            tool_text(&json!({"count": servers.len(), "servers": servers}))
        }
        "usage_stats" => {
            let days = args.get("days").and_then(Value::as_u64).unwrap_or(7);
            let recent = args.get("recent").and_then(Value::as_u64).unwrap_or(10);
            tool_text(&usage(days, recent as usize))
        }
        "policy_events" => {
            let days = args.get("days").and_then(Value::as_u64).unwrap_or(1);
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20);
            let decision = args.get("decision").and_then(Value::as_str);
            tool_text(&policy_events(days, limit as usize, decision))
        }
        "call_tool" => proxy_call(inner, args),
        "update_server" => admin_update(inner, args),
        "list_scope_owners" => {
            let mut owners: Vec<Value> = inner
                .servers
                .iter()
                .flat_map(|s| {
                    let id = s["id"].clone();
                    s["scopes"]
                        .as_str()
                        .unwrap_or_default()
                        .split_whitespace()
                        .map(move |scope| json!({"scope": scope, "server": id}))
                        .collect::<Vec<_>>()
                })
                .collect();
            owners.sort_by(|a, b| a["scope"].as_str().cmp(&b["scope"].as_str()));
            tool_text(&json!({ "owners": owners }))
        }
        "list_connections" => {
            let kind = args.get("kind").and_then(Value::as_str);
            let connections: Vec<&Value> = inner
                .connections
                .iter()
                .filter(|c| kind.is_none_or(|k| c["kind"] == k))
                .collect();
            tool_text(&json!({ "connections": connections }))
        }
        "revoke_user_access" => {
            let Some(sub) = args["sub"].as_str().filter(|s| !s.is_empty()) else {
                return tool_error("Error: sub is required");
            };
            let (mut grants, clients, tokens) = inner.users.remove(sub).unwrap_or_default();
            if sub == inner.opts.sub {
                for grant in inner.grants.iter_mut().filter(|g| !g.revoked) {
                    grant.revoked = true;
                    grants += 1;
                }
                inner.access.clear();
            }
            let before = inner.connections.len();
            inner
                .connections
                .retain(|c| !(c["subject"] == sub && c["kind"] == "okta_user"));
            let removed = inner.connections.len() < before;
            tool_text(
                &json!({"sub": sub, "grants_revoked": grants, "clients": clients,
                              "login_tokens_removed": tokens, "okta_user_connection_removed": removed}),
            )
        }
        "register_server" => admin_register(inner, args),
        "unregister_server" => with_server(inner, args, |inner, i| {
            let removed = inner.servers.remove(i);
            json!({"unregistered": removed["id"], "url": removed["url"]})
        }),
        "set_server_timeout" => {
            let ms = args["timeout_ms"].as_u64().unwrap_or(0);
            if !(1000..=120_000).contains(&ms) {
                return tool_error("Error: timeout_ms must be between 1000 and 120000");
            }
            with_server(inner, args, |inner, i| {
                let was = inner.servers[i]["timeout_ms"].clone();
                inner.servers[i]["timeout_ms"] = json!(ms);
                json!({"server": inner.servers[i]["id"], "was": was, "now": ms})
            })
        }
        "set_server_status" => {
            let status = args["status"].as_str().unwrap_or_default().to_string();
            if !["active", "disabled", "quarantined"].contains(&status.as_str()) {
                return tool_error("Error: status must be active, disabled or quarantined");
            }
            with_server(inner, args, |inner, i| {
                let server = &mut inner.servers[i];
                let was = server["status"].clone();
                server["status"] = json!(status);
                if status == "active" {
                    server["last_error"] = Value::Null;
                    server["call_failures"] = json!(0);
                    server["last_call_error"] = Value::Null;
                    server["health"] = json!("ok");
                } else {
                    server["health"] = json!("failing");
                }
                json!({"server": server["id"], "was": was, "now": status})
            })
        }
        "set_server_access" => {
            let access = args["access"].as_str().unwrap_or_default().to_string();
            with_server(inner, args, |inner, i| {
                let server = &mut inner.servers[i];
                let was = server["access"].clone();
                server["access"] = json!(access);
                let tools = server["tool_classes"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let read: Vec<Value> = tools
                    .iter()
                    .filter(|t| t["classification"] == "read")
                    .map(|t| t["name"].clone())
                    .collect();
                let callable = if access == "read_only" {
                    read.len()
                } else {
                    tools.len()
                };
                let note = if access == "read_only" && read.is_empty() {
                    "No tool on this server is classified read, so every call is refused."
                        .to_string()
                } else {
                    format!("{callable} of {} tools remain callable.", tools.len())
                };
                json!({"server": server["id"], "was": was, "now": access, "callable": callable, "of": tools.len(),
                       "read_tools": read, "note": note})
            })
        }
        "set_tool_class" => {
            let tool = args["tool"].as_str().unwrap_or_default().to_string();
            let class = args["class"].as_str().map(str::to_string);
            let Some(i) = server_index(inner, args) else {
                return tool_error("Error: unknown server");
            };
            let server_id = inner.servers[i]["id"].clone();
            let Some(entry) = inner.servers[i]["tool_classes"]
                .as_array_mut()
                .and_then(|list| list.iter_mut().find(|t| t["name"] == tool.as_str()))
            else {
                return tool_error(&format!(
                    "Error: {server_id} has no tool \"{tool}\" in the catalogue"
                ));
            };
            let was = if entry["source"] == "admin" {
                entry["classification"].clone()
            } else {
                Value::Null
            };
            if entry["source"] == "policy" {
                let effective = entry["classification"].clone();
                let result = json!({"server": server_id, "tool": tool, "was": was, "now": class, "effective": effective,
                                    "source": "policy", "note": "POLICY_CLASSIFY outranks this record."});
                save_state(inner);
                return tool_text(&result);
            }
            match &class {
                Some(class) => {
                    entry["classification"] = json!(class);
                    entry["source"] = json!("admin");
                }
                None => {
                    entry["classification"] = json!("unknown");
                    entry["source"] = json!("default");
                }
            }
            let result = json!({"server": server_id, "tool": tool, "was": was, "now": class,
                                "effective": entry["classification"], "source": entry["source"]});
            save_state(inner);
            tool_text(&result)
        }
        "refresh_server" => admin_refresh(inner, args),
        other => tool_error(&format!("Error: fake-gateway does not implement {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_redirects_match_on_any_port_only() {
        let reg = "http://127.0.0.1/callback";
        assert!(redirect_matches(reg, "http://127.0.0.1:53121/callback"));
        assert!(!redirect_matches(reg, "http://localhost:53121/callback"));
        assert!(!redirect_matches(reg, "http://127.0.0.1:53121/other"));
        assert!(!redirect_matches(
            "https://app.example/cb",
            "https://app.example:444/cb"
        ));
    }
}
