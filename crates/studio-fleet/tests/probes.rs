//! Probes against local mock servers, a fake Okta tool and temp git repos.
//! Nothing here touches the network beyond 127.0.0.1.

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{Canned, Mock, copy_dir, fixtures, git, json};
use futures::future::BoxFuture;
use serde_json::json as j;
use studio_core::check::{Check, Status};
use studio_core::config::GatewayConfig;
use studio_core::{Instance, Secret};
use studio_fleet::discover::{ServerInfo, derive_server};
use studio_fleet::{
    Endpoints, FleetReport, GatewayServerInfo, GatewaySource, Providers, RepoChecker, Source,
    StatusOptions, cache, probes,
};

fn status_of(r: &FleetReport, server: &str, id: &str) -> Option<(Status, String)> {
    r.servers
        .iter()
        .find(|s| s.name == server)?
        .checks
        .iter()
        .find(|c| c.id == id)
        .map(|c| (c.status, c.summary.clone()))
}

fn st(r: &FleetReport, server: &str, id: &str) -> Status {
    status_of(r, server, id)
        .unwrap_or_else(|| panic!("no check {id} for {server}"))
        .0
}

fn http_server(url: &str, scopes: &[&str]) -> ServerInfo {
    ServerInfo {
        repo: "acme/x".into(),
        name: "x".into(),
        local_dir: "/nonexistent".into(),
        local_exists: false,
        origins: vec![],
        worker: Some("x".into()),
        hosts: vec![],
        url: Some(url.into()),
        url_source: Some("config".into()),
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
        version: None,
        gateway: None,
        gateway_id: None,
        marketplace_slug: "x".into(),
        environments: vec![],
        problems: vec![],
    }
}

fn client() -> reqwest::Client {
    studio_fleet::http_client(Duration::from_secs(5))
}

#[tokio::test]
async fn http_probe_passes_a_conforming_server() {
    let m = Mock::start().await;
    let b = m.base.clone();
    m.on(
        "POST /mcp",
        Canned {
            status: 401,
            headers: vec![(
                "www-authenticate".into(),
                format!(
                    r#"Bearer realm="OAuth", resource_metadata="{b}/.well-known/oauth-protected-resource/mcp", scope="weather:read""#
                ),
            )],
            body: String::new(),
        },
    )
    .on(
        "GET /.well-known/oauth-protected-resource/mcp",
        json(200, j!({"resource": format!("{b}/mcp"), "authorization_servers": [b], "scopes_supported": ["weather:read"]})),
    )
    .on(
        "GET /.well-known/oauth-authorization-server",
        json(200, j!({"issuer": b, "client_id_metadata_document_supported": true})),
    );
    let out = probes::probe_http(
        &http_server(&format!("{b}/mcp"), &["weather:read"]),
        &client(),
    )
    .await;
    let ids: Vec<(&str, Status)> = out
        .checks
        .iter()
        .map(|c| (c.id.as_str(), c.status))
        .collect();
    assert_eq!(
        ids,
        vec![
            ("http.unauth_401", Status::Pass),
            ("http.resource_metadata", Status::Pass),
            ("http.prm", Status::Pass),
            ("http.cimd", Status::Pass),
        ]
    );
}

#[tokio::test]
async fn http_probe_reports_scope_drift_missing_header_and_cimd() {
    let m = Mock::start().await;
    let b = m.base.clone();
    m.on("POST /mcp", Canned { status: 401, headers: vec![], body: String::new() })
        .on(
            "GET /.well-known/oauth-protected-resource/mcp",
            json(200, j!({"resource": format!("{b}/mcp"), "authorization_servers": [b], "scopes_supported": ["weather:read"]})),
        )
        .on("GET /.well-known/oauth-authorization-server", json(200, j!({"issuer": b})));
    let out = probes::probe_http(
        &http_server(&format!("{b}/mcp"), &["weather:read", "weather:write"]),
        &client(),
    )
    .await;
    let get = |id: &str| out.checks.iter().find(|c| c.id == id).unwrap();
    assert_eq!(get("http.unauth_401").status, Status::Pass);
    assert_eq!(get("http.resource_metadata").status, Status::Fail);
    assert_eq!(get("http.prm").status, Status::Fail);
    assert!(
        get("http.prm").summary.contains("weather:write"),
        "{:?}",
        get("http.prm")
    );
    assert_eq!(get("http.cimd").status, Status::Warn);
}

