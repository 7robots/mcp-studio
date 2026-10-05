//! Everything the client needs to know about one gateway, taken from an
//! instance's `[[gateway]]` table: where it is, which scopes to ask for, how
//! to recognise an admin, and what to call itself at client registration.

use anyhow::{Context, Result, bail};
use studio_core::config::GatewayConfig;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// The `[[gateway]]` id (`main`), used for Keychain accounts and pickers.
    pub id: String,
    /// The gateway origin, normalized by [`gateway_url`].
    pub url: Url,
    /// Requested at sign-in, space-separated.
    pub login_scopes: String,
    pub admin_scope: String,
    pub access_scope: String,
    /// A tool whose presence in `tools/list` proves the subject allowlist
    /// admits the caller.
    pub admin_probe_tool: String,
    /// `client_name` sent at dynamic client registration.
    pub client_name: String,
    /// TUI poll interval; 0 turns polling off.
    pub refresh_seconds: u64,
    /// Example server URL shown in the register form.
    pub server_url_hint: Option<String>,
}

impl Profile {
    pub fn from_config(g: &GatewayConfig) -> Result<Profile> {
        Ok(Profile {
            id: g.id.clone(),
            url: gateway_url(&g.url)?,
            login_scopes: g.login_scopes(),
            admin_scope: g.admin_scope.clone(),
            access_scope: g.access_scope.clone(),
            admin_probe_tool: g.admin_probe_tool.clone(),
            client_name: g.client_name.clone(),
            refresh_seconds: g.refresh_seconds,
            server_url_hint: g.server_url_hint.clone(),
        })
    }

    /// A profile with every default from `studio.toml`'s `[[gateway]]` table
    /// (the demo and the tests).
    pub fn with_defaults(id: &str, url: &str) -> Result<Profile> {
        let config: GatewayConfig =
            serde_json::from_value(serde_json::json!({"id": id, "url": url}))
                .context("building a default gateway profile")?;
        Profile::from_config(&config)
    }

    /// The gateway host, plus the port when explicit.
    pub fn host_key(&self) -> String {
        host_key(&self.url)
    }
}

/// The gateway origin, with any path dropped. Plain http is refused except
/// for a loopback host: it would send bearer and refresh tokens in cleartext.
pub fn gateway_url(text: &str) -> Result<Url> {
    let mut url = Url::parse(text).with_context(|| format!("gateway URL {text:?}"))?;
    let loopback = matches!(url.host_str(), Some(h) if h == "localhost" || h == "[::1]" || h.starts_with("127."));
    match url.scheme() {
        "https" if url.host_str().is_some() => {}
        "http" if loopback => {}
        "http" => bail!("gateway URL {text:?}: plain http is allowed only for a loopback host"),
        _ => bail!("gateway URL {text:?} must be https with a host"),
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

/// Host, plus the port when explicit: how mcpgw-manager keyed its Keychain items.
pub fn host_key(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_url_is_normalized_to_the_origin() {
        assert_eq!(
            gateway_url("https://gw.example.org/mcp?x=1")
                .unwrap()
                .as_str(),
            "https://gw.example.org/"
        );
        assert!(gateway_url("ftp://gw.example.org").is_err());
        assert!(gateway_url("http://gw.example.org").is_err());
        assert!(gateway_url("http://127.0.0.1:8787").is_ok());
        assert_eq!(
            host_key(&gateway_url("http://127.0.0.1:8787").unwrap()),
            "127.0.0.1:8787"
        );
        assert_eq!(
            host_key(&gateway_url("https://gw.example.org").unwrap()),
            "gw.example.org"
        );
    }

    #[test]
    fn defaults_come_from_the_config_model() {
        let p = Profile::with_defaults("main", "https://gw.example.org/x").unwrap();
        assert_eq!(p.url.as_str(), "https://gw.example.org/");
        assert_eq!(
            p.login_scopes,
            format!("{} {}", p.access_scope, p.admin_scope)
        );
        assert!(!p.admin_probe_tool.is_empty());
        assert!(!p.client_name.is_empty());
    }
}
