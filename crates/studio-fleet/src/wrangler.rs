//! The parts of a `wrangler.toml` the fleet cares about: the Worker name, its
//! routes (custom domains), `[vars]`, and `[env.<name>]` sections.
//!
//! Wrangler does not inherit `name`, `routes` or `vars` into an environment:
//! an environment's Worker is `<name>-<env>` unless it sets its own `name`,
//! and its vars and routes are only what the environment declares.

use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Route {
    pub pattern: String,
    pub custom_domain: bool,
}

impl Route {
    /// The hostname a route serves, without any path or wildcard.
    pub fn host(&self) -> Option<String> {
        let p = self.pattern.trim();
        let p = p
            .strip_prefix("https://")
            .or_else(|| p.strip_prefix("http://"))
            .unwrap_or(p);
        let host = p.split('/').next().unwrap_or("").trim_start_matches("*.");
        (!host.is_empty() && !host.contains('*')).then(|| host.to_ascii_lowercase())
    }
}

/// One deployable Worker described by a wrangler file: the top level or an env.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WorkerSection {
    pub name: Option<String>,
    pub routes: Vec<Route>,
    pub vars: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Wrangler {
    pub top: WorkerSection,
    pub envs: BTreeMap<String, WorkerSection>,
}

impl Wrangler {
    pub fn parse(text: &str) -> Result<Self, String> {
        let v: toml::Table = toml::from_str(text).map_err(|e| e.message().to_string())?;
        let top = section(&v);
        let mut envs = BTreeMap::new();
        if let Some(toml::Value::Table(e)) = v.get("env") {
            for (k, t) in e {
                if let toml::Value::Table(t) = t {
                    envs.insert(k.clone(), section(t));
                }
            }
        }
        Ok(Self { top, envs })
    }

    /// The Worker (script) name of an environment, following wrangler's rule.
    pub fn env_worker_name(&self, env: &str) -> Option<String> {
        let e = self.envs.get(env)?;
        e.name
            .clone()
            .or_else(|| self.top.name.as_ref().map(|n| format!("{n}-{env}")))
    }
}

fn section(t: &toml::Table) -> WorkerSection {
    let name = t.get("name").and_then(|v| v.as_str()).map(String::from);
    let mut routes = Vec::new();
    if let Some(r) = t.get("route") {
        routes.extend(route(r));
    }
    if let Some(toml::Value::Array(rs)) = t.get("routes") {
        routes.extend(rs.iter().filter_map(route));
    }
    let vars = match t.get("vars") {
        Some(toml::Value::Table(vs)) => vs
            .iter()
            .map(|(k, v)| {
                let s = match v {
                    toml::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (k.clone(), s)
            })
            .collect(),
        _ => BTreeMap::new(),
    };
    WorkerSection { name, routes, vars }
}

fn route(v: &toml::Value) -> Option<Route> {
    match v {
        toml::Value::String(s) => Some(Route {
            pattern: s.clone(),
            custom_domain: false,
        }),
        toml::Value::Table(t) => Some(Route {
            pattern: t.get("pattern")?.as_str()?.to_string(),
            custom_domain: t
                .get("custom_domain")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }),
        _ => None,
    }
}

/// `OKTA_M2M_SCOPE`-style lists: space- or comma-separated, de-duplicated, in order.
pub fn parse_scopes(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in s.split(|c: char| c.is_whitespace() || c == ',') {
        if !part.is_empty() && !out.iter().any(|o| o == part) {
            out.push(part.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const TEXT: &str = r#"
name = "weather-mcp-worker"
routes = [{ pattern = "weather.mcp.acme.example", custom_domain = true }, "acme.example/legacy/*"]

[vars]
PUBLIC_MCP_URL = "https://weather.mcp.acme.example/mcp"
OKTA_M2M_SCOPE = "weather:read weather:write"
RETRIES = 3

[env.staging]
workers_dev = true

[env.staging.vars]
OKTA_M2M_SCOPE = "mcp:read"

[[env.staging.routes]]
pattern = "weather-staging.mcp.acme.example"
custom_domain = true

[env.preview]
name = "weather-preview"
"#;

    #[test]
    fn parses_top_level_and_env_sections() {
        let w = Wrangler::parse(TEXT).unwrap();
        assert_eq!(w.top.name.as_deref(), Some("weather-mcp-worker"));
        assert_eq!(w.top.routes.len(), 2);
        assert_eq!(
            w.top.routes[0].host().as_deref(),
            Some("weather.mcp.acme.example")
        );
        assert!(w.top.routes[0].custom_domain);
        assert_eq!(w.top.routes[1].host().as_deref(), Some("acme.example"));
        assert_eq!(w.top.vars["RETRIES"], "3");
        // vars are not inherited
        let st = &w.envs["staging"];
        assert_eq!(st.vars.get("PUBLIC_MCP_URL"), None);
        assert_eq!(st.vars["OKTA_M2M_SCOPE"], "mcp:read");
        assert_eq!(
            st.routes[0].host().as_deref(),
            Some("weather-staging.mcp.acme.example")
        );
        assert_eq!(
            w.env_worker_name("staging").as_deref(),
            Some("weather-mcp-worker-staging")
        );
        assert_eq!(
            w.env_worker_name("preview").as_deref(),
            Some("weather-preview")
        );
        assert_eq!(w.env_worker_name("nope"), None);
    }

    #[test]
    fn bad_toml_is_an_error_not_a_panic() {
        assert!(Wrangler::parse("name = ").is_err());
    }

    #[test]
    fn scope_lists() {
        assert_eq!(
            parse_scopes(" a:read  a:write,a:read "),
            vec!["a:read", "a:write"]
        );
        assert!(parse_scopes("").is_empty());
    }
}
