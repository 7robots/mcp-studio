//! The browser login through the gateway: RFC 8414 discovery, dynamic client
//! registration as a public client with a loopback redirect, the authorization
//! code flow with PKCE S256, and refresh-token rotation.
//!
//! The gateway sends the browser to its identity provider, which may refuse
//! the admin scope to users outside the admin group; the gateway then shows its
//! own consent page. Failures after that hop are rendered as pages in the
//! browser and never reach the loopback listener, which is why the wait has a
//! timeout.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

use crate::tokens::Tokens;
use crate::util::{now_unix, random_token, truncate, write_atomic};

/// What a sign-in asks for, from the gateway's [`crate::Profile`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientSpec {
    /// `client_name` at dynamic client registration.
    pub client_name: String,
    /// Space-separated scopes. Both the access and the admin scope are asked
    /// for; an identity provider refuses the whole login for a non-admin rather
    /// than dropping the admin scope.
    pub scopes: String,
}

impl ClientSpec {
    pub fn for_profile(profile: &crate::Profile) -> ClientSpec {
        ClientSpec {
            client_name: profile.client_name.clone(),
            scopes: profile.login_scopes.clone(),
        }
    }
}
pub const CALLBACK_PATH: &str = "/callback";
/// Registered once; the gateway matches loopback redirects on any port.
pub const REGISTERED_REDIRECT: &str = "http://127.0.0.1/callback";
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// The gateway expires DCR clients after 90 days; register again before that
/// rather than fail on a browser page the CLI cannot see.
pub const REGISTRATION_MAX_AGE_SECONDS: u64 = 80 * 24 * 3600;
/// How long one loopback connection may take to send its request.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on a loopback request; the callback is one short GET.
const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// The fields of the authorization server metadata the client uses.
#[derive(Clone, Debug, Deserialize)]
pub struct Metadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub revocation_endpoint: Option<String>,
    /// RFC 9207: when true, the callback must carry `iss`.
    #[serde(default)]
    pub authorization_response_iss_parameter_supported: bool,
}

impl Metadata {
    /// Every endpoint and the issuer must be on the gateway's own origin, so a
    /// tampered metadata document cannot send a code or refresh token elsewhere.
    fn check_origin(&self, gateway: &Url) -> Result<()> {
        let endpoints = [
            Some(self.issuer.as_str()),
            Some(self.authorization_endpoint.as_str()),
            Some(self.token_endpoint.as_str()),
            self.registration_endpoint.as_deref(),
            self.revocation_endpoint.as_deref(),
        ];
        for endpoint in endpoints.into_iter().flatten() {
            let url =
                Url::parse(endpoint).with_context(|| format!("metadata endpoint {endpoint:?}"))?;
            if url.origin() != gateway.origin() {
                bail!("the gateway's metadata names {endpoint}, which is not on {gateway}");
            }
        }
        Ok(())
    }
}

/// The `/mcp` resource a gateway's tokens are bound to.
pub fn resource_for(gateway: &Url) -> Result<Url> {
    gateway.join("/mcp").context("building the /mcp URL")
}

pub async fn discover(http: &reqwest::Client, gateway: &Url) -> Result<Metadata> {
    let url = gateway.join("/.well-known/oauth-authorization-server")?;
    let response = http
        .get(url.clone())
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    if !response.status().is_success() {
        bail!("{url} answered HTTP {}", response.status());
    }
    let meta: Metadata = response
        .json()
        .await
        .with_context(|| format!("decoding {url}"))?;
    meta.check_origin(gateway)?;
    Ok(meta)
}

/// An OAuth error body from the token endpoint (`invalid_grant`, `invalid_client`, ...).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Deserialize)]
#[error("{error}: {}", error_description.as_deref().unwrap_or("no description"))]
pub struct OAuthError {
    pub error: String,
    pub error_description: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<u64>,
    scope: Option<String>,
    refresh_token: Option<String>,
}

async fn token_request(
    http: &reqwest::Client,
    meta: &Metadata,
    form: &[(&str, &str)],
    client_id: &str,
    previous_refresh: Option<&str>,
) -> Result<Tokens> {
    let response = http
        .post(&meta.token_endpoint)
        .form(form)
        .send()
        .await
        .with_context(|| format!("calling {}", meta.token_endpoint))?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        if let Ok(err) = serde_json::from_str::<OAuthError>(&body) {
            return Err(err.into());
        }
        bail!(
            "token endpoint answered HTTP {status}: {}",
            truncate(&body, 200)
        );
    }
    let parsed: TokenResponse =
        serde_json::from_str(&body).context("decoding the token response")?;
    Ok(Tokens {
        access_token: parsed.access_token,
        refresh_token: parsed
            .refresh_token
            .or_else(|| previous_refresh.map(str::to_string)),
        expires_at: now_unix() + parsed.expires_in.unwrap_or(3600),
        scope: parsed.scope.unwrap_or_default(),
        client_id: client_id.to_string(),
        gateway: None,
    })
}

