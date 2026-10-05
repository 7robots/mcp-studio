//! Which repos are in the fleet, and what each one's files say about it.
//!
//! Membership: `fleet.include` ∪ `[[fleet.server]]` ∪ (repos in `github.orgs`
//! carrying `discover.github_topic` whose default branch has
//! `discover.require_file`) − `fleet.exclude`, minus archived repos when
//! `fleet.exclude_archived`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use studio_core::Instance;
use studio_core::config::repo_name;

use crate::github::{GithubApi, RepoInfo};
use crate::wrangler::{WorkerSection, Wrangler, parse_scopes};

pub const VAR_PUBLIC_URL: &str = "PUBLIC_MCP_URL";
pub const VAR_SCOPES: &str = "OKTA_M2M_SCOPE";

/// One fleet member and the facts derived from config and its local files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// `owner/repo`
    pub repo: String,
    /// The repo name: the short name used for `--server` and table rows.
    pub name: String,
    pub local_dir: PathBuf,
    pub local_exists: bool,
    /// Why it is in the fleet: `include`, `server`, `topic`.
    pub origins: Vec<String>,
    /// The Worker (script) name from `wrangler.toml`.
    pub worker: Option<String>,
    /// Custom-domain hostnames of the top-level Worker.
    pub hosts: Vec<String>,
    /// Public MCP endpoint.
    pub url: Option<String>,
    /// Where `url` came from: `config`, `PUBLIC_MCP_URL`, or `route`.
    pub url_source: Option<String>,
    /// The scopes the server accepts (`OKTA_M2M_SCOPE`).
    pub scopes: Vec<String>,
    /// `version` in `package.json`.
    pub version: Option<String>,
    /// Which `[[gateway]]` it is registered on.
    pub gateway: Option<String>,
    /// Registry id on that gateway.
    pub gateway_id: Option<String>,
    pub marketplace_slug: String,
    /// `[env.*]` sections of `wrangler.toml`.
    pub environments: Vec<EnvInfo>,
    /// Derivation problems (unreadable files and the like).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvInfo {
    pub name: String,
    pub worker: Option<String>,
    pub hosts: Vec<String>,
    pub url: Option<String>,
    pub scopes: Vec<String>,
    /// Listed in `fleet.server.environments`, so probes cover it.
    pub deployed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    pub servers: Vec<ServerInfo>,
    /// Things worth saying about discovery itself (GitHub unreachable, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Membership from config alone, in config order: include, then `[[fleet.server]]`.
pub fn configured_members(inst: &Instance) -> Vec<(String, Vec<String>)> {
    let f = &inst.config.fleet;
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut add = |repo: &str, origin: &str| {
        if let Some((_, o)) = out.iter_mut().find(|(r, _)| r.eq_ignore_ascii_case(repo)) {
            if !o.iter().any(|x| x == origin) {
                o.push(origin.into());
            }
        } else {
            out.push((repo.to_string(), vec![origin.into()]));
        }
    };
    for r in &f.include {
        add(r, "include");
    }
    for s in &f.servers {
        add(&s.repo, "server");
    }
    out
}

/// Discover the fleet. `gh` is used for topic discovery and the archived
/// filter; without it only configured members are returned (with a note).
pub async fn discover(inst: &Instance, gh: Option<&GithubApi>) -> Discovery {
    let cfg = &inst.config;
    let mut members = configured_members(inst);
    let mut notes = Vec::new();

    let topic = cfg
        .fleet
        .discover
        .as_ref()
        .and_then(|d| d.github_topic.clone());
    let require = cfg
        .fleet
        .discover
        .as_ref()
        .and_then(|d| d.require_file.clone());
    let mut remote: BTreeMap<String, RepoInfo> = BTreeMap::new();
    let wants_remote = topic.is_some() || cfg.fleet.exclude_archived;
    match gh.filter(|g| g.has_token()) {
        Some(gh) if wants_remote => {
            if cfg.github.orgs.is_empty() && topic.is_some() {
                notes.push("discover.github_topic is set but github.orgs is empty".into());
            }
            for org in &cfg.github.orgs {
                match gh.owner_repos(org).await {
                    Ok(rs) => {
                        for r in rs {
                            remote.insert(r.full_name.to_ascii_lowercase(), r);
                        }
                    }
                    Err(e) => notes.push(format!("listing {org} repos: {e}")),
                }
            }
            if let Some(topic) = &topic {
                let mut found: Vec<&RepoInfo> = remote
                    .values()
                    .filter(|r| r.topics.iter().any(|t| t == topic))
                    .filter(|r| !members.iter().any(|(m, _)| m.eq_ignore_ascii_case(&r.full_name)))
                    .collect();
                found.sort_by(|a, b| a.full_name.cmp(&b.full_name));
                for r in found {
                    let ok = match &require {
                        None => true,
                        Some(file) => match gh.has_file(&r.full_name, file, &r.default_branch).await {
                            Ok(b) => b,
                            Err(e) => {
                                notes.push(format!("{}: checking {file}: {e}", r.full_name));
                                false
                            }
                        },
                    };
                    if ok {
                        members.push((r.full_name.clone(), vec!["topic".into()]));
                    }
                }
                // configured members carrying the topic also get the origin
                for (m, o) in members.iter_mut() {
                    if let Some(r) = remote.get(&m.to_ascii_lowercase())
                        && r.topics.iter().any(|t| t == topic)
                        && !o.iter().any(|x| x == "topic")
                    {
                        o.push("topic".into());
                    }
                }
            }
        }
        _ if wants_remote => notes.push(
            "GitHub not reachable without a token: topic discovery and the archived filter were skipped"
                .into(),
        ),
        _ => {}
    }

    members.retain(|(r, _)| !cfg.fleet.exclude.iter().any(|x| x.eq_ignore_ascii_case(r)));
    if cfg.fleet.exclude_archived {
        members.retain(|(r, _)| {
            let archived = remote
                .get(&r.to_ascii_lowercase())
                .is_some_and(|i| i.archived);
            if archived {
                notes.push(format!("{r} is archived; excluded"));
            }
            !archived
        });
    }

    Discovery {
        servers: members
            .into_iter()
            .map(|(r, o)| derive_server(inst, &r, o))
            .collect(),
        notes,
    }
}

