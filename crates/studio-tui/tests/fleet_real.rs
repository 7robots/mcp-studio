//! `FleetModule::new` over a real instance: the report comes from
//! `studio_fleet::fleet_status`, answered here by a fresh cache entry so no
//! probe touches the network.

use std::collections::BTreeMap;
use std::sync::Arc;

use studio_core::Instance;
use studio_core::check::Check;
use studio_fleet::{FleetReport, Providers, ServerInfo, ServerStatus};
use studio_tui::fleet::FleetModule;
use studio_tui::{App, Harness};

/// Removes the instance's cache directory, however the test ends.
struct CacheDir(std::path::PathBuf);

impl Drop for CacheDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn server(inst: &Instance) -> ServerStatus {
    let info = ServerInfo {
        repo: "acme/weather".into(),
        name: "weather".into(),
        local_dir: inst.repo_dir("acme/weather"),
        local_exists: false,
        origins: vec!["include".into()],
        worker: Some("weather-worker".into()),
        hosts: vec!["weather.mcp.acme.example".into()],
        url: Some("https://weather.mcp.acme.example/mcp".into()),
        url_source: Some("config".into()),
        scopes: Vec::new(),
        version: None,
        gateway: None,
        gateway_id: None,
        marketplace_slug: "weather".into(),
        environments: Vec::new(),
        problems: Vec::new(),
    };
    ServerStatus {
        repo: info.repo.clone(),
        name: info.name.clone(),
        url: info.url.clone(),
        gateway_id: None,
        local_dir: info.local_dir.clone(),
        checks: vec![Check::pass("http.unauth_401", "401 without a token")],
        columns: Vec::new(),
        facts: BTreeMap::new(),
        info,
    }
}

#[tokio::test]
async fn the_real_loader_reads_fleet_status() {
    let dir = tempfile::tempdir().unwrap();
    let name = format!("acme-tui-fleet-{}", std::process::id());
    std::fs::write(
        dir.path().join("studio.toml"),
        format!(
            r#"[instance]
name = "{name}"
display_name = "Acme test fleet"

[fleet]
include = ["acme/weather"]

[probes]
git = false
github = false
cloudflare = false
http = false
okta = false
gateway = false
marketplace = false
"#
        ),
    )
    .unwrap();
    let inst = Instance::load(dir.path()).unwrap();
    let _cleanup = CacheDir(inst.cache_dir());
    let now = studio_fleet::time::now_unix();
    let report = FleetReport {
        instance: name.clone(),
        generated_at: studio_fleet::time::format_rfc3339(now),
        generated_at_unix: now,
        sources: Vec::new(),
        servers: vec![server(&inst)],
        notes: Vec::new(),
        from_cache: false,
    };
    studio_fleet::cache::store(&inst, &report, &[], false);

    let module = FleetModule::new(Arc::new(inst), Providers::default());
    let (app, rx) = App::new("Acme test fleet", vec![Box::new(module)]);
    let mut h = Harness::new(app, rx, (160, 30));
    h.until_text("Servers (1)").await;
    assert!(h.lines()[1].contains(&name), "{}", h.lines()[1]);
    assert!(h.lines()[1].contains("(cached)"));
    assert!(h.text().contains("weather-worker"));
    assert!(h.text().contains("(no clone)"));
}
