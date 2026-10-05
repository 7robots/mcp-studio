//! The per-server probes. Each returns a [`ProbeOut`]: checks with stable ids,
//! a short cell text for the status matrix, and facts other probes or the TUI
//! can use. A probe never panics and never fails another: a problem it can't
//! get past becomes a `skip` (couldn't run) or `fail` (found something broken).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use studio_core::check::{Check, Status, rollup};
use studio_core::config::{MarketplaceConfig, Target};

use crate::GatewayServerInfo;
use crate::cloudflare::{Build, CfAccount, CloudflareApi};
use crate::discover::{ServerInfo, host_of};
use crate::github::{GithubApi, short_err};
use crate::okta::OktaData;
use crate::time::{age, now_unix, parse_rfc3339};

#[derive(Debug, Clone, Default)]
pub struct ProbeOut {
    pub checks: Vec<Check>,
    /// Cell text when everything passed.
    pub short: Option<String>,
    /// Cell text when everything skipped (default: the first skip's reason).
    pub skip_short: Option<String>,
    pub facts: Vec<(String, String)>,
}

impl ProbeOut {
    fn push(&mut self, c: Check) {
        self.checks.push(c);
    }
    fn fact(&mut self, k: &str, v: impl Into<String>) {
        self.facts.push((k.to_string(), v.into()));
    }
    pub fn skip(id: &str, why: impl Into<String>) -> Self {
        Self {
            checks: vec![Check::skip(id, why)],
            ..Self::default()
        }
    }
    pub fn status(&self) -> Status {
        rollup(&self.checks)
    }
}

pub fn sha7(s: &str) -> &str {
    s.get(..7).unwrap_or(s)
}

fn ago(ts: Option<&str>) -> Option<String> {
    ts.and_then(parse_rfc3339).map(|t| age(now_unix() - t))
}

// ---------------------------------------------------------------- git

async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut c = tokio::process::Command::new("git");
    c.args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(60), c.output())
        .await
        .map_err(|_| format!("git {} timed out", args.join(" ")))?
        .map_err(|_| "git is not installed or not on PATH".to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The checked-out commit of a local clone.
pub async fn local_head(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "HEAD"]).await.ok()
}

