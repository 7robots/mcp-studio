//! The acceptance gate, run against a live gateway with a stored admin
//! sign-in. It drives the real Gateway module headlessly through every admin
//! action against a fixture server and checks the gateway's behaviour through
//! the session:
//!
//!  1. register the fixture (`<fixture_url>/mcp`)
//!  2. classify `<id>_read` as read
//!  3. make the fixture read-only: the read call succeeds, the write call is
//!     refused on both `call_tool` and `run`
//!  4. make it read-write: the write call succeeds
//!  5. disable it (absent from discovery), then restore it
//!  6. change its URL to `/alt/mcp`, with the classification carried over
//!  7. delete it, and find no residue in `list_servers` or, through
//!     `wrangler d1` in the gateway's checkout, in `scope_owners`
//!
//! The fixture is a harmless server with two tools, `<id>_read` and
//! `<id>_write`, where `<id>` is the first label of its host; the gateway
//! registers it under that id and mints `<id>:call` for it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use studio_gateway::model::Server;
use studio_gateway::{GatewayError, Session, Url};

use crate::framework::harness::Harness;
use crate::framework::overlay::Overlay;
use crate::gateway::{GatewayModule, Opener, Pane};
use crate::{App, Component};

/// What the gate needs from the instance.
pub struct GateConfig {
    pub session: Arc<Session>,
    /// The fixture's origin (`[[gateway]] fixture_url`).
    pub fixture_url: String,
    /// `[[gateway]] d1_database`; without it the residue check is skipped.
    pub d1_database: Option<String>,
    /// The gateway repo's checkout, where `wrangler` runs.
    pub gateway_repo_dir: Option<PathBuf>,
}

struct Gate {
    h: Harness,
    session: Arc<Session>,
    id: String,
}

fn fail(label: &str, detail: impl std::fmt::Display) -> String {
    format!("FAIL {label}\n     {detail}")
}

fn pane(app: &App) -> &Pane {
    app.module::<GatewayModule>()
        .and_then(GatewayModule::pane)
        .expect("the gate's gateway pane")
}

impl Gate {
    fn check(&self, cond: bool, label: &str) -> Result<(), String> {
        if cond {
            println!("ok   {label}");
            Ok(())
        } else {
            Err(format!("FAIL {label}"))
        }
    }

    async fn wait(&mut self, label: &str, pred: impl Fn(&App) -> bool) -> Result<(), String> {
        self.h
            .wait_until(pred, Duration::from_secs(60))
            .await
            .map_err(|e| fail(&format!("timeout waiting for: {label}"), e))
    }

    /// Waits for the running action and the reload after it, then returns the toast.
    async fn action_result(&mut self, label: &str) -> Result<String, String> {
        self.wait(label, |app| pane(app).idle()).await?;
        match self.h.app.toast().cloned() {
            Some(t) if !t.error => {
                println!("     {}", t.text);
                Ok(t.text)
            }
            Some(t) => Err(fail(label, t.text)),
            None => Err(fail(label, "no result on the status line")),
        }
    }

    fn select(&mut self) -> Result<(), String> {
        self.h.press("g");
        for _ in 0..256 {
            if pane(&self.h.app)
                .selected_server()
                .is_some_and(|s| s.id == self.id)
            {
                return Ok(());
            }
            self.h.press("j");
        }
        Err(fail(&format!("select {}", self.id), "not in the table"))
    }

    fn fixture(&self) -> Option<Server> {
        pane(&self.h.app)
            .servers
            .data
            .as_ref()?
            .servers
            .iter()
            .find(|s| s.id == self.id)
            .cloned()
    }

    async fn call(&self, tool: &str, note: &str) -> Result<Value, GatewayError> {
        let args = if tool.ends_with("_write") {
            json!({"note": note})
        } else {
            json!({})
        };
        self.session
            .call_tool(
                "call_tool",
                json!({"server": self.id, "tool": tool, "args": args}),
            )
            .await
    }

    async fn run_snippet(&self, tool: &str) -> String {
        let id = &self.id;
        let code = format!(
            "async ({{ servers }}) => {{ try {{ return {{ value: await servers.{id}.{tool}({{ note: \"gate\" }}) }}; }} \
             catch (e) {{ return {{ caught: e.kind, message: String(e.message) }}; }} }}"
        );
        match self
            .session
            .call_tool("run", json!({"servers": [id], "code": code}))
            .await
        {
            Ok(value) => value.to_string(),
            Err(err) => format!("error: {err}"),
        }
    }

