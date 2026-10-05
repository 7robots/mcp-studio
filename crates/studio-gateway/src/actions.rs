//! Admin actions: each is one or a few admin tool calls, reported back as one
//! line for a status bar (the TUI) and the raw result (the CLI's `--json`).
//! The one change the gateway has no tool for — a new URL — is composed out
//! of unregister and register, putting back what a registration does not carry.

use serde_json::{Value, json};

use crate::client::{GatewayError, GatewayResult, Session};
use crate::model::Server;

/// Scopes a refresh found the server advertising, against the approved set.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ScopeDrift {
    pub approved: Vec<String>,
    pub advertised: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Register {
        url: String,
        id: Option<String>,
        description: Option<String>,
        timeout_ms: Option<u64>,
    },
    Timeout {
        server: String,
        timeout_ms: u64,
    },
    ChangeUrl {
        server: Box<Server>,
        url: String,
    },
    /// `None` refreshes every server.
    Refresh {
        server: Option<String>,
    },
    ApproveScopes {
        server: String,
    },
    Status {
        server: String,
        status: &'static str,
    },
    Access {
        server: String,
        access: &'static str,
    },
    Classify {
        server: String,
        tool: String,
        class: Option<&'static str>,
    },
    Delete {
        server: String,
    },
}

impl Action {
    /// What the status bar says while it runs.
    pub fn describe(&self) -> String {
        match self {
            Action::Register { url, .. } => format!("Registering {url}"),
            Action::Timeout { server, .. } => format!("Setting {server}'s timeout"),
            Action::ChangeUrl { server, .. } => format!("Moving {} to its new URL", server.id),
            Action::Refresh { server: Some(s) } => format!("Refreshing {s}"),
            Action::Refresh { server: None } => "Refreshing every server".into(),
            Action::ApproveScopes { server } => format!("Approving {server}'s scopes"),
            Action::Status { server, status } => format!("Setting {server} {status}"),
            Action::Access { server, access } => {
                format!("Setting {server} {}", access.replace('_', "-"))
            }
            Action::Classify { server, tool, .. } => format!("Classifying {server}.{tool}"),
            Action::Delete { server } => format!("Deleting {server}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub message: String,
    /// Drift each refreshed server reported, by server id; an empty vector
    /// means the refresh found none (clearing any earlier report).
    pub drift: Vec<(String, Option<ScopeDrift>)>,
    /// The gateway's result (for a URL change, a summary of the steps).
    pub raw: Value,
}

impl Outcome {
    fn says(message: impl Into<String>, raw: Value) -> Outcome {
        Outcome {
            message: message.into(),
            drift: Vec::new(),
            raw,
        }
    }

    /// The CLI's `--json` shape.
    pub fn to_json(&self) -> Value {
        json!({"ok": true, "message": self.message, "result": self.raw})
    }
}

fn str_of(value: &Value, key: &str) -> String {
    match &value[key] {
        Value::String(s) => s.clone(),
        Value::Null => "none".into(),
        other => other.to_string(),
    }
}

pub async fn run(session: &Session, action: Action) -> GatewayResult<Outcome> {
    match action {
        Action::Register {
            url,
            id,
            description,
            timeout_ms,
        } => {
            let r = register(
                session,
                &url,
                id.as_deref(),
                description.as_deref(),
                timeout_ms,
            )
            .await?;
            Ok(Outcome::says(
                format!(
                    "Registered {} with {} tools",
                    str_of(&r, "registered"),
                    r["tools"]
                ),
                r,
            ))
        }
        Action::Timeout { server, timeout_ms } => {
            let r = session
                .call_tool(
                    "set_server_timeout",
                    json!({"server": server, "timeout_ms": timeout_ms}),
                )
                .await?;
            Ok(Outcome::says(
                format!("{server} timeout {} -> {} ms", r["was"], r["now"]),
                r,
            ))
        }
        Action::ChangeUrl { server, url } => change_url(session, &server, &url).await,
        Action::Refresh { server } => refresh(session, server.as_deref(), false).await,
        Action::ApproveScopes { server } => refresh(session, Some(&server), true).await,
        Action::Status { server, status } => {
            let r = session
                .call_tool(
                    "set_server_status",
                    json!({"server": server, "status": status}),
                )
                .await?;
            Ok(Outcome::says(
                format!("{server} {} -> {}", str_of(&r, "was"), str_of(&r, "now")),
                r,
            ))
        }
        Action::Access { server, access } => {
            let r = session
                .call_tool(
                    "set_server_access",
                    json!({"server": server, "access": access}),
                )
                .await?;
            let note = r["note"].as_str().unwrap_or_default().to_string();
            Ok(Outcome::says(
                format!("{server} is {}. {note}", access.replace('_', "-")),
                r,
            ))
        }
        Action::Classify {
            server,
            tool,
            class,
        } => {
            let r = session
                .call_tool(
                    "set_tool_class",
                    json!({"server": server, "tool": tool, "class": class}),
                )
                .await?;
            let mut message = format!(
                "{server}.{tool}: {} (from {})",
                str_of(&r, "effective"),
                str_of(&r, "source")
            );
            if let Some(note) = r["note"].as_str() {
                message = format!("{message}. {note}");
            }
            Ok(Outcome::says(message, r))
        }
        Action::Delete { server } => {
            let r = session
                .call_tool("unregister_server", json!({"server": server}))
                .await?;
            Ok(Outcome::says(
                format!(
                    "Deleted {} ({})",
                    str_of(&r, "unregistered"),
                    str_of(&r, "url")
                ),
                r,
            ))
        }
    }
}

async fn register(
    session: &Session,
    url: &str,
    id: Option<&str>,
    description: Option<&str>,
    timeout_ms: Option<u64>,
) -> GatewayResult<Value> {
    let mut args = json!({"url": url});
    if let Some(id) = id {
        args["id"] = json!(id);
    }
    if let Some(description) = description {
        args["description"] = json!(description);
    }
    if let Some(timeout_ms) = timeout_ms {
        args["timeout_ms"] = json!(timeout_ms);
    }
    session.call_tool("register_server", args).await
}

async fn refresh(session: &Session, server: Option<&str>, approve: bool) -> GatewayResult<Outcome> {
    let mut args = json!({"approve_scopes": approve});
    if let Some(server) = server {
        args["server"] = json!(server);
    }
    let r = session.call_tool("refresh_server", args).await?;
    let outcomes = r["outcomes"].as_array().cloned().unwrap_or_default();
    let mut drift = Vec::new();
    let mut failures = Vec::new();
    for o in &outcomes {
        let id = str_of(o, "server");
        let found: Option<ScopeDrift> = serde_json::from_value(o["scope_drift"].clone()).ok();
        if o["ok"] != json!(true) {
            failures.push(format!("{id}: {}", str_of(o, "error")));
        }
        drift.push((id, found));
    }
    let drifted: Vec<&str> = drift
        .iter()
        .filter(|(_, d)| d.is_some())
        .map(|(id, _)| id.as_str())
        .collect();
    let mut message = match (approve, server) {
        (true, Some(server)) => {
            let adopted = outcomes
                .first()
                .and_then(|o| o["scopes_approved"].as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                });
            match adopted {
                Some(scopes) => format!("{server} now minted: {scopes}"),
                None => format!("{server} refreshed; nothing to approve"),
            }
        }
        _ => format!("Refreshed {}, {} failed", r["refreshed"], r["failed"]),
    };
    if !drifted.is_empty() && !approve {
        message.push_str(&format!(
            "; scope drift on {} (A to review)",
            drifted.join(", ")
        ));
    }
    if !failures.is_empty() {
        message.push_str(&format!("; {}", failures.join("; ")));
    }
    Ok(Outcome {
        message,
        drift,
        raw: r,
    })
}

/// A new URL for a registered server. The gateway has no tool for it, so this is
/// unregister then register under the same id, which is a new registration: it
/// comes back active, read-write and unclassified. What an administrator had set
/// — timeout (passed to register), status, access and tool classifications —
/// is put back afterwards. If the new URL is refused the old registration is
/// restored the same way, and the error says so.
pub async fn change_url(session: &Session, server: &Server, url: &str) -> GatewayResult<Outcome> {
    let id = server.id.as_str();
    session
        .call_tool("unregister_server", json!({"server": id}))
        .await?;
    let description = server.description.as_deref();
    match register(session, url, Some(id), description, Some(server.timeout_ms)).await {
        Ok(_) => {
            let lost = restore(session, server).await;
            let raw = json!({"server": id, "was": server.url, "now": url, "not_restored": lost});
            Ok(Outcome::says(
                format!("{id} moved to {url}{}", lost_clause(&lost)),
                raw,
            ))
        }
        Err(refused) => match register(
            session,
            &server.url,
            Some(id),
            description,
            Some(server.timeout_ms),
        )
        .await
        {
            Ok(_) => {
                let restored = lost_clause(&restore(session, server).await);
                Err(GatewayError::Tool(format!(
                    "the new URL was refused ({refused}); {id} is registered at its old URL again{restored}"
                )))
            }
            Err(again) => Err(GatewayError::Tool(format!(
                "the new URL was refused ({refused}) and restoring the old one failed ({again}); {id} is no longer registered"
            ))),
        },
    }
}

/// Puts back status, access and admin classifications after a re-registration.
/// Returns what could not be put back.
async fn restore(session: &Session, server: &Server) -> Vec<String> {
    let id = server.id.as_str();
    let mut lost = Vec::new();
    if server.status != "active"
        && session
            .call_tool(
                "set_server_status",
                json!({"server": id, "status": server.status}),
            )
            .await
            .is_err()
    {
        lost.push(format!("status {}", server.status));
    }
    if server.read_only()
        && session
            .call_tool(
                "set_server_access",
                json!({"server": id, "access": "read_only"}),
            )
            .await
            .is_err()
    {
        lost.push("read-only access".to_string());
    }
    for tool in server.tool_classes.iter().filter(|t| t.source == "admin") {
        let args = json!({"server": id, "tool": tool.name, "class": tool.classification});
        if session.call_tool("set_tool_class", args).await.is_err() {
            lost.push(format!("{}={}", tool.name, tool.classification));
        }
    }
    lost
}

/// A clause for the status line naming anything that could not be restored.
fn lost_clause(lost: &[String]) -> String {
    if lost.is_empty() {
        String::new()
    } else {
        format!("; could not restore {}", lost.join(", "))
    }
}