#[tokio::test]
async fn http_probe_fails_an_open_endpoint_and_an_unreachable_one() {
    let m = Mock::start().await;
    m.on("POST /mcp", json(200, j!({})));
    let out = probes::probe_http(&http_server(&format!("{}/mcp", m.base), &[]), &client()).await;
    assert_eq!(out.checks.len(), 1);
    assert_eq!(out.checks[0].status, Status::Fail);
    assert!(out.checks[0].summary.contains("200"));

    // a closed port
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let out = probes::probe_http(
        &http_server(&format!("http://127.0.0.1:{port}/mcp"), &[]),
        &client(),
    )
    .await;
    assert_eq!(out.checks[0].status, Status::Fail);
    assert!(out.checks[0].summary.starts_with("unreachable"));
}

#[test]
fn www_authenticate_parsing() {
    let h = r#"Bearer realm="OAuth", resource_metadata="https://a.example/x", scope="s""#;
    assert_eq!(
        probes::auth_param(h, "resource_metadata").as_deref(),
        Some("https://a.example/x")
    );
    assert_eq!(probes::auth_param(h, "scope").as_deref(), Some("s"));
    assert_eq!(probes::auth_param("Bearer", "scope"), None);
    assert_eq!(
        probes::prm_location("https://a.example/mcp").as_deref(),
        Some("https://a.example/.well-known/oauth-protected-resource/mcp")
    );
    assert_eq!(
        probes::as_metadata_location("https://a.example").as_deref(),
        Some("https://a.example/.well-known/oauth-authorization-server")
    );
}

#[tokio::test]
async fn git_probe_reads_branch_dirty_and_upstream() {
    let t = tempfile::tempdir().unwrap();
    let origin = t.path().join("origin.git");
    let work = t.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(
        t.path(),
        &[
            "init",
            "--quiet",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    git(&work, &["init", "--quiet", "-b", "main"]);
    std::fs::write(work.join("a.txt"), "a").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "one"]);
    git(
        &work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&work, &["push", "--quiet", "-u", "origin", "main"]);
    std::fs::write(work.join("a.txt"), "b").unwrap();
    git(&work, &["commit", "--quiet", "-am", "two"]);
    std::fs::write(work.join("b.txt"), "untracked").unwrap();

    let mut s = http_server("https://x.example/mcp", &[]);
    s.local_dir = work.clone();
    s.local_exists = true;
    let out = probes::probe_git(&s, false).await;
    let get = |id: &str| out.checks.iter().find(|c| c.id == id).unwrap().clone();
    assert_eq!(get("git.clone").status, Status::Pass);
    assert_eq!(get("git.branch").summary, "on main");
    assert_eq!(get("git.dirty").status, Status::Warn);
    assert_eq!(get("git.dirty").summary, "1 uncommitted change");
    assert_eq!(get("git.upstream").status, Status::Warn);
    assert!(get("git.upstream").summary.contains("1 ahead"));
    let head = git(&work, &["rev-parse", "HEAD"]);
    assert!(out.facts.contains(&("git.head".into(), head.clone())));
    assert_eq!(
        out.short.as_deref(),
        Some(format!("main {}", &head[..7]).as_str())
    );

    s.local_exists = false;
    let out = probes::probe_git(&s, false).await;
    assert_eq!(out.checks[0].status, Status::Fail);
    assert_eq!(out.checks[0].summary, "no local clone");
}

