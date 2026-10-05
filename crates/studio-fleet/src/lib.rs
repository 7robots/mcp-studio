//! Fleet discovery and live status.
//!
//! [`discover`] works out which repos are fleet members and what their files
//! say (Worker name, routes, public URL, scopes, version). [`fleet_status`]
//! runs every enabled probe source over them concurrently and returns a
//! [`FleetReport`], served from a short-lived cache unless asked to refresh.
//!
//! Two sources are pluggable so this crate stays independent of the crates
//! that implement them: the gateway registry ([`GatewaySource`]) and pattern
//! conformance ([`RepoChecker`]). Without them those sources report `skip`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use studio_core::check::{Check, Status, rollup};
use studio_core::config::{GatewayConfig, ProbeConfig};
use studio_core::{Instance, Secret};

pub mod cache;
pub mod cloudflare;
pub mod discover;
pub mod github;
pub mod okta;
pub mod probes;
pub mod time;
pub mod wrangler;

pub use discover::{Discovery, EnvInfo, ServerInfo};

use cloudflare::{CfAccount, CloudflareApi};
use github::GithubApi;
use probes::{Catalogs, ProbeOut};

/// A probe source: one column of the status matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Git,
    Github,
    Cloudflare,
    Http,
    Okta,
    Gateway,
    Marketplace,
    Pattern,
}

impl Source {
    pub const ALL: [Source; 8] = [
        Source::Git,
        Source::Github,
        Source::Cloudflare,
        Source::Http,
        Source::Okta,
        Source::Gateway,
        Source::Marketplace,
        Source::Pattern,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Git => "git",
            Source::Github => "github",
            Source::Cloudflare => "cloudflare",
            Source::Http => "http",
            Source::Okta => "okta",
            Source::Gateway => "gateway",
            Source::Marketplace => "marketplace",
            Source::Pattern => "pattern",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        match s.as_str() {
            "cf" => Some(Source::Cloudflare),
            "gh" => Some(Source::Github),
            "mp" | "marketplaces" => Some(Source::Marketplace),
            _ => Self::ALL.into_iter().find(|x| x.as_str() == s),
        }
    }

    /// Whether `[probes]` enables this source. `pattern` has no switch: it
    /// runs whenever a [`RepoChecker`] is provided.
    pub fn enabled(self, p: &ProbeConfig) -> bool {
        match self {
            Source::Git => p.git,
            Source::Github => p.github,
            Source::Cloudflare => p.cloudflare,
            Source::Http => p.http,
            Source::Okta => p.okta,
            Source::Gateway => p.gateway,
            Source::Marketplace => p.marketplace,
            Source::Pattern => true,
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One server as the gateway's `list_servers` tool reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayServerInfo {
    pub id: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub health: Option<String>,
    #[serde(default)]
    pub access: Option<String>,
    #[serde(default)]
    pub auth_mode: Option<String>,
    #[serde(default)]
    pub last_refresh_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    /// The version the server reported to the gateway; absent from an older
    /// gateway.
    #[serde(default)]
    pub server_version: Option<String>,
}

/// The gateway registry, implemented outside this crate (by the gateway client).
pub trait GatewaySource: Send + Sync {
    /// Every server registered on `gateway`. Called once per gateway per run.
    fn list_servers<'a>(
        &'a self,
        gateway: &'a GatewayConfig,
    ) -> BoxFuture<'a, Result<Vec<GatewayServerInfo>, String>>;
}

/// Pattern conformance for one local clone, implemented outside this crate.
/// Runs on a blocking thread.
pub trait RepoChecker: Send + Sync {
    fn check(&self, repo_dir: &Path) -> Vec<Check>;
}

/// The pluggable sources. `Default` is "neither wired".
#[derive(Clone, Default)]
pub struct Providers {
    pub gateway: Option<Arc<dyn GatewaySource>>,
    pub repo_checker: Option<Arc<dyn RepoChecker>>,
}

impl Providers {
    pub fn new(
        gateway: Option<Box<dyn GatewaySource>>,
        repo_checker: Option<Box<dyn RepoChecker>>,
    ) -> Self {
        Self {
            gateway: gateway.map(Arc::from),
            repo_checker: repo_checker.map(Arc::from),
        }
    }

