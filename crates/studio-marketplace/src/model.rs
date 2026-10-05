//! The marketplace model: one [`Entry`] per `server.yaml`, and the
//! [`Marketplace`] identity every generated file is stamped with.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use studio_core::config::{MarketplaceConfig, PublishMode, Target};

/// Slugs that collide with Claude Code / Codex namespaces or the repo layout.
pub const DEFAULT_RESERVED_SLUGS: &[&str] = &[
    "claude",
    "anthropic",
    "codex",
    "openai",
    "plugin",
    "plugins",
    "marketplace",
    "mcp",
    "agent",
    "agents",
    "skill",
    "skills",
    "help",
    "init",
    "config",
    "servers",
];

pub const DEFAULT_VERSION: &str = "0.1.0";
pub const DEFAULT_HEADER: &str = "Authorization";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    #[default]
    McpServer,
    Skill,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::McpServer => "mcp-server",
            Kind::Skill => "skill",
        }
    }
    /// The top-level directory entries of this kind live in.
    pub fn dir(self) -> &'static str {
        match self {
            Kind::McpServer => "servers",
            Kind::Skill => "plugins",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Http,
    Sse,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::Http => "http",
            Transport::Sse => "sse",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    #[default]
    None,
    Bearer,
    ApiKey,
    Oauth,
}

impl AuthType {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthType::None => "none",
            AuthType::Bearer => "bearer",
            AuthType::ApiKey => "api_key",
            AuthType::Oauth => "oauth",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "none" => AuthType::None,
            "bearer" => AuthType::Bearer,
            "api_key" | "api-key" => AuthType::ApiKey,
            "oauth" => AuthType::Oauth,
            _ => return None,
        })
    }
    /// Credentialed entries carry a placeholder and a Codex-only MCP file.
    pub fn is_credentialed(self) -> bool {
        matches!(self, AuthType::Bearer | AuthType::ApiKey)
    }
    /// Codex catalog `policy.authentication`; `None` means omit the key.
    pub fn codex_policy(self) -> Option<&'static str> {
        match self {
            AuthType::Oauth => Some("ON_FIRST_USE"),
            AuthType::Bearer | AuthType::ApiKey => Some("ON_INSTALL"),
            AuthType::None => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<AuthType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Installation {
    #[default]
    Available,
    InstalledByDefault,
    NotAvailable,
}

impl Installation {
    pub fn as_str(self) -> &'static str {
        match self {
            Installation::Available => "AVAILABLE",
            Installation::InstalledByDefault => "INSTALLED_BY_DEFAULT",
            Installation::NotAvailable => "NOT_AVAILABLE",
        }
    }
}

/// Claude-only fields for the catalog entry (and plugin manifest).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

impl ClaudeOverrides {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Codex-only fields for the catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation: Option<Installation>,
}

impl CodexOverrides {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// One `server.yaml`, as written. Optional fields stay `None` when absent so a
/// rewrite reproduces the file; use the accessor methods for effective values.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<ClaudeOverrides>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<CodexOverrides>,
}

impl Entry {
    pub fn kind(&self) -> Kind {
        self.kind.unwrap_or_default()
    }
    pub fn transport(&self) -> Transport {
        self.transport.unwrap_or_default()
    }
    pub fn auth_type(&self) -> AuthType {
        self.auth.as_ref().and_then(|a| a.kind).unwrap_or_default()
    }
    pub fn header_name(&self) -> &str {
        self.auth
            .as_ref()
            .and_then(|a| a.header_name.as_deref())
            .unwrap_or(DEFAULT_HEADER)
    }
    pub fn version(&self) -> &str {
        self.version.as_deref().unwrap_or(DEFAULT_VERSION)
    }
    pub fn tags(&self) -> &[String] {
        self.tags.as_deref().unwrap_or(&[])
    }
    pub fn url(&self) -> &str {
        self.url.as_deref().unwrap_or("")
    }
    pub fn is_deprecated(&self) -> bool {
        self.deprecated.unwrap_or(false)
    }
    pub fn installation(&self) -> Installation {
        self.codex
            .as_ref()
            .and_then(|c| c.installation)
            .unwrap_or_default()
    }
    /// Claude `keywords`: `mcp` first for servers, then the tags.
    pub fn keywords(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.kind() == Kind::McpServer {
            out.push("mcp".into());
        }
        for t in self.tags() {
            if !out.contains(t) {
                out.push(t.clone());
            }
        }
        out
    }
}

/// An entry as found in a marketplace tree.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginDir {
    /// Effective slug: `slug:` in server.yaml, else the directory name.
    pub slug: String,
    /// Repo-relative directory, e.g. `servers/acme-search`.
    pub rel_dir: String,
    pub entry: Entry,
    /// The directory has a `skills/` subdirectory (Codex needs `skills`).
    pub has_skills: bool,
}