pub async fn probe_git(s: &ServerInfo, fetch: bool) -> ProbeOut {
    let mut o = ProbeOut::default();
    let dir = &s.local_dir;
    if !s.local_exists {
        o.push(Check::fail("git.clone", "no local clone").with_evidence(dir.display().to_string()));
        return o;
    }
    if let Err(e) = git(dir, &["rev-parse", "--git-dir"]).await {
        o.push(Check::fail("git.clone", "not a git repository").with_evidence(e));
        return o;
    }
    o.push(Check::pass("git.clone", "cloned").with_evidence(dir.display().to_string()));
    if fetch {
        match git(dir, &["fetch", "--quiet"]).await {
            Ok(_) => o.push(Check::pass("git.fetch", "fetched")),
            Err(e) => o.push(Check::warn("git.fetch", "fetch failed").with_evidence(e)),
        }
    }

    let branch = git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await
        .ok();
    let default = git(
        dir,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .await
    .ok()
    .map(|d| d.trim_start_matches("origin/").to_string());
    let head = local_head(dir).await;
    if let Some(h) = &head {
        o.fact("git.head", h.clone());
    }
    match (&branch, &default) {
        (None, _) => o.push(Check::warn("git.branch", "detached HEAD")),
        (Some(b), Some(d)) if b != d => {
            o.push(Check::warn("git.branch", format!("on {b}, default is {d}")))
        }
        (Some(b), _) => o.push(Check::pass("git.branch", format!("on {b}"))),
    }
    if let Some(b) = &branch {
        o.fact("git.branch", b.clone());
    }

    match git(dir, &["status", "--porcelain"]).await {
        Ok(out) => {
            let n = out.lines().filter(|l| !l.trim().is_empty()).count();
            if n == 0 {
                o.push(Check::pass("git.dirty", "clean"));
            } else {
                o.push(
                    Check::warn("git.dirty", format!("{n} uncommitted change{}", plural(n)))
                        .with_evidence(out.lines().take(10).collect::<Vec<_>>().join("\n")),
                );
            }
        }
        Err(e) => o.push(Check::skip("git.dirty", "git status failed").with_evidence(e)),
    }

    match git(
        dir,
        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
    )
    .await
    {
        Ok(out) => {
            let mut it = out.split_whitespace().filter_map(|x| x.parse::<u64>().ok());
            let (ahead, behind) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
            o.fact("git.ahead", ahead.to_string());
            o.fact("git.behind", behind.to_string());
            let note = if fetch { "" } else { " (as of last fetch)" };
            let c = match (ahead, behind) {
                (0, 0) => Check::pass("git.upstream", "up to date with upstream"),
                (a, 0) => Check::warn("git.upstream", format!("{a} ahead of upstream (unpushed)")),
                (0, b) => Check::warn("git.upstream", format!("{b} behind upstream")),
                (a, b) => Check::warn("git.upstream", format!("diverged: {a} ahead, {b} behind")),
            };
            o.push(c.with_evidence(format!("ahead {ahead}, behind {behind}{note}")));
        }
        Err(_) => o.push(Check::skip("git.upstream", "no upstream branch")),
    }

    let mut short = branch.unwrap_or_else(|| "detached".into());
    if let Some(h) = &head {
        short = format!("{short} {}", sha7(h));
    }
    o.short = Some(short);
    o
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

// ---------------------------------------------------------------- github

pub async fn probe_github(
    s: &ServerInfo,
    gh: Result<&GithubApi, &str>,
    topic: Option<&str>,
) -> ProbeOut {
    let gh = match gh {
        Ok(g) if g.has_token() => g,
        Ok(_) => return ProbeOut::skip("github.repo", "no GitHub token"),
        Err(why) => return ProbeOut::skip("github.repo", why.to_string()),
    };
    let mut o = ProbeOut::default();
    let info = match gh.repo(&s.repo).await {
        Ok(Some(i)) => i,
        Ok(None) => {
            o.push(
                Check::fail("github.repo", "repository not found (or no access)")
                    .with_evidence(&s.repo),
            );
            return o;
        }
        Err(e) => return ProbeOut::skip("github.repo", "GitHub API unreachable").with_ev(e),
    };
    o.push(Check::pass("github.repo", &info.full_name));
    o.push(if info.archived {
        Check::fail("github.archived", "repository is archived")
    } else {
        Check::pass("github.archived", "not archived")
    });
    if let Some(t) = topic {
        o.push(if info.topics.iter().any(|x| x == t) {
            Check::pass("github.topic", format!("has topic {t}"))
        } else {
            Check::warn("github.topic", format!("missing topic {t}"))
                .with_evidence(format!("topics: {}", info.topics.join(", ")))
        });
    }
    let pushed = ago(info.pushed_at.as_deref());
    let branch = if info.default_branch.is_empty() {
        "main".to_string()
    } else {
        info.default_branch.clone()
    };
    o.fact("github.default_branch", branch.clone());
    let remote = match gh.branch_head(&s.repo, &branch).await {
        Ok(Some(sha)) => {
            let when = pushed
                .as_deref()
                .map(|a| format!(", pushed {a} ago"))
                .unwrap_or_default();
            o.push(
                Check::pass("github.head", format!("{branch} @ {}{when}", sha7(&sha)))
                    .with_evidence(sha.clone()),
            );
            o.fact("github.head", sha.clone());
            Some(sha)
        }
        Ok(None) => {
            o.push(Check::fail(
                "github.head",
                format!("default branch {branch} not found"),
            ));
            None
        }
        Err(e) => {
            o.push(Check::skip("github.head", "GitHub API unreachable").with_evidence(e));
            None
        }
    };
    match (remote.as_deref(), s.local_exists) {
        (_, false) => o.push(Check::skip("github.sync", "no local clone")),
        (None, _) => o.push(Check::skip("github.sync", "GitHub HEAD unknown")),
        (Some(r), true) => match local_head(&s.local_dir).await {
            None => o.push(Check::skip("github.sync", "local HEAD unknown")),
            Some(l) if l == r => o.push(Check::pass(
                "github.sync",
                format!("local HEAD = GitHub {branch}"),
            )),
            Some(l) => o.push(
                Check::warn(
                    "github.sync",
                    format!("local {} ≠ GitHub {branch} {}", sha7(&l), sha7(r)),
                )
                .with_evidence(format!("local {l}\nremote {r}")),
            ),
        },
    }
    o.short = Some(match (&remote, &pushed) {
        (Some(r), Some(p)) => format!("{} {p}", sha7(r)),
        (Some(r), None) => sha7(r).to_string(),
        _ => "ok".into(),
    });
    o
}

trait WithEv {
    fn with_ev(self, e: String) -> Self;
}
impl WithEv for ProbeOut {
    fn with_ev(mut self, e: String) -> Self {
        if let Some(c) = self.checks.last_mut() {
            c.evidence = Some(e);
        }
        self
    }
}

// ---------------------------------------------------------------- cloudflare

pub async fn probe_cloudflare(
    s: &ServerInfo,
    cf: Result<&(CloudflareApi, CfAccount), &(Status, String)>,
    github_head: Option<&str>,
) -> ProbeOut {
    let (api, acct) = match cf {
        Ok(x) => (&x.0, &x.1),
        Err((st, why)) => {
            return ProbeOut {
                checks: vec![Check::new("cf.script", *st, why.clone())],
                ..Default::default()
            };
        }
    };
    let mut o = ProbeOut::default();
    let Some(worker) = &s.worker else {
        return ProbeOut::skip("cf.script", "no Worker name (wrangler.toml missing?)");
    };
    let script = acct.scripts.get(worker);
    match script {
        Some(sc) => {
            let m = ago(sc.modified_on.as_deref())
                .map(|a| format!(", modified {a} ago"))
                .unwrap_or_default();
            o.push(Check::pass("cf.script", format!("{worker} exists{m}")));
        }
        None => o.push(Check::fail(
            "cf.script",
            format!("no Worker named {worker}"),
        )),
    }
    domain_check(&mut o, "cf.domain", &s.hosts, worker, acct);

    // Workers Builds
    let mut short = None;
    match script.and_then(|sc| sc.tag.as_deref()) {
        None if script.is_some() => o.push(Check::skip("cf.build", "script has no tag")),
        None => o.push(Check::skip("cf.build", "no Worker")),
        Some(tag) => match api.builds(&acct.account_id, tag).await {
            Err(e) => {
                o.push(Check::skip("cf.build", "Workers Builds API unreachable").with_evidence(e))
            }
            Ok(builds) if builds.is_empty() => {
                o.push(Check::warn("cf.build", "no Workers Builds for this Worker"));
            }
            Ok(builds) => {
                let latest = &builds[0];
                o.push(build_check(latest));
                if let Some(c) = latest
                    .build_trigger_metadata
                    .as_ref()
                    .and_then(|m| m.commit_hash.as_deref())
                {
                    o.fact("cf.build.commit", c);
                }
                let deployed = builds
                    .iter()
                    .find(|b| b.build_outcome.as_deref() == Some("success"));
                let dep_commit = deployed
                    .and_then(|b| b.build_trigger_metadata.as_ref())
                    .and_then(|m| m.commit_hash.clone());
                if let Some(d) = &dep_commit {
                    o.fact("cf.deployed.commit", d.clone());
                }
                let when =
                    deployed.and_then(|b| ago(b.stopped_on.as_deref().or(b.created_on.as_deref())));
                short = Some(match (&dep_commit, &when) {
                    (Some(c), Some(w)) => format!("{} {w}", sha7(c)),
                    (Some(c), None) => sha7(c).to_string(),
                    _ => "built".into(),
                });
                o.push(match (dep_commit.as_deref(), github_head) {
                    (None, _) => Check::warn("cf.deployed", "no successful build"),
                    (Some(_), None) => Check::skip("cf.deployed", "GitHub HEAD unknown"),
                    (Some(d), Some(h)) if d == h => Check::pass(
                        "cf.deployed",
                        format!(
                            "HEAD {} deployed{}",
                            sha7(h),
                            when.map(|w| format!(" {w} ago")).unwrap_or_default()
                        ),
                    ),
                    (Some(d), Some(h)) => {
                        let pending = matches!(
                            latest.status.as_deref(),
                            Some("queued" | "initializing" | "running")
                        );
                        Check::warn(
                            "cf.deployed",
                            format!(
                                "deployed {} ≠ HEAD {}{}",
                                sha7(d),
                                sha7(h),
                                if pending { " (build running)" } else { "" }
                            ),
                        )
                        .with_evidence(format!("deployed {d}\nhead     {h}"))
                    }
                });
            }
        },
    }

    for env in s.environments.iter().filter(|e| e.deployed) {
        let id = |k: &str| format!("cf.{k}.{}", env.name);
        match &env.worker {
            None => o.push(Check::skip(id("script"), "no Worker name")),
            Some(w) => {
                o.push(if acct.scripts.contains_key(w) {
                    Check::pass(id("script"), format!("{w} exists"))
                } else {
                    Check::fail(id("script"), format!("no Worker named {w}"))
                });
                domain_check(&mut o, &id("domain"), &env.hosts, w, acct);
            }
        }
    }
    o.short = short.or(Some("ok".into()));
    o
}

fn domain_check(o: &mut ProbeOut, id: &str, hosts: &[String], worker: &str, acct: &CfAccount) {
    if hosts.is_empty() {
        o.push(Check::skip(id, "no custom domain in wrangler.toml"));
        return;
    }
    let mut bad = Vec::new();
    for h in hosts {
        match acct
            .domains
            .iter()
            .find(|d| d.hostname.eq_ignore_ascii_case(h))
        {
            Some(d) if d.service == worker => {}
            Some(d) => bad.push(format!("{h} → {}", d.service)),
            None => bad.push(format!("{h} not attached")),
        }
    }
    o.push(if bad.is_empty() {
        Check::pass(id, format!("{} → {worker}", hosts.join(", ")))
    } else {
        Check::fail(id, bad.join("; "))
    });
}

fn build_check(b: &Build) -> Check {
    let when = ago(b.stopped_on.as_deref().or(b.created_on.as_deref()))
        .map(|a| format!(" {a} ago"))
        .unwrap_or_default();
    let commit = b
        .build_trigger_metadata
        .as_ref()
        .and_then(|m| m.commit_hash.as_deref())
        .map(|c| format!(" ({})", sha7(c)))
        .unwrap_or_default();
    let ev = b.build_uuid.clone().unwrap_or_default();
    let c = match (b.build_outcome.as_deref(), b.status.as_deref()) {
        (Some("success"), _) => {
            Check::pass("cf.build", format!("latest build succeeded{when}{commit}"))
        }
        (Some(o @ ("fail" | "cancelled" | "terminated")), _) => {
            Check::fail("cf.build", format!("latest build {o}{when}{commit}"))
        }
        (Some("skipped"), _) => {
            Check::warn("cf.build", format!("latest build skipped{when}{commit}"))
        }
        (_, Some(st @ ("queued" | "initializing" | "running"))) => {
            Check::warn("cf.build", format!("build {st}{commit}"))
        }
        (o, st) => Check::warn(
            "cf.build",
            format!("latest build: {}", o.or(st).unwrap_or("unknown")),
        ),
    };
    if ev.is_empty() {
        c
    } else {
        c.with_evidence(ev)
    }
}

// ---------------------------------------------------------------- http

pub async fn probe_http(s: &ServerInfo, client: &reqwest::Client) -> ProbeOut {
    let mut o = ProbeOut::default();
    match &s.url {
        None => o.push(Check::skip("http.unauth_401", "no public URL")),
        Some(u) => http_checks(&mut o, client, u, &s.scopes, "").await,
    }
    for env in s.environments.iter().filter(|e| e.deployed) {
        let sfx = format!(".{}", env.name);
        match &env.url {
            None => o.push(Check::skip(
                format!("http.unauth_401{sfx}"),
                "no public URL",
            )),
            Some(u) => http_checks(&mut o, client, u, &env.scopes, &sfx).await,
        }
    }
    o.short = Some("401 ok".into());
    o
}

async fn http_checks(
    o: &mut ProbeOut,
    client: &reqwest::Client,
    url: &str,
    scopes: &[String],
    sfx: &str,
) {
    let id = |k: &str| format!("http.{k}{sfx}");
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "mcp-studio", "version": env!("CARGO_PKG_VERSION")}}
    });
    let resp = client
        .post(url)
        .header("accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            o.push(
                Check::fail(id("unauth_401"), format!("unreachable: {}", short_err(&e)))
                    .with_evidence(url),
            );
            return;
        }
    };
    let status = resp.status().as_u16();
    if status != 401 {
        o.push(
            Check::fail(
                id("unauth_401"),
                format!("HTTP {status} without a token, want 401"),
            )
            .with_evidence(url),
        );
        return;
    }
    o.push(Check::pass(id("unauth_401"), "401 without a token"));
    let www = resp
        .headers()
        .get_all("www-authenticate")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join(", ");
    let rm = auth_param(&www, "resource_metadata");
    let prm_url = match &rm {
        Some(u) => {
            o.push(Check::pass(
                id("resource_metadata"),
                "WWW-Authenticate names resource_metadata",
            ));
            u.clone()
        }
        None => {
            o.push(
                Check::fail(
                    id("resource_metadata"),
                    "WWW-Authenticate lacks resource_metadata",
                )
                .with_evidence(if www.is_empty() {
                    "(no header)".into()
                } else {
                    www.clone()
                }),
            );
            match prm_location(url) {
                Some(u) => u,
                None => return,
            }
        }
    };

    let prm = match get_json(client, &prm_url).await {
        Ok(v) => v,
        Err(e) => {
            o.push(
                Check::fail(id("prm"), "protected-resource metadata unreadable")
                    .with_evidence(format!("{prm_url}: {e}")),
            );
            return;
        }
    };
    let supported: Vec<String> = prm
        .get("scopes_supported")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut want = scopes.to_vec();
    want.sort();
    let mut got = supported.clone();
    got.sort();
    got.dedup();
    let resource = prm.get("resource").and_then(|v| v.as_str()).unwrap_or("");
    let mut problems = Vec::new();
    if want != got {
        problems.push(format!(
            "scopes_supported [{}] ≠ wrangler [{}]",
            got.join(" "),
            want.join(" ")
        ));
    }
    if resource.trim_end_matches('/') != url.trim_end_matches('/') {
        problems.push(format!("resource {resource} ≠ {url}"));
    }
    o.push(if problems.is_empty() {
        Check::pass(id("prm"), format!("scopes_supported = [{}]", got.join(" ")))
    } else {
        Check::fail(id("prm"), problems.join("; ")).with_evidence(prm_url.clone())
    });

    let Some(as_url) = prm
        .pointer("/authorization_servers/0")
        .and_then(|v| v.as_str())
        .and_then(as_metadata_location)
    else {
        o.push(Check::skip(id("cimd"), "no authorization server in PRM"));
        return;
    };
    match get_json(client, &as_url).await {
        Ok(v) => o.push(
            match v
                .get("client_id_metadata_document_supported")
                .and_then(|b| b.as_bool())
            {
                Some(true) => Check::pass(id("cimd"), "client ID metadata documents supported"),
                _ => Check::warn(
                    id("cimd"),
                    "client_id_metadata_document_supported is not true",
                )
                .with_evidence(as_url),
            },
        ),
        Err(e) => o.push(
            Check::fail(id("cimd"), "authorization-server metadata unreadable")
                .with_evidence(format!("{as_url}: {e}")),
        ),
    }
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    let r = client.get(url).send().await.map_err(|e| short_err(&e))?;
    if !r.status().is_success() {
        return Err(format!("HTTP {}", r.status().as_u16()));
    }
    r.json().await.map_err(|e| format!("bad JSON: {e}"))
}