    fn key(&self) -> Vec<String> {
        let mut k = Vec::new();
        if self.gateway.is_some() {
            k.push("gateway".into());
        }
        if self.repo_checker.is_some() {
            k.push("pattern".into());
        }
        k
    }
}

/// API bases and credential overrides. Defaults are the public APIs with
/// credentials from config; tests point these at local mocks.
#[derive(Clone)]
pub struct Endpoints {
    pub github_api: String,
    pub cloudflare_api: String,
    /// Use this instead of `gh auth token`.
    pub github_token: Option<Secret>,
    /// Use this instead of resolving `cloudflare.api_token`.
    pub cloudflare_token: Option<Secret>,
    pub http_timeout: Duration,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            github_api: "https://api.github.com".into(),
            cloudflare_api: "https://api.cloudflare.com/client/v4".into(),
            github_token: None,
            cloudflare_token: None,
            http_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Clone, Default)]
pub struct StatusOptions {
    /// Limit to these sources (still subject to `[probes]`). `None` = all enabled.
    pub sources: Option<BTreeSet<Source>>,
    /// Limit to these servers (repo name or `owner/repo`). Empty = all.
    pub servers: Vec<String>,
    /// Ignore the cache.
    pub refresh: bool,
    /// Run `git fetch` in each clone before reading ahead/behind.
    pub fetch: bool,
    pub endpoints: Endpoints,
}

/// One matrix cell: a source's rollup for a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub source: Source,
    pub status: Status,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerStatus {
    pub repo: String,
    pub name: String,
    pub url: Option<String>,
    pub gateway_id: Option<String>,
    pub local_dir: PathBuf,
    /// Every check from every source, in source order.
    pub checks: Vec<Check>,
    /// One cell per source that ran, in source order.
    pub columns: Vec<Column>,
    /// Facts gathered along the way (`git.head`, `github.head`, `cf.deployed.commit`, …).
    pub facts: BTreeMap<String, String>,
    /// The derived facts the probes worked from.
    pub info: ServerInfo,
}

