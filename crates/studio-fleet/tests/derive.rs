//! Discovery and derivation from the acme fixture repos (no network).

mod common;

use pretty_assertions::assert_eq;
use studio_core::Instance;
use studio_fleet::discover::{configured_members, derive_server, first_label};

fn inst() -> Instance {
    Instance::load(&common::fixtures().join("instance")).unwrap()
}

#[test]
fn membership_is_include_then_servers_minus_exclude() {
    let i = inst();
    let m: Vec<String> = configured_members(&i).into_iter().map(|(r, _)| r).collect();
    assert_eq!(
        m,
        vec![
            "acme/weather-mcp-worker",
            "acme/retired-mcp-worker",
            "acme/tides-mcp-worker"
        ]
    );
}

#[tokio::test]
async fn discovery_without_github_uses_config_and_notes_it() {
    let i = inst();
    let d = studio_fleet::discover::discover(&i, None).await;
    let names: Vec<&str> = d.servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["weather-mcp-worker", "tides-mcp-worker"]);
    assert_eq!(d.servers[0].origins, vec!["include", "server"]);
    assert!(
        d.notes.iter().any(|n| n.contains("GitHub")),
        "{:?}",
        d.notes
    );
}

#[test]
fn derives_url_scopes_version_and_gateway_id_from_public_url() {
    let s = derive_server(&inst(), "acme/weather-mcp-worker", vec![]);
    assert!(s.local_exists);
    assert_eq!(s.worker.as_deref(), Some("weather-mcp-worker"));
    assert_eq!(
        s.url.as_deref(),
        Some("https://weather.mcp.acme.example/mcp")
    );
    assert_eq!(s.url_source.as_deref(), Some("PUBLIC_MCP_URL"));
    assert_eq!(s.hosts, vec!["weather.mcp.acme.example"]);
    assert_eq!(s.scopes, vec!["weather:read"]);
    assert_eq!(s.version.as_deref(), Some("1.2.0"));
    assert_eq!(s.gateway_id.as_deref(), Some("weather"));
    assert_eq!(s.gateway.as_deref(), Some("main"));
    assert_eq!(s.marketplace_slug, "weather-mcp-worker");
    assert!(s.environments.is_empty());
    assert!(s.problems.is_empty(), "{:?}", s.problems);
}

#[test]
fn derives_from_routes_envs_and_config_overrides() {
    let s = derive_server(&inst(), "acme/tides-mcp-worker", vec![]);
    // no PUBLIC_MCP_URL: the custom domain gives the URL
    assert_eq!(s.url.as_deref(), Some("https://tides.mcp.acme.example/mcp"));
    assert_eq!(s.url_source.as_deref(), Some("route"));
    assert_eq!(s.hosts, vec!["tides.mcp.acme.example"]);
    assert_eq!(s.scopes, vec!["tides:read", "tides:write"]);
    assert_eq!(
        s.gateway_id.as_deref(),
        Some("tide"),
        "config wins over the host label"
    );
    assert_eq!(s.marketplace_slug, "acme-tides");
    let names: Vec<_> = s.environments.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["dev", "staging"]);
    let st = &s.environments[1];
    assert!(st.deployed);
    assert_eq!(st.worker.as_deref(), Some("tides-mcp-worker-staging"));
    assert_eq!(
        st.url.as_deref(),
        Some("https://tides-staging.mcp.acme.example/mcp")
    );
    assert_eq!(st.scopes, vec!["mcp:read"]);
    assert_eq!(st.hosts, vec!["tides-staging.mcp.acme.example"]);
    let dev = &s.environments[0];
    assert!(!dev.deployed);
    assert_eq!(dev.worker.as_deref(), Some("tides-dev"));
    assert_eq!(dev.url, None);
}

#[test]
fn missing_clone_is_not_a_problem_but_is_reported() {
    let s = derive_server(&inst(), "acme/retired-mcp-worker", vec![]);
    assert!(!s.local_exists);
    assert_eq!(s.url, None);
    assert_eq!(s.gateway_id, None);
    assert!(s.problems.is_empty());
}

#[test]
fn gateway_id_is_the_first_host_label() {
    assert_eq!(first_label("https://a.b.example/mcp").as_deref(), Some("a"));
    assert_eq!(first_label("http://127.0.0.1:8080/mcp"), None);
    assert_eq!(first_label("not a url"), None);
}