/// A parameter from a `WWW-Authenticate` challenge, e.g. `resource_metadata="…"`.
pub fn auth_param(header: &str, name: &str) -> Option<String> {
    let lower = header.to_ascii_lowercase();
    let key = format!("{name}=");
    let mut from = 0;
    while let Some(i) = lower[from..].find(&key) {
        let start = from + i;
        let boundary = start == 0 || matches!(lower.as_bytes()[start - 1], b' ' | b',' | b'\t');
        let rest = &header[start + key.len()..];
        if boundary {
            return Some(if let Some(r) = rest.strip_prefix('"') {
                r.split('"').next().unwrap_or("").to_string()
            } else {
                rest.split([',', ' ']).next().unwrap_or("").to_string()
            });
        }
        from = start + key.len();
    }
    None
}

/// RFC 9728: `<origin>/.well-known/oauth-protected-resource<path>`.
pub fn prm_location(resource: &str) -> Option<String> {
    well_known(resource, "oauth-protected-resource")
}

/// RFC 8414: `<origin>/.well-known/oauth-authorization-server<path>`.
pub fn as_metadata_location(issuer: &str) -> Option<String> {
    well_known(issuer, "oauth-authorization-server")
}

fn well_known(u: &str, name: &str) -> Option<String> {
    let p = url::Url::parse(u).ok()?;
    let path = p.path().trim_end_matches('/');
    Some(format!(
        "{}/.well-known/{name}{path}",
        p.origin().ascii_serialization()
    ))
}