impl ServerStatus {
    pub fn rollup(&self) -> Status {
        rollup(&self.checks)
    }
    pub fn column(&self, s: Source) -> Option<&Column> {
        self.columns.iter().find(|c| c.source == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetReport {
    pub instance: String,
    /// RFC 3339, UTC.
    pub generated_at: String,
    pub generated_at_unix: i64,
    /// The sources that ran, in column order.
    pub sources: Vec<Source>,
    pub servers: Vec<ServerStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// True when served from the cache rather than probed now.
    #[serde(default)]
    pub from_cache: bool,
}

impl FleetReport {
    /// `(server name, check)` for every failing or warning check.
    pub fn problems(&self) -> impl Iterator<Item = (&str, &Check)> {
        self.servers.iter().flat_map(|s| {
            s.checks
                .iter()
                .filter(|c| matches!(c.status, Status::Fail | Status::Warn))
                .map(move |c| (s.name.as_str(), c))
        })
    }
    pub fn count(&self, status: Status) -> usize {
        self.servers
            .iter()
            .flat_map(|s| &s.checks)
            .filter(|c| c.status == status)
            .count()
    }
}

/// The shared HTTP client: 10 s timeouts, a Studio user agent.
pub fn http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .user_agent(concat!("mcp-studio/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

async fn github_api(
    inst: &Instance,
    ep: &Endpoints,
    client: &reqwest::Client,
) -> Result<GithubApi, String> {
    let token = match &ep.github_token {
        Some(t) => t.clone(),
        None => {
            let account = inst.config.github.account.clone();
            tokio::task::spawn_blocking(move || studio_core::exec::github_token(account.as_deref()))
                .await
                .map_err(|_| "token lookup panicked".to_string())?
                .map_err(|e| e.to_string())?
        }
    };
    Ok(GithubApi::new(&ep.github_api, Some(token), client.clone()))
}

/// Discover the fleet (config + GitHub topic discovery).
pub async fn discover(inst: &Instance, endpoints: &Endpoints) -> Discovery {
    let client = http_client(endpoints.http_timeout);
    let gh = github_api(inst, endpoints, &client).await;
    let mut d = discover::discover(inst, gh.as_ref().ok()).await;
    if let Err(e) = &gh {
        d.notes.insert(0, format!("GitHub token: {e}"));
    }
    d
}

/// The sources a run covers: requested ∩ enabled (+ pattern when a checker exists).
pub fn active_sources(inst: &Instance, opts: &StatusOptions, providers: &Providers) -> Vec<Source> {
    Source::ALL
        .into_iter()
        .filter(|s| s.enabled(&inst.config.probes))
        .filter(|s| *s != Source::Pattern || providers.repo_checker.is_some())
        .filter(|s| opts.sources.as_ref().is_none_or(|w| w.contains(s)))
        .collect()
}

/// Select servers by repo name or `owner/repo` (case-insensitive).
pub fn select<'a>(servers: &'a [ServerInfo], names: &[String]) -> Vec<&'a ServerInfo> {
    servers
        .iter()
        .filter(|s| {
            names.is_empty()
                || names
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(&s.name) || n.eq_ignore_ascii_case(&s.repo))
        })
        .collect()
}

/// The entry point for the CLI and the TUI: discovery + probes, through the
/// cache at `<cache_dir>/fleet.json` (fresh for `probes.ttl_seconds`).
pub async fn fleet_status(
    inst: &Instance,
    opts: &StatusOptions,
    providers: &Providers,
) -> FleetReport {
    let sources = active_sources(inst, opts, providers);
    let key = providers.key();
    if !opts.refresh
        && let Some(r) = cache::load(inst, &sources, &opts.servers, &key, opts.fetch)
    {
        return r;
    }
    let disc = discover(inst, &opts.endpoints).await;
    let report = probe(inst, &disc, opts, providers).await;
    if opts.servers.is_empty() && opts.sources.is_none() {
        cache::store(inst, &report, &key, opts.fetch);
    }
    report
}

/// Everything a run fetches once and shares across servers.
struct Shared {
    sources: Vec<Source>,
    fetch: bool,
    client: reqwest::Client,
    github: Result<GithubApi, String>,
    topic: Option<String>,
    cf: Result<(CloudflareApi, CfAccount), (Status, String)>,
    okta: Result<okta::OktaData, String>,
    gateways: BTreeMap<String, Result<Vec<GatewayServerInfo>, String>>,
    gateway_wired: bool,
    catalogs: Vec<Catalogs>,
    checker: Option<Arc<dyn RepoChecker>>,
}

/// Run the probes over already-discovered servers (no cache).
pub async fn probe(
    inst: &Instance,
    disc: &Discovery,
    opts: &StatusOptions,
    providers: &Providers,
) -> FleetReport {
    let sources = active_sources(inst, opts, providers);
    let want = |s: Source| sources.contains(&s);
    let ep = &opts.endpoints;
    let client = http_client(ep.http_timeout);
    let cfg = &inst.config;
    let mut notes = disc.notes.clone();

    let selected: Vec<ServerInfo> = select(&disc.servers, &opts.servers)
        .into_iter()
        .cloned()
        .collect();
    for n in &opts.servers {
        if !disc
            .servers
            .iter()
            .any(|s| n.eq_ignore_ascii_case(&s.name) || n.eq_ignore_ascii_case(&s.repo))
        {
            notes.push(format!("no fleet server named {n}"));
        }
    }

    let github_fut = async {
        if want(Source::Github) || want(Source::Cloudflare) {
            github_api(inst, ep, &client).await
        } else {
            Err("github source not selected".into())
        }
    };
    let cf_fut = async {
        if !want(Source::Cloudflare) {
            return Err((Status::Skip, "cloudflare source not selected".into()));
        }
        load_cloudflare(inst, ep, &client).await
    };
    let okta_fut = async {
        if !want(Source::Okta) {
            return Err("okta source not selected".to_string());
        }
        match &cfg.identity.okta {
            None => Err("no [identity.okta] in studio.toml".into()),
            Some(o) if o.admin_api.is_none() => {
                Err("identity.okta.admin_api not configured".into())
            }
            Some(o) => okta::load(o).await,
        }
    };
    let gw_fut = async {
        let mut out = BTreeMap::new();
        if let (true, Some(src)) = (want(Source::Gateway), &providers.gateway) {
            let ids: BTreeSet<&str> = selected
                .iter()
                .filter_map(|s| s.gateway.as_deref())
                .collect();
            for id in ids {
                if let Some(g) = cfg.gateway(Some(id)) {
                    out.insert(id.to_string(), src.list_servers(g).await);
                }
            }
        }
        out
    };
    // Both of these read 1Password; keep them from contending with each other.
    let secrets_fut = async { (cf_fut.await, okta_fut.await) };
    let (github, (cf, okta), gateways) = tokio::join!(github_fut, secrets_fut, gw_fut);
    let catalogs = if want(Source::Marketplace) {
        cfg.marketplaces
            .iter()
            .map(|m| probes::load_catalogs(m, &inst.repo_dir(&m.repo)))
            .collect()
    } else {
        Vec::new()
    };

    let shared = Arc::new(Shared {
        sources: sources.clone(),
        fetch: opts.fetch,
        client,
        github,
        topic: cfg
            .fleet
            .discover
            .as_ref()
            .and_then(|d| d.github_topic.clone()),
        cf,
        okta,
        gateways,
        gateway_wired: providers.gateway.is_some(),
        catalogs,
        checker: providers.repo_checker.clone(),
    });

    let servers = futures::future::join_all(
        selected
            .into_iter()
            .map(|s| probe_server(s, shared.clone())),
    )
    .await;

    let now = time::now_unix();
    FleetReport {
        instance: inst.name().to_string(),
        generated_at: time::format_rfc3339(now),
        generated_at_unix: now,
        sources,
        servers,
        notes,
        from_cache: false,
    }
}

async fn load_cloudflare(
    inst: &Instance,
    ep: &Endpoints,
    client: &reqwest::Client,
) -> Result<(CloudflareApi, CfAccount), (Status, String)> {
    let Some(cf) = &inst.config.cloudflare else {
        return Err((Status::Skip, "no [cloudflare] in studio.toml".into()));
    };
    let token = match &ep.cloudflare_token {
        Some(t) => t.clone(),
        None => {
            let r = cf.api_token.clone();
            match tokio::task::spawn_blocking(move || r.resolve()).await {
                Ok(Ok(t)) => t,
                Ok(Err(studio_core::secret::SecretError::Locked)) => {
                    return Err((Status::Skip, "cloudflare token: 1Password is locked".into()));
                }
                _ => return Err((Status::Skip, "cloudflare token not configured".into())),
            }
        }
    };
    let api = CloudflareApi::new(&ep.cloudflare_api, token, client.clone());
    match api.load(&cf.account_id).await {
        Ok(acct) => Ok((api, acct)),
        Err(e) => Err((Status::Skip, format!("Cloudflare API: {e}"))),
    }
}

async fn probe_server(s: ServerInfo, sh: Arc<Shared>) -> ServerStatus {
    let want = |x: Source| sh.sources.contains(&x);

    let git = async {
        if want(Source::Git) {
            Some(probes::probe_git(&s, sh.fetch).await)
        } else {
            None
        }
    };
    let gh_cf = async {
        let gh = if want(Source::Github) {
            Some(
                probes::probe_github(
                    &s,
                    sh.github.as_ref().map_err(String::as_str),
                    sh.topic.as_deref(),
                )
                .await,
            )
        } else {
            None
        };
        let cf = if want(Source::Cloudflare) {
            let head = match &gh {
                Some(o) => fact(o, "github.head"),
                // github column off: still compare against GitHub HEAD if we can
                None => match &sh.github {
                    Ok(api) => match api.repo(&s.repo).await {
                        Ok(Some(i)) => api
                            .branch_head(&s.repo, &i.default_branch)
                            .await
                            .ok()
                            .flatten(),
                        _ => None,
                    },
                    Err(_) => None,
                },
            };
            Some(probes::probe_cloudflare(&s, sh.cf.as_ref(), head.as_deref()).await)
        } else {
            None
        };
        (gh, cf)
    };
    let http = async {
        if want(Source::Http) {
            Some(probes::probe_http(&s, &sh.client).await)
        } else {
            None
        }
    };
    let pattern = async {
        if !want(Source::Pattern) {
            return None;
        }
        let Some(checker) = sh.checker.clone() else {
            return Some(ProbeOut::skip("pattern", "pattern checker not wired"));
        };
        if !s.local_exists {
            return Some(ProbeOut::skip("pattern", "no local clone"));
        }
        let dir = s.local_dir.clone();
        Some(
            match tokio::task::spawn_blocking(move || checker.check(&dir)).await {
                Ok(cs) if cs.is_empty() => ProbeOut {
                    checks: vec![Check::pass("pattern", "conforms")],
                    ..Default::default()
                },
                Ok(cs) => {
                    let n = cs.iter().filter(|c| c.status == Status::Pass).count();
                    ProbeOut {
                        short: Some(format!("{n}/{} ok", cs.len())),
                        checks: cs,
                        ..Default::default()
                    }
                }
                Err(_) => ProbeOut::skip("pattern", "pattern checker panicked"),
            },
        )
    };
    let (git, (github, cf), http, pattern) = tokio::join!(git, gh_cf, http, pattern);

    let okta = want(Source::Okta)
        .then(|| probes::probe_okta(&s, sh.okta.as_ref().map_err(String::as_str)));
    let gateway = want(Source::Gateway).then(|| {
        if !sh.gateway_wired {
            return ProbeOut::skip("gateway.registered", "gateway client not wired");
        }
        let Some(gid) = &s.gateway else {
            return ProbeOut::skip("gateway.registered", "no [[gateway]] configured");
        };
        match sh.gateways.get(gid) {
            None => ProbeOut::skip(
                "gateway.registered",
                format!("gateway {gid} not configured"),
            ),
            Some(Err(e)) => ProbeOut::skip("gateway.registered", format!("gateway {gid}: {e}")),
            Some(Ok(list)) => probes::probe_gateway(&s, Ok(list)),
        }
    });
    let marketplace =
        want(Source::Marketplace).then(|| probes::probe_marketplace(&s, &sh.catalogs));

    let mut checks = Vec::new();
    let mut columns = Vec::new();
    let mut facts = BTreeMap::new();
    for (src, out) in [
        (Source::Git, git),
        (Source::Github, github),
        (Source::Cloudflare, cf),
        (Source::Http, http),
        (Source::Okta, okta),
        (Source::Gateway, gateway),
        (Source::Marketplace, marketplace),
        (Source::Pattern, pattern),
    ] {
        let Some(out) = out else { continue };
        let status = out.status();
        columns.push(Column {
            source: src,
            status,
            text: cell_text(&out, status),
        });
        facts.extend(out.facts);
        checks.extend(out.checks);
    }
    ServerStatus {
        repo: s.repo.clone(),
        name: s.name.clone(),
        url: s.url.clone(),
        gateway_id: s.gateway_id.clone(),
        local_dir: s.local_dir.clone(),
        checks,
        columns,
        facts,
        info: s,
    }
}

fn fact(o: &ProbeOut, k: &str) -> Option<String> {
    o.facts.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone())
}

fn cell_text(o: &ProbeOut, status: Status) -> String {
    match status {
        Status::Pass => o.short.clone().unwrap_or_else(|| "ok".into()),
        Status::Skip if o.skip_short.is_some() => o.skip_short.clone().unwrap_or_default(),
        _ => o
            .checks
            .iter()
            .find(|c| c.status == status)
            .map(|c| c.summary.clone())
            .unwrap_or_default(),
    }
}