struct FakeGateway;
impl GatewaySource for FakeGateway {
    fn list_servers<'a>(
        &'a self,
        g: &'a GatewayConfig,
    ) -> BoxFuture<'a, Result<Vec<GatewayServerInfo>, String>> {
        assert_eq!(g.id, "main");
        Box::pin(async {
            Ok(vec![GatewayServerInfo {
                id: "weather".into(),
                status: Some("active".into()),
                health: Some("unhealthy".into()),
                access: Some("public".into()),
                auth_mode: Some("m2m".into()),
                last_error: Some("upstream 502".into()),
                ..Default::default()
            }])
        })
    }
}

struct FakeChecker;
impl RepoChecker for FakeChecker {
    fn check(&self, repo_dir: &Path) -> Vec<Check> {
        let name = repo_dir.file_name().unwrap().to_string_lossy().to_string();
        vec![Check::pass("pattern.pins", format!("{name} pinned"))]
    }
}

/// A temp instance: the fixture repos copied (weather made a git repo), the
/// fake Okta tool behind a call-logging wrapper.
fn temp_instance(t: &Path, cloudflare: bool) -> (Instance, String) {
    copy_dir(&fixtures().join("repos"), &t.join("repos"));
    let w = t.join("repos/weather-mcp-worker");
    git(&w, &["init", "--quiet", "-b", "main"]);
    git(&w, &["add", "."]);
    git(&w, &["commit", "--quiet", "-m", "init"]);
    let head = git(&w, &["rev-parse", "HEAD"]);

    let log = t.join("okta-calls.log");
    let tool = t.join("okta-tool");
    std::fs::write(
        &tool,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            fixtures().join("okta-fake.sh").display()
        ),
    )
    .unwrap();
    std::process::Command::new("chmod")
        .args(["+x", tool.to_str().unwrap()])
        .status()
        .unwrap();

    let base = std::fs::read_to_string(fixtures().join("instance/studio.toml")).unwrap();
    let mut text = base.replace(
        "[github]",
        &format!(
            r#"[identity.okta]
domain = "https://acme.okta.example"
authorization_server = "default"
interactive_client_id = "0oaINTERACTIVE"
policy_rules = ["0prINTERACTIVE", "0prM2M"]
admin_api = {{ tool = "{}", org = "acme" }}

[github]"#,
            tool.display()
        ),
    );
    text = text.replace(
        "[fleet]",
        "[fleet]\ndiscover = { github_topic = \"mcp-server\", require_file = \"conformance.json\" }",
    );
    if cloudflare {
        text.push_str(
            "\n[cloudflare]\naccount_id = \"resolve\"\napi_token = \"env:STUDIO_FLEET_TEST_UNSET_CF_TOKEN\"\n",
        );
    }
    std::fs::create_dir_all(t.join("instance")).unwrap();
    std::fs::write(t.join("instance/studio.toml"), text).unwrap();
    (Instance::load(&t.join("instance")).unwrap(), head)
}

fn github_mock(m: &Mock, weather_head: &str) {
    let repo = |name: &str, archived: bool, topics: &[&str]| {
        j!({"full_name": format!("acme/{name}"), "archived": archived, "topics": topics,
            "default_branch": "main", "pushed_at": "2026-01-01T00:00:00Z"})
    };
    m.on(
        "GET /orgs/acme/repos",
        json(
            200,
            j!([
                repo("weather-mcp-worker", false, &["mcp-server"]),
                repo("tides-mcp-worker", false, &[]),
                repo("retired-mcp-worker", true, &["mcp-server"]),
                repo("surf-mcp-worker", false, &["mcp-server"]),
                repo("notes", false, &["mcp-server"]),
                repo("old-mcp-worker", true, &["mcp-server"]),
            ]),
        ),
    )
    .on(
        "GET /repos/acme/surf-mcp-worker/contents/conformance.json",
        json(200, j!({})),
    )
    .on(
        "GET /repos/acme/old-mcp-worker/contents/conformance.json",
        json(200, j!({})),
    )
    .on(
        "GET /repos/acme/weather-mcp-worker",
        json(200, repo("weather-mcp-worker", false, &["mcp-server"])),
    )
    .on(
        "GET /repos/acme/weather-mcp-worker/branches/main",
        json(200, j!({"commit": {"sha": weather_head}})),
    )
    .on(
        "GET /repos/acme/tides-mcp-worker",
        json(200, repo("tides-mcp-worker", false, &[])),
    )
    .on(
        "GET /repos/acme/tides-mcp-worker/branches/main",
        json(
            200,
            j!({"commit": {"sha": "1111111111111111111111111111111111111111"}}),
        ),
    );
}

