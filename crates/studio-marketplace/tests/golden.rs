//! Generator golden tests: `tests/fixtures/acme` is a complete marketplace
//! whose generated files were produced by this crate and reviewed by hand.
//! Regenerating it must be byte-identical. `UPDATE_GOLDEN=1` rewrites it.

mod common;

use common::{acme, copy_tree, fixtures};
use pretty_assertions::assert_eq;
use studio_core::check::Status;
use studio_marketplace::generate;
use studio_marketplace::ojson::Json;
use studio_marketplace::{CLAUDE_CATALOG, CODEX_CATALOG, ops, reconcile, tree, verify, yaml};

fn golden() -> std::path::PathBuf {
    fixtures().join("acme")
}

#[test]
fn regenerating_the_fixture_is_byte_identical() {
    let root = golden();
    let m = acme();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        let scan = tree::scan(&root);
        let files = generate::generate(&m, &scan.plugins, None, None);
        ops::write_tree(&root, &files).unwrap();
    }
    let r = verify(&root, &m).unwrap();
    assert!(r.is_clean(), "{:#?}", r.diffs);
    assert_eq!(r.generated, r.identical);
    // catalogs + skill plugin (3) + search/wiki (4 each) + tickets/weather (5 each)
    assert_eq!(r.generated, 2 + 3 + 4 + 4 + 5 + 5);
}

#[test]
fn server_yaml_round_trips_unchanged() {
    for p in tree::scan(&golden()).plugins {
        let path = golden().join(p.path("server.yaml"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            yaml::rewrite(&p.entry, Some(&text)),
            text,
            "{}",
            path.display()
        );
    }
}

#[test]
fn reconcile_is_all_green_on_the_fixture() {
    let checks = reconcile(&golden(), &acme());
    let bad: Vec<_> = checks.iter().filter(|c| c.status != Status::Pass).collect();
    assert!(bad.is_empty(), "{bad:#?}");
}

fn read(rel: &str) -> Json {
    Json::parse(&std::fs::read_to_string(golden().join(rel)).unwrap()).unwrap()
}

#[test]
fn claude_catalog_entries_never_carry_url() {
    let c = read(CLAUDE_CATALOG);
    for e in c.get("plugins").unwrap().as_array().unwrap() {
        assert!(e.get("url").is_none());
    }
}

#[test]
fn codex_auth_follows_the_table() {
    let c = read(CODEX_CATALOG);
    let auth = |name: &str| {
        c.get("plugins")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e.get("name").and_then(Json::as_str) == Some(name))
            .unwrap()
            .get("policy")
            .unwrap()
            .get("authentication")
            .and_then(Json::as_str)
            .map(str::to_string)
    };
    assert_eq!(auth("acme-search"), None);
    assert_eq!(auth("acme-wiki").as_deref(), Some("ON_FIRST_USE"));
    assert_eq!(auth("acme-tickets").as_deref(), Some("ON_INSTALL"));
    assert_eq!(auth("acme-weather").as_deref(), Some("ON_INSTALL"));
}

#[test]
fn only_credentialed_entries_get_a_codex_mcp_file() {
    let has = |slug: &str| {
        golden()
            .join(format!("servers/{slug}/.codex-plugin/mcp.json"))
            .exists()
    };
    assert!(has("acme-tickets") && has("acme-weather"));
    assert!(!has("acme-search") && !has("acme-wiki"));
    let t = read("servers/acme-tickets/.codex-plugin/mcp.json");
    let h = t.get("mcpServers").unwrap().get("acme-tickets").unwrap();
    assert_eq!(
        h.get("http_headers")
            .unwrap()
            .get("Authorization")
            .unwrap()
            .as_str(),
        Some("Bearer <YOUR_API_KEY>")
    );
    let w = read("servers/acme-weather/.mcp.json");
    let h = w.get("mcpServers").unwrap().get("acme-weather").unwrap();
    assert_eq!(
        h.get("headers").unwrap().get("X-API-Key").unwrap().as_str(),
        Some("<YOUR_API_KEY>")
    );
}

#[test]
fn catalog_upsert_keeps_order_and_foreign_keys() {
    let root = tempfile::tempdir().unwrap();
    copy_tree(&golden(), root.path());
    let c = read(CLAUDE_CATALOG).with("metadata", Json::object().with("version", Json::str("1")));
    let scan = tree::scan(root.path());
    let p = scan.find("acme-search").unwrap();
    let up = generate::upsert(&c, generate::claude_entry(p));
    assert_eq!(up.to_pretty(), c.to_pretty());
    let removed = generate::remove_from(&c, "acme-search");
    let names: Vec<_> = removed
        .get("plugins")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.get("name").unwrap().as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        [
            "acme-marketplace-tools",
            "acme-tickets",
            "acme-weather",
            "acme-wiki"
        ]
    );
}

#[test]
fn drift_is_reported_by_id_and_by_diff() {
    let t = tempfile::tempdir().unwrap();
    copy_tree(&golden(), t.path());
    let root = t.path();
    let edit = |rel: &str, from: &str, to: &str| {
        let p = root.join(rel);
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains(from));
        std::fs::write(p, s.replacen(from, to, 1)).unwrap();
    };
    // The two kinds of drift seen in the wild: an oauth entry whose Codex
    // policy lacks ON_FIRST_USE, and a category that never reached the catalog.
    edit(
        CODEX_CATALOG,
        "\"installation\": \"AVAILABLE\",\n        \"authentication\": \"ON_FIRST_USE\"",
        "\"installation\": \"AVAILABLE\"",
    );
    edit(
        CLAUDE_CATALOG,
        "      \"category\": \"Developer Tools\",\n",
        "",
    );

    let failing: Vec<String> = reconcile(root, &acme())
        .into_iter()
        .filter(|c| c.status != Status::Pass)
        .map(|c| c.id)
        .collect();
    assert_eq!(
        failing,
        [
            "marketplace.acme-search.category",
            "marketplace.acme-wiki.codex_auth"
        ]
    );
    let r = verify(root, &acme()).unwrap();
    let paths: Vec<_> = r.diffs.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(paths, [CODEX_CATALOG, CLAUDE_CATALOG]);
    assert!(
        r.diffs[0]
            .diff
            .contains("+        \"authentication\": \"ON_FIRST_USE\"")
    );

    // Regenerating each entry fixes it.
    let m = acme();
    for slug in ["acme-search", "acme-wiki"] {
        let cs = ops::plan_regenerate(root, &m, slug).unwrap();
        cs.apply(root).unwrap();
    }
    assert!(verify(root, &m).unwrap().is_clean());
}

#[test]
fn a_missing_marketplace_is_a_skip() {
    let checks = reconcile(std::path::Path::new("/nonexistent/acme"), &acme());
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, Status::Skip);
}
