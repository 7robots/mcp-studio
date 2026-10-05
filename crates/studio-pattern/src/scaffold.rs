//! Scaffolding a new server from a pack: render the template with the
//! instance's values, then fill the template's own `REPLACE` placeholders with
//! the new server's facts, so the result passes the pack's placeholder rules
//! and its conformance gate from the first commit.
//!
//! Naming, matching the fleet's convention:
//!
//! - **slug** (`weather`): the scope prefix (`weather:read`), the first label
//!   of the public host (`weather.<domain_suffix>`), the gateway id, and the
//!   `resourceKey()` fallback.
//! - **worker** (`weather-mcp-worker`, default `<slug>-mcp-worker`): the repo,
//!   Worker, `package.json` and D1 database name, and the `skill://` id.
//!
//! Some values cannot be known before the server's resources exist. Those are
//! filled with obviously-pending sentinels ([`PENDING_KV_ID`],
//! [`PENDING_D1_ID`]) that pass the placeholder rules but fail a deploy fast;
//! `mcp-studio server plan` reports them as steps.

use std::collections::BTreeMap;
use std::path::Path;

use regex::Regex;
use serde::Serialize;
use studio_core::config::is_slug;

use crate::conformance::{BLESS_COMMAND, RepoManifest, sha256_hex};
use crate::pack::{FileTree, Pack, PackError, RenderedFile, Values, write_tree};

/// KV namespace id written until the namespace is created.
pub const PENDING_KV_ID: &str = "00000000000000000000000000000000";
/// D1 database id written until the database is created.
pub const PENDING_D1_ID: &str = "00000000-0000-0000-0000-000000000000";
/// Default repo/Worker name suffix: `<slug>-mcp-worker`.
pub const WORKER_SUFFIX: &str = "-mcp-worker";

/// The per-repo CI gate the pack's `[required]` names but its template does
/// not carry (the template's own CI must pass before it is blessed anywhere).
pub const CHECK_CONFORMANCE_MJS: &str = r#"// Security-scaffold conformance gate: the per-repo half of the fleet
// mechanism. conformance.json names the sha256 of every security file as last
// BLESSED; this fails CI when any of them changes without a re-bless, which
// forces the question: is this a deliberate per-repo difference, or a fix that
// belongs in the pattern first?
//
// It deliberately knows nothing about the template: Workers Builds runs each
// repo alone, so the cross-repo comparison lives in MCP Studio and this gate
// only proves "nothing changed since it was blessed".

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

const manifest = JSON.parse(readFileSync("conformance.json", "utf8"));
let failures = 0;
for (const [file, blessed] of Object.entries(manifest.files)) {
  let actual;
  try {
    actual = createHash("sha256").update(readFileSync(file)).digest("hex");
  } catch {
    console.error(`conformance: ${file} is MISSING`);
    failures += 1;
    continue;
  }
  if (actual !== blessed) {
    console.error(
      `conformance: ${file} changed without a bless.\n` +
        `  If the change is deliberate, run\n` +
        `    mcp-studio pattern bless <this-repo>\n` +
        `  and commit the updated conformance.json.`,
    );
    failures += 1;
  }
}
if (failures > 0) process.exit(1);
console.log(`conformance: ${Object.keys(manifest.files).length} security files match scaffold ${manifest.scaffold_version}`);
"#;

/// What to scaffold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScaffoldSpec {
    /// Lowercase slug (`[a-z0-9-]`): scope prefix and host label.
    pub slug: String,
    /// Human-readable name, shown on the consent page.
    pub display_name: String,
    /// One-line description: `package.json`, MCP `instructions`, the skill.
    pub description: String,
    /// Exactly two `<prefix>:<action>` scopes on one prefix: the read scope,
    /// then the write scope. Empty means `<slug>:read`, `<slug>:write`.
    pub scopes: Vec<String>,
    /// Keep the template's D1 binding (`DB`).
    pub with_d1: bool,
    /// Repo / Worker name; default `<slug>-mcp-worker`.
    pub worker: Option<String>,
}

