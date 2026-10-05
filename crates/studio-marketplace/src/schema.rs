//! The seven draft-07 schemas, embedded from `marketplace/schemas/`.
//!
//! These are the same files `provision` seeds into a marketplace repo's
//! `schema/` directory, so Studio and the repo's CI gate judge a file by the
//! same contract.

use std::sync::OnceLock;

use jsonschema::Validator;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SchemaId {
    ClaudeMarketplace,
    ClaudePlugin,
    CodexMarketplace,
    CodexMcpJson,
    CodexPlugin,
    McpJson,
    Server,
}

impl SchemaId {
    pub const ALL: [SchemaId; 7] = [
        SchemaId::ClaudeMarketplace,
        SchemaId::ClaudePlugin,
        SchemaId::CodexMarketplace,
        SchemaId::CodexMcpJson,
        SchemaId::CodexPlugin,
        SchemaId::McpJson,
        SchemaId::Server,
    ];

    pub fn file_name(self) -> &'static str {
        match self {
            SchemaId::ClaudeMarketplace => "claude-marketplace.schema.json",
            SchemaId::ClaudePlugin => "claude-plugin.schema.json",
            SchemaId::CodexMarketplace => "codex-marketplace.schema.json",
            SchemaId::CodexMcpJson => "codex-mcp-json.schema.json",
            SchemaId::CodexPlugin => "codex-plugin.schema.json",
            SchemaId::McpJson => "mcp-json.schema.json",
            SchemaId::Server => "server.schema.json",
        }
    }

    pub fn source(self) -> &'static str {
        match self {
            SchemaId::ClaudeMarketplace => {
                include_str!("../../../marketplace/schemas/claude-marketplace.schema.json")
            }
            SchemaId::ClaudePlugin => {
                include_str!("../../../marketplace/schemas/claude-plugin.schema.json")
            }
            SchemaId::CodexMarketplace => {
                include_str!("../../../marketplace/schemas/codex-marketplace.schema.json")
            }
            SchemaId::CodexMcpJson => {
                include_str!("../../../marketplace/schemas/codex-mcp-json.schema.json")
            }
            SchemaId::CodexPlugin => {
                include_str!("../../../marketplace/schemas/codex-plugin.schema.json")
            }
            SchemaId::McpJson => include_str!("../../../marketplace/schemas/mcp-json.schema.json"),
            SchemaId::Server => include_str!("../../../marketplace/schemas/server.schema.json"),
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// Compile a draft-07 schema from text. `Err` means the *schema* is broken.
pub fn compile(text: &str) -> Result<Validator, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    jsonschema::draft7::new(&v).map_err(|e| e.to_string())
}

/// The compiled embedded schema. Panics only if a shipped schema is broken,
/// which the crate's tests rule out.
pub fn validator(id: SchemaId) -> &'static Validator {
    static CELLS: [OnceLock<Validator>; 7] = [const { OnceLock::new() }; 7];
    CELLS[id.index()].get_or_init(|| {
        compile(id.source())
            .unwrap_or_else(|e| panic!("embedded {} is invalid: {e}", id.file_name()))
    })
}

/// Validate a value; each error is `"<json pointer>: <message>"`.
pub fn check(id: SchemaId, value: &serde_json::Value) -> Vec<String> {
    validator(id)
        .iter_errors(value)
        .map(|e| {
            let at = e.instance_path().to_string();
            if at.is_empty() {
                e.to_string()
            } else {
                format!("{at}: {e}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_schema_compiles_with_a_neutral_id() {
        for id in SchemaId::ALL {
            let _ = validator(id);
            let v: serde_json::Value = serde_json::from_str(id.source()).unwrap();
            let sid = v["$id"].as_str().unwrap();
            assert!(sid.starts_with("urn:mcp-studio:schema:"), "{sid}");
        }
    }

    #[test]
    fn a_broken_schema_is_reported_as_a_schema_problem() {
        assert!(compile(r#"{"type": "nonsense"}"#).is_err());
        assert!(compile("{").unwrap_err().starts_with("not valid JSON"));
    }
}