fn cloudflare_mock(m: &Mock, weather_head: &str) {
    let ok = |v: serde_json::Value| json(200, j!({"success": true, "errors": [], "result": v}));
    m.on("GET /accounts", ok(j!([{"id": "acct1", "name": "Acme"}])))
        .on(
            "GET /accounts/acct1/workers/scripts",
            ok(j!([
                {"id": "weather-mcp-worker", "tag": "tagweather", "modified_on": "2026-01-01T00:00:00Z"},
                {"id": "tides-mcp-worker", "tag": "tagtides"},
                {"id": "tides-mcp-worker-staging", "tag": "tagstaging"}
            ])),
        )
        .on(
            "GET /accounts/acct1/workers/domains",
            ok(j!([
                {"hostname": "weather.mcp.acme.example", "service": "weather-mcp-worker", "environment": "production"},
                {"hostname": "tides.mcp.acme.example", "service": "someone-else", "environment": "production"}
            ])),
        )
        .on(
            "GET /accounts/acct1/builds/workers/tagweather/builds",
            ok(j!([
                {"build_uuid": "b1", "status": "stopped", "build_outcome": "success",
                 "created_on": "2026-01-02T00:00:00Z",
                 "build_trigger_metadata": {"branch": "main", "commit_hash": weather_head}},
                {"build_uuid": "b0", "status": "stopped", "build_outcome": "fail",
                 "created_on": "2026-01-01T00:00:00Z",
                 "build_trigger_metadata": {"branch": "main", "commit_hash": "0000000"}}
            ])),
        )
        .on(
            "GET /accounts/acct1/builds/workers/tagtides/builds",
            ok(j!([
                {"build_uuid": "t0", "status": "stopped", "build_outcome": "success",
                 "created_on": "2025-12-01T00:00:00Z",
                 "build_trigger_metadata": {"commit_hash": "2222222222222222222222222222222222222222"}},
                {"build_uuid": "t1", "status": "stopped", "build_outcome": "fail",
                 "created_on": "2026-01-03T00:00:00Z",
                 "build_trigger_metadata": {"commit_hash": "1111111111111111111111111111111111111111"}}
            ])),
        );
}