/// Everything about one server that can be read from config and local files.
pub fn derive_server(inst: &Instance, repo: &str, origins: Vec<String>) -> ServerInfo {
    let fs = inst.config.fleet_server(repo);
    let local_dir = inst.repo_dir(repo);
    let mut problems = Vec::new();

    let wrangler = match std::fs::read_to_string(local_dir.join("wrangler.toml")) {
        Ok(t) => match Wrangler::parse(&t) {
            Ok(w) => Some(w),
            Err(e) => {
                problems.push(format!("wrangler.toml: {e}"));
                None
            }
        },
        Err(_) if !local_dir.exists() => None,
        Err(_) => {
            problems.push("no wrangler.toml".into());
            None
        }
    };
    let top = wrangler.as_ref().map(|w| w.top.clone()).unwrap_or_default();
    let hosts = custom_hosts(&top);

    let (url, url_source) = match fs.and_then(|s| s.url.clone()) {
        Some(u) => (Some(u), Some("config")),
        None => match section_url(&top) {
            (Some(u), src) => (Some(u), src),
            (None, _) => (None, None),
        },
    };

    let gateway_id = fs
        .and_then(|s| s.gateway_id.clone())
        .or_else(|| url.as_deref().and_then(first_label));

    let deployed_envs: Vec<String> = fs.map(|s| s.environments.clone()).unwrap_or_default();
    let mut environments = Vec::new();
    if let Some(w) = &wrangler {
        for (name, sec) in &w.envs {
            environments.push(EnvInfo {
                name: name.clone(),
                worker: w.env_worker_name(name),
                hosts: custom_hosts(sec),
                url: section_url(sec).0,
                scopes: sec
                    .vars
                    .get(VAR_SCOPES)
                    .map(|s| parse_scopes(s))
                    .unwrap_or_default(),
                deployed: deployed_envs.iter().any(|d| d == name),
            });
        }
    }
    for d in &deployed_envs {
        if !environments.iter().any(|e| &e.name == d) {
            problems.push(format!(
                "environment {d} is configured but wrangler.toml has no [env.{d}]"
            ));
        }
    }

    let version = std::fs::read_to_string(local_dir.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("version")?.as_str().map(String::from));

    ServerInfo {
        repo: repo.to_string(),
        name: repo_name(repo).to_string(),
        local_exists: local_dir.is_dir(),
        local_dir,
        origins,
        worker: top.name.clone(),
        hosts,
        url,
        url_source: url_source.map(String::from),
        scopes: top
            .vars
            .get(VAR_SCOPES)
            .map(|s| parse_scopes(s))
            .unwrap_or_default(),
        version,
        gateway: fs
            .and_then(|s| s.gateway.clone())
            .or_else(|| inst.config.gateway(None).map(|g| g.id.clone())),
        gateway_id,
        marketplace_slug: fs
            .and_then(|s| s.marketplace_slug.clone())
            .unwrap_or_else(|| repo_name(repo).to_string()),
        environments,
        problems,
    }
}

fn custom_hosts(s: &WorkerSection) -> Vec<String> {
    s.routes
        .iter()
        .filter(|r| r.custom_domain)
        .filter_map(|r| r.host())
        .collect()
}

/// `PUBLIC_MCP_URL`, else `https://<first custom domain>/mcp`.
fn section_url(s: &WorkerSection) -> (Option<String>, Option<&'static str>) {
    if let Some(u) = s.vars.get(VAR_PUBLIC_URL).filter(|u| !u.is_empty()) {
        return (Some(u.clone()), Some(VAR_PUBLIC_URL));
    }
    match custom_hosts(s).first() {
        Some(h) => (Some(format!("https://{h}/mcp")), Some("route")),
        None => (None, None),
    }
}

/// The host of a URL.
pub fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .host_str()
        .map(|h| h.to_ascii_lowercase())
}

/// `https://weather.mcp.acme.example/mcp` → `weather`.
pub fn first_label(url: &str) -> Option<String> {
    let h = host_of(url)?;
    if h.parse::<std::net::IpAddr>().is_ok() || h == "localhost" {
        return None;
    }
    h.split('.')
        .next()
        .filter(|l| !l.is_empty())
        .map(String::from)
}