/// Trades the refresh token for a new pair. The caller must store the result at
/// once: the gateway rotates the refresh token on every use.
pub async fn refresh(
    http: &reqwest::Client,
    meta: &Metadata,
    tokens: &Tokens,
    resource: &Url,
) -> Result<Tokens> {
    let refresh_token = tokens
        .refresh_token
        .as_deref()
        .ok_or_else(|| anyhow!("no refresh token stored"))?;
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", tokens.client_id.as_str()),
        ("resource", resource.as_str()),
    ];
    let mut fresh =
        token_request(http, meta, &form, &tokens.client_id, Some(refresh_token)).await?;
    fresh.gateway = tokens.gateway.clone();
    Ok(fresh)
}

/// Revokes `token` (RFC 7009); the gateway's revocation endpoint is `/token`.
pub async fn revoke(
    http: &reqwest::Client,
    meta: &Metadata,
    token: &str,
    client_id: &str,
) -> Result<()> {
    let endpoint = meta
        .revocation_endpoint
        .as_deref()
        .unwrap_or(&meta.token_endpoint);
    let response = http
        .post(endpoint)
        .form(&[("token", token), ("client_id", client_id)])
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("revocation answered HTTP {}", response.status());
    }
    Ok(())
}

// -- client registration -------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedClient {
    pub client_id: String,
    pub issued_at: u64,
}

/// DCR registrations per gateway URL, in the instance's cache directory. A
/// `client_id` is not a secret; losing the file only costs one registration.
///
/// `legacy` names the stand-alone client's cache (same format): when `path`
/// does not exist yet and `legacy` does, it is copied over once, so a
/// migrated sign-in keeps the registration its refresh token belongs to.
pub struct ClientCache {
    pub path: PathBuf,
    pub legacy: Option<PathBuf>,
}

impl ClientCache {
    pub fn new(path: PathBuf) -> ClientCache {
        ClientCache { path, legacy: None }
    }

    pub fn with_legacy(path: PathBuf, legacy: PathBuf) -> ClientCache {
        ClientCache {
            path,
            legacy: Some(legacy),
        }
    }

    fn read(&self) -> BTreeMap<String, CachedClient> {
        let parse = |text: String| serde_json::from_str(&text).ok();
        match std::fs::read_to_string(&self.path) {
            Ok(text) => parse(text).unwrap_or_default(),
            Err(_) => {
                let Some(entries) = self
                    .legacy
                    .as_ref()
                    .and_then(|legacy| std::fs::read_to_string(legacy).ok())
                    .and_then(parse)
                else {
                    return BTreeMap::new();
                };
                // Best effort: an unwritable cache only costs a registration.
                let _ = self.write(&entries);
                entries
            }
        }
    }

    fn write(&self, entries: &BTreeMap<String, CachedClient>) -> Result<()> {
        write_atomic(&self.path, &serde_json::to_string_pretty(entries)?, false)
            .with_context(|| format!("writing {}", self.path.display()))
    }

    /// A registration young enough to still be valid at the gateway.
    pub fn get(&self, gateway: &Url) -> Option<CachedClient> {
        self.read()
            .remove(gateway.as_str())
            .filter(|c| now_unix().saturating_sub(c.issued_at) < REGISTRATION_MAX_AGE_SECONDS)
    }

    pub fn put(&self, gateway: &Url, client: CachedClient) -> Result<()> {
        let mut entries = self.read();
        entries.insert(gateway.to_string(), client);
        self.write(&entries)
    }

    pub fn remove(&self, gateway: &Url) -> Result<()> {
        let mut entries = self.read();
        if entries.remove(gateway.as_str()).is_some() {
            self.write(&entries)?;
        }
        Ok(())
    }
}