// ---------------------------------------------------------------- okta

pub fn probe_okta(s: &ServerInfo, data: Result<&OktaData, &str>) -> ProbeOut {
    let data = match data {
        Ok(d) => d,
        Err(why) => return ProbeOut::skip("okta.scopes", why.to_string()),
    };
    let mut o = ProbeOut::default();
    if s.scopes.is_empty() {
        o.push(Check::skip(
            "okta.scopes",
            "server declares no OKTA_M2M_SCOPE",
        ));
    } else if data.rules.is_empty() && data.missing_rules.is_empty() {
        o.push(Check::skip(
            "okta.scopes",
            "no identity.okta.policy_rules configured",
        ));
    } else {
        let mut bad = Vec::new();
        for id in &data.missing_rules {
            bad.push(format!("rule {id} not found on the authorization server"));
        }
        for r in &data.rules {
            if !r.active {
                bad.push(format!("rule {} is inactive", r.name));
            }
            if r.scopes.iter().any(|x| x == "*") {
                continue;
            }
            let missing: Vec<&str> = s
                .scopes
                .iter()
                .filter(|sc| !r.scopes.contains(sc))
                .map(String::as_str)
                .collect();
            if !missing.is_empty() {
                bad.push(format!("{} lacks {}", r.name, missing.join(" ")));
            }
        }
        o.push(if bad.is_empty() {
            Check::pass(
                "okta.scopes",
                format!(
                    "{} granted by {} rule{}",
                    s.scopes.join(" "),
                    data.rules.len(),
                    plural(data.rules.len())
                ),
            )
        } else {
            Check::fail("okta.scopes", bad.join("; "))
        });
    }

    let host = s.url.as_deref().and_then(host_of);
    match (&data.redirect_uris, host) {
        (_, None) => o.push(Check::skip("okta.redirect_uri", "no public URL")),
        (Ok(None), _) => o.push(Check::skip(
            "okta.redirect_uri",
            "no identity.okta.interactive_client_id",
        )),
        (Err(e), _) => o.push(
            Check::skip("okta.redirect_uri", "interactive client unreadable")
                .with_evidence(e.clone()),
        ),
        (Ok(Some(uris)), Some(h)) => {
            let want = format!("https://{h}/callback");
            o.push(if uris.iter().any(|u| u == &want) {
                Check::pass("okta.redirect_uri", format!("{want} registered"))
            } else {
                Check::fail(
                    "okta.redirect_uri",
                    format!("{want} not registered on the interactive client"),
                )
            });
        }
    }
    o.short = Some("ok".into());
    o
}

