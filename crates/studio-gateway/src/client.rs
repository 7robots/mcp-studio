//! The gateway client: JSON-RPC over `POST /mcp`, stateless (no `initialize`,
//! no session), bearer tokens refreshed on demand.
//!
//! Requests carry `Mcp-Method` and `Mcp-Name` so the gateway's header gate can
//! answer an under-scoped admin call with a 403 `insufficient_scope` challenge
//! instead of an opaque unknown-tool error. `MCP-Protocol-Version` is left out:
//! the gateway then takes its legacy stateless path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;

use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell};
use url::Url;

use crate::model::{
    Connection, Health, Identity, PolicyEvents, ScopeOwner, ServerList, UsageStats, Whoami,
};
use crate::oauth::{self, ClientCache, ClientSpec, Metadata, OAuthError};
use crate::profile::Profile;
use crate::tokens::{TokenStore, Tokens};
use crate::util::truncate;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("not signed in to {0}; run `mcp-studio gateway login`")]
    NotLoggedIn(String),
    #[error("the sign-in has expired; run `mcp-studio gateway login`")]
    SessionExpired,
    #[error("the gateway rejected the token: {0}")]
    Unauthorized(String),
    #[error("{description} (needs the '{scope}' scope)")]
    InsufficientScope { scope: String, description: String },
    #[error("rate limited by the gateway; retry after {retry_after}s")]
    RateLimited { retry_after: u64 },
    /// A tool ran and reported failure (`isError`); the text is the gateway's.
    #[error("{0}")]
    Tool(String),
    /// The gateway does not offer this tool to the caller (an older gateway).
    #[error("{0} is not supported by this gateway")]
    Unsupported(String),
    #[error("JSON-RPC error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("unexpected response from the gateway: {0}")]
    Protocol(String),
    #[error("cannot reach the gateway: {0}")]
    Transport(#[from] reqwest::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type GatewayResult<T> = Result<T, GatewayError>;

/// One tool from `tools/list`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// One signed-in (or not yet signed-in) connection to one gateway. Cheap to
/// share behind an `Arc`: every method takes `&self`.
pub struct Session {
    http: reqwest::Client,
    profile: Profile,
    gateway: Url,
    mcp: Url,
    store: Box<dyn TokenStore>,
    cache: ClientCache,
    /// Held across a refresh, so concurrent callers share one rotation.
    tokens: Mutex<Option<Tokens>>,
    meta: OnceCell<Metadata>,
    next_id: AtomicU64,
    /// Set when the gateway refused the refresh token. Callers that were
    /// waiting on that refresh then report the expiry, not a missing sign-in.
    expired: AtomicBool,
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("mcp-studio/", env!("CARGO_PKG_VERSION")))
        .timeout(REQUEST_TIMEOUT)
        // A redirect could re-send a token request's form, code or refresh token
        // included, to wherever it points. The gateway never redirects these.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("building the HTTP client")
}

impl Session {
    /// Loads stored tokens; a missing pair is reported on first use, not here,
    /// so the TUI can start and show the sign-in prompt.
    /// `cache` holds the DCR registration a sign-in reuses.
    pub fn new(
        http: reqwest::Client,
        profile: Profile,
        store: Box<dyn TokenStore>,
        cache: ClientCache,
    ) -> anyhow::Result<Session> {
        let gateway = profile.url.clone();
        let mcp = oauth::resource_for(&gateway)?;
        let tokens = store.load()?.filter(|t| issued_by(t, &gateway));
        Ok(Session {
            http,
            profile,
            gateway,
            mcp,
            store,
            cache,
            tokens: Mutex::new(tokens),
            meta: OnceCell::new(),
            next_id: AtomicU64::new(1),
            expired: AtomicBool::new(false),
        })
    }

