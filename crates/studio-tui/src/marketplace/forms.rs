//! The entry form: one field per `server.yaml` field the operations take,
//! prefilled from an entry (edit, draft) or empty (add), and parsed back
//! into an [`Entry`] or an [`EntryPatch`] with only the changed fields.

use studio_marketplace::model::{AuthType, Installation, Kind, Transport};
use studio_marketplace::{Entry, EntryPatch};

use crate::framework::overlay::Form;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum F {
    Slug,
    Name,
    Kind,
    Description,
    Url,
    Transport,
    Auth,
    Header,
    Tags,
    Category,
    Homepage,
    Version,
    Owner,
    Installation,
    Strict,
    License,
    Repository,
}

/// (field, label, hint). Labels fit the form's 12-column label width.
const FIELDS: &[(F, &str, &str)] = &[
    (
        F::Slug,
        "slug",
        "lowercase-with-hyphens (default: from the name)",
    ),
    (F::Name, "name", "display name"),
    (F::Kind, "kind", "mcp-server | skill"),
    (F::Description, "description", "one sentence"),
    (F::Url, "url", "https://... (required for an mcp-server)"),
    (F::Transport, "transport", "http | sse"),
    (F::Auth, "auth", "none | bearer | api_key | oauth"),
    (
        F::Header,
        "header",
        "default Authorization (bearer / api_key)",
    ),
    (F::Tags, "tags", "comma-separated"),
    (F::Category, "category", ""),
    (F::Homepage, "homepage", "https://..."),
    (F::Version, "version", "default 0.1.0"),
    (F::Owner, "owner", ""),
    (
        F::Installation,
        "installation",
        "AVAILABLE | INSTALLED_BY_DEFAULT | NOT_AVAILABLE",
    ),
    (F::Strict, "strict", "true | false (Claude)"),
    (F::License, "license", "e.g. MIT (Claude)"),
    (F::Repository, "repository", "https://..."),
];

/// The fields a form shows: all of them for add, all but slug and kind for
/// edit (those are the entry's identity).
fn fields(add: bool) -> Vec<(F, &'static str, &'static str)> {
    FIELDS
        .iter()
        .copied()
        .filter(|(f, _, _)| add || !matches!(f, F::Slug | F::Kind))
        .collect()
}

/// The raw text of a field for `e` (empty when absent).
fn value(f: F, slug: &str, e: &Entry) -> String {
    let opt = |v: &Option<String>| v.clone().unwrap_or_default();
    match f {
        F::Slug => slug.to_string(),
        F::Name => e.name.clone(),
        F::Kind => e.kind().as_str().to_string(),
        F::Description => e.description.clone(),
        F::Url => opt(&e.url),
        F::Transport => e
            .transport
            .map(|t| t.as_str().to_string())
            .unwrap_or_default(),
        F::Auth => e
            .auth
            .as_ref()
            .and_then(|a| a.kind)
            .map(|k| k.as_str().to_string())
            .unwrap_or_default(),
        F::Header => e
            .auth
            .as_ref()
            .and_then(|a| a.header_name.clone())
            .unwrap_or_default(),
        F::Tags => e.tags().join(", "),
        F::Category => opt(&e.category),
        F::Homepage => opt(&e.homepage),
        F::Version => opt(&e.version),
        F::Owner => opt(&e.owner),
        F::Installation => e
            .codex
            .as_ref()
            .and_then(|c| c.installation)
            .map(|i| i.as_str().to_string())
            .unwrap_or_default(),
        F::Strict => e
            .claude
            .as_ref()
            .and_then(|c| c.strict)
            .map(|s| s.to_string())
            .unwrap_or_default(),
        F::License => e
            .claude
            .as_ref()
            .and_then(|c| c.license.clone())
            .unwrap_or_default(),
        F::Repository => e
            .claude
            .as_ref()
            .and_then(|c| c.repository.clone())
            .unwrap_or_default(),
    }
}

/// An add form, empty or prefilled from a draft.
pub fn add_form(title: &str, draft: Option<&Entry>) -> Form {
    let empty = Entry::default();
    let e = draft.unwrap_or(&empty);
    let slug = e.slug.clone().unwrap_or_default();
    let mut form = Form::new(title);
    for (f, label, hint) in fields(true) {
        let v = match (f, draft) {
            // An empty form starts as a server; the hint says what else works.
            (F::Kind, None) => "mcp-server".to_string(),
            (_, None) => String::new(),
            _ => value(f, &slug, e),
        };
        form = form.field(label, hint, &v);
    }
    form
}

/// An edit form prefilled with the entry as it is.
pub fn edit_form(title: &str, slug: &str, e: &Entry) -> Form {
    let mut form = Form::new(title);
    for (f, label, hint) in fields(false) {
        form = form.field(label, hint, &value(f, slug, e));
    }
    form
}

fn parse_kind(s: &str) -> Result<Kind, String> {
    match s.trim() {
        "" | "mcp-server" | "server" => Ok(Kind::McpServer),
        "skill" => Ok(Kind::Skill),
        other => Err(format!("kind {other:?}: use mcp-server or skill")),
    }
}

fn parse_transport(s: &str) -> Result<Transport, String> {
    match s.trim() {
        "http" => Ok(Transport::Http),
        "sse" => Ok(Transport::Sse),
        other => Err(format!("transport {other:?}: use http or sse")),
    }
}

