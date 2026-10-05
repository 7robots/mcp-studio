//! Port of the schema self-test: every shipped template validates, every
//! bad fixture is rejected — as a *data* problem, never a broken schema.
//! Plus the invariants no schema can express.

mod common;

use std::path::Path;

use common::{acme, copy_tree, fixtures};
use studio_marketplace::schema::SchemaId;
use studio_marketplace::validate::{FindingKind, validate_dir, validate_text};

fn templates() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../marketplace/templates")
}

fn check(id: SchemaId, path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).unwrap();
    let yaml = path.extension().is_some_and(|e| e == "yaml");
    validate_text(id, &text, yaml)
}

#[test]
fn templates_validate() {
    for (id, file) in [
        (SchemaId::Server, "server.yaml"),
        (SchemaId::Server, "server-skill.yaml"),
        (SchemaId::ClaudeMarketplace, "claude-marketplace.json"),
        (SchemaId::CodexMarketplace, "codex-marketplace.json"),
        (SchemaId::ClaudePlugin, "claude-plugin.json"),
        (SchemaId::CodexPlugin, "codex-plugin.json"),
        (SchemaId::McpJson, "mcp.json"),
        (SchemaId::CodexMcpJson, "codex-mcp.json"),
    ] {
        let errors = check(id, &templates().join(file)).unwrap();
        assert!(errors.is_empty(), "{file}: {errors:?}");
    }
}

#[test]
fn bad_fixtures_are_rejected() {
    let bad = fixtures().join("bad");
    for (id, file) in [
        (SchemaId::ClaudeMarketplace, "claude-url-in-entry.json"),
        (SchemaId::ClaudeMarketplace, "claude-bad-source.json"),
        (SchemaId::ClaudeMarketplace, "claude-bad-slug.json"),
        (SchemaId::CodexMarketplace, "codex-auth-none.json"),
        (SchemaId::Server, "server-missing-description.yaml"),
        (SchemaId::Server, "server-http-url.yaml"),
        (SchemaId::Server, "server-mcp-no-url.yaml"),
        (SchemaId::McpJson, "mcp-missing-url.json"),
        (SchemaId::CodexMcpJson, "codex-mcp-wrong-header-key.json"),
    ] {
        // Ok(..) means the file parsed and the schema ran: a data verdict.
        let errors = check(id, &bad.join(file)).expect(file);
        assert!(!errors.is_empty(), "{file} should be rejected");
    }
}

#[test]
fn overrides_are_part_of_the_server_schema() {
    let ok = "name: A\ndescription: d\nurl: https://a.example/mcp\nclaude:\n  strict: true\ncodex:\n  installation: NOT_AVAILABLE\n";
    assert!(
        validate_text(SchemaId::Server, ok, true)
            .unwrap()
            .is_empty()
    );
    let bad =
        "name: A\ndescription: d\nurl: https://a.example/mcp\ncodex:\n  installation: SOMETIMES\n";
    assert!(
        !validate_text(SchemaId::Server, bad, true)
            .unwrap()
            .is_empty()
    );
}

fn acme_copy() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    copy_tree(&fixtures().join("acme"), t.path());
    t
}

fn edit(root: &Path, rel: &str, from: &str, to: &str) {
    let p = root.join(rel);
    let s = std::fs::read_to_string(&p).unwrap();
    assert!(s.contains(from), "{rel} lacks {from:?}");
    std::fs::write(p, s.replacen(from, to, 1)).unwrap();
}

#[test]
fn the_fixture_validates_clean() {
    let t = acme_copy();
    let r = validate_dir(t.path(), &acme());
    assert!(r.is_ok(), "{:#?}", r.findings);
    assert!(r.files_checked > 20);
}

#[test]
fn invariants_catch_what_schemas_cannot() {
    let t = acme_copy();
    let root = t.path();
    // Version disagreement.
    edit(
        root,
        "servers/acme-search/.codex-plugin/plugin.json",
        "\"0.1.0\"",
        "\"0.2.0\"",
    );
    // A real-looking credential where the placeholder belongs.
    edit(
        root,
        "servers/acme-tickets/.mcp.json",
        "Bearer <YOUR_API_KEY>",
        "Bearer sk-live-0123456789abcdef",
    );
    // A catalog source that doesn't exist.
    edit(
        root,
        ".agents/plugins/marketplace.json",
        "./servers/acme-wiki",
        "./servers/acme-wikki",
    );
    // A reserved slug.
    std::fs::create_dir_all(root.join("servers/mcp")).unwrap();
    std::fs::write(
        root.join("servers/mcp/server.yaml"),
        "name: M\nslug: mcp\ndescription: d\nurl: https://m.example/mcp\n",
    )
    .unwrap();

    let r = validate_dir(root, &acme());
    let inv: Vec<String> = r
        .findings
        .iter()
        .filter(|f| f.kind == FindingKind::Invariant)
        .map(|f| format!("{}: {}", f.file, f.message))
        .collect();
    let has = |needle: &str| inv.iter().any(|l| l.contains(needle));
    assert!(has("version 0.1.0 disagrees"), "{inv:#?}");
    assert!(has("may be a real credential"), "{inv:#?}");
    assert!(
        !inv.iter().any(|l| l.contains("sk-live")),
        "a credential was echoed"
    );
    assert!(has("./servers/acme-wikki does not exist"), "{inv:#?}");
    assert!(has("slug \"mcp\" is reserved"), "{inv:#?}");
    assert_eq!(r.schema_errors().count(), 0);
}

#[test]
fn schema_violations_and_broken_schemas_are_reported_apart() {
    let t = acme_copy();
    let root = t.path();
    edit(
        root,
        ".claude-plugin/marketplace.json",
        "\"version\": \"1.2.0\"",
        "\"version\": \"1.2.0\", \"url\": \"https://x.example\"",
    );
    std::fs::create_dir_all(root.join("schema")).unwrap();
    std::fs::write(root.join("schema/server.schema.json"), "{\"type\": 12}").unwrap();
    let r = validate_dir(root, &acme());
    assert_eq!(r.schema_errors().count(), 1);
    assert!(
        r.schema_errors()
            .all(|f| f.file == "schema/server.schema.json")
    );
    assert!(
        r.data_errors()
            .any(|f| f.kind == FindingKind::Schema && f.file == ".claude-plugin/marketplace.json")
    );
}

#[test]
fn a_bad_placeholder_setting_is_refused() {
    let t = acme_copy();
    let mut m = acme();
    m.placeholder = "ghp_realtoken".into();
    let r = validate_dir(t.path(), &m);
    assert!(r.findings.iter().any(|f| f.file == "studio.toml"));
}

#[test]
fn sources_may_not_climb_out_of_the_repo() {
    // The schemas' source pattern relies on a lookahead; make sure the
    // engine honours it rather than silently accepting.
    let cat = r#"{"name": "acme-plugin-marketplace", "interface": {"displayName": "Acme"},
      "plugins": [{"name": "a", "source": "./servers/../../escape"}]}"#;
    assert!(
        !validate_text(SchemaId::CodexMarketplace, cat, false)
            .unwrap()
            .is_empty()
    );
    let ok = cat.replace("./servers/../../escape", "./servers/a");
    assert!(
        validate_text(SchemaId::CodexMarketplace, &ok, false)
            .unwrap()
            .is_empty()
    );
}