/// Registers a public client (`token_endpoint_auth_method: none`; left out, the
/// gateway would default to `client_secret_basic` and issue a secret).
pub async fn register(
    http: &reqwest::Client,
    meta: &Metadata,
    client_name: &str,
) -> Result<CachedClient> {
    let endpoint = meta
        .registration_endpoint
        .as_deref()
        .ok_or_else(|| anyhow!("the gateway advertises no registration endpoint"))?;
    let body = serde_json::json!({
        "client_name": client_name,
        "redirect_uris": [REGISTERED_REDIRECT],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let response = http
        .post(endpoint)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("calling {endpoint}"))?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        bail!(
            "client registration answered HTTP {status}: {}",
            truncate(&text, 200)
        );
    }
    #[derive(Deserialize)]
    struct Registered {
        client_id: String,
    }
    let registered: Registered =
        serde_json::from_str(&text).context("decoding the registration response")?;
    Ok(CachedClient {
        client_id: registered.client_id,
        issued_at: now_unix(),
    })
}

// -- PKCE and the authorization request ---------------------------------------

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Pkce {
        let verifier = random_token(32);
        let challenge = challenge_for(&verifier);
        Pkce {
            verifier,
            challenge,
        }
    }
}

/// S256: base64url(SHA-256(verifier)), no padding (RFC 7636 section 4.2).
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[allow(clippy::too_many_arguments)]
pub fn authorize_url(
    meta: &Metadata,
    client_id: &str,
    scopes: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
    resource: &Url,
) -> Result<Url> {
    let mut url =
        Url::parse(&meta.authorization_endpoint).context("parsing the authorization endpoint")?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", scopes)
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("resource", resource.as_str());
    Ok(url)
}

// -- loopback listener ---------------------------------------------------------

/// A one-shot HTTP listener on `127.0.0.1` at an OS-chosen port.
pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

enum Callback {
    Code(String),
    Denied(String),
    Ignored,
}

impl Loopback {
    pub async fn bind() -> Result<Loopback> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("binding the loopback listener")?;
        let port = listener.local_addr()?.port();
        Ok(Loopback { listener, port })
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{CALLBACK_PATH}", self.port)
    }

    /// Serves requests until one carries our `state` with a code or an error.
    /// Other requests (favicon, a stale tab's callback) get an answer and are
    /// otherwise ignored.
    /// `require_iss` makes a callback without `iss` a refusal (RFC 9207, when
    /// the metadata advertises it). A connection that sends nothing is dropped
    /// after [`CONNECTION_TIMEOUT`], so it cannot hold the listener.
    pub async fn wait_for_code(
        self,
        state: &str,
        issuer: &str,
        require_iss: bool,
    ) -> Result<String> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            let handled = tokio::time::timeout(
                CONNECTION_TIMEOUT,
                handle_callback(stream, state, issuer, require_iss),
            );
            match handled.await {
                Ok(Ok(Callback::Code(code))) => return Ok(code),
                Ok(Ok(Callback::Denied(reason))) => {
                    bail!("the gateway refused the login: {reason}")
                }
                _ => continue,
            }
        }
    }
}

async fn handle_callback(
    mut stream: TcpStream,
    state: &str,
    issuer: &str,
    require_iss: bool,
) -> Result<Callback> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < MAX_REQUEST_BYTES {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf);
    let target = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let url = Url::parse(&format!("http://127.0.0.1{target}"))?;
    if url.path() != CALLBACK_PATH {
        respond(&mut stream, "404 Not Found", "Not found.").await?;
        return Ok(Callback::Ignored);
    }
    let params: BTreeMap<String, String> = url.query_pairs().into_owned().collect();
    if params.get("state").map(String::as_str) != Some(state) {
        respond(
            &mut stream,
            "400 Bad Request",
            "This sign-in link is stale or was not started by MCP Studio.",
        )
        .await?;
        return Ok(Callback::Ignored);
    }
    if require_iss && !params.contains_key("iss") {
        respond(
            &mut stream,
            "400 Bad Request",
            "The authorization response did not say who issued it.",
        )
        .await?;
        return Ok(Callback::Denied(
            "the callback carried no iss, which this gateway promises".into(),
        ));
    }
    if let Some(iss) = params.get("iss")
        && iss.trim_end_matches('/') != issuer.trim_end_matches('/')
    {
        respond(
            &mut stream,
            "400 Bad Request",
            "The authorization response came from an unexpected issuer.",
        )
        .await?;
        return Ok(Callback::Denied(format!(
            "issuer mismatch: got {iss}, expected {issuer}"
        )));
    }
    if let Some(error) = params.get("error") {
        let reason = match params.get("error_description") {
            Some(description) => format!("{error}: {description}"),
            None => error.clone(),
        };
        respond(
            &mut stream,
            "400 Bad Request",
            &format!("Sign-in failed: {reason}"),
        )
        .await?;
        return Ok(Callback::Denied(reason));
    }
    match params.get("code") {
        Some(code) => {
            respond(
                &mut stream,
                "200 OK",
                "Signed in to the MCP gateway. You can close this tab.",
            )
            .await?;
            Ok(Callback::Code(code.clone()))
        }
        None => {
            respond(
                &mut stream,
                "400 Bad Request",
                "The callback carried no code.",
            )
            .await?;
            Ok(Callback::Ignored)
        }
    }
}