fn parse_auth(s: &str) -> Result<AuthType, String> {
    AuthType::parse(s.trim())
        .ok_or_else(|| format!("auth {s:?}: use none, bearer, api_key or oauth"))
}

fn parse_installation(s: &str) -> Result<Installation, String> {
    match s.trim().to_ascii_uppercase().replace('-', "_").as_str() {
        "AVAILABLE" => Ok(Installation::Available),
        "INSTALLED_BY_DEFAULT" => Ok(Installation::InstalledByDefault),
        "NOT_AVAILABLE" => Ok(Installation::NotAvailable),
        _ => Err(format!(
            "installation {s:?}: use AVAILABLE, INSTALLED_BY_DEFAULT or NOT_AVAILABLE"
        )),
    }
}

fn parse_bool(s: &str) -> Result<bool, String> {
    match s.trim() {
        "true" | "yes" => Ok(true),
        "false" | "no" => Ok(false),
        other => Err(format!("strict {other:?}: use true or false")),
    }
}

fn parse_tags(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Sets field `f` of `patch` from `v`. Enumerated fields left empty are not
/// set (they cannot be cleared through a patch).
fn set(patch: &mut EntryPatch, f: F, v: &str) -> Result<(), String> {
    let text = Some(v.trim().to_string());
    let enumerated = !v.trim().is_empty();
    match f {
        F::Slug | F::Kind => {}
        F::Name => patch.name = text,
        F::Description => patch.description = text,
        F::Url => patch.url = text,
        F::Transport if enumerated => patch.transport = Some(parse_transport(v)?),
        F::Auth if enumerated => patch.auth = Some(parse_auth(v)?),
        F::Header => patch.header_name = text,
        F::Tags => patch.tags = Some(parse_tags(v)),
        F::Category => patch.category = text,
        F::Homepage => patch.homepage = text,
        F::Version => patch.version = text,
        F::Owner => patch.owner = text,
        F::Installation if enumerated => patch.installation = Some(parse_installation(v)?),
        F::Strict if enumerated => patch.strict = Some(parse_bool(v)?),
        F::License => patch.license = text,
        F::Repository => patch.repository = text,
        F::Transport | F::Auth | F::Installation | F::Strict => {}
    }
    Ok(())
}

/// A new entry and its slug from an add form's values.
pub fn parse_add(values: &[String]) -> Result<(Entry, String), String> {
    let mut patch = EntryPatch::default();
    let mut slug = String::new();
    let mut kind = Kind::McpServer;
    for ((f, _, _), v) in fields(true).into_iter().zip(values) {
        match f {
            F::Slug => slug = v.trim().to_string(),
            F::Kind => kind = parse_kind(v)?,
            // Empty optional text fields stay absent in a new entry.
            _ if v.trim().is_empty() && !matches!(f, F::Name | F::Description) => {}
            _ => set(&mut patch, f, v)?,
        }
    }
    let entry = patch.into_entry(kind);
    if slug.is_empty() {
        slug = studio_marketplace::model::slugify(&entry.name);
    }
    if slug.is_empty() {
        return Err("a slug (or a name to derive it from) is required".into());
    }
    Ok((entry, slug))
}

/// A patch with only the fields an edit form changed.
pub fn parse_edit(slug: &str, original: &Entry, values: &[String]) -> Result<EntryPatch, String> {
    let mut patch = EntryPatch::default();
    for ((f, _, _), v) in fields(false).into_iter().zip(values) {
        if *v != value(f, slug, original) {
            set(&mut patch, f, v)?;
        }
    }
    Ok(patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_form_round_trips_a_draft() {
        let e = Entry {
            name: "Acme Search".into(),
            slug: Some("acme-search".into()),
            description: "Search Acme docs.".into(),
            url: Some("https://search.mcp.acme.example/mcp".into()),
            transport: Some(Transport::Http),
            tags: Some(vec!["search".into(), "docs".into()]),
            version: Some("1.2.0".into()),
            ..Default::default()
        };
        let form = add_form("Add", Some(&e));
        let (entry, slug) = parse_add(&form.values()).unwrap();
        assert_eq!(slug, "acme-search");
        assert_eq!(entry.url, e.url);
        assert_eq!(entry.tags, e.tags);
        assert_eq!(entry.version.as_deref(), Some("1.2.0"));
        assert_eq!(entry.kind(), Kind::McpServer);
    }

    #[test]
    fn edit_patch_has_only_changed_fields() {
        let e = Entry {
            name: "Acme Search".into(),
            description: "Search.".into(),
            version: Some("1.0.0".into()),
            ..Default::default()
        };
        let mut form = edit_form("Edit", "acme-search", &e);
        assert!(
            parse_edit("acme-search", &e, &form.values())
                .unwrap()
                .is_empty()
        );
        let at = form
            .fields
            .iter()
            .position(|f| f.label == "version")
            .unwrap();
        form.fields[at].value = "1.1.0".into();
        let patch = parse_edit("acme-search", &e, &form.values()).unwrap();
        assert_eq!(patch.version.as_deref(), Some("1.1.0"));
        assert!(patch.name.is_none());
    }

    #[test]
    fn bad_enums_are_reported() {
        let mut form = add_form("Add", None);
        let at = form.fields.iter().position(|f| f.label == "auth").unwrap();
        form.fields[at].value = "magic".into();
        assert!(parse_add(&form.values()).unwrap_err().contains("auth"));
    }
}