// ---------------------------------------------------------------- gateway

pub fn probe_gateway(s: &ServerInfo, list: Result<&[GatewayServerInfo], &str>) -> ProbeOut {
    let list = match list {
        Ok(l) => l,
        Err(why) => return ProbeOut::skip("gateway.registered", why.to_string()),
    };
    let Some(id) = &s.gateway_id else {
        return ProbeOut::skip(
            "gateway.registered",
            "no gateway id (set fleet.server.gateway_id)",
        );
    };
    let mut o = ProbeOut::default();
    let gw = s.gateway.as_deref().unwrap_or("gateway");
    let Some(e) = list.iter().find(|x| &x.id == id) else {
        o.push(Check::fail(
            "gateway.registered",
            format!("not registered on {gw} as {id}"),
        ));
        return o;
    };
    o.push(Check::pass(
        "gateway.registered",
        format!("registered on {gw} as {id}"),
    ));
    for (k, v) in [
        ("gateway.access", &e.access),
        ("gateway.auth_mode", &e.auth_mode),
        ("gateway.last_refresh_at", &e.last_refresh_at),
        ("gateway.version", &e.server_version),
    ] {
        if let Some(v) = v {
            o.fact(k, v.clone());
        }
    }
    let status = e.status.as_deref().unwrap_or("unknown");
    o.push(
        match status.to_ascii_lowercase().as_str() {
            "active" | "enabled" | "ok" | "ready" | "registered" => {
                Check::pass("gateway.status", status)
            }
            "disabled" | "error" | "failed" | "removed" => Check::fail("gateway.status", status),
            _ => Check::warn("gateway.status", status),
        }
        .with_evidence(format!(
            "access {}, auth_mode {}",
            e.access.as_deref().unwrap_or("?"),
            e.auth_mode.as_deref().unwrap_or("?")
        )),
    );
    let health = e.health.as_deref().unwrap_or("unknown");
    let hc = match health.to_ascii_lowercase().as_str() {
        "healthy" | "ok" | "up" | "pass" => Check::pass("gateway.health", health),
        "unhealthy" | "down" | "error" | "fail" | "failed" => Check::fail("gateway.health", health),
        _ => Check::warn("gateway.health", health),
    };
    o.push(match &e.last_error {
        Some(err) if !err.is_empty() => hc.with_evidence(err.clone()),
        _ => hc,
    });
    o.short = Some(format!("{status}/{health}"));
    o
}