async fn respond(stream: &mut TcpStream, status: &str, message: &str) -> Result<()> {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>MCP Studio</title><p style=\"font:16px system-ui;margin:3em\">{}</p>",
        html_escape(message)
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// -- the whole login -----------------------------------------------------------

/// Runs the login end to end. `open` shows the authorization URL to the user
/// (the browser in production; a redirect-following HTTP GET in tests).
/// `timeout` is normally [`LOGIN_TIMEOUT`].
pub async fn login(
    http: &reqwest::Client,
    gateway: &Url,
    spec: &ClientSpec,
    cache: &ClientCache,
    timeout: Duration,
    open: impl FnOnce(&Url) -> Result<()>,
) -> Result<Tokens> {
    let meta = discover(http, gateway).await?;
    let resource = resource_for(gateway)?;
    let client = match cache.get(gateway) {
        Some(client) => client,
        None => {
            let client = register(http, &meta, &spec.client_name).await?;
            cache.put(gateway, client.clone())?;
            client
        }
    };
    let loopback = Loopback::bind().await?;
    let redirect_uri = loopback.redirect_uri();
    let pkce = Pkce::generate();
    let state = random_token(24);
    let url = authorize_url(
        &meta,
        &client.client_id,
        &spec.scopes,
        &redirect_uri,
        &state,
        &pkce.challenge,
        &resource,
    )?;
    open(&url)?;
    let require_iss = meta.authorization_response_iss_parameter_supported;
    let code = tokio::time::timeout(timeout, loopback.wait_for_code(&state, &meta.issuer, require_iss))
        .await
        .map_err(|_| {
            anyhow!(
                "no answer from the browser within {}s; the gateway shows most login failures on the browser page",
                timeout.as_secs()
            )
        })??;
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("code_verifier", pkce.verifier.as_str()),
        ("client_id", client.client_id.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("resource", resource.as_str()),
    ];
    let exchanged = token_request(http, &meta, &form, &client.client_id, None)
        .await
        .map(|tokens| Tokens {
            gateway: Some(gateway.to_string()),
            ..tokens
        });
    match exchanged {
        Err(err)
            if err
                .downcast_ref::<OAuthError>()
                .is_some_and(|e| e.error == "invalid_client") =>
        {
            cache.remove(gateway)?;
            Err(err.context(
                "the client registration is no longer valid; run login again to register afresh",
            ))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_matches_rfc7636_appendix_b() {
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorize_url_carries_pkce_scope_and_resource() {
        let meta = Metadata {
            issuer: "https://gw.example".into(),
            authorization_endpoint: "https://gw.example/authorize".into(),
            token_endpoint: "https://gw.example/token".into(),
            registration_endpoint: None,
            revocation_endpoint: None,
            authorization_response_iss_parameter_supported: false,
        };
        let resource = Url::parse("https://gw.example/mcp").unwrap();
        let url = authorize_url(
            &meta,
            "cid",
            "mcp-access gateway:admin",
            "http://127.0.0.1:5/callback",
            "st",
            "ch",
            &resource,
        )
        .unwrap();
        let params: BTreeMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(params["scope"], "mcp-access gateway:admin");
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(params["code_challenge"], "ch");
        assert_eq!(params["resource"], "https://gw.example/mcp");
        assert_eq!(params["redirect_uri"], "http://127.0.0.1:5/callback");
    }

    #[test]
    fn client_cache_expires_old_registrations() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ClientCache::new(dir.path().join("clients.json"));
        let gw = Url::parse("https://gw.example").unwrap();
        cache
            .put(
                &gw,
                CachedClient {
                    client_id: "old".into(),
                    issued_at: now_unix() - REGISTRATION_MAX_AGE_SECONDS - 1,
                },
            )
            .unwrap();
        assert!(cache.get(&gw).is_none());
        cache
            .put(
                &gw,
                CachedClient {
                    client_id: "new".into(),
                    issued_at: now_unix(),
                },
            )
            .unwrap();
        assert_eq!(cache.get(&gw).unwrap().client_id, "new");
        cache.remove(&gw).unwrap();
        assert!(cache.get(&gw).is_none());
    }

    #[tokio::test]
    async fn loopback_ignores_strays_and_returns_the_code() {
        let loopback = Loopback::bind().await.unwrap();
        let base = loopback.redirect_uri();
        let wait = tokio::spawn(loopback.wait_for_code("good", "https://gw.example", true));
        let http = reqwest::Client::new();
        let stray = http
            .get(format!("{base}?state=bad&code=x"))
            .send()
            .await
            .unwrap();
        assert_eq!(stray.status(), 400);
        let favicon = http
            .get(base.replace("/callback", "/favicon.ico"))
            .send()
            .await
            .unwrap();
        assert_eq!(favicon.status(), 404);
        let ok = http
            .get(format!(
                "{base}?state=good&code=the-code&iss=https%3A%2F%2Fgw.example"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), 200);
        assert_eq!(wait.await.unwrap().unwrap(), "the-code");
    }

    #[tokio::test]
    async fn loopback_reports_an_error_redirect() {
        let loopback = Loopback::bind().await.unwrap();
        let base = loopback.redirect_uri();
        let wait = tokio::spawn(loopback.wait_for_code("s", "https://gw.example", false));
        reqwest::get(format!(
            "{base}?state=s&error=access_denied&error_description=%3Cno%3E"
        ))
        .await
        .unwrap();
        let err = wait.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("access_denied: <no>"), "{err}");
    }

    #[tokio::test]
    async fn loopback_requires_iss_when_promised_and_drops_idle_connections() {
        let loopback = Loopback::bind().await.unwrap();
        let base = loopback.redirect_uri();
        let wait = tokio::spawn(loopback.wait_for_code("s", "https://gw.example", true));
        // An idle connection must not keep the real callback out.
        let _idle = tokio::net::TcpStream::connect(
            base.trim_start_matches("http://")
                .trim_end_matches("/callback"),
        )
        .await
        .unwrap();
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            reqwest::get(format!("{base}?state=s&code=c")),
        )
        .await
        .expect("the callback was served despite the idle connection")
        .unwrap();
        assert_eq!(response.status(), 400);
        assert!(
            wait.await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("no iss")
        );
    }

    #[test]
    fn the_legacy_client_cache_is_copied_once() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("old/clients.json");
        let gw = Url::parse("https://gw.example").unwrap();
        ClientCache::new(legacy.clone())
            .put(
                &gw,
                CachedClient {
                    client_id: "from-before".into(),
                    issued_at: now_unix(),
                },
            )
            .unwrap();
        let cache = ClientCache::with_legacy(dir.path().join("new/clients.json"), legacy.clone());
        assert_eq!(cache.get(&gw).unwrap().client_id, "from-before");
        assert!(cache.path.exists());
        assert!(legacy.exists(), "the old cache is left in place");
        // From now on the new file is the cache; the old one is not consulted.
        cache.remove(&gw).unwrap();
        assert!(cache.get(&gw).is_none());
    }

    #[test]
    fn metadata_endpoints_must_share_the_gateway_origin() {
        let gateway = Url::parse("https://gw.example/").unwrap();
        let mut meta = Metadata {
            issuer: "https://gw.example".into(),
            authorization_endpoint: "https://gw.example/authorize".into(),
            token_endpoint: "https://gw.example/token".into(),
            registration_endpoint: Some("https://gw.example/register".into()),
            revocation_endpoint: None,
            authorization_response_iss_parameter_supported: true,
        };
        assert!(meta.check_origin(&gateway).is_ok());
        meta.token_endpoint = "https://evil.example/token".into();
        assert!(
            meta.check_origin(&gateway)
                .unwrap_err()
                .to_string()
                .contains("evil.example")
        );
    }

    #[tokio::test]
    async fn loopback_rejects_a_foreign_issuer() {
        let loopback = Loopback::bind().await.unwrap();
        let base = loopback.redirect_uri();
        let wait = tokio::spawn(loopback.wait_for_code("s", "https://gw.example", false));
        reqwest::get(format!(
            "{base}?state=s&code=c&iss=https%3A%2F%2Fevil.example"
        ))
        .await
        .unwrap();
        assert!(
            wait.await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("issuer mismatch")
        );
    }
}