impl PluginDir {
    pub fn new(entry: Entry, rel_dir: impl Into<String>) -> Self {
        let rel_dir = rel_dir.into();
        let dir_name = rel_dir.rsplit('/').next().unwrap_or(&rel_dir).to_string();
        Self {
            slug: entry.slug.clone().unwrap_or(dir_name),
            rel_dir,
            entry,
            has_skills: false,
        }
    }
    /// The catalog `source`: `./servers/<slug>`.
    pub fn source(&self) -> String {
        format!("./{}", self.rel_dir)
    }
    pub fn path(&self, file: &str) -> String {
        format!("{}/{}", self.rel_dir, file)
    }
}

/// The identity a marketplace stamps on its files.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Marketplace {
    pub id: String,
    /// `owner/repo`
    pub repo: String,
    pub branch: String,
    pub catalog_name: String,
    /// Claude `owner.name` / Codex `interface.displayName`.
    pub owner_name: String,
    /// `author.name` in plugin manifests.
    pub author_name: String,
    pub targets: BTreeSet<Target>,
    pub placeholder: String,
    pub reserved_slugs: Vec<String>,
    pub github_account: Option<String>,
    pub publish: PublishMode,
}

impl Marketplace {
    pub fn from_config(c: &MarketplaceConfig, default_account: Option<&str>) -> Self {
        Self {
            id: c.id.clone(),
            repo: c.repo.clone(),
            branch: c.branch.clone(),
            catalog_name: c.catalog_name().to_string(),
            owner_name: c.owner_name.clone(),
            author_name: c.author_name().to_string(),
            targets: c.targets.iter().copied().collect(),
            placeholder: c.credential_placeholder.clone(),
            reserved_slugs: c.reserved_slugs.clone().unwrap_or_else(default_reserved),
            github_account: c
                .github_account
                .clone()
                .or_else(|| default_account.map(str::to_string)),
            publish: c.publish,
        }
    }

    /// A marketplace known only from its tree (validating an arbitrary path):
    /// identity is read from the Claude catalog, everything else defaulted.
    pub fn from_tree(dir: &Path) -> Self {
        let read = |p: &str| {
            std::fs::read_to_string(dir.join(p))
                .ok()
                .and_then(|t| crate::ojson::Json::parse(&t).ok())
        };
        let claude = read(crate::CLAUDE_CATALOG);
        let codex = read(crate::CODEX_CATALOG);
        let name = claude
            .as_ref()
            .or(codex.as_ref())
            .and_then(|c| c.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("marketplace")
            .to_string();
        let owner = claude
            .as_ref()
            .and_then(|c| c.get("owner"))
            .and_then(|o| o.get("name"))
            .or_else(|| {
                codex
                    .as_ref()
                    .and_then(|c| c.get("interface"))
                    .and_then(|i| i.get("displayName"))
            })
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let mut targets = BTreeSet::new();
        if claude.is_some() {
            targets.insert(Target::Claude);
        }
        if codex.is_some() {
            targets.insert(Target::Codex);
        }
        Self {
            id: name.clone(),
            repo: format!("local/{name}"),
            branch: "main".into(),
            catalog_name: name,
            author_name: owner.clone(),
            owner_name: owner,
            targets,
            placeholder: "<YOUR_API_KEY>".into(),
            reserved_slugs: default_reserved(),
            github_account: None,
            publish: PublishMode::Auto,
        }
    }

    pub fn has(&self, t: Target) -> bool {
        self.targets.contains(&t)
    }

    pub fn is_reserved(&self, slug: &str) -> bool {
        self.reserved_slugs.iter().any(|r| r == slug)
    }
}

pub fn default_reserved() -> Vec<String> {
    DEFAULT_RESERVED_SLUGS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Display name → slug: lowercase, non-alphanumerics to hyphens, trimmed.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Normalize a tag: trim, lowercase, whitespace/underscores to hyphens.
pub fn normalize_tag(t: &str) -> String {
    let lower = t.trim().to_lowercase();
    let mut out = String::new();
    for c in lower.chars() {
        if c.is_whitespace() || c == '_' {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(c);
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_tags_normalize() {
        assert_eq!(slugify("  Acme Search (Beta)! "), "acme-search-beta");
        assert_eq!(normalize_tag(" Big Data_Tools "), "big-data-tools");
    }

    #[test]
    fn auth_policy_table() {
        assert_eq!(AuthType::Oauth.codex_policy(), Some("ON_FIRST_USE"));
        assert_eq!(AuthType::Bearer.codex_policy(), Some("ON_INSTALL"));
        assert_eq!(AuthType::ApiKey.codex_policy(), Some("ON_INSTALL"));
        assert_eq!(AuthType::None.codex_policy(), None);
    }
}