// ---------------------------------------------------------------- marketplace

/// A plugin entry from one target catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Catalogs {
    pub id: String,
    /// `Err` when the local clone or a catalog file is missing/unreadable.
    pub targets: Result<BTreeMap<Target, Vec<Listing>>, String>,
}

pub fn target_name(t: Target) -> &'static str {
    match t {
        Target::Claude => "claude",
        Target::Codex => "codex",
    }
}

/// Read each enabled target's catalog from the marketplace's local clone.
pub fn load_catalogs(m: &MarketplaceConfig, dir: &Path) -> Catalogs {
    let id = m.id.clone();
    if !dir.is_dir() {
        return Catalogs {
            id,
            targets: Err(format!("no local clone at {}", dir.display())),
        };
    }
    let mut out = BTreeMap::new();
    for &t in &m.targets {
        let (file, manifest) = match t {
            Target::Claude => (
                ".claude-plugin/marketplace.json",
                ".claude-plugin/plugin.json",
            ),
            Target::Codex => (
                ".agents/plugins/marketplace.json",
                ".codex-plugin/plugin.json",
            ),
        };
        let v: Value = match std::fs::read_to_string(dir.join(file))
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        {
            Ok(v) => v,
            Err(e) => {
                return Catalogs {
                    id,
                    targets: Err(format!("{file}: {e}")),
                };
            }
        };
        let mut list = Vec::new();
        for p in v
            .get("plugins")
            .and_then(|p| p.as_array())
            .into_iter()
            .flatten()
        {
            let Some(name) = p.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let version = p
                .get("version")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| {
                    let src = p.get("source").and_then(|s| s.as_str())?;
                    let t = std::fs::read_to_string(dir.join(src).join(manifest)).ok()?;
                    let mv: Value = serde_json::from_str(&t).ok()?;
                    mv.get("version")?.as_str().map(String::from)
                });
            list.push(Listing {
                name: name.to_string(),
                version,
            });
        }
        out.insert(t, list);
    }
    Catalogs {
        id,
        targets: Ok(out),
    }
}