#[tokio::test]
async fn a_full_run_over_mocks_covers_every_source() {
    let t = tempfile::tempdir().unwrap();
    let (inst, head) = temp_instance(t.path(), true);
    let gh = Mock::start().await;
    github_mock(&gh, &head);
    let cf = Mock::start().await;
    cloudflare_mock(&cf, &head);

    let endpoints = Endpoints {
        github_api: gh.base.clone(),
        cloudflare_api: cf.base.clone(),
        github_token: Some(Secret::new("gh-test-token")),
        cloudflare_token: Some(Secret::new("cf-test-token")),
        http_timeout: Duration::from_secs(5),
    };
    let disc = studio_fleet::discover(&inst, &endpoints).await;
    let names: Vec<&str> = disc.servers.iter().map(|s| s.name.as_str()).collect();
    // topic + require_file adds surf; archived old/retired are dropped; notes lacks the file
    assert_eq!(
        names,
        vec!["weather-mcp-worker", "tides-mcp-worker", "surf-mcp-worker"]
    );
    assert!(
        disc.notes
            .iter()
            .any(|n| n.contains("old-mcp-worker is archived")),
        "{:?}",
        disc.notes
    );

    let mut sources: BTreeSet<Source> = Source::ALL.into_iter().collect();
    sources.remove(&Source::Http); // no real network
    let opts = StatusOptions {
        sources: Some(sources),
        servers: vec!["weather-mcp-worker".into(), "acme/tides-mcp-worker".into()],
        refresh: true,
        fetch: false,
        endpoints,
    };
    let providers = Providers::new(Some(Box::new(FakeGateway)), Some(Box::new(FakeChecker)));
    let r = studio_fleet::probe(&inst, &disc, &opts, &providers).await;
    assert_eq!(r.servers.len(), 2);
    assert_eq!(
        r.sources,
        vec![
            Source::Git,
            Source::Github,
            Source::Cloudflare,
            Source::Okta,
            Source::Gateway,
            Source::Marketplace,
            Source::Pattern
        ]
    );

    // git
    assert_eq!(st(&r, "weather-mcp-worker", "git.clone"), Status::Pass);
    assert_eq!(st(&r, "weather-mcp-worker", "git.dirty"), Status::Pass);
    assert_eq!(st(&r, "weather-mcp-worker", "git.upstream"), Status::Skip);
    // github
    assert_eq!(st(&r, "weather-mcp-worker", "github.head"), Status::Pass);
    assert_eq!(st(&r, "weather-mcp-worker", "github.sync"), Status::Pass);
    assert_eq!(st(&r, "weather-mcp-worker", "github.topic"), Status::Pass);
    assert_eq!(st(&r, "tides-mcp-worker", "github.topic"), Status::Warn);
    // cloudflare
    assert_eq!(st(&r, "weather-mcp-worker", "cf.script"), Status::Pass);
    assert_eq!(st(&r, "weather-mcp-worker", "cf.domain"), Status::Pass);
    assert_eq!(st(&r, "weather-mcp-worker", "cf.build"), Status::Pass);
    assert_eq!(st(&r, "weather-mcp-worker", "cf.deployed"), Status::Pass);
    assert_eq!(st(&r, "tides-mcp-worker", "cf.domain"), Status::Fail);
    assert_eq!(st(&r, "tides-mcp-worker", "cf.build"), Status::Fail);
    assert_eq!(st(&r, "tides-mcp-worker", "cf.deployed"), Status::Warn);
    assert_eq!(
        st(&r, "tides-mcp-worker", "cf.script.staging"),
        Status::Pass
    );
    assert_eq!(
        st(&r, "tides-mcp-worker", "cf.domain.staging"),
        Status::Fail
    );
    let tides = r
        .servers
        .iter()
        .find(|s| s.name == "tides-mcp-worker")
        .unwrap();
    assert_eq!(
        tides.facts.get("cf.deployed.commit").map(String::as_str),
        Some("2222222222222222222222222222222222222222")
    );
    // okta: fetched once for the run, not per server
    assert_eq!(st(&r, "weather-mcp-worker", "okta.scopes"), Status::Pass);
    assert_eq!(
        st(&r, "weather-mcp-worker", "okta.redirect_uri"),
        Status::Pass
    );
    let (s, why) = status_of(&r, "tides-mcp-worker", "okta.scopes").unwrap();
    assert_eq!(s, Status::Fail);
    assert_eq!(why, "Interactive lacks tides:write");
    assert_eq!(
        st(&r, "tides-mcp-worker", "okta.redirect_uri"),
        Status::Fail
    );
    let calls = std::fs::read_to_string(t.path().join("okta-calls.log")).unwrap();
    assert_eq!(calls.lines().count(), 4, "{calls}");
    assert!(
        calls
            .lines()
            .all(|l| l.starts_with("GET /api/v1/") && l.ends_with("--org acme"))
    );
    // gateway
    assert_eq!(
        st(&r, "weather-mcp-worker", "gateway.registered"),
        Status::Pass
    );
    assert_eq!(st(&r, "weather-mcp-worker", "gateway.status"), Status::Pass);
    assert_eq!(st(&r, "weather-mcp-worker", "gateway.health"), Status::Fail);
    assert_eq!(
        st(&r, "tides-mcp-worker", "gateway.registered"),
        Status::Fail
    );
    // marketplace
    let (s, why) = status_of(&r, "weather-mcp-worker", "marketplace.acme").unwrap();
    assert_eq!(
        (s, why.as_str()),
        (
            Status::Fail,
            "weather-mcp-worker listed for claude but not codex"
        )
    );
    let (s, why) = status_of(&r, "tides-mcp-worker", "marketplace.acme").unwrap();
    assert_eq!(s, Status::Warn);
    assert!(
        why.contains("claude 0.2.0") && why.contains("0.3.0"),
        "{why}"
    );
    // pattern
    assert_eq!(st(&r, "weather-mcp-worker", "pattern.pins"), Status::Pass);

    // columns: one per source, worst status wins
    let w = r
        .servers
        .iter()
        .find(|s| s.name == "weather-mcp-worker")
        .unwrap();
    assert_eq!(w.columns.len(), 7);
    assert_eq!(w.column(Source::Cloudflare).unwrap().status, Status::Pass);
    assert_eq!(w.column(Source::Gateway).unwrap().status, Status::Fail);
    assert_eq!(w.column(Source::Gateway).unwrap().text, "unhealthy");

    // every GitHub and Cloudflare call carried its token
    for (k, auth) in gh
        .seen
        .lock()
        .unwrap()
        .iter()
        .chain(cf.seen.lock().unwrap().iter())
    {
        assert!(
            auth.as_deref().is_some_and(|a| a.starts_with("Bearer ")),
            "{k} without auth"
        );
    }
    // the report serializes and never contains a token
    let text = serde_json::to_string(&r).unwrap();
    assert!(!text.contains("gh-test-token") && !text.contains("cf-test-token"));
}

