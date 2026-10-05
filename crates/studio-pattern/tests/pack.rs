//! Packs, rendering, conformance and lint against tiny fixtures (acme values
//! only), plus the real `cf-workers-ts` pack rendered with the example
//! instance's values.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use studio_core::check::{Check, Status};
use studio_pattern::{BlessOptions, Conformance, Pack, PackError, Values, lint_repo};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn acme() -> Values {
    [
        ("domain_suffix", "mcp.acme.example"),
        ("gateway_host", "gateway.mcp.acme.example"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

const MANIFEST: &str = r#"
[pack]
name = "tiny"
version = "2026-01-02.1"

[pins]
"hono" = "4.13.13"

[dev_pins]
"vitest" = "4.1.11"

[overrides]
"miniflare" = "5.1.0"

[majors]
"zod" = 4

[forbidden]
dependencies = ["agents"]
legacy_markers = ["McpAgent"]

[security]
files = ["src/auth.ts", "src/index.ts"]

[required]
files = ["conformance.json", "wrangler.toml", "src/mcp.ts"]
[required.scripts]
ci = "npm run conformance"

[wrangler]
main = "src/index.ts"
compatibility_flags = ["nodejs_compat"]
min_compatibility_date = "2026-05-01"
required_vars = ["PUBLIC_MCP_URL", "OKTA_M2M_SCOPE"]
required_kv_bindings = ["OAUTH_KV"]
public_url_var = "PUBLIC_MCP_URL"
forbid_durable_objects = true
scope_var = "OKTA_M2M_SCOPE"

[lockfile]
required_prefix_counts = { "@img/" = 2 }

[placeholders]
patterns = ['REPLACE-WITH-', 'replace\.mcp\.']
extensions = ["ts", "toml", "json", "md"]
exclude = '^(README\.md|node_modules/)'

[variables]
domain_suffix = "server hostname suffix"
gateway_host = "the gateway"
"#;

/// A pack whose template has two security files and one other file.
fn tiny_pack(root: &Path) -> Pack {
    write(root, "pattern.toml", MANIFEST);
    let auth: String = (1..=12).map(|i| format!("// auth line {i}\n")).collect();
    write(root, "template/src/auth.ts", &auth);
    write(
        root,
        "template/src/index.ts",
        "export const ORIGIN = \"https://replace.{{domain_suffix}}\";\nexport const GW = \"{{gateway_host}}\";\n",
    );
    write(
        root,
        "template/README.md",
        "Render me. Literal: {{\"{{\"}}x}}\n",
    );
    write(
        root,
        "template/node_modules/junk/index.js",
        "{{not_rendered}}",
    );
    write(
        root,
        "skill/SKILL.md",
        "Servers live at <name>.{{domain_suffix}}.\n",
    );
    Pack::load(root).unwrap()
}

/// A repo that conforms to the tiny pack once blessed.
fn tiny_repo(pack: &Pack, root: &Path) {
    pack.render_to(&acme(), root).unwrap();
    // A deliberate per-repo difference in a security file.
    let auth = fs::read_to_string(root.join("src/auth.ts")).unwrap();
    write(
        root,
        "src/auth.ts",
        &auth.replace("auth line 6", "auth line six"),
    );
    write(
        root,
        "src/index.ts",
        "export const ORIGIN = \"https://weather.mcp.acme.example\";\nexport const GW = \"gateway.mcp.acme.example\";\n",
    );
    write(root, "src/mcp.ts", "export {};\n");
    write(
        root,
        "package.json",
        r#"{
  "name": "weather",
  "scripts": { "ci": "npm run conformance && npm test", "conformance": "node scripts/check-conformance.mjs" },
  "dependencies": { "hono": "4.13.13", "zod": "^4.6.5" },
  "devDependencies": { "vitest": "4.1.11" },
  "overrides": { "miniflare": "5.1.0" }
}"#,
    );
    write(
        root,
        "package-lock.json",
        r#"{ "packages": {
  "": {},
  "node_modules/hono": { "version": "4.13.13" },
  "node_modules/vitest": { "version": "4.1.11" },
  "node_modules/miniflare": { "version": "5.1.0" },
  "node_modules/zod": { "version": "4.6.5" },
  "node_modules/@img/colour": { "version": "1.0.0" },
  "node_modules/@img/sharp-linux-x64": { "version": "0.34.0" }
} }"#,
    );
    write(
        root,
        "wrangler.toml",
        r#"name = "weather"
main = "src/index.ts"
compatibility_date = "2026-05-01"
compatibility_flags = ["nodejs_compat", "global_fetch_strictly_public"]
routes = [{ pattern = "weather.mcp.acme.example", custom_domain = true }]

[[kv_namespaces]]
binding = "OAUTH_KV"
id = "abc"

[[migrations]]
tag = "v1"
new_sqlite_classes = ["WeatherMcp"]

[[migrations]]
tag = "v2"
deleted_classes = ["WeatherMcp"]

[vars]
PUBLIC_MCP_URL = "https://weather.mcp.acme.example/mcp"
OKTA_M2M_SCOPE = "weather:read weather:write"
"#,
    );
}