impl ScaffoldSpec {
    pub fn new(slug: impl Into<String>) -> Self {
        let slug = slug.into();
        Self {
            display_name: title_case(&slug),
            description: format!("MCP server for {}", title_case(&slug)),
            slug,
            scopes: Vec::new(),
            with_d1: false,
            worker: None,
        }
    }

    pub fn worker_name(&self) -> String {
        self.worker
            .clone()
            .unwrap_or_else(|| format!("{}{WORKER_SUFFIX}", self.slug))
    }

    /// The scopes, defaulted.
    pub fn scope_list(&self) -> Vec<String> {
        if self.scopes.is_empty() {
            vec![
                format!("{}:read", self.slug),
                format!("{}:write", self.slug),
            ]
        } else {
            self.scopes.clone()
        }
    }

    /// `__Host-<cookie>_oauth_csrf-`: the slug without separators.
    pub fn cookie_prefix(&self) -> String {
        self.slug
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect()
    }

    /// The `McpServer` name.
    pub fn server_name(&self) -> String {
        format!("{}-mcp", self.slug)
    }

    /// Every problem with the spec. Empty means valid.
    pub fn validate(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !is_slug(&self.slug) {
            out.push(format!(
                "slug `{}` must be a lowercase slug ([a-z0-9-])",
                self.slug
            ));
        }
        let w = self.worker_name();
        if !is_slug(&w) {
            out.push(format!("worker name `{w}` must be a lowercase slug"));
        }
        for (what, v) in [
            ("display name", &self.display_name),
            ("description", &self.description),
        ] {
            if v.trim().is_empty() {
                out.push(format!("{what} is empty"));
            }
            // These land unescaped in TS, JSON, TOML and a template literal.
            if v.contains(['"', '\\', '`', '\n', '\r']) || v.contains("${") {
                out.push(format!(
                    "{what} may not contain quotes, backslashes, backticks, newlines or `${{`"
                ));
            }
        }
        let scopes = self.scope_list();
        let shape = Regex::new(r"^[a-z0-9][a-z0-9_-]*:[a-z0-9][a-z0-9_-]*$").expect("static regex");
        if scopes.len() != 2 {
            out.push(format!(
                "give exactly two scopes (read, then write); got {}. The pattern ships two so the step-up and broader-consent paths are exercised from the first commit",
                scopes.len()
            ));
        }
        for s in &scopes {
            if !shape.is_match(s) {
                out.push(format!("scope `{s}` is not `<prefix>:<action>`"));
            }
        }
        let prefixes: std::collections::BTreeSet<&str> = scopes
            .iter()
            .filter_map(|s| s.split_once(':'))
            .map(|p| p.0)
            .collect();
        if prefixes.len() > 1 {
            out.push("scopes must share one prefix".into());
        }
        if scopes.len() == 2 && scopes[0] == scopes[1] {
            out.push("the two scopes must differ".into());
        }
        out
    }
}

/// What a scaffold produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScaffoldSummary {
    pub pack: String,
    pub version: String,
    pub slug: String,
    pub worker: String,
    pub display_name: String,
    pub host: String,
    pub url: String,
    pub scopes: Vec<String>,
    pub d1_database: Option<String>,
    pub files: usize,
    /// Security file → sha256, as written to `conformance.json`.
    pub conformance: BTreeMap<String, String>,
    /// Values left for a human (or the plan) to finish.
    pub follow_ups: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScaffoldError {
    #[error(transparent)]
    Pack(#[from] PackError),
    #[error("invalid scaffold spec:\n{}", .0.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n"))]
    Spec(Vec<String>),
    #[error("the instance's pattern values lack `{0}`")]
    MissingValue(String),
    #[error(
        "{file}: the template no longer contains `{needle}`; the scaffolder needs updating for this pack"
    )]
    TemplateShape { file: String, needle: String },
    #[error("placeholders survived scaffolding:\n{}", .0.join("\n"))]
    Leftover(Vec<String>),
}

