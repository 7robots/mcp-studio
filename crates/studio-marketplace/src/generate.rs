//! Generators: pure functions from entries to file contents.
//!
//! Every function here is deterministic and touches no disk, so the same
//! code drives `add`/`update` (write the result), `verify` (regenerate and
//! diff) and `reconcile` (compare field by field).

use std::collections::BTreeMap;

use studio_core::config::Target;

use crate::model::{AuthType, Kind, Marketplace, PluginDir};
use crate::ojson::Json;
use crate::{CLAUDE_CATALOG, CODEX_CATALOG};

/// Repo-relative path → file content.
pub type FileTree = BTreeMap<String, String>;

pub const CLAUDE_MANIFEST: &str = ".claude-plugin/plugin.json";
pub const CODEX_MANIFEST: &str = ".codex-plugin/plugin.json";
pub const MCP_JSON: &str = ".mcp.json";
pub const CODEX_MCP_JSON: &str = ".codex-plugin/mcp.json";
pub const README: &str = "README.md";
pub const SERVER_YAML: &str = "server.yaml";

/// Every per-entry file Studio owns (whether or not this entry needs it).
pub const ENTRY_FILES: &[&str] = &[
    CLAUDE_MANIFEST,
    CODEX_MANIFEST,
    MCP_JSON,
    CODEX_MCP_JSON,
    README,
];

pub fn claude_manifest(m: &Marketplace, p: &PluginDir) -> Json {
    let e = &p.entry;
    let keywords = e.keywords();
    let claude = e.claude.clone().unwrap_or_default();
    Json::object()
        .with("name", Json::str(&p.slug))
        .with("description", Json::str(&e.description))
        .with("version", Json::str(e.version()))
        .with(
            "author",
            Json::object().with("name", Json::str(&m.author_name)),
        )
        .with_opt(
            "keywords",
            (!keywords.is_empty()).then(|| Json::strings(keywords)),
        )
        .with_opt("homepage", e.homepage.as_deref().map(Json::str))
        .with_opt("repository", claude.repository.as_deref().map(Json::str))
        .with_opt("license", claude.license.as_deref().map(Json::str))
}

pub fn codex_manifest(m: &Marketplace, p: &PluginDir) -> Json {
    let e = &p.entry;
    let mcp = match e.kind() {
        Kind::Skill => None,
        Kind::McpServer if e.auth_type().is_credentialed() => Some("./.codex-plugin/mcp.json"),
        Kind::McpServer => Some("./.mcp.json"),
    };
    let skills = (p.has_skills || e.kind() == Kind::Skill).then_some("./skills/");
    Json::object()
        .with("name", Json::str(&p.slug))
        .with("version", Json::str(e.version()))
        .with("description", Json::str(&e.description))
        .with(
            "author",
            Json::object().with("name", Json::str(&m.author_name)),
        )
        .with_opt("mcpServers", mcp.map(Json::str))
        .with_opt("skills", skills.map(Json::str))
        .with(
            "interface",
            Json::object().with("displayName", Json::str(&e.name)),
        )
}

fn credential(m: &Marketplace, auth: AuthType) -> String {
    match auth {
        AuthType::Bearer => format!("Bearer {}", m.placeholder),
        _ => m.placeholder.clone(),
    }
}

/// The shared `.mcp.json` (servers only).
pub fn mcp_json(m: &Marketplace, p: &PluginDir) -> Option<Json> {
    let e = &p.entry;
    if e.kind() != Kind::McpServer {
        return None;
    }
    let mut server = Json::object()
        .with("type", Json::str(e.transport().as_str()))
        .with("url", Json::str(e.url()));
    if e.auth_type().is_credentialed() {
        server.set(
            "headers",
            Json::object().with(e.header_name(), Json::str(credential(m, e.auth_type()))),
        );
    }
    Some(Json::object().with("mcpServers", Json::object().with(&p.slug, server)))
}

/// `.codex-plugin/mcp.json`: credentialed servers only (Codex reads
/// `http_headers`, not Claude's `headers`).
pub fn codex_mcp_json(m: &Marketplace, p: &PluginDir) -> Option<Json> {
    let e = &p.entry;
    if e.kind() != Kind::McpServer || !e.auth_type().is_credentialed() {
        return None;
    }
    let server = Json::object().with("url", Json::str(e.url())).with(
        "http_headers",
        Json::object().with(e.header_name(), Json::str(credential(m, e.auth_type()))),
    );
    Some(Json::object().with("mcpServers", Json::object().with(&p.slug, server)))
}

/// A Markdown code span the value cannot break out of.
fn span(v: &str) -> String {
    format!("`{}`", v.replace('`', "'"))
}