fn by_id<'a>(checks: &'a [Check], id: &str) -> &'a Check {
    checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no check {id} in {checks:#?}"))
}

fn failing(checks: &[Check]) -> Vec<String> {
    checks
        .iter()
        .filter(|c| c.status == Status::Fail)
        .map(|c| c.id.clone())
        .collect()
}

#[test]
fn render_substitutes_skips_node_modules_and_keeps_literals() {
    let dir = tempfile::tempdir().unwrap();
    let pack = tiny_pack(dir.path());
    let tree = pack.render(&acme()).unwrap();
    let keys: Vec<&str> = tree.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["README.md", "src/auth.ts", "src/index.ts"]);
    assert_eq!(
        String::from_utf8_lossy(&tree["src/index.ts"].bytes),
        "export const ORIGIN = \"https://replace.mcp.acme.example\";\nexport const GW = \"gateway.mcp.acme.example\";\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&tree["README.md"].bytes),
        "Render me. Literal: {{x}}\n"
    );

    let skill = pack.render_skill(&acme()).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&skill["SKILL.md"].bytes),
        "Servers live at <name>.mcp.acme.example.\n"
    );
}

#[test]
fn render_fails_on_missing_values_and_refuses_a_non_empty_out_dir() {
    let dir = tempfile::tempdir().unwrap();
    let pack = tiny_pack(dir.path());
    let mut v = acme();
    v.remove("gateway_host");
    let e = pack.render(&v).unwrap_err();
    assert!(matches!(e, PackError::MissingValues { .. }), "{e}");

    let out = tempfile::tempdir().unwrap();
    write(out.path(), "keep.txt", "x");
    let e = pack.render_to(&acme(), out.path()).unwrap_err();
    assert!(matches!(e, PackError::NotEmpty(_)), "{e}");
}

#[test]
fn install_skill_writes_the_rendered_skill() {
    let dir = tempfile::tempdir().unwrap();
    let pack = tiny_pack(dir.path());
    let dest = tempfile::tempdir().unwrap();
    write(dest.path(), "other.md", "untouched");
    studio_pattern::install_skill(dir.path(), dest.path(), &acme()).unwrap();
    assert_eq!(
        fs::read_to_string(dest.path().join("SKILL.md")).unwrap(),
        "Servers live at <name>.mcp.acme.example.\n"
    );
    assert_eq!(
        fs::read_to_string(dest.path().join("other.md")).unwrap(),
        "untouched"
    );
    let _ = pack;
}