fn title_case(slug: &str) -> String {
    slug.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

struct Facts {
    worker: String,
    host: String,
    url: String,
    scopes: Vec<String>,
}

/// Render and fill, in memory. The tree includes `conformance.json` and
/// `scripts/check-conformance.mjs`.
pub fn render_scaffold(
    pack: &Pack,
    values: &Values,
    spec: &ScaffoldSpec,
) -> Result<(FileTree, ScaffoldSummary), ScaffoldError> {
    let problems = spec.validate();
    if !problems.is_empty() {
        return Err(ScaffoldError::Spec(problems));
    }
    let value = |k: &str| {
        values
            .get(k)
            .cloned()
            .ok_or_else(|| ScaffoldError::MissingValue(k.into()))
    };
    let suffix = value("domain_suffix")?.trim_start_matches('.').to_string();
    let okta_domain = value("okta_domain")?;
    let okta_issuer = value("okta_issuer")?;
    let okta_client_id = value("okta_client_id")?;

    let facts = Facts {
        worker: spec.worker_name(),
        host: format!("{}.{suffix}", spec.slug),
        url: format!("https://{}.{suffix}/mcp", spec.slug),
        scopes: spec.scope_list(),
    };
    let mut tree = pack.render(values)?;

    // ---- wrangler.toml
    let mut edits: Vec<(&str, Vec<(String, String)>)> = vec![(
        "wrangler.toml",
        vec![
            (
                "# Fill in every REPLACE value, then push — Workers Builds deploys on push to main.".into(),
                format!(
                    "# Scaffolded by mcp-studio from the {} pattern ({}). Workers Builds deploys on push to main.",
                    pack.name(),
                    pack.version()
                ),
            ),
            (
                "# routes = [{ pattern = \"replace.example.com\", custom_domain = true }]".into(),
                format!("routes = [{{ pattern = \"{}\", custom_domain = true }}]", facts.host),
            ),
            (
                "id = \"REPLACE-WITH-KV-NAMESPACE-ID\"".into(),
                format!("id = \"{PENDING_KV_ID}\""),
            ),
            (
                "OKTA_CLIENT_ID = \"REPLACE-WITH-OKTA-CLIENT-ID\"".into(),
                format!("OKTA_CLIENT_ID = \"{okta_client_id}\""),
            ),
        ],
    )];
    edits.push((
        "package.json",
        vec![
            (
                "\"ci\": \"npm run typecheck && npm run test\",".into(),
                "\"ci\": \"npm run conformance && npm run check:placeholders && npm run typecheck && npm run test\",".into(),
            ),
            (
                "\"check:placeholders\": \"bash scripts/check-placeholders.sh\"".into(),
                "\"check:placeholders\": \"bash scripts/check-placeholders.sh\",\n    \"conformance\": \"node scripts/check-conformance.mjs\"".into(),
            ),
        ],
    ));
    edits.push((
        "worker-configuration.d.ts",
        vec![(
            "// REPLACE — data-layer bindings.".into(),
            "// Data-layer bindings.".into(),
        )],
    ));
    edits.push((
        "src/consent.ts",
        vec![
            (
                "// REPLACE: what each scope actually permits".into(),
                "// What each scope actually permits".into(),
            ),
            (
                "  \"REPLACE:read\": \"REPLACE — what a read token can see, in plain words\",\n  \"REPLACE:write\": \"REPLACE — what a write token can change or destroy, in plain words\",\n".into(),
                facts
                    .scopes
                    .iter()
                    .map(|s| format!("  \"{s}\": \"{}\",\n", scope_help(s, &spec.display_name)))
                    .collect(),
            ),
        ],
    ));
    edits.push((
        "src/scopes.ts",
        vec![(
            "// REPLACE: one entry per tool in src/mcp.ts.".into(),
            "// One entry per tool in src/mcp.ts.".into(),
        )],
    ));
    edits.push((
        "src/mcp.ts",
        vec![(
            "const INSTRUCTIONS = \"REPLACE — short description of what this MCP server does and when to use its tools.\";".into(),
            format!("const INSTRUCTIONS = \"{}\";", spec.description),
        )],
    ));
    edits.push((
        "src/skill.ts",
        vec![
            (
                "\"REPLACE — one-line description of what this server does (shown to MCP clients).\"".into(),
                format!("\"{}\"", spec.description),
            ),
            (
                "REPLACE — the LLM-facing usage guide for this server: data model, available\ntools, and query tips. Keep it concise and practical so a model can use the\nserver well without trial and error.\n".into(),
                format!(
                    "{}.\n\n{}\n\nTools: the scaffold's stubs (echo, list_examples, delete_example, whoami).\nRewrite this guide as the real tools land: data model, each tool, and query tips.\n",
                    spec.description.trim_end_matches('.'),
                    spec.display_name
                ),
            ),
        ],
    ));
    edits.push((
        "test/m2m.workerd.test.ts",
        vec![("REPLACE_write_tool".into(), "delete_example".into())],
    ));
    if spec.with_d1 {
        edits[0].1.extend([
            (
                "database_name = \"REPLACE-WITH-D1-DATABASE-NAME\"".into(),
                format!("database_name = \"{}\"", facts.worker),
            ),
            (
                "database_id = \"REPLACE-WITH-D1-UUID\"".into(),
                format!("database_id = \"{PENDING_D1_ID}\""),
            ),
        ]);
    }
    for (file, list) in &edits {
        let mut text = text_of(&tree, file)?;
        for (needle, with) in list {
            if !text.contains(needle.as_str()) {
                return Err(ScaffoldError::TemplateShape {
                    file: (*file).into(),
                    needle: needle.lines().next().unwrap_or_default().into(),
                });
            }
            text = text.replace(needle.as_str(), with);
        }
        set_text(&mut tree, file, text);
    }
    if !spec.with_d1 {
        let mut text = text_of(&tree, "wrangler.toml")?;
        text = cut_block(
            &text,
            "# D1 database, bound directly",
            "database_id = \"REPLACE-WITH-D1-UUID\"\n",
        )
        .ok_or_else(|| ScaffoldError::TemplateShape {
            file: "wrangler.toml".into(),
            needle: "[[d1_databases]]".into(),
        })?;
        set_text(
            &mut tree,
            "wrangler.toml",
            text.replacen("\n\n\n", "\n\n", 1),
        );
    }

    // ---- generic replacements, every text file
    let generic: Vec<(String, String)> = vec![
        (format!("replace.{suffix}"), facts.host.clone()),
        (
            "https://replace-with-tenant.okta.com/oauth2/default".into(),
            okta_issuer.clone(),
        ),
        ("https://replace-with-tenant.okta.com".into(), okta_domain),
        ("replace-with-your-server-name".into(), facts.worker.clone()),
        ("REPLACE-WITH-SERVER-NAME".into(), spec.server_name()),
        (
            "REPLACE-WITH-RESOURCE-NAME".into(),
            spec.display_name.clone(),
        ),
        ("REPLACE-WITH-DESCRIPTION".into(), spec.description.clone()),
        ("REPLACE-WITH-SERVER-SLUG".into(), spec.slug.clone()),
        (
            "__Host-REPLACE_".into(),
            format!("__Host-{}_", spec.cookie_prefix()),
        ),
        ("REPLACE:read".into(), facts.scopes[0].clone()),
        ("REPLACE:write".into(), facts.scopes[1].clone()),
    ];
    for f in tree.values_mut() {
        let Ok(text) = std::str::from_utf8(&f.bytes) else {
            continue;
        };
        if !generic.iter().any(|(n, _)| text.contains(n.as_str())) {
            continue;
        }
        let mut t = text.to_string();
        for (n, w) in &generic {
            t = t.replace(n.as_str(), w);
        }
        f.bytes = t.into_bytes();
    }

    // ---- new files
    tree.insert(
        "README.md".into(),
        RenderedFile {
            bytes: readme(pack, spec, &facts).into_bytes(),
            executable: false,
        },
    );
    tree.insert(
        "scripts/check-conformance.mjs".into(),
        RenderedFile {
            bytes: CHECK_CONFORMANCE_MJS.as_bytes().to_vec(),
            executable: false,
        },
    );
    let mut hashes = Vec::new();
    for rel in pack.security_files() {
        let f = tree.get(rel).ok_or_else(|| ScaffoldError::TemplateShape {
            file: rel.clone(),
            needle: "(security file)".into(),
        })?;
        hashes.push((rel.clone(), sha256_hex(&f.bytes)));
    }
    tree.insert(
        pack.manifest.security.conformance_file.clone(),
        RenderedFile {
            bytes: RepoManifest::render(pack.version(), &hashes).into_bytes(),
            executable: false,
        },
    );

    let leftover = placeholder_hits(pack, &tree);
    if !leftover.is_empty() {
        return Err(ScaffoldError::Leftover(leftover));
    }

    let mut follow_ups = vec![
        "src/mcp.ts: replace the stub tools (echo, list_examples, delete_example, whoami) and keep TOOL_SCOPES in src/scopes.ts in step".to_string(),
        "src/skill.ts: rewrite the LLM-facing guide for the real tools (no test asserts it)".into(),
        "src/consent.ts SCOPE_HELP: say what each scope actually permits, in the user's terms".into(),
        format!("wrangler.toml: OAUTH_KV id is the pending sentinel {PENDING_KV_ID}"),
    ];
    if spec.with_d1 {
        follow_ups.push(format!(
            "wrangler.toml: D1 database {} has the pending id {PENDING_D1_ID}",
            facts.worker
        ));
    } else {
        follow_ups.push(
            "no D1 binding: src/data.ts and the list_examples/delete_example stubs still read env.DB — replace them with your data layer".into(),
        );
    }
    follow_ups.push(format!(
        "{}: written for {} {}; bless into the instance's store once the repo is in the fleet (`{BLESS_COMMAND} {}`)",
        pack.manifest.security.conformance_file,
        pack.name(),
        pack.version(),
        facts.worker
    ));

    let summary = ScaffoldSummary {
        pack: pack.name().into(),
        version: pack.version().into(),
        slug: spec.slug.clone(),
        worker: facts.worker.clone(),
        display_name: spec.display_name.clone(),
        host: facts.host.clone(),
        url: facts.url.clone(),
        scopes: facts.scopes.clone(),
        d1_database: spec.with_d1.then(|| facts.worker.clone()),
        files: tree.len(),
        conformance: hashes.into_iter().collect(),
        follow_ups,
    };
    Ok((tree, summary))
}

/// Render, fill and write into `out`, which must be absent or empty.
pub fn scaffold(
    pack: &Pack,
    values: &Values,
    spec: &ScaffoldSpec,
    out: &Path,
) -> Result<ScaffoldSummary, ScaffoldError> {
    let (tree, summary) = render_scaffold(pack, values, spec)?;
    write_tree(&tree, out)?;
    Ok(summary)
}

fn scope_help(scope: &str, name: &str) -> String {
    match scope.split_once(':').map(|p| p.1).unwrap_or(scope) {
        "read" => format!("See your {name} data. It cannot change anything."),
        "write" => format!("Create, change or permanently delete your {name} data."),
        action => format!("{} in {name}.", title_case(action)),
    }
}

fn text_of(tree: &FileTree, file: &str) -> Result<String, ScaffoldError> {
    tree.get(file)
        .and_then(|f| String::from_utf8(f.bytes.clone()).ok())
        .ok_or_else(|| ScaffoldError::TemplateShape {
            file: file.into(),
            needle: "(file)".into(),
        })
}

fn set_text(tree: &mut FileTree, file: &str, text: String) {
    if let Some(f) = tree.get_mut(file) {
        f.bytes = text.into_bytes();
    }
}

/// `text` without the span from the line starting with `start` through `end`.
fn cut_block(text: &str, start: &str, end: &str) -> Option<String> {
    let a = text.find(start)?;
    let b = text[a..].find(end)? + a + end.len();
    Some(format!("{}{}", &text[..a], &text[b..]))
}

/// Lines in `tree` that match the pack's placeholder rules.
pub fn placeholder_hits(pack: &Pack, tree: &FileTree) -> Vec<String> {
    let p = &pack.manifest.placeholders;
    if p.patterns.is_empty() {
        return Vec::new();
    }
    let joined = p
        .patterns
        .iter()
        .map(|s| format!("(?:{s})"))
        .collect::<Vec<_>>()
        .join("|");
    let (Ok(re), Ok(exclude)) = (
        Regex::new(&joined),
        p.exclude.as_deref().map(Regex::new).transpose(),
    ) else {
        return vec!["pattern.toml has an invalid placeholder regex".into()];
    };
    let mut hits = Vec::new();
    for (rel, f) in tree {
        let ext = rel.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if !p.extensions.is_empty() && !p.extensions.iter().any(|x| x == ext) {
            continue;
        }
        if exclude.as_ref().is_some_and(|x| x.is_match(rel)) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&f.bytes) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            if re.is_match(line) {
                hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    hits
}

fn readme(pack: &Pack, spec: &ScaffoldSpec, f: &Facts) -> String {
    format!(
        "# {name}\n\n{desc}\n\n\
         - MCP endpoint: `{url}`\n\
         - Scopes: {scopes}\n\
         - Worker: `{worker}`\n\
         - Pattern: `{pack}` {version} (security files under conformance; see `conformance.json`)\n\n\
         Scaffolded by MCP Studio. What the server still needs before it serves \
         (identity-provider scopes and redirect URI, Cloudflare bindings and secrets, \
         Workers Builds, gateway registration) is a machine-readable plan:\n\n\
         ```sh\nmcp-studio server plan {worker}\n```\n\n\
         `npm run ci` runs the conformance gate, the placeholder check, both \
         typecheck projects and the tests; Workers Builds runs it before every deploy.\n",
        name = spec.display_name,
        desc = spec.description,
        url = f.url,
        scopes = f
            .scopes
            .iter()
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(", "),
        worker = f.worker,
        pack = pack.name(),
        version = pack.version(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_defaults_follow_the_fleet_convention() {
        let s = ScaffoldSpec::new("acme-notes");
        assert_eq!(s.worker_name(), "acme-notes-mcp-worker");
        assert_eq!(s.scope_list(), vec!["acme-notes:read", "acme-notes:write"]);
        assert_eq!(s.cookie_prefix(), "acmenotes");
        assert_eq!(s.display_name, "Acme Notes");
        assert!(s.validate().is_empty(), "{:?}", s.validate());
    }

    #[test]
    fn spec_validation_names_every_problem() {
        let mut s = ScaffoldSpec::new("Bad Slug");
        s.description = "has \"quotes\"".into();
        s.scopes = vec!["a:read".into(), "b:write".into(), "c".into()];
        let p = s.validate();
        assert!(p.iter().any(|x| x.contains("slug")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("quotes")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("exactly two")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("one prefix")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("`c`")), "{p:?}");
    }

    #[test]
    fn cut_block_removes_an_inclusive_span() {
        assert_eq!(
            cut_block("a\n# B start\nx\nend\nc\n", "# B", "end\n").as_deref(),
            Some("a\nc\n")
        );
        assert_eq!(cut_block("a", "x", "y"), None);
    }

    #[test]
    fn scope_help_reads_as_consequences() {
        assert!(scope_help("w:read", "Weather").contains("cannot change"));
        assert!(scope_help("w:write", "Weather").contains("delete"));
        assert_eq!(scope_help("w:refresh", "Weather"), "Refresh in Weather.");
    }
}