pub fn readme(m: &Marketplace, p: &PluginDir) -> String {
    let e = &p.entry;
    let mut l: Vec<String> = vec![format!("# {}", e.name), String::new()];
    if e.is_deprecated() {
        let reason = e
            .deprecated_reason
            .as_deref()
            .map(|r| format!(" {r}"))
            .unwrap_or_default();
        l.push(format!("> **DEPRECATED.**{reason}"));
        l.push(String::new());
    }
    l.push(if e.description.is_empty() {
        "_No description provided._".into()
    } else {
        e.description.clone()
    });
    l.push(String::new());
    l.push("## Install".into());
    if m.has(Target::Claude) {
        l.extend([
            String::new(),
            "**Claude Code**".into(),
            "```".into(),
            format!("/plugin marketplace add {}", m.repo),
            format!("/plugin install {}@{}", p.slug, m.catalog_name),
            "```".into(),
        ]);
    }
    if m.has(Target::Codex) {
        l.extend([
            String::new(),
            "**Codex**".into(),
            "```".into(),
            format!("codex plugin marketplace add {}", m.repo),
            "```".into(),
        ]);
    }
    l.push(String::new());
    let auth = e.auth_type();
    match e.kind() {
        Kind::McpServer => {
            l.extend([
                "## Server".into(),
                String::new(),
                format!("- **Endpoint:** {}", span(e.url())),
                format!("- **Transport:** {}", span(e.transport().as_str())),
                format!("- **Auth:** {}", span(auth.as_str())),
            ]);
        }
        Kind::Skill => {
            l.extend([
                "## Plugin".into(),
                String::new(),
                format!("- **Kind:** {}", span(Kind::Skill.as_str())),
            ]);
        }
    }
    if !e.tags().is_empty() {
        l.push(format!("- **Tags:** {}", e.tags().join(", ")));
    }
    if let Some(h) = &e.homepage {
        l.push(format!("- **Homepage:** {h}"));
    }
    if e.kind() == Kind::McpServer && auth.is_credentialed() {
        l.extend([
            String::new(),
            "## Authentication".into(),
            String::new(),
            format!(
                "This server requires a credential. After installing, replace `{}` with your own key in `.mcp.json` (Claude Code) or `.codex-plugin/mcp.json` (Codex) — credentials are never distributed through the marketplace.",
                m.placeholder
            ),
        ]);
    } else if e.kind() == Kind::McpServer && auth == AuthType::Oauth {
        l.extend([
            String::new(),
            "## Authentication".into(),
            String::new(),
            "This server uses OAuth. Your MCP client will prompt you to authenticate on first use."
                .into(),
        ]);
    }
    l.extend([
        String::new(),
        "---".into(),
        String::new(),
        "_Generated from `server.yaml`. Edit that file and open a PR to change this plugin — do not hand-edit generated files._".into(),
        String::new(),
    ]);
    l.join("\n")
}

/// Every generated file for one entry, keyed by repo-relative path.
pub fn entry_files(m: &Marketplace, p: &PluginDir) -> FileTree {
    let mut out = FileTree::new();
    if m.has(Target::Claude) {
        out.insert(p.path(CLAUDE_MANIFEST), claude_manifest(m, p).to_pretty());
    }
    if m.has(Target::Codex) {
        out.insert(p.path(CODEX_MANIFEST), codex_manifest(m, p).to_pretty());
    }
    if let Some(j) = mcp_json(m, p) {
        out.insert(p.path(MCP_JSON), j.to_pretty());
    }
    if m.has(Target::Codex)
        && let Some(j) = codex_mcp_json(m, p)
    {
        out.insert(p.path(CODEX_MCP_JSON), j.to_pretty());
    }
    out.insert(p.path(README), readme(m, p));
    out
}

/// One Claude catalog entry. Never carries `url`.
pub fn claude_entry(p: &PluginDir) -> Json {
    let e = &p.entry;
    let claude = e.claude.clone().unwrap_or_default();
    Json::object()
        .with("name", Json::str(&p.slug))
        .with("source", Json::str(p.source()))
        .with("description", Json::str(&e.description))
        .with("version", Json::str(e.version()))
        .with_opt("category", e.category.as_deref().map(Json::str))
        .with_opt(
            "tags",
            (!e.tags().is_empty()).then(|| Json::strings(e.tags().iter().cloned())),
        )
        .with_opt("homepage", e.homepage.as_deref().map(Json::str))
        .with_opt("repository", claude.repository.as_deref().map(Json::str))
        .with_opt("license", claude.license.as_deref().map(Json::str))
        .with_opt("strict", claude.strict.map(Json::Bool))
}