#[test]
fn bless_then_check_then_drift() {
    let dir = tempfile::tempdir().unwrap();
    let pack = tiny_pack(dir.path());
    let repos = tempfile::tempdir().unwrap();
    let repo = repos.path().join("weather-mcp-worker");
    tiny_repo(&pack, &repo);
    let store = tempfile::tempdir().unwrap();
    let c = Conformance::new(&pack, &acme(), store.path()).unwrap();

    // Before any bless: no conformance.json, no stored diffs.
    let before = c.check("weather-mcp-worker", &repo);
    assert_eq!(
        failing(&before),
        vec!["pattern.version", "pattern.hashes", "pattern.drift"]
    );

    // --store-only writes the store and never the repo.
    let o = c
        .bless(
            "weather-mcp-worker",
            &repo,
            BlessOptions { store_only: true },
        )
        .unwrap();
    assert!(!o.manifest_written && o.manifest_stale);
    assert!(!repo.join("conformance.json").exists());
    assert_eq!(o.diffs_changed.len(), 2);

    let o = c
        .bless("weather-mcp-worker", &repo, BlessOptions::default())
        .unwrap();
    assert!(o.manifest_written && o.diffs_changed.is_empty());
    let manifest = fs::read_to_string(repo.join("conformance.json")).unwrap();
    assert!(manifest.contains("mcp-studio pattern bless"), "{manifest}");
    assert!(manifest.starts_with("{\n  \"comment\": "), "{manifest}");
    assert!(
        manifest.contains("\"scaffold_version\": \"2026-01-02.1\""),
        "{manifest}"
    );

    let after = c.check("weather-mcp-worker", &repo);
    assert_eq!(failing(&after), Vec::<String>::new());

    // The stored diff is the Python-compatible unified diff.
    let stored =
        fs::read_to_string(store.path().join("weather-mcp-worker/src__auth.ts.diff")).unwrap();
    assert!(
        stored.starts_with(
            "--- template/src/auth.ts\n+++ weather-mcp-worker/src/auth.ts\n@@ -3,7 +3,7 @@\n"
        ),
        "{stored}"
    );

    // A re-bless with nothing changed leaves conformance.json alone, even with
    // a hand-edited comment.
    let edited = manifest.replace("Security-scaffold", "Hand-edited");
    fs::write(repo.join("conformance.json"), &edited).unwrap();
    let o = c
        .bless("weather-mcp-worker", &repo, BlessOptions::default())
        .unwrap();
    assert!(!o.manifest_written && !o.manifest_stale && o.diffs_changed.is_empty());
    assert_eq!(
        fs::read_to_string(repo.join("conformance.json")).unwrap(),
        edited
    );

    // An un-blessed edit fails hashes and drift; status counts the lines.
    let auth = fs::read_to_string(repo.join("src/auth.ts")).unwrap();
    fs::write(
        repo.join("src/auth.ts"),
        auth.replace("auth line 1\n", "auth line one\n"),
    )
    .unwrap();
    let drifted = c.check("weather-mcp-worker", &repo);
    assert_eq!(failing(&drifted), vec!["pattern.hashes", "pattern.drift"]);
    let st = c.status("weather-mcp-worker", &repo);
    assert_eq!(
        st.iter()
            .map(|s| (s.file.as_str(), s.differing_lines, s.matches_store))
            .collect::<Vec<_>>(),
        vec![("src/auth.ts", 4, false), ("src/index.ts", 2, true)]
    );

    // A pack version bump makes every repo behind until re-blessed.
    let mut newer = pack.clone();
    newer.manifest.pack.version = "2026-02-01.1".into();
    let c2 = Conformance::new(&newer, &acme(), store.path()).unwrap();
    let v = by_id(&c2.check("weather-mcp-worker", &repo), "pattern.version").clone();
    assert_eq!(v.status, Status::Fail);
    assert!(v.summary.starts_with("behind"), "{}", v.summary);
}