    pub fn gateway(&self) -> &Url {
        &self.gateway
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub fn cache(&self) -> &ClientCache {
        &self.cache
    }

    /// The browser sign-in, end to end, and the resulting pair installed.
    /// `open` shows the authorization URL to the user: the browser in
    /// production, a redirect-following GET in the demo and the tests.
    pub async fn login(
        &self,
        timeout: Duration,
        open: impl FnOnce(&Url) -> Result<()>,
    ) -> Result<Tokens> {
        let spec = ClientSpec::for_profile(&self.profile);
        let tokens =
            oauth::login(&self.http, &self.gateway, &spec, &self.cache, timeout, open).await?;
        self.install(tokens.clone()).await?;
        Ok(tokens)
    }

    pub fn store(&self) -> &dyn TokenStore {
        self.store.as_ref()
    }

    /// Replaces the stored pair (after `login`).
    pub async fn install(&self, mut tokens: Tokens) -> anyhow::Result<()> {
        tokens
            .gateway
            .get_or_insert_with(|| self.gateway.to_string());
        self.store.save(&tokens)?;
        *self.tokens.lock().await = Some(tokens);
        self.expired.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// The current pair, without refreshing.
    pub async fn tokens(&self) -> Option<Tokens> {
        self.tokens.lock().await.clone()
    }

    async fn metadata(&self) -> GatewayResult<&Metadata> {
        Ok(self
            .meta
            .get_or_try_init(|| oauth::discover(&self.http, &self.gateway))
            .await?)
    }

    /// A usable access token. `rejected` names one the gateway just refused:
    /// refresh unless another caller already replaced it.
    async fn bearer(&self, rejected: Option<&str>) -> GatewayResult<String> {
        let mut guard = self.tokens.lock().await;
        let Some(current) = guard.as_ref() else {
            if self.expired.load(Ordering::Relaxed) {
                return Err(GatewayError::SessionExpired);
            }
            return Err(GatewayError::NotLoggedIn(self.gateway.to_string()));
        };
        let stale = match rejected {
            Some(token) => current.access_token == token,
            None => !current.is_fresh(),
        };
        if !stale {
            return Ok(current.access_token.clone());
        }
        // Another process (a second terminal) may have rotated the pair since
        // this one loaded it; its copy is the newer one.
        if let Some(stored) = self.stored_newer(current)? {
            if stored.is_fresh() && rejected != Some(stored.access_token.as_str()) {
                let token = stored.access_token.clone();
                *guard = Some(stored);
                return Ok(token);
            }
            *guard = Some(stored);
        }
        let current = guard.as_ref().expect("set above");
        if current.refresh_token.is_none() {
            return Err(GatewayError::SessionExpired);
        }
        let meta = self.metadata().await?;
        let used = current.refresh_token.clone();
        match oauth::refresh(&self.http, meta, current, &self.mcp).await {
            Ok(fresh) => {
                self.store.save(&fresh)?;
                let token = fresh.access_token.clone();
                *guard = Some(fresh);
                Ok(token)
            }
            Err(err)
                if err
                    .downcast_ref::<OAuthError>()
                    .is_some_and(|e| e.error == "invalid_grant") =>
            {
                // Refused because another process rotated it meanwhile? Then its
                // pair is in the store, and clearing it would sign both out.
                if let Some(stored) = self.store.load()?.filter(|t| issued_by(t, &self.gateway))
                    && stored.refresh_token != used
                {
                    let token = stored.access_token.clone();
                    let fresh = stored.is_fresh();
                    *guard = Some(stored);
                    return if fresh {
                        Ok(token)
                    } else {
                        Err(GatewayError::SessionExpired)
                    };
                }
                self.store.clear()?;
                *guard = None;
                self.expired.store(true, Ordering::Relaxed);
                Err(GatewayError::SessionExpired)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Sends one JSON-RPC request and returns its `result`. A 401 triggers one
    /// refresh and one retry.
    pub async fn rpc(&self, method: &str, params: Value) -> GatewayResult<Value> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .filter(|_| method == "tools/call");
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut rejected: Option<String> = None;
        loop {
            let token = self.bearer(rejected.as_deref()).await?;
            let mut request = self
                .http
                .post(self.mcp.clone())
                .bearer_auth(&token)
                .header("accept", "application/json, text/event-stream")
                .header("mcp-method", method)
                .json(&body);
            if let Some(name) = name {
                request = request.header("mcp-name", name);
            }
            let response = request.send().await?;
            let status = response.status().as_u16();
            let challenge = response
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok())
                .map(auth_params)
                .unwrap_or_default();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(60);
            let is_sse = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("text/event-stream"));
            let text = response.text().await?;
            match status {
                200..=299 => return parse_rpc(&text, is_sse),
                401 if rejected.is_none() => rejected = Some(token),
                401 => {
                    let reason = challenge
                        .get("error_description")
                        .or(challenge.get("error"))
                        .cloned();
                    return Err(GatewayError::Unauthorized(
                        reason.unwrap_or_else(|| truncate(&text, 200)),
                    ));
                }
                403 if challenge.get("error").map(String::as_str) == Some("insufficient_scope") => {
                    return Err(GatewayError::InsufficientScope {
                        scope: challenge.get("scope").cloned().unwrap_or_default(),
                        description: challenge
                            .get("error_description")
                            .cloned()
                            .unwrap_or_else(|| "insufficient scope".into()),
                    });
                }
                429 => return Err(GatewayError::RateLimited { retry_after }),
                _ => {
                    return Err(GatewayError::Http {
                        status,
                        body: truncate(&text, 300),
                    });
                }
            }
        }
    }

    /// Calls a tool and decodes `content[0].text` as JSON (a plain string when it
    /// is not JSON). `isError` results become [`GatewayError::Tool`].
    pub async fn call_tool(&self, name: &str, arguments: Value) -> GatewayResult<Value> {
        let result = self.call_tool_raw(name, arguments).await?;
        let text = result
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                GatewayError::Protocol(format!("tool {name} returned no text content"))
            })?;
        Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string())))
    }

    /// Calls a tool and returns its whole result; `isError` results become
    /// [`GatewayError::Tool`] with their text.
    pub async fn call_tool_raw(&self, name: &str, arguments: Value) -> GatewayResult<Value> {
        let result = self
            .rpc("tools/call", json!({"name": name, "arguments": arguments}))
            .await?;
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let text = result
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .unwrap_or("the tool reported an error")
                .to_string();
            return Err(GatewayError::Tool(text));
        }
        Ok(result)
    }

    pub async fn list_tools(&self) -> GatewayResult<Vec<ToolInfo>> {
        let result = self.rpc("tools/list", json!({})).await?;
        let tools = result.get("tools").cloned().unwrap_or(Value::Array(vec![]));
        serde_json::from_value(tools)
            .map_err(|e| GatewayError::Protocol(format!("tools/list: {e}")))
    }

    pub async fn whoami(&self) -> GatewayResult<Whoami> {
        decode("whoami", self.call_tool("whoami", json!({})).await?)
    }

    /// `whoami` plus the admin determination (scope and admin tools present).
    pub async fn identity(&self) -> GatewayResult<Identity> {
        let (whoami, tools) = tokio::join!(self.whoami(), self.list_tools());
        let whoami = whoami?;
        let tools: Vec<String> = tools?.into_iter().map(|t| t.name).collect();
        let admin = whoami.scopes.contains(&self.profile.admin_scope)
            && tools.contains(&self.profile.admin_probe_tool);
        Ok(Identity {
            whoami,
            admin,
            tools,
        })
    }

    /// Whether `tools/list` offers this caller `tool`.
    pub async fn supports(&self, tool: &str) -> GatewayResult<bool> {
        Ok(self.list_tools().await?.iter().any(|t| t.name == tool))
    }

    /// Calls `tool` only if the gateway offers it, else
    /// [`GatewayError::Unsupported`] (an older gateway).
    pub async fn call_if_supported(&self, tool: &str, arguments: Value) -> GatewayResult<Value> {
        if !self.supports(tool).await? {
            return Err(GatewayError::Unsupported(tool.to_string()));
        }
        self.call_tool(tool, arguments).await
    }

    /// `health`, raw (scripts) and typed.
    pub async fn health_raw(&self) -> GatewayResult<Value> {
        self.call_tool("health", json!({})).await
    }

    /// `health`; a shape this client does not know reads as no build info.
    pub async fn health(&self) -> GatewayResult<Health> {
        Ok(serde_json::from_value(self.health_raw().await?).unwrap_or_default())
    }

    /// Which server claims each scope (`list_scope_owners`, admin).
    pub async fn list_scope_owners(&self) -> GatewayResult<Vec<ScopeOwner>> {
        let r = self
            .call_if_supported("list_scope_owners", json!({}))
            .await?;
        decode("list_scope_owners", list_of(r, "owners"))
    }

    /// Stored downstream credentials' metadata (`list_connections`, admin);
    /// `kind` narrows to one kind. Takes `{connections: [...]}` or a bare array.
    pub async fn list_connections(&self, kind: Option<&str>) -> GatewayResult<Vec<Connection>> {
        let mut args = json!({});
        if let Some(kind) = kind {
            args["kind"] = json!(kind);
        }
        let r = self.call_if_supported("list_connections", args).await?;
        decode("list_connections", list_of(r, "connections"))
    }

    /// Calls `tool` on a registered server through the gateway's `call_tool`,
    /// and returns the downstream tool's whole result.
    pub async fn call_downstream(
        &self,
        server: &str,
        tool: &str,
        args: Value,
    ) -> GatewayResult<Value> {
        self.call_tool_raw(
            "call_tool",
            json!({"server": server, "tool": tool, "args": args}),
        )
        .await
    }

    pub async fn list_servers_raw(
        &self,
        include_inactive: bool,
        include_tools: bool,
    ) -> GatewayResult<Value> {
        let args = json!({"include_inactive": include_inactive, "include_tools": include_tools});
        self.call_tool("list_servers", args).await
    }

    pub async fn list_servers(
        &self,
        include_inactive: bool,
        include_tools: bool,
    ) -> GatewayResult<ServerList> {
        decode(
            "list_servers",
            self.list_servers_raw(include_inactive, include_tools)
                .await?,
        )
    }

    pub async fn usage_stats(&self, days: u64, recent: u64) -> GatewayResult<UsageStats> {
        let args = json!({"days": days, "recent": recent});
        decode("usage_stats", self.call_tool("usage_stats", args).await?)
    }

    /// `decision` narrows to `flag` or `deny`; `None` is both.
    pub async fn policy_events(
        &self,
        days: u64,
        limit: u64,
        decision: Option<&str>,
    ) -> GatewayResult<PolicyEvents> {
        let mut args = json!({"days": days, "limit": limit});
        if let Some(decision) = decision {
            args["decision"] = json!(decision);
        }
        decode(
            "policy_events",
            self.call_tool("policy_events", args).await?,
        )
    }

    /// Forgets the pair locally whatever happens, then revokes the refresh
    /// token at the gateway and reports if that failed.
    pub async fn logout(&self) -> GatewayResult<()> {
        let mut guard = self.tokens.lock().await;
        let tokens = guard.take();
        self.store.clear()?;
        if let Some(tokens) = tokens
            && let Some(refresh) = tokens.refresh_token.as_deref()
        {
            let meta = self.metadata().await?;
            oauth::revoke(&self.http, meta, refresh, &tokens.client_id).await?;
        }
        Ok(())
    }

    /// The stored pair, when it differs from `current` and belongs here.
    fn stored_newer(&self, current: &Tokens) -> anyhow::Result<Option<Tokens>> {
        Ok(self
            .store
            .load()?
            .filter(|t| issued_by(t, &self.gateway) && t.refresh_token != current.refresh_token))
    }
}