/// One Codex catalog entry; `policy.authentication` per the auth table.
pub fn codex_entry(p: &PluginDir) -> Json {
    let e = &p.entry;
    let policy = Json::object()
        .with("installation", Json::str(e.installation().as_str()))
        .with_opt(
            "authentication",
            e.auth_type().codex_policy().map(Json::str),
        );
    Json::object()
        .with("name", Json::str(&p.slug))
        .with("source", Json::str(p.source()))
        .with("policy", policy)
}

pub fn empty_claude_catalog(m: &Marketplace) -> Json {
    Json::object()
        .with("name", Json::str(&m.catalog_name))
        .with(
            "owner",
            Json::object().with("name", Json::str(&m.owner_name)),
        )
        .with("plugins", Json::Array(Vec::new()))
}

pub fn empty_codex_catalog(m: &Marketplace) -> Json {
    Json::object()
        .with("name", Json::str(&m.catalog_name))
        .with(
            "interface",
            Json::object().with("displayName", Json::str(&m.owner_name)),
        )
        .with("plugins", Json::Array(Vec::new()))
}

fn entry_name(j: &Json) -> &str {
    j.get("name").and_then(Json::as_str).unwrap_or("")
}

/// Insert or replace `entry` (by `name`) in a catalog, keeping `plugins`
/// sorted by name. Everything else in the catalog is left as it was.
pub fn upsert(catalog: &Json, entry: Json) -> Json {
    let mut c = catalog.clone();
    let name = entry_name(&entry).to_string();
    if c.get("plugins").and_then(Json::as_array).is_none() {
        c.set("plugins", Json::Array(Vec::new()));
    }
    if let Some(plugins) = c.get_mut("plugins").and_then(Json::as_array_mut) {
        plugins.retain(|p| entry_name(p) != name);
        plugins.push(entry);
        plugins.sort_by(|a, b| entry_name(a).cmp(entry_name(b)));
    }
    c
}

/// Remove the entry named `name`, if present.
pub fn remove_from(catalog: &Json, name: &str) -> Json {
    let mut c = catalog.clone();
    if let Some(plugins) = c.get_mut("plugins").and_then(Json::as_array_mut) {
        plugins.retain(|p| entry_name(p) != name);
    }
    c
}

/// Rebuild a catalog from scratch: identity from config, extra top-level
/// keys (`metadata`, `owner.email`, …) kept from `existing`, one entry per
/// plugin dir, sorted by name.
fn rebuild(base: Json, existing: Option<&Json>, entries: Vec<Json>) -> Json {
    let mut c = base;
    if let Some(Json::Object(kv)) = existing {
        for (k, v) in kv {
            match (k.as_str(), v) {
                ("name" | "plugins", _) => {}
                ("owner" | "interface", Json::Object(inner)) => {
                    if let Some(slot) = c.get_mut(k) {
                        for (ik, iv) in inner {
                            if slot.get(ik).is_none() {
                                slot.set(ik, iv.clone());
                            }
                        }
                    }
                }
                _ => {
                    if c.get(k).is_none() {
                        c.set(k, v.clone());
                    }
                }
            }
        }
        // Keep `plugins` last, where it was.
        if let Json::Object(kv) = &mut c
            && let Some(i) = kv.iter().position(|(k, _)| k == "plugins")
        {
            let p = kv.remove(i);
            kv.push(p);
        }
    }
    let mut entries = entries;
    entries.sort_by(|a, b| entry_name(a).cmp(entry_name(b)));
    c.set("plugins", Json::Array(entries));
    c
}

pub fn claude_catalog(m: &Marketplace, plugins: &[PluginDir], existing: Option<&Json>) -> Json {
    rebuild(
        empty_claude_catalog(m),
        existing,
        plugins.iter().map(claude_entry).collect(),
    )
}

pub fn codex_catalog(m: &Marketplace, plugins: &[PluginDir], existing: Option<&Json>) -> Json {
    rebuild(
        empty_codex_catalog(m),
        existing,
        plugins.iter().map(codex_entry).collect(),
    )
}

/// The whole generated tree: every entry's files and both catalogs.
/// `existing` catalogs only contribute extra top-level keys.
pub fn generate(
    m: &Marketplace,
    plugins: &[PluginDir],
    existing_claude: Option<&Json>,
    existing_codex: Option<&Json>,
) -> FileTree {
    let mut out = FileTree::new();
    for p in plugins {
        out.extend(entry_files(m, p));
    }
    if m.has(Target::Claude) {
        out.insert(
            CLAUDE_CATALOG.into(),
            claude_catalog(m, plugins, existing_claude).to_pretty(),
        );
    }
    if m.has(Target::Codex) {
        out.insert(
            CODEX_CATALOG.into(),
            codex_catalog(m, plugins, existing_codex).to_pretty(),
        );
    }
    out
}
