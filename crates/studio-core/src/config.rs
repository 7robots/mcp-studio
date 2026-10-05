//! The instance configuration: `studio.toml` in an instance directory.
//!
//! Everything org-specific lives here, never in source. A field is optional
//! when a probe or module can run without it; the module that needs it says
//! so at the point of use (see [`StudioConfig::gateway`] and friends) rather
//! than failing the whole load.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::secret::SecretRef;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StudioConfig {
    pub instance: InstanceMeta,
    #[serde(default)]
    pub identity: Identity,
    pub cloudflare: Option<CloudflareConfig>,
    #[serde(default)]
    pub github: GithubConfig,
    #[serde(default, rename = "gateway")]
    pub gateways: Vec<GatewayConfig>,
    pub pattern: Option<PatternConfig>,
    #[serde(default)]
    pub fleet: FleetConfig,
    #[serde(default, rename = "marketplace")]
    pub marketplaces: Vec<MarketplaceConfig>,
    #[serde(default)]
    pub probes: ProbeConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InstanceMeta {
    /// Short slug: used in Keychain account names and cache paths.
    pub name: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub okta: Option<OktaConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OktaConfig {
    /// `https://<org>.okta.com`
    pub domain: String,
    /// Custom authorization server id (`aus…`), or `default`.
    pub authorization_server: String,
    /// The `aud` the fleet's M2M tokens carry.
    pub audience: Option<String>,
    /// The shared interactive client every server's `/callback` is registered on.
    pub interactive_client_id: Option<String>,
    pub m2m_client_id: Option<String>,
    pub m2m_client_secret: Option<SecretRef>,
    /// Access-policy rules that must grant each server's scopes.
    #[serde(default)]
    pub policy_rules: Vec<String>,
    /// Read-only Management API access through an external helper.
    pub admin_api: Option<OktaAdminApi>,
}

impl OktaConfig {
    pub fn issuer(&self) -> String {
        format!(
            "{}/oauth2/{}",
            self.domain.trim_end_matches('/'),
            self.authorization_server
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OktaAdminApi {
    /// Executable that takes `GET <path>` (e.g. `okta-api`).
    pub tool: String,
    /// Passed as `--org <org>`.
    pub org: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CloudflareConfig {
    /// An account id, or `"resolve"` to take the token's only account.
    #[serde(default = "default_resolve")]
    pub account_id: String,
    pub api_token: SecretRef,
    /// The `<sub>` in `<worker>.<sub>.workers.dev`.
    pub workers_subdomain: Option<String>,
}

fn default_resolve() -> String {
    "resolve".into()
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    /// `gh` account whose token is used by default (`gh auth token -u`).
    pub account: Option<String>,
    #[serde(default)]
    pub orgs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    pub id: String,
    /// Origin of the gateway (`https://gateway.example.org`).
    pub url: String,
    /// `owner/repo` of the gateway's source.
    pub repo: Option<String>,
    /// D1 database name, for read-only `wrangler d1` views and gates.
    pub d1_database: Option<String>,
    /// A disposable server the acceptance gate registers and deletes.
    pub fixture_url: Option<String>,
    #[serde(default = "default_admin_scope")]
    pub admin_scope: String,
    #[serde(default = "default_access_scope")]
    pub access_scope: String,
    /// The tool whose presence in `tools/list` proves admin rights.
    #[serde(default = "default_admin_probe_tool")]
    pub admin_probe_tool: String,
    /// `client_name` sent at dynamic client registration.
    #[serde(default = "default_client_name")]
    pub client_name: String,
    /// Polling interval for the current gateway screen; 0 disables.
    #[serde(default = "default_refresh_seconds")]
    pub refresh_seconds: u64,
    /// Example URL shown in the register form.
    pub server_url_hint: Option<String>,
}

fn default_admin_scope() -> String {
    "gateway:admin".into()
}
fn default_access_scope() -> String {
    "mcp-access".into()
}
fn default_admin_probe_tool() -> String {
    "register_server".into()
}
fn default_client_name() -> String {
    "mcp-studio".into()
}
fn default_refresh_seconds() -> u64 {
    30
}

impl GatewayConfig {
    /// Scopes requested at sign-in.
    pub fn login_scopes(&self) -> String {
        format!("{} {}", self.access_scope, self.admin_scope)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PatternConfig {
    /// Pack name under `patterns/` (e.g. `cf-workers-ts`).
    pub pack: String,
    /// Directory of the pack. Relative paths resolve against the instance dir.
    pub source: PathBuf,
    /// Values substituted into the template, relative to the instance dir.
    #[serde(default = "default_values_file")]
    pub values: PathBuf,
    /// Blessed per-repo diffs, relative to the instance dir.
    #[serde(default = "default_conformance_dir")]
    pub conformance_dir: PathBuf,
    /// Hostname suffix every server's custom domain ends with.
    pub domain_suffix: Option<String>,
}

fn default_values_file() -> PathBuf {
    "pattern-values.toml".into()
}
fn default_conformance_dir() -> PathBuf {
    "conformance".into()
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FleetConfig {
    /// Where repos are cloned locally (`<repos_dir>/<repo name>`).
    #[serde(default = "default_repos_dir")]
    pub repos_dir: PathBuf,
    pub discover: Option<Discover>,
    /// Extra `owner/repo`s merged with discovery.
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default = "default_true")]
    pub exclude_archived: bool,
    #[serde(default, rename = "server")]
    pub servers: Vec<FleetServer>,
}

impl Default for FleetConfig {
    fn default() -> Self {
        Self {
            repos_dir: default_repos_dir(),
            discover: None,
            include: Vec::new(),
            exclude: Vec::new(),
            exclude_archived: true,
            servers: Vec::new(),
        }
    }
}

fn default_repos_dir() -> PathBuf {
    "~/GitHub".into()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Discover {
    /// Repos in `github.orgs` carrying this topic are fleet members.
    pub github_topic: Option<String>,
    /// …and that contain this file at the root of the default branch.
    pub require_file: Option<String>,
}

/// Per-server facts and overrides. Anything left out is derived from the repo.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FleetServer {
    /// `owner/repo`
    pub repo: String,
    /// Registry id on the gateway, when it differs from the derived one.
    pub gateway_id: Option<String>,
    /// Which `[[gateway]]` this server is registered on (default: the first).
    pub gateway: Option<String>,
    /// Public MCP endpoint, when it can't be read from `wrangler.toml`.
    pub url: Option<String>,
    /// Local checkout, when not `<repos_dir>/<repo name>`.
    pub local_path: Option<PathBuf>,
    /// Wrangler environments deployed besides the top level.
    #[serde(default)]
    pub environments: Vec<String>,
    /// Marketplace slug, when it differs from the repo name.
    pub marketplace_slug: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PublishMode {
    /// Read branch protection: push when allowed, otherwise open a PR.
    #[default]
    Auto,
    Push,
    Pr,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MarketplaceConfig {
    pub id: String,
    /// `owner/repo`
    pub repo: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    #[serde(default = "default_targets")]
    pub targets: Vec<Target>,
    /// Catalog name users type after `@`; default: the repo name.
    pub catalog_name: Option<String>,
    /// Claude `owner.name` / Codex `interface.displayName`.
    pub owner_name: String,
    /// `author.name` in plugin manifests; default: `owner_name`.
    pub author_name: Option<String>,
    /// `gh` account for this repo; default: `github.account`.
    pub github_account: Option<String>,
    #[serde(default)]
    pub publish: PublishMode,
    /// Local clone, when not `<repos_dir>/<repo name>`.
    pub local_path: Option<PathBuf>,
    #[serde(default = "default_placeholder")]
    pub credential_placeholder: String,
    /// Replaces the default reserved-slug list when set.
    pub reserved_slugs: Option<Vec<String>>,
}

fn default_branch() -> String {
    "main".into()
}
fn default_targets() -> Vec<Target> {
    vec![Target::Claude, Target::Codex]
}
fn default_placeholder() -> String {
    "<YOUR_API_KEY>".into()
}

impl MarketplaceConfig {
    pub fn catalog_name(&self) -> &str {
        self.catalog_name
            .as_deref()
            .unwrap_or_else(|| repo_name(&self.repo))
    }
    pub fn author_name(&self) -> &str {
        self.author_name.as_deref().unwrap_or(&self.owner_name)
    }
}

/// Which probe sources run, and how long their results stay fresh.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProbeConfig {
    #[serde(default = "default_true")]
    pub git: bool,
    #[serde(default = "default_true")]
    pub github: bool,
    #[serde(default = "default_true")]
    pub cloudflare: bool,
    #[serde(default = "default_true")]
    pub http: bool,
    #[serde(default = "default_true")]
    pub okta: bool,
    #[serde(default = "default_true")]
    pub gateway: bool,
    #[serde(default = "default_true")]
    pub marketplace: bool,
    #[serde(default = "default_ttl")]
    pub ttl_seconds: u64,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            git: true,
            github: true,
            cloudflare: true,
            http: true,
            okta: true,
            gateway: true,
            marketplace: true,
            ttl_seconds: default_ttl(),
        }
    }
}

fn default_ttl() -> u64 {
    300
}

/// `owner/repo` → `repo`.
pub fn repo_name(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

/// A semantic problem found after parsing. `path` is a dotted key path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl StudioConfig {
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// The `[[gateway]]` with this id, or the first one when `id` is `None`.
    pub fn gateway(&self, id: Option<&str>) -> Option<&GatewayConfig> {
        match id {
            Some(id) => self.gateways.iter().find(|g| g.id == id),
            None => self.gateways.first(),
        }
    }

    pub fn marketplace(&self, id: &str) -> Option<&MarketplaceConfig> {
        self.marketplaces.iter().find(|m| m.id == id)
    }

    pub fn fleet_server(&self, repo: &str) -> Option<&FleetServer> {
        self.fleet
            .servers
            .iter()
            .find(|s| s.repo.eq_ignore_ascii_case(repo) || repo_name(&s.repo) == repo)
    }

    /// Every problem the type system can't catch. Empty means valid.
    pub fn validate(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        let mut p = |path: String, message: &str| {
            out.push(Problem {
                path,
                message: message.into(),
            })
        };

        if !is_slug(&self.instance.name) {
            p("instance.name".into(), "must be a lowercase slug ([a-z0-9-])");
        }
        if let Some(okta) = &self.identity.okta {
            if !is_https(&okta.domain) {
                p("identity.okta.domain".into(), "must be an https:// URL");
            }
        }

        let mut ids = BTreeSet::new();
        for (i, g) in self.gateways.iter().enumerate() {
            let at = format!("gateway[{i}]");
            if !ids.insert(g.id.as_str()) {
                p(format!("{at}.id"), "duplicate gateway id");
            }
            if !is_https_or_loopback(&g.url) {
                p(format!("{at}.url"), "must be https:// (http:// only for loopback)");
            }
            if let Some(r) = &g.repo {
                if !is_repo(r) {
                    p(format!("{at}.repo"), "must be owner/repo");
                }
            }
        }

        for (i, r) in self.fleet.include.iter().chain(&self.fleet.exclude).enumerate() {
            if !is_repo(r) {
                p(format!("fleet.include/exclude[{i}]"), "must be owner/repo");
            }
        }
        let mut repos = BTreeSet::new();
        for (i, s) in self.fleet.servers.iter().enumerate() {
            let at = format!("fleet.server[{i}]");
            if !is_repo(&s.repo) {
                p(format!("{at}.repo"), "must be owner/repo");
            }
            if !repos.insert(s.repo.to_ascii_lowercase()) {
                p(format!("{at}.repo"), "duplicate server");
            }
            if let Some(g) = &s.gateway {
                if self.gateway(Some(g)).is_none() {
                    p(format!("{at}.gateway"), "names no [[gateway]]");
                }
            }
            if let Some(u) = &s.url {
                if !is_https(u) {
                    p(format!("{at}.url"), "must be an https:// URL");
                }
            }
        }

        let mut mids = BTreeSet::new();
        for (i, m) in self.marketplaces.iter().enumerate() {
            let at = format!("marketplace[{i}]");
            if !mids.insert(m.id.as_str()) {
                p(format!("{at}.id"), "duplicate marketplace id");
            }
            if !is_repo(&m.repo) {
                p(format!("{at}.repo"), "must be owner/repo");
            }
            if m.targets.is_empty() {
                p(format!("{at}.targets"), "needs at least one of claude, codex");
            }
            if !is_slug(m.catalog_name()) {
                p(format!("{at}.catalog_name"), "must be a lowercase slug");
            }
        }
        out
    }
}

pub fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

pub fn is_repo(s: &str) -> bool {
    let mut parts = s.split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(o), Some(r), None) if !o.is_empty() && !r.is_empty())
}

pub fn is_https(s: &str) -> bool {
    url::Url::parse(s).is_ok_and(|u| u.scheme() == "https" && u.host().is_some())
}

pub fn is_https_or_loopback(s: &str) -> bool {
    match url::Url::parse(s) {
        Ok(u) if u.scheme() == "https" => u.host().is_some(),
        Ok(u) if u.scheme() == "http" => matches!(
            u.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]")
        ),
        _ => false,
    }
}

/// Expand a leading `~` and resolve a relative path against `base`.
pub fn resolve_path(base: &Path, p: &Path) -> PathBuf {
    let expanded = expand_tilde(p);
    if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    }
}

pub fn expand_tilde(p: &Path) -> PathBuf {
    let Some(s) = p.to_str() else {
        return p.to_path_buf();
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (s, home) {
        ("~", Some(h)) => h,
        (s, Some(h)) if s.starts_with("~/") => h.join(&s[2..]),
        _ => p.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const EXAMPLE: &str = include_str!("../../../examples/instance/studio.toml");

    #[test]
    fn example_instance_parses_and_validates() {
        let c = StudioConfig::parse(EXAMPLE).unwrap();
        assert_eq!(c.instance.name, "acme");
        assert_eq!(c.validate(), vec![]);
        let g = c.gateway(None).unwrap();
        assert_eq!(g.login_scopes(), "mcp-access gateway:admin");
        assert_eq!(g.client_name, "mcp-studio");
        assert_eq!(c.marketplaces[0].catalog_name(), "acme-plugin-marketplace");
        assert_eq!(
            c.identity.okta.as_ref().unwrap().issuer(),
            "https://acme.okta.example/oauth2/default"
        );
    }

    #[test]
    fn minimal_config_is_valid() {
        let c = StudioConfig::parse("[instance]\nname = \"x\"\n").unwrap();
        assert!(c.validate().is_empty());
        assert!(c.gateway(None).is_none());
        assert!(c.fleet.exclude_archived);
    }

    #[test]
    fn unknown_keys_are_refused_with_a_line() {
        let e = StudioConfig::parse("[instance]\nname = \"x\"\nnmae = 1\n").unwrap_err();
        assert!(e.to_string().contains("nmae"), "{e}");
        assert!(e.span().is_some());
    }

    #[test]
    fn semantic_problems_are_reported_by_path() {
        let c = StudioConfig::parse(
            r#"
[instance]
name = "Bad Name"
[[gateway]]
id = "g"
url = "http://gw.example.org"
[[gateway]]
id = "g"
url = "https://gw.example.org"
[[fleet.server]]
repo = "nope"
gateway = "missing"
[[marketplace]]
id = "m"
repo = "o/r"
owner_name = "O"
targets = []
"#,
        )
        .unwrap();
        let paths: Vec<_> = c.validate().into_iter().map(|p| p.path).collect();
        assert_eq!(
            paths,
            vec![
                "instance.name",
                "gateway[0].url",
                "gateway[1].id",
                "fleet.server[0].repo",
                "fleet.server[0].gateway",
                "marketplace[0].targets",
            ]
        );
    }

    #[test]
    fn loopback_http_is_allowed_for_gateways() {
        assert!(is_https_or_loopback("http://127.0.0.1:8787"));
        assert!(!is_https_or_loopback("http://example.org"));
    }

    #[test]
    fn paths_resolve_against_the_instance() {
        let base = Path::new("/inst");
        assert_eq!(resolve_path(base, Path::new("conformance")), PathBuf::from("/inst/conformance"));
        assert_eq!(resolve_path(base, Path::new("/abs")), PathBuf::from("/abs"));
    }
}
