//! Scaffolding a new server from the real `cf-workers-ts` pack with the
//! example (acme) instance's values.

use std::fs;
use std::path::{Path, PathBuf};

use studio_core::check::Status;
use studio_pattern::conformance::RepoManifest;
use studio_pattern::scaffold::{PENDING_D1_ID, PENDING_KV_ID, placeholder_hits};
use studio_pattern::{
    BlessOptions, Conformance, Pack, ScaffoldError, ScaffoldSpec, Values, lint_repo,
    render_scaffold, scaffold,
};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn acme_values() -> Values {
    let text =
        fs::read_to_string(workspace().join("examples/instance/pattern-values.toml")).unwrap();
    toml::from_str(&text).unwrap()
}

fn pack() -> Pack {
    Pack::load(&workspace().join("patterns/cf-workers-ts")).unwrap()
}

fn spec() -> ScaffoldSpec {
    let mut s = ScaffoldSpec::new("weather");
    s.display_name = "Acme Weather".into();
    s.description = "Forecasts and observations for Acme sites".into();
    s
}

#[test]
fn a_scaffold_passes_the_placeholder_rules_and_lint() {
    let pack = pack();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("weather-mcp-worker");
    let summary = scaffold(&pack, &acme_values(), &spec(), &out).unwrap();

    assert_eq!(summary.worker, "weather-mcp-worker");
    assert_eq!(summary.host, "weather.mcp.acme.example");
    assert_eq!(summary.url, "https://weather.mcp.acme.example/mcp");
    assert_eq!(summary.scopes, vec!["weather:read", "weather:write"]);
    assert_eq!(summary.d1_database, None);
    assert_eq!(summary.conformance.len(), pack.security_files().len());

    let checks = lint_repo(&pack, &out);
    let bad: Vec<_> = checks
        .iter()
        .filter(|c| c.status != Status::Pass)
        .map(|c| format!("{} {:?}: {} {:?}", c.id, c.status, c.summary, c.evidence))
        .collect();
    assert!(bad.is_empty(), "{bad:#?}");
    for id in [
        "pattern.version",
        "pattern.hashes",
        "pattern.files",
        "pattern.scripts",
        "pattern.placeholders",
        "pattern.wrangler.route",
        "pattern.wrangler.scopes",
        "pattern.lockfile",
        "pattern.pins.lock",
    ] {
        assert!(checks.iter().any(|c| c.id == id), "{id} not reported");
    }

    let wrangler = fs::read_to_string(out.join("wrangler.toml")).unwrap();
    assert!(wrangler.contains("name = \"weather-mcp-worker\""));
    assert!(
        wrangler.contains(
            "\nroutes = [{ pattern = \"weather.mcp.acme.example\", custom_domain = true }]"
        )
    );
    assert!(wrangler.contains("PUBLIC_MCP_URL = \"https://weather.mcp.acme.example/mcp\""));
    assert!(wrangler.contains("OKTA_M2M_SCOPE = \"weather:read weather:write\""));
    assert!(wrangler.contains("OKTA_ISSUER = \"https://acme.okta.example/oauth2/default\""));
    assert!(wrangler.contains("OKTA_CLIENT_ID = \"0oaEXAMPLEINTERACTIVE\""));
    assert!(wrangler.contains(PENDING_KV_ID));
    assert!(
        !wrangler.contains("d1_databases"),
        "D1 block kept without --d1"
    );

    let pkg: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(out.join("package.json")).unwrap()).unwrap();
    assert_eq!(pkg["name"], "weather-mcp-worker");
    assert_eq!(
        pkg["scripts"]["ci"],
        "npm run conformance && npm run check:placeholders && npm run typecheck && npm run test"
    );
    assert_eq!(
        pkg["scripts"]["conformance"],
        "node scripts/check-conformance.mjs"
    );

    let scopes = fs::read_to_string(out.join("src/scopes.ts")).unwrap();
    assert!(scopes.contains("export const READ_SCOPE = \"weather:read\";"));
    let state = fs::read_to_string(out.join("src/oauth-state.ts")).unwrap();
    assert!(state.contains("\"__Host-weather_oauth_csrf-\""));
    let consent = fs::read_to_string(out.join("src/consent.ts")).unwrap();
    assert!(consent.contains("\"weather:write\": \"Create, change or permanently delete"));

    // conformance.json names the pack version and every security file's hash.
    let m = RepoManifest::read(&out, "conformance.json")
        .unwrap()
        .unwrap();
    assert_eq!(m.scaffold_version.as_deref(), Some(pack.version()));
    assert_eq!(m.files, summary.conformance);
}

