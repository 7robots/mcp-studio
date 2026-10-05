//! `server.yaml` in and out.
//!
//! Reading goes YAML → `serde_json::Value` (so the JSON schema sees exactly
//! what CI's `js-yaml` would) → [`Entry`]. Writing uses a small emitter that
//! follows the block style the existing files were written in: keys in a
//! fixed order, indentless `- item` lists, plain scalars where safe, long
//! plain scalars folded past column 80.

use crate::model::Entry;

/// The comment every file Studio writes starts with.
pub const HEADER: &str =
    "# Source of truth for this entry. Edit here, then regenerate with mcp-studio marketplace.\n";

const WIDTH: usize = 80;

#[derive(Debug, thiserror::Error)]
pub enum YamlError {
    #[error("not valid YAML: {0}")]
    Syntax(String),
    #[error("top level must be a mapping")]
    NotMapping,
    #[error("{0}")]
    Shape(String),
}

/// Parse YAML text into a JSON value (for schema validation).
pub fn to_json(text: &str) -> Result<serde_json::Value, YamlError> {
    let v: serde_json::Value =
        serde_saphyr::from_str(text).map_err(|e| YamlError::Syntax(first_line(&e.to_string())))?;
    if !v.is_object() {
        return Err(YamlError::NotMapping);
    }
    Ok(v)
}

/// Parse a `server.yaml` into an [`Entry`]. Schema validation is separate.
pub fn parse_entry(text: &str) -> Result<Entry, YamlError> {
    let v = to_json(text)?;
    serde_json::from_value(v).map_err(|e| YamlError::Shape(e.to_string()))
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or(s).to_string()
}

/// Render an [`Entry`] as `server.yaml` text with the neutral [`HEADER`].
pub fn emit(entry: &Entry) -> String {
    emit_with_header(entry, HEADER)
}

/// The leading comment block of an existing file (comment and blank lines
/// before the first key), so a rewrite keeps it.
pub fn leading_comments(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with('#') || t.is_empty() {
            out.push_str(line);
            out.push('\n');
        } else {
            break;
        }
    }
    out
}

/// Rewrite `entry` keeping `previous`'s leading comments (the neutral
/// header when there were none).
pub fn rewrite(entry: &Entry, previous: Option<&str>) -> String {
    let header = previous
        .map(leading_comments)
        .filter(|h| !h.trim().is_empty());
    emit_with_header(entry, header.as_deref().unwrap_or(HEADER))
}

pub fn emit_with_header(entry: &Entry, header: &str) -> String {
    let mut w = Writer::default();
    w.out.push_str(header);
    w.scalar_field(0, "name", &entry.name);
    if let Some(s) = &entry.slug {
        w.scalar_field(0, "slug", s);
    }
    if let Some(k) = entry.kind {
        w.scalar_field(0, "kind", k.as_str());
    }
    w.scalar_field(0, "description", &entry.description);
    if let Some(u) = &entry.url {
        w.scalar_field(0, "url", u);
    }
    if let Some(t) = entry.transport {
        w.scalar_field(0, "transport", t.as_str());
    }
    if let Some(a) = &entry.auth {
        if a.kind.is_none() && a.header_name.is_none() {
            w.raw_line("auth: {}");
        } else {
            w.raw_line("auth:");
            if let Some(k) = a.kind {
                w.scalar_field(1, "type", k.as_str());
            }
            if let Some(h) = &a.header_name {
                w.scalar_field(1, "header_name", h);
            }
        }
    }
    if let Some(tags) = &entry.tags {
        w.list_field("tags", tags);
    }
    if let Some(c) = &entry.category {
        w.scalar_field(0, "category", c);
    }
    if let Some(h) = &entry.homepage {
        w.scalar_field(0, "homepage", h);
    }
    if let Some(v) = &entry.version {
        w.scalar_field(0, "version", v);
    }
    if let Some(o) = &entry.owner {
        w.scalar_field(0, "owner", o);
    }
    if let Some(d) = entry.deprecated {
        w.raw_line(&format!("deprecated: {d}"));
    }
    if let Some(r) = &entry.deprecated_reason {
        w.scalar_field(0, "deprecated_reason", r);
    }
    if let Some(c) = entry.claude.as_ref().filter(|c| !c.is_empty()) {
        w.raw_line("claude:");
        if let Some(s) = c.strict {
            w.raw_line(&format!("  strict: {s}"));
        }
        if let Some(l) = &c.license {
            w.scalar_field(1, "license", l);
        }
        if let Some(r) = &c.repository {
            w.scalar_field(1, "repository", r);
        }
    }
    if let Some(c) = entry.codex.as_ref().filter(|c| !c.is_empty()) {
        w.raw_line("codex:");
        if let Some(i) = c.installation {
            w.scalar_field(1, "installation", i.as_str());
        }
    }
    w.out
}

#[derive(Default)]
struct Writer {
    out: String,
}

impl Writer {
    fn raw_line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn scalar_field(&mut self, level: usize, key: &str, value: &str) {
        let pad = "  ".repeat(level);
        let lead = format!("{pad}{key}: ");
        self.out.push_str(&lead);
        let col = lead.chars().count();
        write_scalar(&mut self.out, col, (level + 1) * 2, value);
        self.out.push('\n');
    }