#[tokio::test]
async fn unconfigured_sources_skip_instead_of_failing() {
    let t = tempfile::tempdir().unwrap();
    let (inst, _) = temp_instance(t.path(), true);
    let disc = studio_fleet::Discovery {
        servers: vec![derive_server(&inst, "acme/weather-mcp-worker", vec![])],
        notes: vec![],
    };
    let opts = StatusOptions {
        sources: Some([Source::Cloudflare, Source::Gateway, Source::Pattern].into()),
        refresh: true,
        endpoints: Endpoints {
            // unreachable on purpose: must never be called without a token
            cloudflare_api: "http://127.0.0.1:9".into(),
            github_api: "http://127.0.0.1:9".into(),
            github_token: Some(Secret::new("x")),
            ..Endpoints::default()
        },
        ..StatusOptions::default()
    };
    let r = studio_fleet::probe(&inst, &disc, &opts, &Providers::default()).await;
    // pattern isn't a column without a checker
    assert_eq!(r.sources, vec![Source::Cloudflare, Source::Gateway]);
    assert_eq!(
        status_of(&r, "weather-mcp-worker", "cf.script"),
        Some((Status::Skip, "cloudflare token not configured".into()))
    );
    assert_eq!(
        status_of(&r, "weather-mcp-worker", "gateway.registered"),
        Some((Status::Skip, "gateway client not wired".into()))
    );
}

