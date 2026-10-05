//! Draft a `server.yaml` from a fleet server's repo, for "add this server to
//! a marketplace". The result is a draft: the operator confirms it (or edits
//! it) before `add` writes anything.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::model::{Auth, AuthType, Entry, Transport, slugify};

/// Where each drafted value came from, for the operator to check.
#[derive(Debug, Clone, Serialize)]
pub struct Draft {
    pub entry: Entry,
    pub sources: Vec<(String, String)>,
}

/// Build a draft entry from a Workers MCP server repo: endpoint from
/// `PUBLIC_MCP_URL` in `wrangler.toml` (else its custom-domain route), auth
/// `oauth`, version from `package.json`, description from the `McpServer`
/// instructions (else `package.json`), name from the slug.
pub fn entry_from_server(repo_dir: &Path, slug: Option<&str>) -> Result<Draft> {
    let mut sources = Vec::new();
    let pkg: Option<serde_json::Value> = std::fs::read_to_string(repo_dir.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let pkg_str = |k: &str| {
        pkg.as_ref()
            .and_then(|p| p.get(k))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };

    let repo_name = repo_dir
        .canonicalize()
        .unwrap_or_else(|_| repo_dir.to_path_buf())
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let slug = match slug {
        Some(s) => {
            sources.push((
                "slug".into(),
                "given (--slug, or the fleet's marketplace_slug)".into(),
            ));
            s.to_string()
        }
        None => {
            sources.push(("slug".into(), "repo directory name".into()));
            slugify(&repo_name)
        }
    };

    let url = wrangler_url(repo_dir)?;
    sources.push(("url".into(), url.1.clone()));

    let (description, from) = match instructions(repo_dir) {
        Some(d) => (d, "McpServer instructions".to_string()),
        None => match pkg_str("description") {
            Some(d) => (d, "package.json description".to_string()),
            None => bail!(
                "no McpServer instructions or package.json description to describe the server"
            ),
        },
    };
    sources.push(("description".into(), from));

    let version = pkg_str("version");
    if version.is_some() {
        sources.push(("version".into(), "package.json".into()));
    }
    sources.push(("name".into(), "title-cased slug".into()));
    sources.push((
        "auth".into(),
        "oauth (fleet servers sign in through the IdP)".into(),
    ));

    let entry = Entry {
        name: title_case(&slug),
        slug: Some(slug),
        description,
        url: Some(url.0),
        transport: Some(Transport::Http),
        auth: Some(Auth {
            kind: Some(AuthType::Oauth),
            header_name: None,
        }),
        version,
        ..Default::default()
    };
    Ok(Draft { entry, sources })
}

fn title_case(slug: &str) -> String {
    slug.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `(url, where it came from)`.
fn wrangler_url(repo_dir: &Path) -> Result<(String, String)> {
    let path = repo_dir.join("wrangler.toml");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let doc: toml::Table =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if let Some(u) = doc
        .get("vars")
        .and_then(|v| v.get("PUBLIC_MCP_URL"))
        .and_then(|v| v.as_str())
    {
        return Ok((u.to_string(), "wrangler.toml [vars] PUBLIC_MCP_URL".into()));
    }
    let routes = doc
        .get("routes")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    for r in routes {
        let custom = r
            .get("custom_domain")
            .and_then(|c| c.as_bool())
            .unwrap_or(false);
        if let (true, Some(p)) = (custom, r.get("pattern").and_then(|p| p.as_str())) {
            return Ok((
                format!("https://{}/mcp", p.trim_end_matches('/')),
                "wrangler.toml custom-domain route (+ /mcp)".into(),
            ));
        }
    }
    bail!(
        "{} has no PUBLIC_MCP_URL and no custom-domain route",
        path.display()
    )
}

/// The `instructions` given to `new McpServer(...)`: either a literal, or a
/// `const INSTRUCTIONS = "…" + "…";` it names.
fn instructions(repo_dir: &Path) -> Option<String> {
    let mut files = Vec::new();
    collect_ts(&repo_dir.join("src"), &mut files);
    files.sort();
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        if !text.contains("McpServer") {
            continue;
        }
        for marker in ["const INSTRUCTIONS", "instructions:"] {
            let mut from = 0;
            while let Some(i) = text[from..].find(marker) {
                let start = from + i + marker.len();
                let rest = text[start..].trim_start_matches([' ', '=', ':', '\t', '\n', '\r']);
                if let Some(s) = string_concat(rest)
                    && !s.trim().is_empty()
                {
                    return Some(s.trim().to_string());
                }
                from = start;
            }
        }
    }
    None
}

fn collect_ts(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                collect_ts(&p, out);
            } else if p.extension().is_some_and(|x| x == "ts" || x == "js") {
                out.push(p);
            }
        }
    }
}

/// Parse `"a" + 'b' + `c`` at the start of `s` (no interpolation).
fn string_concat(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = s;
    let mut any = false;
    loop {
        rest = rest.trim_start();
        let q = rest.chars().next()?;
        if !matches!(q, '"' | '\'' | '`') {
            break;
        }
        let mut chars = rest[1..].char_indices();
        let mut lit = String::new();
        let mut end = None;
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => {
                    if let Some((_, n)) = chars.next() {
                        lit.push(match n {
                            'n' => '\n',
                            't' => '\t',
                            other => other,
                        });
                    }
                }
                c if c == q => {
                    end = Some(i + 2);
                    break;
                }
                '$' if q == '`' && rest[1 + i..].starts_with("${") => return None,
                c => lit.push(c),
            }
        }
        out.push_str(&lit);
        any = true;
        rest = rest.get(end?..)?.trim_start();
        match rest.strip_prefix('+') {
            Some(r) => rest = r,
            None => break,
        }
    }
    any.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concatenated_literals_parse() {
        assert_eq!(
            string_concat("\"Use these \" +\n  'tools to search.';").as_deref(),
            Some("Use these tools to search.")
        );
        assert_eq!(string_concat("`a ${b}`"), None);
        assert_eq!(string_concat("SOMETHING"), None);
    }

    #[test]
    fn titles_come_from_slugs() {
        assert_eq!(title_case("acme-weather-server"), "Acme Weather Server");
    }
}