pub fn probe_marketplace(s: &ServerInfo, catalogs: &[Catalogs]) -> ProbeOut {
    if catalogs.is_empty() {
        return ProbeOut::skip("marketplace", "no [[marketplace]] configured");
    }
    let mut o = ProbeOut::default();
    let mut listed_in = Vec::new();
    for c in catalogs {
        let id = format!("marketplace.{}", c.id);
        let targets = match &c.targets {
            Ok(t) => t,
            Err(e) => {
                o.push(
                    Check::skip(id, format!("{}: catalog unavailable", c.id))
                        .with_evidence(e.clone()),
                );
                continue;
            }
        };
        let mut have = Vec::new();
        let mut lacking = Vec::new();
        let mut versions = Vec::new();
        for (t, list) in targets {
            match list.iter().find(|l| l.name == s.marketplace_slug) {
                Some(l) => {
                    have.push(target_name(*t));
                    if let Some(v) = &l.version {
                        versions.push((target_name(*t), v.clone()));
                    }
                }
                None => lacking.push(target_name(*t)),
            }
        }
        if have.is_empty() {
            o.push(Check::skip(id, format!("not listed in {}", c.id)));
            continue;
        }
        if !lacking.is_empty() {
            o.push(Check::fail(
                id,
                format!(
                    "{} listed for {} but not {}",
                    s.marketplace_slug,
                    have.join("+"),
                    lacking.join("+")
                ),
            ));
            continue;
        }
        let pkg = s.version.as_deref();
        let off: Vec<String> = versions
            .iter()
            .filter(|(_, v)| pkg.is_some_and(|p| p != v))
            .map(|(t, v)| format!("{t} {v}"))
            .collect();
        let shown = versions
            .first()
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "?".into());
        listed_in.push(format!("{} {shown}", c.id));
        o.push(if off.is_empty() {
            Check::pass(id, format!("listed as {} ({shown})", s.marketplace_slug))
        } else {
            Check::warn(
                id,
                format!(
                    "version {} ≠ package.json {}",
                    off.join(", "),
                    pkg.unwrap_or("?")
                ),
            )
        });
    }
    if listed_in.is_empty() {
        let any_loaded = catalogs.iter().any(|c| c.targets.is_ok());
        o.skip_short = Some(
            if any_loaded {
                "not listed"
            } else {
                "no catalog clones"
            }
            .into(),
        );
    }
    o.short = Some(if listed_in.is_empty() {
        "–".into()
    } else {
        listed_in.join(", ")
    });
    o
}