#[tokio::test]
async fn cache_round_trips_and_filters() {
    let t = tempfile::tempdir().unwrap();
    let (inst, _) = temp_instance(t.path(), false);
    let disc = studio_fleet::Discovery {
        servers: vec![
            derive_server(&inst, "acme/weather-mcp-worker", vec![]),
            derive_server(&inst, "acme/tides-mcp-worker", vec![]),
        ],
        notes: vec![],
    };
    let opts = StatusOptions {
        sources: Some([Source::Git, Source::Marketplace].into()),
        ..StatusOptions::default()
    };
    let r = studio_fleet::probe(&inst, &disc, &opts, &Providers::default()).await;
    let file = t.path().join("cache/fleet.json");
    cache::store_to(&file, &r, &[], false);

    let all = [Source::Git, Source::Marketplace];
    let hit = cache::load_from(&file, 300, &all, &[], &[], false).unwrap();
    assert!(hit.from_cache);
    assert_eq!(hit.servers.len(), 2);

    let one = cache::load_from(
        &file,
        300,
        &[Source::Git],
        &["tides-mcp-worker".into()],
        &[],
        false,
    )
    .unwrap();
    assert_eq!(one.servers.len(), 1);
    assert_eq!(one.sources, vec![Source::Git]);
    assert!(
        one.servers[0]
            .checks
            .iter()
            .all(|c| c.id.starts_with("git."))
    );
    assert_eq!(one.servers[0].columns.len(), 1);

    // misses: expired, uncovered source, unknown server, providers changed, fetch wanted
    assert!(cache::load_from(&file, 0, &all, &[], &[], false).is_none());
    assert!(cache::load_from(&file, 300, &[Source::Okta], &[], &[], false).is_none());
    assert!(cache::load_from(&file, 300, &all, &["nope".into()], &[], false).is_none());
    assert!(cache::load_from(&file, 300, &all, &[], &["gateway".into()], false).is_none());
    assert!(cache::load_from(&file, 300, &all, &[], &[], true).is_none());
}

#[tokio::test]
async fn okta_tool_failures_become_skips() {
    let mut cfg = studio_core::config::OktaConfig {
        domain: "https://acme.okta.example".into(),
        authorization_server: "missing".into(),
        audience: None,
        interactive_client_id: None,
        m2m_client_id: None,
        m2m_client_secret: None,
        policy_rules: vec!["0prX".into()],
        admin_api: Some(studio_core::config::OktaAdminApi {
            tool: fixtures().join("okta-fake.sh").display().to_string(),
            org: None,
        }),
    };
    let e = studio_fleet::okta::load(&cfg).await.unwrap_err();
    assert!(e.contains("E0000007"), "{e}");
    cfg.admin_api.as_mut().unwrap().tool = "/nonexistent/okta-tool".into();
    let e = studio_fleet::okta::load(&cfg).await.unwrap_err();
    assert!(e.contains("not installed"), "{e}");

    let s = http_server("https://x.example/mcp", &["x:read"]);
    let out = probes::probe_okta(&s, Err(e.as_str()));
    assert_eq!(out.checks[0].status, Status::Skip);
}

#[tokio::test]
async fn github_probe_handles_missing_repos_and_api_errors() {
    let m = Mock::start().await;
    m.on("GET /repos/acme/broken", json(500, j!({"message": "boom"})));
    let api = studio_fleet::github::GithubApi::new(&m.base, Some(Secret::new("t")), client());
    let mut s = http_server("https://x.example/mcp", &[]);
    s.repo = "acme/gone".into();
    let out = probes::probe_github(&s, Ok(&api), None).await;
    assert_eq!(out.checks[0].status, Status::Fail);
    s.repo = "acme/broken".into();
    let out = probes::probe_github(&s, Ok(&api), None).await;
    assert_eq!(out.checks[0].status, Status::Skip);
    assert!(out.checks[0].evidence.as_deref().unwrap().contains("500"));
    let none = studio_fleet::github::GithubApi::new(&m.base, None, client());
    assert_eq!(
        probes::probe_github(&s, Ok(&none), None).await.checks[0].status,
        Status::Skip
    );
}

#[test]
fn a_fake_checker_is_send_and_sync() {
    let _p: Arc<dyn RepoChecker> = Arc::new(FakeChecker);
}