#[test]
fn check_repo_combines_drift_and_lint() {
    let dir = tempfile::tempdir().unwrap();
    let pack = tiny_pack(dir.path());
    let repos = tempfile::tempdir().unwrap();
    let repo = repos.path().join("weather-mcp-worker");
    tiny_repo(&pack, &repo);
    write(&repo, "scripts/check-conformance.mjs", "");
    let store = tempfile::tempdir().unwrap();
    Conformance::new(&pack, &acme(), store.path())
        .unwrap()
        .bless("weather-mcp-worker", &repo, BlessOptions::default())
        .unwrap();
    let checks = studio_pattern::check_repo(&pack, &acme(), &repo, store.path()).unwrap();
    let ids: Vec<&str> = checks.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        &ids[..3],
        &["pattern.version", "pattern.hashes", "pattern.drift"]
    );
    assert_eq!(failing(&checks), Vec::<String>::new(), "{checks:#?}");
    assert!(
        by_id(&checks, "pattern.wrangler.durable_objects")
            .summary
            .contains("WeatherMcp")
    );
}

#[test]
fn lint_catches_each_rule() {
    let dir = tempfile::tempdir().unwrap();
    let pack = tiny_pack(dir.path());
    let repos = tempfile::tempdir().unwrap();
    let repo = repos.path().join("weather-mcp-worker");
    tiny_repo(&pack, &repo);

    let edit = |rel: &str, from: &str, to: &str| {
        let p = repo.join(rel);
        let t = fs::read_to_string(&p).unwrap();
        assert!(t.contains(from), "{rel} lacks {from}");
        fs::write(&p, t.replace(from, to)).unwrap();
    };
    edit(
        "package.json",
        "\"hono\": \"4.13.13\"",
        "\"hono\": \"^4.13.13\", \"agents\": \"1.0.0\"",
    );
    edit(
        "package.json",
        "\"ci\": \"npm run conformance && ",
        "\"ci\": \"",
    );
    edit(
        "package.json",
        "\"zod\": \"^4.6.5\"",
        "\"zod\": \"^3.25.0\"",
    );
    edit(
        "package.json",
        "\"miniflare\": \"5.1.0\"",
        "\"miniflare\": \"5.0.0\"",
    );
    edit(
        "package-lock.json",
        "\"node_modules/@img/colour\": { \"version\": \"1.0.0\" },",
        "",
    );
    edit(
        "package-lock.json",
        "\"node_modules/vitest\": { \"version\": \"4.1.11\" }",
        "\"node_modules/vitest\": { \"version\": \"4.1.12\" }",
    );
    edit("wrangler.toml", "\"nodejs_compat\", ", "");
    edit("wrangler.toml", "2026-05-01", "2025-01-01");
    edit(
        "wrangler.toml",
        "binding = \"OAUTH_KV\"",
        "binding = \"OTHER_KV\"",
    );
    edit("wrangler.toml", "https://weather.mcp", "https://storm.mcp");
    edit(
        "wrangler.toml",
        "deleted_classes = [\"WeatherMcp\"]",
        "deleted_classes = []",
    );
    edit(
        "wrangler.toml",
        "weather:read weather:write",
        "weather:read storm:write",
    );
    write(
        &repo,
        "src/mcp.ts",
        "import { McpAgent } from \"agents/mcp\";\n// REPLACE-WITH-NAME\n",
    );

    let checks = lint_repo(&pack, &repo);
    let fails = failing(&checks);
    for id in [
        "pattern.version",
        "pattern.hashes",
        "pattern.files",
        "pattern.scripts",
        "pattern.pins",
        "pattern.pins.lock",
        "pattern.overrides",
        "pattern.forbidden",
        "pattern.lockfile",
        "pattern.majors",
        "pattern.wrangler.flags",
        "pattern.wrangler.date",
        "pattern.wrangler.kv",
        "pattern.wrangler.route",
        "pattern.wrangler.durable_objects",
        "pattern.placeholders",
        "pattern.legacy",
    ] {
        assert!(
            fails.iter().any(|f| f == id),
            "{id} did not fail: {:#?}",
            by_id(&checks, id)
        );
    }
    assert_eq!(
        by_id(&checks, "pattern.wrangler.scopes").status,
        Status::Warn
    );
    assert_eq!(by_id(&checks, "pattern.wrangler.main").status, Status::Pass);
    assert!(
        by_id(&checks, "pattern.placeholders")
            .evidence
            .as_deref()
            .unwrap()
            .contains("src/mcp.ts:2")
    );
}

