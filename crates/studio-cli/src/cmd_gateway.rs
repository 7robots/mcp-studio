//! `mcp-studio gateway …`: sign-in, the registry, and every admin action, for
//! scripts and agents (each read and each action has `--json`), plus the
//! acceptance gate. The TUI (no subcommand) is [`tui`].
//!
//! Environment: `MCP_STUDIO_TOKEN_FILE` keeps the token pair in a 0600 file
//! instead of the Keychain; `MCP_STUDIO_BROWSER` names a command (split on
//! whitespace, the URL appended) that shows the sign-in page instead of the
//! system's `open`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};
use studio_core::Instance;
use studio_core::config::GatewayConfig;
use studio_gateway::actions::{self, Action};
use studio_gateway::oauth::{ClientCache, LOGIN_TIMEOUT};
use studio_gateway::tokens::{self, FileStore, TokenStore};
use studio_gateway::util::now_unix;
use studio_gateway::{Profile, Session, Url, http_client};
use studio_tui::gateway::{GatewayModule, Opener};

use crate::Ctx;

const BROWSER_ENV: &str = "MCP_STUDIO_BROWSER";

#[derive(Args, Clone, Debug, Default)]
pub struct Pick {
    /// Which [[gateway]] of the instance (default: the first).
    #[arg(long = "gateway", value_name = "ID")]
    gateway: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Sign in through the gateway in the browser.
    Login {
        #[command(flatten)]
        pick: Pick,
    },
    /// Revoke the stored sign-in at the gateway and forget it.
    Logout {
        #[command(flatten)]
        pick: Pick,
    },
    /// Who is signed in, and whether they can administer the gateway.
    Whoami {
        #[command(flatten)]
        pick: Pick,
        /// The whoami result as JSON, with `admin` added.
        #[arg(long)]
        json: bool,
    },
    /// Registered servers.
    Servers {
        #[command(flatten)]
        pick: Pick,
        /// Include disabled and quarantined servers.
        #[arg(long)]
        all: bool,
        /// The gateway's list_servers result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Register a server (admin).
    Register {
        #[command(flatten)]
        pick: Pick,
        /// The server's MCP endpoint (https).
        url: String,
        /// Registry id; default: the first label of the host.
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        description: Option<String>,
        /// Call timeout, 1000-120000 ms.
        #[arg(long)]
        timeout_ms: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Re-read a server's tools and scopes, or every server's (admin).
    Refresh {
        #[command(flatten)]
        pick: Pick,
        /// Server id; every server when left out.
        server: Option<String>,
        /// Adopt the scope drift the refresh finds (one server only).
        #[arg(long, requires = "server")]
        approve: bool,
        #[arg(long)]
        json: bool,
    },
    /// Set a server's status (admin). `active` also lifts quarantine.
    Status {
        #[command(flatten)]
        pick: Pick,
        server: String,
        status: StatusArg,
        #[arg(long)]
        json: bool,
    },
    /// Make a server read-only or read-write (admin).
    Access {
        #[command(flatten)]
        pick: Pick,
        server: String,
        access: AccessArg,
        #[arg(long)]
        json: bool,
    },
    /// Set a server's call timeout (admin).
    Timeout {
        #[command(flatten)]
        pick: Pick,
        server: String,
        /// 1000-120000.
        ms: u64,
        #[arg(long)]
        json: bool,
    },
    /// Classify one of a server's tools, or clear the record (admin).
    Classify {
        #[command(flatten)]
        pick: Pick,
        server: String,
        tool: String,
        class: ClassArg,
        #[arg(long)]
        json: bool,
    },
    /// Unregister a server (admin). Needs --yes.
    Unregister {
        #[command(flatten)]
        pick: Pick,
        server: String,
        /// Confirm: its tools, classifications and scope claims go with it.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Move a server to a new URL: unregister, re-register under the same id,
    /// restore status, access and classifications; the old registration comes
    /// back if the new URL is refused (admin). Needs --yes.
    SetUrl {
        #[command(flatten)]
        pick: Pick,
        server: String,
        url: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// The acceptance gate: every admin action through the TUI against the
    /// gateway's fixture server, which it registers and deletes again.
    Gate {
        #[command(flatten)]
        pick: Pick,
    },
    /// The TUI (also the default with no subcommand).
    Tui {
        /// Against an in-process fake gateway; needs no instance.
        #[arg(long)]
        demo: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum StatusArg {
    Active,
    Disabled,
    Quarantined,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AccessArg {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ClassArg {
    Read,
    Write,
    Destructive,
    /// Remove the admin record; the default classification applies again.
    Clear,
}

/// Where tokens, caches and the browser come from. The process environment in
/// production; explicit in tests, which must never touch the Keychain.
struct Env {
    token_file: Option<PathBuf>,
    /// Overrides the instance's cache directory.
    cache_dir: Option<PathBuf>,
    /// The stand-alone client's DCR cache, copied once.
    legacy_cache: Option<PathBuf>,
    opener: Opener,
}

impl Env {
    fn from_process(announce: bool) -> Env {
        let legacy_cache = std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|base| {
                base.join(tokens::LEGACY_KEYCHAIN_SERVICE)
                    .join("clients.json")
            });
        Env {
            token_file: std::env::var_os(tokens::TOKEN_FILE_ENV)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            cache_dir: None,
            legacy_cache,
            opener: browser(announce),
        }
    }

    fn session(&self, inst: &Instance, gateway: &GatewayConfig) -> Result<Session> {
        let profile = Profile::from_config(gateway)?;
        let cache_dir = self.cache_dir.clone().unwrap_or_else(|| inst.cache_dir());
        let store: Box<dyn TokenStore> = match &self.token_file {
            Some(path) => Box::new(FileStore { path: path.clone() }),
            None => tokens::default_store(inst.name(), &profile, &cache_dir)?,
        };
        let clients = cache_dir.join("clients.json");
        let cache = match &self.legacy_cache {
            Some(legacy) => ClientCache::with_legacy(clients, legacy.clone()),
            None => ClientCache::new(clients),
        };
        Session::new(http_client(), profile, store, cache)
    }

    fn pick(&self, inst: &Instance, pick: &Pick) -> Result<Session> {
        let gateway = inst
            .config
            .gateway(pick.gateway.as_deref())
            .ok_or_else(|| match &pick.gateway {
                Some(id) => anyhow!(
                    "no [[gateway]] with id {id:?} in {}; have: {}",
                    inst.root.display(),
                    inst.config
                        .gateways
                        .iter()
                        .map(|g| g.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None => anyhow!("{} has no [[gateway]]", inst.root.display()),
            })?;
        self.session(inst, gateway)
    }
}

/// Shows a sign-in page with `$MCP_STUDIO_BROWSER` or the system opener.
/// `announce` prints the URL first (the CLI); the TUI shows it itself.
fn browser(announce: bool) -> Opener {
    Arc::new(move |url: &Url| {
        if announce {
            eprintln!("Opening the gateway sign-in page. If no browser appears, visit:\n  {url}");
        }
        let default = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let command = std::env::var(BROWSER_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| default.into());
        let mut parts = command.split_whitespace();
        let program = parts.next().unwrap_or(default);
        std::process::Command::new(program)
            .args(parts)
            .arg(url.as_str())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("starting {program}"))?;
        Ok(())
    })
}

pub async fn run(cmd: Cmd, ctx: &Ctx) -> Result<()> {
    if let Cmd::Tui { demo } = cmd {
        return tui(ctx, demo).await;
    }
    let env = Env::from_process(true);
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    execute(cmd, ctx, &env, &mut out, &mut err).await
}

fn describe_expiry(expires_at: u64) -> String {
    let now = now_unix();
    if expires_at <= now {
        return "expired (refreshed on next use)".into();
    }
    format!("expires in {}m", (expires_at - now) / 60)
}

async fn execute(
    cmd: Cmd,
    ctx: &Ctx,
    env: &Env,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    let inst = ctx.instance()?;
    let (pick, json) = match &cmd {
        Cmd::Login { pick } | Cmd::Logout { pick } | Cmd::Gate { pick } => (pick.clone(), false),
        Cmd::Whoami { pick, json }
        | Cmd::Servers { pick, json, .. }
        | Cmd::Register { pick, json, .. }
        | Cmd::Refresh { pick, json, .. }
        | Cmd::Status { pick, json, .. }
        | Cmd::Access { pick, json, .. }
        | Cmd::Timeout { pick, json, .. }
        | Cmd::Classify { pick, json, .. }
        | Cmd::Unregister { pick, json, .. }
        | Cmd::SetUrl { pick, json, .. } => (pick.clone(), *json),
        Cmd::Tui { .. } => bail!("the TUI does not run here"),
    };
    let session = env.pick(&inst, &pick)?;
    let gateway = session.gateway().clone();
    let action = match cmd {
        Cmd::Login { .. } => {
            let open = env.opener.clone();
            let tokens = session.login(LOGIN_TIMEOUT, move |url| open(url)).await?;
            writeln!(err, "Signed in to {gateway} with scopes: {}", tokens.scope)?;
            writeln!(err, "Tokens stored in {}", session.store().describe())?;
            return Ok(());
        }
        Cmd::Logout { .. } => {
            session.logout().await?;
            writeln!(err, "Signed out of {gateway}")?;
            return Ok(());
        }
        Cmd::Gate { .. } => {
            let config = inst
                .config
                .gateway(pick.gateway.as_deref())
                .expect("picked above");
            let fixture_url = config
                .fixture_url
                .clone()
                .ok_or_else(|| anyhow!("the gate needs [[gateway]] fixture_url"))?;
            let gate = studio_tui::gateway::gate::GateConfig {
                session: Arc::new(session),
                fixture_url,
                d1_database: config.d1_database.clone(),
                gateway_repo_dir: config.repo.as_deref().map(|r| inst.repo_dir(r)),
            };
            return studio_tui::gateway::gate::run(gate)
                .await
                .map_err(|failure| anyhow!("{failure}"));
        }
        Cmd::Whoami { .. } => {
            let identity = session.identity().await?;
            if json {
                let mut value = serde_json::to_value(&identity.whoami)?;
                value["admin"] = serde_json::Value::Bool(identity.admin);
                writeln!(out, "{}", serde_json::to_string_pretty(&value)?)?;
            } else {
                let w = &identity.whoami;
                let dash = || "-".to_string();
                writeln!(out, "gateway  {} {gateway}", session.profile().id)?;
                writeln!(out, "sub      {}", w.sub.clone().unwrap_or_else(dash))?;
                writeln!(out, "email    {}", w.email.clone().unwrap_or_else(dash))?;
                writeln!(out, "name     {}", w.name.clone().unwrap_or_else(dash))?;
                writeln!(out, "client   {}", w.client_id.clone().unwrap_or_else(dash))?;
                writeln!(out, "scopes   {}", w.scopes.join(" "))?;
                writeln!(
                    out,
                    "access   {}",
                    if identity.admin { "admin" } else { "view-only" }
                )?;
                if let Some(tokens) = session.tokens().await {
                    writeln!(
                        out,
                        "token    {}; {}",
                        describe_expiry(tokens.expires_at),
                        session.store().describe()
                    )?;
                }
            }
            return Ok(());
        }
        Cmd::Servers { all, .. } => {
            if json {
                let value = session.list_servers_raw(all, false).await?;
                writeln!(out, "{}", serde_json::to_string_pretty(&value)?)?;
            } else {
                let list = session.list_servers(all, false).await?;
                let width = list
                    .servers
                    .iter()
                    .map(|s| s.id.len())
                    .max()
                    .unwrap_or(2)
                    .max(2);
                writeln!(
                    out,
                    "{:width$}  {:11}  {:9}  {:6}  {:>5}  LAST ERROR",
                    "ID", "STATUS", "HEALTH", "ACCESS", "TOOLS"
                )?;
                for s in &list.servers {
                    let error = s
                        .last_error
                        .as_deref()
                        .or(s.last_call_error.as_deref())
                        .unwrap_or("");
                    let access = if s.read_only() { "RO" } else { "RW" };
                    writeln!(
                        out,
                        "{:width$}  {:11}  {:9}  {:6}  {:>5}  {error}",
                        s.id, s.status, s.health, access, s.tools
                    )?;
                }
            }
            return Ok(());
        }
        Cmd::Register {
            url,
            id,
            description,
            timeout_ms,
            ..
        } => Action::Register {
            url,
            id,
            description,
            timeout_ms,
        },
        Cmd::Refresh {
            server, approve, ..
        } => match (server, approve) {
            (Some(server), true) => Action::ApproveScopes { server },
            (server, _) => Action::Refresh { server },
        },
        Cmd::Status { server, status, .. } => Action::Status {
            server,
            status: match status {
                StatusArg::Active => "active",
                StatusArg::Disabled => "disabled",
                StatusArg::Quarantined => "quarantined",
            },
        },
        Cmd::Access { server, access, .. } => Action::Access {
            server,
            access: match access {
                AccessArg::ReadOnly => "read_only",
                AccessArg::ReadWrite => "read_write",
            },
        },
        Cmd::Timeout { server, ms, .. } => Action::Timeout {
            server,
            timeout_ms: ms,
        },
        Cmd::Classify {
            server,
            tool,
            class,
            ..
        } => Action::Classify {
            server,
            tool,
            class: match class {
                ClassArg::Read => Some("read"),
                ClassArg::Write => Some("write"),
                ClassArg::Destructive => Some("destructive"),
                ClassArg::Clear => None,
            },
        },
        Cmd::Unregister { server, yes, .. } => {
            if !yes {
                bail!(
                    "unregistering {server} drops its tools, classifications and scope claims; pass --yes to confirm"
                );
            }
            Action::Delete { server }
        }
        Cmd::SetUrl {
            server, url, yes, ..
        } => {
            if !url.starts_with("https://") || url.len() <= 8 {
                bail!("the URL must start with https://");
            }
            // The composite needs what an administrator set, to put it back.
            let list = session.list_servers(true, true).await?;
            let current = list
                .servers
                .into_iter()
                .find(|s| s.id == server)
                .ok_or_else(|| anyhow!("no server {server:?} on {gateway}"))?;
            if current.url == url {
                bail!("{server} is already at {url}");
            }
            if !yes {
                bail!(
                    "moving {server} from {} to {url} unregisters and re-registers it; pass --yes to confirm",
                    current.url
                );
            }
            Action::ChangeUrl {
                server: Box::new(current),
                url,
            }
        }
        Cmd::Tui { .. } => unreachable!("handled above"),
    };
    writeln!(err, "{}...", action.describe())?;
    let outcome = actions::run(&session, action).await?;
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&outcome.to_json())?)?;
    } else {
        writeln!(out, "{}", outcome.message)?;
    }
    Ok(())
}

/// One session per configured gateway, keyed by gateway id, for the other
/// modules (fleet status reads the registry through these).
pub(crate) fn sessions(inst: &Instance) -> Result<Vec<(String, Arc<Session>)>> {
    let env = Env::from_process(false);
    inst.config
        .gateways
        .iter()
        .map(|g| Ok((g.id.clone(), Arc::new(env.session(inst, g)?))))
        .collect()
}

/// The TUI (default when no subcommand is given). `demo` runs it against an
/// in-process fake gateway and needs no instance.
pub async fn tui(ctx: &Ctx, demo: bool) -> Result<()> {
    if demo {
        return tui_demo().await;
    }
    let inst = ctx.instance()?;
    let env = Env::from_process(false);
    if env.token_file.is_none() && !inst.config.gateways.is_empty() {
        // The stored sign-in is read before the screen opens; the first run
        // also copies an mcpgw-manager sign-in, which macOS asks to allow.
        eprintln!(
            "Reading the gateway sign-in from the Keychain (macOS may ask to allow access)..."
        );
    }
    let sessions = inst
        .config
        .gateways
        .iter()
        .map(|g| env.session(&inst, g).map(Arc::new))
        .collect::<Result<Vec<_>>>()?;
    let gateway_module = GatewayModule::new(sessions.clone(), env.opener.clone());

    // The fleet's probes read the registry through the same sessions, and
    // pattern conformance through the instance's pack.
    let ids: Vec<String> = inst.config.gateways.iter().map(|g| g.id.clone()).collect();
    let registry = crate::wiring::GatewayRegistry::new(ids.into_iter().zip(sessions).collect());
    let checker = crate::wiring::PatternChecker::from_instance(&inst)?;
    let providers = studio_fleet::Providers::new(
        (!inst.config.gateways.is_empty())
            .then(|| Box::new(registry) as Box<dyn studio_fleet::GatewaySource>),
        checker.map(|c| Box::new(c) as Box<dyn studio_fleet::RepoChecker>),
    );

    let title = inst.display_name().to_string();
    let inst = Arc::new(inst);
    let modules = studio_tui::Modules {
        fleet: Some(Box::new(studio_tui::fleet::FleetModule::new(
            inst.clone(),
            providers,
        ))),
        pattern: Some(Box::new(studio_tui::pattern::PatternModule::new(
            inst.clone(),
        ))),
        gateway: Some(Box::new(gateway_module)),
        marketplaces: Some(Box::new(studio_tui::marketplace::MarketplaceModule::new(
            inst,
        ))),
    };
    let (app, rx) = studio_tui::studio_shell(title, modules, None);
    studio_tui::run(app, rx).await
}

/// The TUI against an in-process fake gateway, signed in as a demo admin. Its
/// servers persist in the cache directory, so the demo keeps its changes.
pub async fn tui_demo() -> Result<()> {
    let cache = studio_core::instance::cache_home();
    let demo = studio_tui::demo::Demo::start(
        Some(cache.join("demo-gateway.json")),
        cache.join("demo-clients.json"),
    )
    .await?;
    let (app, rx) = studio_tui::studio_app("Demo (fake gateway)", demo.module());
    studio_tui::run(app, rx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use studio_tui::fake::{FakeGateway, Options};

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: Cmd,
    }

    /// An instance directory with one `[[gateway]]` per fake, a token file and
    /// a cache directory, removed on drop.
    struct Fixture {
        dir: PathBuf,
        fakes: Vec<FakeGateway>,
    }

    impl Fixture {
        async fn new(gateways: &[(&str, Options)]) -> Fixture {
            let dir = std::env::temp_dir().join(format!(
                "studio-cli-gateway-{}-{}",
                std::process::id(),
                studio_gateway::util::random_token(6)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let mut toml = "[instance]\nname = \"acme\"\ndisplay_name = \"Acme\"\n".to_string();
            let mut fakes = Vec::new();
            for (id, opts) in gateways {
                let fake = FakeGateway::start(opts.clone()).await.unwrap();
                toml.push_str(&format!(
                    "\n[[gateway]]\nid = \"{id}\"\nurl = \"{}\"\nrefresh_seconds = 0\n",
                    fake.base
                ));
                fakes.push(fake);
            }
            std::fs::write(dir.join("studio.toml"), toml).unwrap();
            Fixture { dir, fakes }
        }

        fn ctx(&self) -> Ctx {
            Ctx {
                instance_arg: Some(self.dir.clone()),
            }
        }

        fn env(&self) -> Env {
            Env {
                token_file: Some(self.dir.join("tokens.json")),
                cache_dir: Some(self.dir.join("cache")),
                legacy_cache: None,
                opener: studio_tui::demo::auto_consent(),
            }
        }

        /// Runs `mcp-studio gateway <args>`: (result, stdout, stderr).
        async fn run(&self, args: &str) -> (Result<()>, String, String) {
            let cli =
                Cli::try_parse_from(std::iter::once("gateway").chain(args.split_whitespace()))
                    .unwrap_or_else(|e| panic!("{args}: {e}"));
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let result = execute(cli.cmd, &self.ctx(), &self.env(), &mut out, &mut err).await;
            (
                result,
                String::from_utf8(out).unwrap(),
                String::from_utf8(err).unwrap(),
            )
        }

        async fn ok(&self, args: &str) -> String {
            let (result, out, err) = self.run(args).await;
            if let Err(e) = result {
                panic!("{args}: {e:#}\n{err}");
            }
            out
        }

        async fn json(&self, args: &str) -> serde_json::Value {
            serde_json::from_str(&self.ok(args).await).unwrap()
        }

        fn calls(&self, entry: &str) -> usize {
            self.fakes[0].log().iter().filter(|l| *l == entry).count()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn login_whoami_servers_logout() {
        let f = Fixture::new(&[("main", Options::default())]).await;

        let (result, _, _) = f.run("whoami").await;
        let message = format!("{:#}", result.unwrap_err());
        assert!(message.contains("mcp-studio gateway login"), "{message}");

        let (result, _, err) = f.run("login").await;
        result.unwrap();
        assert!(err.contains("gateway:admin"), "{err}");
        assert!(f.dir.join("cache/clients.json").exists());

        let out = f.ok("whoami").await;
        assert!(out.contains("sub      00ufakeadmin"), "{out}");
        assert!(out.contains("access   admin"), "{out}");
        assert!(out.contains("gateway  main"), "{out}");
        assert_eq!(f.json("whoami --json").await["admin"], true);

        assert_eq!(f.json("servers --json --all").await["count"], 4);
        let out = f.ok("servers").await;
        assert!(out.contains("tasks") && out.contains("degraded"), "{out}");
        assert!(!out.contains("weather"), "{out}");

        f.ok("logout").await;
        assert!(!f.dir.join("tokens.json").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_admin_action_with_json() {
        let f = Fixture::new(&[("main", Options::default())]).await;
        f.ok("login").await;

        let r = f
            .json(
                "register https://newbie.mcp.example.org/mcp --id newbie --timeout-ms 60000 --json",
            )
            .await;
        assert_eq!(r["ok"], true);
        assert_eq!(r["result"]["registered"], "newbie");
        assert!(
            r["message"]
                .as_str()
                .unwrap()
                .contains("Registered newbie with 2 tools")
        );

        let r = f.json("timeout tasks 90000 --json").await;
        assert_eq!(r["result"]["now"], 90000);
        assert_eq!(
            f.ok("timeout tasks 20000").await.trim(),
            "tasks timeout 90000 -> 20000 ms"
        );

        let r = f.json("status scratch active --json").await;
        assert_eq!(
            (r["result"]["was"].as_str(), r["result"]["now"].as_str()),
            (Some("quarantined"), Some("active"))
        );

        let r = f.json("access tasks read-only --json").await;
        assert_eq!(r["result"]["now"], "read_only");

        let r = f.json("classify notes notes_tags read --json").await;
        assert_eq!(r["result"]["effective"], "read");
        let r = f.json("classify notes notes_tags clear --json").await;
        assert_eq!(r["result"]["source"], "default");

        let r = f.json("refresh scratch --json").await;
        assert!(
            r["message"]
                .as_str()
                .unwrap()
                .contains("scope drift on scratch"),
            "{r}"
        );
        let r = f.json("refresh scratch --approve --json").await;
        assert!(
            r["message"]
                .as_str()
                .unwrap()
                .contains("scratch now minted: files:write scratch:call"),
            "{r}"
        );
        assert!(f.ok("refresh").await.contains("Refreshed 5, 0 failed"));

        // The destructive two need --yes, and send nothing without it.
        let (result, _, _) = f.run("unregister weather").await;
        assert!(format!("{:#}", result.unwrap_err()).contains("--yes"));
        let (result, _, _) = f
            .run("set-url notes https://notes2.mcp.example.org/mcp")
            .await;
        assert!(format!("{:#}", result.unwrap_err()).contains("--yes"));
        assert_eq!(f.calls("mcp:tools/call:unregister_server"), 0);

        let r = f
            .json("set-url notes https://notes2.mcp.example.org/mcp --yes --json")
            .await;
        assert_eq!(r["result"]["now"], "https://notes2.mcp.example.org/mcp");
        assert!(
            r["message"]
                .as_str()
                .unwrap()
                .contains("could not restore notes_search=read"),
            "{r}"
        );

        let r = f.json("unregister weather --yes --json").await;
        assert_eq!(r["result"]["unregistered"], "weather");

        // A refusal is an error carrying the gateway's own words.
        let (result, _, _) = f.run("register https://refuse.example.com/mcp").await;
        assert!(
            format!("{:#}", result.unwrap_err()).contains("outside the gateway's trust perimeter")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gateway_picks_among_several() {
        let f = Fixture::new(&[
            ("main", Options::default()),
            (
                "staging",
                Options {
                    email: "other@example.org".into(),
                    ..Options::default()
                },
            ),
        ])
        .await;
        f.ok("login --gateway staging").await;
        assert!(
            f.ok("whoami --gateway staging")
                .await
                .contains("other@example.org")
        );
        // The token belongs to staging; main ignores it.
        let (result, _, _) = f.run("whoami --gateway main").await;
        assert!(format!("{:#}", result.unwrap_err()).contains("not signed in"));
        let (result, _, _) = f.run("whoami --gateway nope").await;
        let message = format!("{:#}", result.unwrap_err());
        assert!(
            message.contains("no [[gateway]] with id \"nope\"")
                && message.contains("main, staging"),
            "{message}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_gate_needs_a_fixture() {
        let f = Fixture::new(&[("main", Options::default())]).await;
        let (result, _, _) = f.run("gate").await;
        assert!(format!("{:#}", result.unwrap_err()).contains("fixture_url"));
    }

    #[test]
    fn approve_needs_a_server() {
        assert!(Cli::try_parse_from(["gateway", "refresh", "--approve"]).is_err());
        assert!(Cli::try_parse_from(["gateway", "status", "x", "paused"]).is_err());
        assert!(Cli::try_parse_from(["gateway", "access", "x", "read-only"]).is_ok());
    }
}