#[test]
fn the_template_check_script_agrees() {
    let pack = pack();
    let dir = tempfile::tempdir().unwrap();
    scaffold(&pack, &acme_values(), &spec(), dir.path()).unwrap();
    let Ok(out) = std::process::Command::new("bash")
        .arg("scripts/check-placeholders.sh")
        .current_dir(dir.path())
        .output()
    else {
        return; // no bash: the lint test above covers the same patterns
    };
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn drift_passes_once_blessed_into_the_store() {
    let pack = pack();
    let values = acme_values();
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("weather-mcp-worker");
    let store = dir.path().join("conformance");
    scaffold(&pack, &values, &spec(), &repo).unwrap();
    let before = fs::read(repo.join("conformance.json")).unwrap();

    let c = Conformance::new(&pack, &values, &store).unwrap();
    let drift = c.drift_check("weather-mcp-worker", &repo);
    assert_eq!(drift.status, Status::Fail, "nothing blessed yet");

    let outcome = c
        .bless("weather-mcp-worker", &repo, BlessOptions::default())
        .unwrap();
    assert!(
        !outcome.manifest_written,
        "scaffolded conformance.json is current"
    );
    assert_eq!(fs::read(repo.join("conformance.json")).unwrap(), before);
    let all = c.check_and_lint("weather-mcp-worker", &repo);
    let bad: Vec<_> = all.iter().filter(|c| c.status != Status::Pass).collect();
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
fn d1_and_custom_scopes_are_filled() {
    let pack = pack();
    let mut s = spec();
    s.slug = "acme-notes".into();
    s.worker = Some("acme-notes-mcp-worker".into());
    s.scopes = vec!["notes:read".into(), "notes:refresh".into()];
    s.with_d1 = true;
    let (tree, summary) = render_scaffold(&pack, &acme_values(), &s).unwrap();
    assert!(placeholder_hits(&pack, &tree).is_empty());
    assert_eq!(
        summary.d1_database.as_deref(),
        Some("acme-notes-mcp-worker")
    );
    let text = |f: &str| String::from_utf8(tree[f].bytes.clone()).unwrap();
    let w = text("wrangler.toml");
    assert!(w.contains("database_name = \"acme-notes-mcp-worker\""));
    assert!(w.contains(PENDING_D1_ID));
    assert!(w.contains("OKTA_M2M_SCOPE = \"notes:read notes:refresh\""));
    assert!(text("src/consent.ts").contains("\"notes:refresh\": \"Refresh in Acme Weather.\""));
    assert!(text("src/oauth-state.ts").contains("__Host-acmenotes_consent_csrf-"));
    assert!(
        text("test/matrix.params.ts")
            .contains("advertisedScopes: [\"notes:read\", \"notes:refresh\"]")
    );
}

#[test]
fn a_non_empty_target_and_a_bad_spec_are_refused() {
    let pack = pack();
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("keep.txt"), "x").unwrap();
    let e = scaffold(&pack, &acme_values(), &spec(), dir.path()).unwrap_err();
    assert!(e.to_string().contains("not empty"), "{e}");

    let mut bad = spec();
    bad.scopes = vec!["weather:read".into()];
    let e = render_scaffold(&pack, &acme_values(), &bad).unwrap_err();
    assert!(matches!(e, ScaffoldError::Spec(_)), "{e}");

    let mut values = acme_values();
    values.remove("okta_client_id");
    let e = render_scaffold(&pack, &values, &spec()).unwrap_err();
    assert!(e.to_string().contains("okta_client_id"), "{e}");
}