    fn list_field(&mut self, key: &str, items: &[String]) {
        if items.is_empty() {
            self.raw_line(&format!("{key}: []"));
            return;
        }
        self.raw_line(&format!("{key}:"));
        for it in items {
            self.out.push_str("- ");
            write_scalar(&mut self.out, 2, 2, it);
            self.out.push('\n');
        }
    }
}

#[derive(PartialEq)]
enum Style {
    Plain,
    Single,
    Double,
}

fn style_for(s: &str) -> Style {
    if s.chars()
        .any(|c| c == '\n' || c == '\r' || c == '\t' || c.is_control() || c == '\u{feff}')
    {
        return Style::Double;
    }
    if plain_ok(s) {
        Style::Plain
    } else {
        Style::Single
    }
}

fn plain_ok(s: &str) -> bool {
    if s.is_empty() || s.starts_with(' ') || s.ends_with(' ') {
        return false;
    }
    let first = s.chars().next().unwrap_or(' ');
    if "#,[]{}&*!|>'\"%@`".contains(first) {
        return false;
    }
    if "-?:".contains(first) {
        let second = s.chars().nth(1);
        if second.is_none() || second == Some(' ') {
            return false;
        }
    }
    if s.contains(": ") || s.ends_with(':') || s.contains(" #") {
        return false;
    }
    !resolves_to_non_string(s)
}

/// Would a YAML 1.1 or 1.2 reader take this plain scalar as something other
/// than a string? (Conservative: quoting a string is always safe.)
fn resolves_to_non_string(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "~" | "null" | "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n" | "=" | "<<"
    ) {
        return true;
    }
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    if matches!(body.to_ascii_lowercase().as_str(), ".inf" | ".nan") {
        return true;
    }
    // int / float / sexagesimal: digits with at most one dot, `_`, `:`, exponent
    let numeric = body.replace('_', "");
    if !numeric.is_empty() {
        let dots = numeric.matches('.').count();
        let digits_ok = numeric
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ':');
        if digits_ok && dots <= 1 && numeric.chars().any(|c| c.is_ascii_digit()) {
            return true;
        }
        if numeric.parse::<f64>().is_ok() {
            return true;
        }
        if let Some(hex) = numeric
            .strip_prefix("0x")
            .or_else(|| numeric.strip_prefix("0o"))
            && !hex.is_empty()
            && hex.chars().all(|c| c.is_ascii_hexdigit())
        {
            return true;
        }
    }
    // timestamps: YYYY-MM-DD…
    let b = s.as_bytes();
    b.len() >= 8 && b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'-' && b[5].is_ascii_digit()
}

fn write_scalar(out: &mut String, col: usize, indent: usize, s: &str) {
    match style_for(s) {
        Style::Plain => write_folded(out, col, indent, s, false),
        Style::Single => {
            out.push('\'');
            write_folded(out, col + 1, indent, &s.replace('\'', "''"), true);
            out.push('\'');
        }
        Style::Double => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c.is_control() || c == '\u{feff}' => {
                        out.push_str(&format!("\\u{:04X}", c as u32))
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
        }
    }
}

/// Write words, breaking at a single space once past [`WIDTH`] — the same
/// rule PyYAML's emitter uses, so files it wrote round-trip unchanged.
fn write_folded(out: &mut String, start_col: usize, indent: usize, s: &str, quoted: bool) {
    let chars: Vec<char> = s.chars().collect();
    let mut col = start_col;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let single_space = c == ' '
            && chars.get(i + 1).is_some_and(|n| *n != ' ')
            && (i == 0 || chars[i - 1] != ' ');
        let edge = quoted && (i == 0 || i + 1 == chars.len());
        if single_space && !edge && col > WIDTH {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
            col = indent;
        } else {
            out.push(c);
            col += 1;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Auth, AuthType};
    use pretty_assertions::assert_eq;

    #[test]
    fn long_descriptions_fold_like_pyyaml() {
        let e = Entry {
            name: "Acme Search".into(),
            slug: Some("acme-search".into()),
            description: "Search Acme's product documentation — every manual, guide, and release note the team has published since launch.".into(),
            url: Some("https://acme.example.com/mcp".into()),
            auth: Some(Auth {
                kind: Some(AuthType::None),
                header_name: None,
            }),
            tags: Some(vec!["search".into(), "docs".into()]),
            version: Some("0.1.0".into()),
            ..Default::default()
        };
        let text = emit(&e);
        assert_eq!(
            text,
            format!(
                "{HEADER}name: Acme Search\nslug: acme-search\ndescription: Search Acme's product documentation — every manual, guide, and release\n  note the team has published since launch.\nurl: https://acme.example.com/mcp\nauth:\n  type: none\ntags:\n- search\n- docs\nversion: 0.1.0\n"
            )
        );
        assert_eq!(parse_entry(&text).unwrap(), e);
    }

    #[test]
    fn ambiguous_scalars_are_quoted_and_round_trip() {
        for s in [
            "1.0",
            "true",
            "no",
            "2026-01-01",
            "- x",
            "a: b",
            "#tag",
            "it's",
            "",
            "x\ny",
        ] {
            let e = Entry {
                name: s.into(),
                description: "d".into(),
                version: Some(s.into()),
                ..Default::default()
            };
            assert_eq!(parse_entry(&emit(&e)).unwrap(), e, "{s:?}");
        }
    }

    #[test]
    fn non_mappings_are_refused() {
        assert!(matches!(to_json("- a\n- b\n"), Err(YamlError::NotMapping)));
        assert!(matches!(to_json("a: [\n"), Err(YamlError::Syntax(_))));
    }
}