    async fn delete(&mut self, label: &str) -> Result<String, String> {
        self.select()?;
        self.h.press("D");
        let id = self.id.clone();
        self.h.type_text(&id);
        self.h.press("enter");
        self.action_result(label).await
    }
}

fn text_of(value: &Value) -> String {
    value
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

/// The fixture's registry id: the first label of its host.
pub fn fixture_id(fixture_url: &str) -> Option<String> {
    let url = Url::parse(fixture_url).ok()?;
    Some(url.host_str()?.split('.').next()?.to_string())
}

/// Runs the gate, printing one line per check. `Ok` means every check passed.
pub async fn run(config: GateConfig) -> Result<(), String> {
    let id = fixture_id(&config.fixture_url)
        .ok_or_else(|| fail("fixture_url", "not a URL with a host"))?;
    let base = config.fixture_url.trim_end_matches('/').to_string();
    let (url, alt_url) = (format!("{base}/mcp"), format!("{base}/alt/mcp"));
    let (read_tool, write_tool) = (format!("{id}_read"), format!("{id}_write"));
    let opener: Opener = Arc::new(|_| anyhow::bail!("the gate never signs in"));
    let module = GatewayModule::new(vec![config.session.clone()], opener);
    let modules: Vec<Box<dyn Component>> = vec![Box::new(module)];
    let (app, rx) = App::new("gate", modules);
    let mut g = Gate {
        h: Harness::new(app, rx, (160, 50)),
        session: config.session.clone(),
        id: id.clone(),
    };

    // 0. signed in as an admin; no fixture left from an earlier run.
    g.wait("whoami and list_servers", |app| {
        let p = pane(app);
        (p.identity.data.is_some() || p.signed_out.is_some())
            && (p.servers.data.is_some() || p.signed_out.is_some())
    })
    .await?;
    if let Some(reason) = &pane(&g.h.app).signed_out {
        return Err(fail("signed in", reason));
    }
    g.check(
        pane(&g.h.app).admin() == Some(true),
        "signed in as a gateway admin",
    )?;
    if g.fixture().is_some() {
        println!("     removing a fixture left by an earlier run");
        g.delete("remove the leftover fixture").await?;
    }

    // 1. register
    g.h.press("a");
    g.h.type_text(&url);
    g.h.press("enter");
    let toast = g.action_result("register the fixture").await?;
    g.check(
        toast.starts_with(&format!("Registered {id} with 2 tools")),
        "registered through the TUI form",
    )?;
    g.check(
        g.fixture()
            .is_some_and(|s| s.url == url && s.status == "active"),
        "listed, active, at /mcp",
    )?;

    // 2. classify <id>_read
    g.select()?;
    g.h.press("c");
    let tools: Vec<String> = match pane(&g.h.app).overlay() {
        Some(Overlay::Picker(p)) => p.items.iter().map(|i| i.label.clone()).collect(),
        other => return Err(fail("open the tool picker", format!("{other:?}"))),
    };
    let at = tools
        .iter()
        .position(|t| *t == read_tool)
        .ok_or_else(|| fail(&format!("find {read_tool}"), tools.join(", ")))?;
    for _ in 0..at {
        g.h.press("j");
    }
    g.h.press("enter");
    g.h.press("r");
    let toast = g.action_result(&format!("classify {read_tool}")).await?;
    g.check(
        toast.contains(&format!("{id}.{read_tool}: read (from admin)")),
        &format!("{read_tool} classified read"),
    )?;

    // 3. read-only
    g.select()?;
    g.h.press("o");
    g.check(
        g.h.text().contains("1 of 3 tools remain callable"),
        "the toggle shows 1 of 3 callable",
    )?;
    g.h.press("y");
    g.action_result("make it read-only").await?;
    g.check(g.fixture().is_some_and(|s| s.read_only()), "read-only")?;
    match g.call(&read_tool, "").await {
        Ok(v) => g.check(
            text_of(&v).contains(&format!("{id}-read ok")),
            "read call succeeds on call_tool",
        )?,
        Err(e) => return Err(fail("read call succeeds on call_tool", e)),
    }
    match g.call(&write_tool, "gate").await {
        Err(GatewayError::Tool(m)) => {
            println!("     {m}");
            g.check(
                m.contains("refused by policy") && m.contains("read-only"),
                "write call refused on call_tool",
            )?
        }
        other => {
            return Err(fail(
                "write call refused on call_tool",
                format!("{other:?}"),
            ));
        }
    }
    let snippet = g.run_snippet(&write_tool).await;
    if !snippet.contains("policy") {
        println!("     {snippet}");
    }
    g.check(
        snippet.contains("\\\"caught\\\":\\\"policy\\\"")
            || snippet.contains("\"caught\":\"policy\""),
        "write call refused on run",
    )?;
    let snippet = g.run_snippet(&read_tool).await;
    g.check(
        snippet.contains(&format!("{id}-read ok")),
        "read call succeeds on run",
    )?;

    // 4. read-write
    g.select()?;
    g.h.press("o");
    g.h.press("y");
    g.action_result("make it read-write").await?;
    match g.call(&write_tool, "gate").await {
        Ok(v) => g.check(
            text_of(&v).contains(&format!("{id}-write ok: gate")),
            "write call succeeds once read-write",
        )?,
        Err(e) => return Err(fail("write call succeeds once read-write", e)),
    }

    // 5. disable, then restore
    g.select()?;
    g.h.press("s");
    g.h.press("d");
    g.action_result("disable").await?;
    let active = g
        .session
        .list_servers(false, false)
        .await
        .map_err(|e| fail("list_servers", e))?;
    g.check(
        !active.servers.iter().any(|s| s.id == id),
        "disabled: absent from list_servers",
    )?;
    let found = g
        .session
        .call_tool("search_tools", json!({"query": id}))
        .await
        .map_err(|e| fail("search_tools", e))?;
    g.check(
        !found.to_string().contains(&format!("{id}.{id}_")),
        "disabled: absent from search_tools",
    )?;
    g.select()?;
    g.h.press("s");
    g.h.press("a");
    g.action_result("restore").await?;
    let active = g
        .session
        .list_servers(false, false)
        .await
        .map_err(|e| fail("list_servers", e))?;
    g.check(
        active.servers.iter().any(|s| s.id == id),
        "restored: back in list_servers",
    )?;

    // 6. change URL
    g.select()?;
    g.h.press("u");
    g.h.erase(200);
    g.h.type_text(&alt_url);
    g.h.press("enter");
    g.h.type_text(&id);
    g.h.press("enter");
    let toast = g.action_result("change the URL").await?;
    g.check(
        // update_server's "url <old> -> <new>", or the composite's "moved to <new>".
        toast.contains(alt_url.as_str()) && !toast.contains("could not restore"),
        "moved, with nothing lost",
    )?;
    let moved = g.fixture();
    g.check(
        moved.as_ref().is_some_and(|s| s.url == alt_url),
        "listed at /alt/mcp",
    )?;
    g.check(
        moved.is_some_and(|s| {
            s.tool_classes
                .iter()
                .any(|t| t.name == read_tool && t.classification == "read" && t.source == "admin")
        }),
        &format!("{read_tool} still classified read"),
    )?;

    // 7. delete, and no residue
    g.delete("delete").await?;
    let all = g
        .session
        .list_servers(true, false)
        .await
        .map_err(|e| fail("list_servers", e))?;
    g.check(
        !all.servers.iter().any(|s| s.id == id),
        "deleted: absent from list_servers, inactive included",
    )?;
    match (&config.d1_database, &config.gateway_repo_dir) {
        (Some(database), Some(dir)) => {
            let output = tokio::process::Command::new("npx")
                .args([
                    "wrangler",
                    "d1",
                    "execute",
                    database,
                    "--remote",
                    "--json",
                    "--command",
                ])
                .arg(format!(
                    "SELECT scope FROM scope_owners WHERE server_id = '{id}' OR scope = '{id}:call'"
                ))
                .current_dir(dir)
                .output()
                .await
                .map_err(|e| fail("run wrangler", e))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let rows: Value = serde_json::from_str(&stdout)
                .map_err(|e| fail("parse wrangler output", format!("{e}: {stdout}")))?;
            let residue = rows
                .pointer("/0/results")
                .and_then(Value::as_array)
                .map_or(usize::MAX, Vec::len);
            g.check(
                output.status.success() && residue == 0,
                "no scope_owners residue",
            )?;
        }
        _ => println!("skip no scope_owners check: needs [[gateway]] d1_database and repo"),
    }

    println!("GATE PASSED");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixture_id_is_the_first_host_label() {
        assert_eq!(
            fixture_id("https://fixture.mcp.example.org").as_deref(),
            Some("fixture")
        );
        assert_eq!(fixture_id("not a url"), None);
    }
}