/// A pair belongs to `gateway` if it says so, or says nothing (stored before
/// the origin was recorded, and found under this gateway's key).
fn issued_by(tokens: &Tokens, gateway: &Url) -> bool {
    tokens
        .gateway
        .as_deref()
        .is_none_or(|g| g == gateway.as_str())
}

/// A list result given as `{key: [...]}` or as the bare array.
fn list_of(value: Value, key: &str) -> Value {
    match value {
        Value::Array(_) => value,
        other => other.get(key).cloned().unwrap_or(other),
    }
}

fn decode<T: serde::de::DeserializeOwned>(tool: &str, value: Value) -> GatewayResult<T> {
    serde_json::from_value(value).map_err(|e| GatewayError::Protocol(format!("{tool}: {e}")))
}

/// Picks the JSON-RPC message out of the body and returns `result`, or the
/// `error` as [`GatewayError::Rpc`].
fn parse_rpc(text: &str, is_sse: bool) -> GatewayResult<Value> {
    let payload = if is_sse {
        last_sse_data(text).ok_or_else(|| GatewayError::Protocol("empty event stream".into()))?
    } else {
        text.to_string()
    };
    let message: Value = serde_json::from_str(&payload)
        .map_err(|e| GatewayError::Protocol(format!("{e}: {}", truncate(&payload, 200))))?;
    if let Some(error) = message.get("error") {
        return Err(GatewayError::Rpc {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| GatewayError::Protocol("response has neither result nor error".into()))
}

/// The data of the last event in a `text/event-stream` body. Multi-line data
/// fields are joined with newlines, per the SSE spec.
pub fn last_sse_data(body: &str) -> Option<String> {
    let mut last = None;
    let mut current: Vec<&str> = Vec::new();
    for line in body.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !current.is_empty() {
                last = Some(current.join("\n"));
                current.clear();
            }
        } else if let Some(data) = line.strip_prefix("data:") {
            current.push(data.strip_prefix(' ').unwrap_or(data));
        }
    }
    last
}