// ---- the real pack -------------------------------------------------------

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn example_values() -> Values {
    let text =
        fs::read_to_string(workspace().join("examples/instance/pattern-values.toml")).unwrap();
    toml::from_str::<BTreeMap<String, String>>(&text).unwrap()
}

#[test]
fn the_real_pack_renders_with_the_example_instance() {
    let pack = Pack::load(&workspace().join("patterns/cf-workers-ts")).unwrap();
    let values = example_values();

    let declared: Vec<&String> = pack.manifest.variables.keys().collect();
    let used = pack.used_variables().unwrap();
    assert_eq!(
        declared,
        used.iter().collect::<Vec<_>>(),
        "declared vs used variables"
    );
    for v in &used {
        assert!(
            values.contains_key(v),
            "examples/instance/pattern-values.toml lacks {v}"
        );
    }

    let tree = pack.render(&values).unwrap();
    for f in pack.security_files() {
        assert!(tree.contains_key(f), "template lacks security file {f}");
    }
    assert!(tree["scripts/check-placeholders.sh"].executable);
    let wrangler = String::from_utf8_lossy(&tree["wrangler.toml"].bytes);
    assert!(wrangler.contains("PUBLIC_MCP_URL = \"https://replace.mcp.acme.example/mcp\""));
    for (rel, f) in &tree {
        assert!(
            !String::from_utf8_lossy(&f.bytes).contains("{{"),
            "{rel} still has a placeholder"
        );
    }

    let skill = pack.render_skill(&values).unwrap();
    assert!(skill.contains_key("SKILL.md"));
    assert!(skill.keys().any(|k| k.starts_with("references/")));
}

/// Acceptance: rendering with a real instance's values reproduces the source
/// template the pack was cut from, byte for byte, except for files the pack
/// deliberately corrected. Opt-in, because both paths live outside this repo:
///
/// ```sh
/// STUDIO_PATTERN_SOURCE_TEMPLATE=<old template dir> \
/// STUDIO_PATTERN_VALUES=<instance>/pattern-values.toml \
///   cargo test -p studio-pattern -- --ignored
/// ```
#[test]
#[ignore = "needs STUDIO_PATTERN_SOURCE_TEMPLATE and STUDIO_PATTERN_VALUES"]
fn render_reproduces_the_source_template() {
    /// Corrected in the pack after the move (stale test count, actor wiring,
    /// introspection lines); not a security file.
    const KNOWN_DIVERGENCES: &[&str] = &["README.md"];

    let src = PathBuf::from(
        std::env::var("STUDIO_PATTERN_SOURCE_TEMPLATE").expect("STUDIO_PATTERN_SOURCE_TEMPLATE"),
    );
    let values_path =
        PathBuf::from(std::env::var("STUDIO_PATTERN_VALUES").expect("STUDIO_PATTERN_VALUES"));
    let values: Values = toml::from_str(&fs::read_to_string(values_path).unwrap()).unwrap();
    let pack = Pack::load(&workspace().join("patterns/cf-workers-ts")).unwrap();
    let tree = pack.render(&values).unwrap();

    let source = studio_pattern::pack::walk(&src).unwrap();
    let source_files: Vec<&str> = source.iter().map(|(r, _)| r.as_str()).collect();
    let rendered_files: Vec<&str> = tree.keys().map(String::as_str).collect();
    assert_eq!(rendered_files, source_files, "file lists differ");

    let mut differing = Vec::new();
    for (rel, path) in &source {
        if fs::read(path).unwrap() != tree[rel].bytes {
            differing.push(rel.as_str());
        }
    }
    assert_eq!(
        differing, KNOWN_DIVERGENCES,
        "files that differ from the source template"
    );
}