/// `key="value"` parameters of a `WWW-Authenticate: Bearer ...` header.
pub fn auth_params(header: &str) -> HashMap<String, String> {
    let rest = header
        .trim()
        .strip_prefix("Bearer")
        .unwrap_or(header)
        .trim();
    let mut params = HashMap::new();
    let mut chars = rest.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ',' || c.is_whitespace()) {
            chars.next();
        }
        let key: String = chars
            .by_ref()
            .take_while(|c| *c != '=')
            .collect::<String>()
            .trim()
            .to_string();
        if key.is_empty() {
            break;
        }
        let mut value = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => value.extend(chars.next()),
                    '"' => break,
                    c => value.push(c),
                }
            }
        } else {
            while let Some(c) = chars.peek().copied() {
                if c == ',' {
                    break;
                }
                value.push(c);
                chars.next();
            }
        }
        params.insert(key.to_ascii_lowercase(), value.trim().to_string());
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_takes_the_last_event_and_joins_lines() {
        let body = "event: message\ndata: {\"a\":1}\n\nevent: message\ndata: {\"b\":\ndata: 2}\n\n";
        assert_eq!(last_sse_data(body).unwrap(), "{\"b\":\n2}");
        assert_eq!(last_sse_data("data:x").unwrap(), "x");
        assert!(last_sse_data(": comment\n\n").is_none());
    }

    #[test]
    fn rpc_error_and_result() {
        let err = parse_rpc(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"Tool x not found"}}"#,
            false,
        )
        .unwrap_err();
        assert!(
            matches!(err, GatewayError::Rpc { code: -32602, .. }),
            "{err:?}"
        );
        let ok = parse_rpc(
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n",
            true,
        )
        .unwrap();
        assert_eq!(ok, json!({"tools": []}));
    }

    #[test]
    fn lists_come_wrapped_or_bare() {
        let row = json!({"subject": "u1", "kind": "okta_user"});
        let wrapped: Vec<crate::model::Connection> =
            decode("x", list_of(json!({"connections": [row]}), "connections")).unwrap();
        let bare: Vec<crate::model::Connection> =
            decode("x", list_of(json!([row]), "connections")).unwrap();
        assert_eq!(wrapped, bare);
        assert_eq!(bare[0].kind.as_deref(), Some("okta_user"));
        assert!(bare[0].expires_at.is_none());
    }

    #[test]
    fn www_authenticate_parameters() {
        let header = r#"Bearer error="insufficient_scope", scope="gateway:admin", resource_metadata="https://gw/.well-known/oauth-protected-resource/mcp", error_description="Tool 'x' requires the \"gateway:admin\" scope.""#;
        let params = auth_params(header);
        assert_eq!(params["error"], "insufficient_scope");
        assert_eq!(params["scope"], "gateway:admin");
        assert_eq!(
            params["error_description"],
            "Tool 'x' requires the \"gateway:admin\" scope."
        );
        assert_eq!(auth_params("Bearer realm=OAuth")["realm"], "OAuth");
    }
}
