//! What a server still needs, as a machine-readable plan.
//!
//! Studio never writes to the identity provider or Cloudflare. A plan names
//! each remaining change as exact shell commands, who should run them (the
//! okta-admin or cloudflare skill, mcp-studio itself, or the user), whether
//! the change is destructive, and the check id that turns green once it is
//! done — so `mcp-studio server verify` can tell which steps are satisfied.
//!
//! Plans are derived purely from checks: a fleet member's [`ServerStatus`]
//! ([`plan_for`]), or a scaffold that is not deployed yet, from its repo alone
//! ([`plan_for_repo`]). Secrets are referenced by `op://` path only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use studio_core::Instance;
use studio_core::check::{Check, Status, rollup};
use studio_core::config::repo_name;
use studio_fleet::wrangler::{Wrangler, parse_scopes};
use studio_fleet::{ServerInfo, ServerStatus};
use studio_pattern::scaffold::{PENDING_D1_ID, PENDING_KV_ID};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum System {
    Okta,
    Cloudflare,
    Github,
    Gateway,
    Marketplace,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Executor {
    #[serde(rename = "okta-admin skill")]
    OktaAdmin,
    #[serde(rename = "cloudflare skill")]
    Cloudflare,
    #[serde(rename = "mcp-studio")]
    Studio,
    #[serde(rename = "user")]
    User,
}

impl Executor {
    pub fn as_str(self) -> &'static str {
        match self {
            Executor::OktaAdmin => "okta-admin skill",
            Executor::Cloudflare => "cloudflare skill",
            Executor::Studio => "mcp-studio",
            Executor::User => "user",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StepState {
    /// Its check fails (or, for a new server, has never run).
    Todo,
    /// Its check passes.
    Done,
    /// Its check was skipped or not run, so nothing says either way.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// A fleet member, planned from live probes.
    Fleet,
    /// A repo not in the fleet yet, planned from its files.
    Repo,
}

/// Why a step is in the plan: the check that is not green.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Why {
    pub check: String,
    /// `None` when the check has not run (a new server).
    pub status: Option<Status>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub id: String,
    pub system: System,
    pub title: String,
    pub why: Why,
    /// Exact shell commands, in order. Empty when the step is an edit.
    pub commands: Vec<String>,
    pub executor: Executor,
    pub destructive: bool,
    /// The check id that turns green once this is done; `None` when no probe
    /// can see it (see `notes`).
    pub verify: Option<String>,
    pub state: StepState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub server: String,
    pub repo: String,
    pub url: Option<String>,
    pub local_dir: PathBuf,
    pub mode: Mode,
    /// What remains, in execution order.
    pub steps: Vec<Step>,
    /// Steps whose checks did not run, so they may or may not be needed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified: Vec<Why>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// An access-policy rule the server's scopes must be granted by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleRef {
    pub id: String,
    /// The policy holding it, when resolved; otherwise the plan looks it up.
    pub policy: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OktaCtx {
    pub tool: String,
    pub org: Option<String>,
    pub auth_server: String,
    pub rules: Vec<RuleRef>,
    pub interactive_client: Option<String>,
    pub issuer: String,
}

/// The instance facts a plan needs. Built from an [`Instance`]; rule
/// policies can be filled in afterwards from a live read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanContext {
    pub instance_root: PathBuf,
    pub okta: Option<OktaCtx>,
    pub github_account: Option<String>,
    pub topic: Option<String>,
    /// Gateway ids, the default first.
    pub gateways: Vec<String>,
    pub marketplaces: Vec<String>,
    pub op_vault: Option<String>,
    /// `<repos_dir>`, where a fleet repo's checkout is unless `local_path` says otherwise.
    pub repos_dir: PathBuf,
}

impl PlanContext {
    pub fn from_instance(inst: &Instance) -> Self {
        let c = &inst.config;
        let values = inst.pattern_values().unwrap_or_default();
        Self {
            instance_root: inst.root.clone(),
            okta: c.identity.okta.as_ref().map(|o| OktaCtx {
                tool: o
                    .admin_api
                    .as_ref()
                    .map(|a| a.tool.clone())
                    .unwrap_or_else(|| "okta-api".into()),
                org: o.admin_api.as_ref().and_then(|a| a.org.clone()),
                auth_server: o.authorization_server.clone(),
                rules: o
                    .policy_rules
                    .iter()
                    .map(|id| RuleRef {
                        id: id.clone(),
                        policy: None,
                        name: None,
                    })
                    .collect(),
                interactive_client: o.interactive_client_id.clone(),
                issuer: o.issuer(),
            }),
            github_account: c.github.account.clone(),
            topic: c
                .fleet
                .discover
                .as_ref()
                .and_then(|d| d.github_topic.clone()),
            gateways: c.gateways.iter().map(|g| g.id.clone()).collect(),
            marketplaces: c.marketplaces.iter().map(|m| m.id.clone()).collect(),
            op_vault: values.get("op_vault").cloned(),
            repos_dir: inst.path(&c.fleet.repos_dir),
        }
    }

    fn studio(&self, args: &str) -> String {
        format!(
            "mcp-studio --instance {} {args}",
            sh(&self.instance_root.display().to_string())
        )
    }

    fn gh(&self) -> String {
        match &self.github_account {
            Some(a) => format!("GH_TOKEN=\"$(gh auth token -u {})\" gh", sh(a)),
            None => "gh".into(),
        }
    }
}

/// Single-quote for a POSIX shell when needed.
pub fn sh(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@=+,%".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Checks Studio can make from the checkout alone: bindings still holding the
/// scaffold's pending ids.
pub fn local_checks(info: &ServerInfo) -> Vec<Check> {
    let mut out = Vec::new();
    let Ok(text) = std::fs::read_to_string(info.local_dir.join("wrangler.toml")) else {
        return out;
    };
    out.push(if text.contains(PENDING_KV_ID) {
        Check::fail("repo.kv", "OAUTH_KV still has the pending namespace id")
    } else {
        Check::pass("repo.kv", "OAUTH_KV has a namespace id")
    });
    if text.contains(PENDING_D1_ID) {
        out.push(Check::fail(
            "repo.d1",
            "the D1 binding still has the pending database id",
        ));
    } else if text.contains("[[d1_databases]]") {
        out.push(Check::pass("repo.d1", "the D1 binding has a database id"));
    }
    out
}

/// Facts about a repo that is not (yet) a fleet member, from its files.
pub fn repo_info(inst: &Instance, repo: &str, dir: &Path) -> ServerInfo {
    let mut problems = Vec::new();
    let w = match std::fs::read_to_string(dir.join("wrangler.toml")) {
        Ok(t) => Wrangler::parse(&t).unwrap_or_else(|e| {
            problems.push(format!("wrangler.toml: {e}"));
            Wrangler::default()
        }),
        Err(_) => {
            problems.push("no wrangler.toml".into());
            Wrangler::default()
        }
    };
    let hosts: Vec<String> = w
        .top
        .routes
        .iter()
        .filter(|r| r.custom_domain)
        .filter_map(|r| r.host())
        .collect();
    let (url, url_source) = match w.top.vars.get("PUBLIC_MCP_URL").filter(|u| !u.is_empty()) {
        Some(u) => (Some(u.clone()), Some("PUBLIC_MCP_URL".to_string())),
        None => match hosts.first() {
            Some(h) => (Some(format!("https://{h}/mcp")), Some("route".into())),
            None => (None, None),
        },
    };
    let label = url.as_deref().and_then(studio_fleet::discover::first_label);
    let version = std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("version")?.as_str().map(String::from));
    ServerInfo {
        repo: repo.to_string(),
        name: repo_name(repo).to_string(),
        local_dir: dir.to_path_buf(),
        local_exists: dir.is_dir(),
        origins: Vec::new(),
        worker: w.top.name.clone(),
        hosts,
        url,
        url_source,
        scopes: w
            .top
            .vars
            .get("OKTA_M2M_SCOPE")
            .map(|s| parse_scopes(s))
            .unwrap_or_default(),
        version,
        gateway: inst.config.gateway(None).map(|g| g.id.clone()),
        marketplace_slug: label.clone().unwrap_or_else(|| repo_name(repo).to_string()),
        gateway_id: label,
        environments: Vec::new(),
        problems,
    }
}

/// The plan for a fleet member, from its probe results.
pub fn plan_for(status: &ServerStatus, ctx: &PlanContext) -> Plan {
    let mut checks = status.checks.clone();
    checks.extend(local_checks(&status.info));
    checks.push(Check::pass("fleet.member", "in the fleet"));
    finish(&status.info, &checks, Mode::Fleet, ctx)
}

/// The plan for a repo outside the fleet (typically a fresh scaffold): every
/// remote step is to do. `lint` is the pack's lint of the checkout.
pub fn plan_for_repo(info: &ServerInfo, lint: &[Check], ctx: &PlanContext) -> Plan {
    let mut checks = lint.to_vec();
    checks.extend(local_checks(info));
    checks.push(Check::fail(
        "fleet.member",
        "not in the instance's fleet yet",
    ));
    finish(info, &checks, Mode::Repo, ctx)
}

fn finish(info: &ServerInfo, checks: &[Check], mode: Mode, ctx: &PlanContext) -> Plan {
    let all = catalog(info, checks, mode, ctx);
    let mut notes: Vec<String> = info.problems.clone();
    if info.url.is_none() {
        notes.push(
            "no public URL: wrangler.toml has neither PUBLIC_MCP_URL nor a custom-domain route"
                .into(),
        );
    }
    Plan {
        server: info.name.clone(),
        repo: info.repo.clone(),
        url: info.url.clone(),
        local_dir: info.local_dir.clone(),
        mode,
        unverified: {
            let mut u: Vec<Why> = Vec::new();
            for s in all.iter().filter(|s| s.state == StepState::Unknown) {
                if !u.iter().any(|w| w.check == s.why.check) {
                    u.push(s.why.clone());
                }
            }
            u
        },
        steps: all
            .into_iter()
            .filter(|s| s.state == StepState::Todo)
            .collect(),
        notes,
    }
}

struct Lookup<'a> {
    checks: &'a [Check],
    mode: Mode,
}

impl Lookup<'_> {
    fn get(&self, ids: &[&str]) -> Vec<&Check> {
        self.checks
            .iter()
            .filter(|c| ids.contains(&c.id.as_str()))
            .collect()
    }

    /// The state the gate checks put a step in, and why.
    fn state(&self, ids: &[&str], new_why: &str) -> (StepState, Why) {
        let found = self.get(ids);
        if found.is_empty() {
            let why = Why {
                check: ids[0].to_string(),
                status: None,
                summary: match self.mode {
                    Mode::Repo => new_why.to_string(),
                    Mode::Fleet => "not probed in this run".to_string(),
                },
            };
            let st = match self.mode {
                Mode::Repo => StepState::Todo,
                Mode::Fleet => StepState::Unknown,
            };
            return (st, why);
        }
        let worst = rollup(found.iter().copied());
        let c = found
            .iter()
            .find(|c| c.status == worst)
            .copied()
            .unwrap_or(found[0]);
        let why = Why {
            check: c.id.clone(),
            status: Some(c.status),
            summary: c.summary.clone(),
        };
        let st = match worst {
            Status::Pass => StepState::Done,
            Status::Fail | Status::Warn => StepState::Todo,
            Status::Skip => StepState::Unknown,
        };
        (st, why)
    }
}

struct Draft {
    id: String,
    system: System,
    title: String,
    gates: Vec<String>,
    new_why: String,
    commands: Vec<String>,
    executor: Executor,
    destructive: bool,
    verify: Option<String>,
    depends: Vec<&'static str>,
    notes: Vec<String>,
}

impl Draft {
    fn new(id: &str, system: System, executor: Executor, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            system,
            title: title.into(),
            gates: Vec::new(),
            new_why: String::new(),
            commands: Vec::new(),
            executor,
            destructive: false,
            verify: None,
            depends: Vec::new(),
            notes: Vec::new(),
        }
    }
    fn gate(mut self, ids: &[&str], new_why: impl Into<String>) -> Self {
        self.gates = ids.iter().map(|s| s.to_string()).collect();
        self.verify.get_or_insert_with(|| ids[0].to_string());
        self.new_why = new_why.into();
        self
    }
    fn verify(mut self, v: Option<&str>) -> Self {
        self.verify = v.map(String::from);
        self
    }
    fn cmd(mut self, c: impl Into<String>) -> Self {
        self.commands.push(c.into());
        self
    }
    fn note(mut self, n: impl Into<String>) -> Self {
        self.notes.push(n.into());
        self
    }
    /// Steps whose being needed makes this one needed too when its own
    /// check could not run (no Worker → no build to check).
    fn needs(mut self, ids: &[&'static str]) -> Self {
        self.depends = ids.to_vec();
        self
    }
}

/// Every step a server can need, in execution order, each with its state.
pub fn catalog(info: &ServerInfo, checks: &[Check], mode: Mode, ctx: &PlanContext) -> Vec<Step> {
    let look = Lookup { checks, mode };
    let dir = sh(&info.local_dir.display().to_string());
    let worker = info.worker.clone().unwrap_or_else(|| info.name.clone());
    let host = info
        .url
        .as_deref()
        .and_then(studio_fleet::discover::host_of);
    let gid = info.gateway_id.clone().unwrap_or_else(|| info.name.clone());
    let mut drafts: Vec<Draft> = Vec::new();

    // ---- github
    drafts.push(
        Draft::new(
            "github.repo",
            System::Github,
            Executor::User,
            format!("Create the GitHub repo {} and push", info.repo),
        )
        .gate(&["github.repo"], "new server: no GitHub repo yet")
        .cmd(format!(
            "{} repo create {} --private --source {dir} --remote origin --push",
            ctx.gh(),
            sh(&info.repo)
        ))
        .note("Private by default; `mcp-studio server new --create-repo` does the same at scaffold time."),
    );
    if let Some(topic) = &ctx.topic {
        drafts.push(
            Draft::new(
                "github.topic",
                System::Github,
                Executor::User,
                format!("Add the fleet discovery topic {topic}"),
            )
            .gate(
                &["github.topic"],
                "new server: repo lacks the discovery topic",
            )
            .cmd(format!(
                "{} repo edit {} --add-topic {}",
                ctx.gh(),
                sh(&info.repo),
                sh(topic)
            ))
            .needs(&["github.repo"]),
        );
    }

    // ---- fleet membership and the conformance store
    let slug_line = format!("marketplace_slug = \"{}\"", info.marketplace_slug);
    let mut extra = slug_line.clone();
    if info.local_dir != ctx.repos_dir.join(&info.name) {
        extra.push_str(&format!("\nlocal_path = \"{}\"", info.local_dir.display()));
    }
    drafts.push(
        Draft::new(
            "local.fleet",
            System::Local,
            Executor::User,
            "Add the server to the instance's fleet",
        )
        .gate(&["fleet.member"], "not in the instance's fleet")
        .cmd(format!(
            "printf '\\n[[fleet.server]]\\nrepo = \"%s\"\\ngateway_id = \"%s\"\\n%s\\n' {} {} {} >> {}",
            sh(&info.repo),
            sh(&gid),
            sh(&extra),
            sh(&ctx.instance_root.join("studio.toml").display().to_string())
        ))
        .note("Commit the instance repo afterwards; topic discovery also finds it once the repo carries the topic and conformance.json."),
    );
    drafts.push(
        Draft::new(
            "local.bless",
            System::Local,
            Executor::Studio,
            "Bless the security files into the instance's conformance store",
        )
        .gate(
            &["pattern.drift", "pattern.hashes", "pattern.version"],
            "new server: no blessed diffs in the conformance store",
        )
        .cmd(ctx.studio(&format!("pattern status {}", sh(&info.name))))
        .cmd(ctx.studio(&format!("pattern bless {}", sh(&info.name))))
        .note("A bless accepts the current diff against the template: read `pattern status` first. Commit the store and the repo's conformance.json.")
        .needs(&["local.fleet"]),
    );

    // ---- okta
    if let Some(o) = &ctx.okta {
        let org = o
            .org
            .as_ref()
            .map(|g| format!(" --org {}", sh(g)))
            .unwrap_or_default();
        let api = |method: &str, path: &str, body: bool| {
            format!(
                "{} {method} {}{}{org}",
                o.tool,
                sh(path),
                if body { " @-" } else { "" }
            )
        };
        let as_path = format!("/api/v1/authorizationServers/{}", o.auth_server);
        let scopes = if info.scopes.is_empty() {
            vec![format!("{gid}:read"), format!("{gid}:write")]
        } else {
            info.scopes.clone()
        };
        let mut create = Draft::new(
            "okta.scopes.create",
            System::Okta,
            Executor::OktaAdmin,
            format!("Ensure the scopes {} exist on the authorization server", scopes.join(" ")),
        )
        .gate(&["okta.scopes"], "new server: its scopes are not on the authorization server")
        .note("Idempotent: each command creates the scope only when it is missing. Never a default scope.");
        for s in &scopes {
            create = create.cmd(format!(
                "{} | jq -e --arg n {s} 'any(.[]; .name == $n)' >/dev/null || jq -n --arg n {s} --arg d {} '{{name: $n, displayName: $d, description: $d, consent: \"IMPLICIT\", default: false, metadataPublish: \"ALL_CLIENTS\"}}' | {}",
                api("GET", &format!("{as_path}/scopes"), false) + " --all",
                sh(&format!("{s} on {}", info.name)),
                api("POST", &format!("{as_path}/scopes"), true),
                s = sh(s),
            ));
        }
        drafts.push(create);

        let failing = look
            .get(&["okta.scopes"])
            .iter()
            .find(|c| c.status == Status::Fail)
            .map(|c| c.summary.clone());
        let named: Vec<&RuleRef> = o
            .rules
            .iter()
            .filter(|r| match (&failing, &r.name) {
                (Some(f), Some(n)) => f.contains(&format!("{n} lacks")),
                _ => true,
            })
            .collect();
        let rules = if named.is_empty() {
            o.rules.iter().collect()
        } else {
            named
        };
        let add = serde_json::to_string(&scopes).unwrap_or_default();
        let mut grant = Draft::new(
            "okta.scopes.rules",
            System::Okta,
            Executor::OktaAdmin,
            format!(
                "Grant {} in the access-policy rule{} {}",
                scopes.join(" "),
                if rules.len() == 1 { "" } else { "s" },
                rules
                    .iter()
                    .map(|r| r.name.clone().unwrap_or_else(|| r.id.clone()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .gate(&["okta.scopes"], "new server: no policy rule grants its scopes")
        .note("Rule updates are full-replace PUTs: GET → jq → PUT keeps every other field. A rule matches only when EVERY requested scope is in it, so both the interactive and the M2M rule need them.");
        for r in &rules {
            let (pid, lookup) = match &r.policy {
                Some(p) => (p.clone(), None),
                None => (
                    "$P".to_string(),
                    Some(format!(
                        "P=$({} | jq -r '.[].id' | while read -r p; do {tool} GET \"{as_path}/policies/$p/rules\"{org} | jq -e --arg r {} 'any(.[]; .id == $r)' >/dev/null && echo \"$p\"; done | head -n1)",
                        api("GET", &format!("{as_path}/policies"), false),
                        sh(&r.id),
                        tool = o.tool,
                    )),
                ),
            };
            if let Some(l) = lookup {
                grant = grant.cmd(l);
            }
            let rule_path = format!("{as_path}/policies/{pid}/rules/{}", r.id);
            let quoted = if pid.starts_with('$') {
                format!("\"{rule_path}\"")
            } else {
                sh(&rule_path)
            };
            grant = grant.cmd(format!(
                "{tool} GET {quoted}{org} | jq --argjson add {} 'del(._links) | .conditions.scopes.include |= (. + $add | unique)' | {tool} PUT {quoted} @-{org}",
                sh(&add),
                tool = o.tool,
            ));
        }
        drafts.push(grant);

        if let (Some(client), Some(h)) = (&o.interactive_client, &host) {
            let app = format!("/api/v1/apps/{client}");
            drafts.push(
                Draft::new(
                    "okta.redirect_uri",
                    System::Okta,
                    Executor::OktaAdmin,
                    format!("Register https://{h}/callback on the shared interactive client"),
                )
                .gate(&["okta.redirect_uri"], "new server: its callback is not registered")
                .cmd(format!(
                    "{} | jq --arg u {} 'del(._links, ._embedded) | .settings.oauthClient.redirect_uris |= (. + [$u] | unique)' | {}",
                    api("GET", &app, false),
                    sh(&format!("https://{h}/callback")),
                    api("PUT", &app, true)
                ))
                .note("Add only; never remove a redirect URI as part of this.")
                ,
            );
        }
    }

    // ---- cloudflare
    if !look.get(&["repo.kv"]).is_empty() {
        drafts.push(
        Draft::new(
            "cf.kv",
            System::Cloudflare,
            Executor::Cloudflare,
            "Create the OAUTH_KV namespace and put its id in wrangler.toml",
        )
        .gate(&["repo.kv"], "wrangler.toml has no KV namespace id")
        .cmd(format!("cd {dir} && npx wrangler kv namespace create OAUTH_KV"))
        .note(format!(
            "Replace {PENDING_KV_ID} in wrangler.toml [[kv_namespaces]] with the printed id, then commit."
        )),
    );
    }
    if look
        .get(&["repo.d1"])
        .iter()
        .any(|c| c.status != Status::Pass)
    {
        let db = d1_name(&info.local_dir).unwrap_or_else(|| worker.clone());
        let mut d = Draft::new(
            "cf.d1",
            System::Cloudflare,
            Executor::Cloudflare,
            format!("Create the D1 database {db} and put its id in wrangler.toml"),
        )
        .gate(&["repo.d1"], "wrangler.toml has no D1 database id")
        .cmd(format!("cd {dir} && npx wrangler d1 create {}", sh(&db)))
        .note(format!(
            "Replace {PENDING_D1_ID} in wrangler.toml [[d1_databases]] with the printed uuid, then commit."
        ));
        if info.local_dir.join("migrations").is_dir() {
            d = d.cmd(format!(
                "cd {dir} && npx wrangler d1 migrations apply {} --remote",
                sh(&db)
            ));
        }
        drafts.push(d);
    }
    drafts.push(
        Draft::new(
            "cf.script",
            System::Cloudflare,
            Executor::Cloudflare,
            format!("First deploy of the Worker {worker}"),
        )
        .gate(&["cf.script"], "new server: no Worker deployed")
        .cmd(format!("cd {dir} && npm ci && npm run ci && npx wrangler deploy"))
        .note("Once only: later deploys come from Workers Builds on push. Deploying attaches the custom-domain route in wrangler.toml.")
        .needs(&["cf.kv", "cf.d1"]),
    );
    if let Some(vault) = &ctx.op_vault {
        let item = |field: &str| format!("op://{vault}/{worker}/{field}");
        let put = |field: &str| {
            format!(
                "cd {dir} && printf '%s' \"$(op read {})\" | npx wrangler secret put {field} --name {}",
                sh(&item(field)),
                sh(&worker)
            )
        };
        drafts.push(
            Draft::new(
                "cf.secrets",
                System::Cloudflare,
                Executor::Cloudflare,
                "Set the Worker secrets from 1Password",
            )
            .gate(&["cf.script"], "new server: no Worker, so no secrets")
            .verify(None)
            .cmd(format!(
                "op item get {w} --vault {v} >/dev/null 2>&1 || op item create --category='API Credential' --title={w} --vault {v}",
                w = sh(&worker),
                v = sh(vault)
            ))
            .cmd(format!(
                "op read {} >/dev/null 2>&1 || op item edit {} --vault {} --generate-password='letters,digits,48' 'REQUEST_STATE_KEY[concealed]='",
                sh(&item("REQUEST_STATE_KEY")),
                sh(&worker),
                sh(vault)
            ))
            .cmd(put("OKTA_CLIENT_SECRET"))
            .cmd(put("REQUEST_STATE_KEY"))
            .note(format!(
                "OKTA_CLIENT_SECRET is the shared interactive client's secret; copy it into {} first (Studio never reads secret values).",
                item("OKTA_CLIENT_SECRET")
            ))
            .note("No probe sees secrets: a real interactive login proves OKTA_CLIENT_SECRET, and the destructive tool's confirmation proves REQUEST_STATE_KEY.")
            .needs(&["cf.script"]),
        );
    }
    if !info.hosts.is_empty() {
        drafts.push(
            Draft::new(
                "cf.domain",
                System::Cloudflare,
                Executor::Cloudflare,
                format!("Attach the custom domain {} to {worker}", info.hosts.join(", ")),
            )
            .gate(&["cf.domain"], "new server: custom domain not attached")
            .cmd(format!("cd {dir} && npx wrangler deploy"))
            .note("wrangler attaches the custom-domain route (and its DNS record and certificate) on deploy.")
            .needs(&["cf.script"]),
        );
    }
    drafts.push(
        Draft::new(
            "cf.build",
            System::Cloudflare,
            Executor::Cloudflare,
            "Wire Workers Builds with `npm run ci` as the build gate",
        )
        .gate(&["cf.build"], "new server: no Workers Builds trigger")
        .cmd(format!(
            "{} api repos/{} --jq '{{repo_id: .id, owner_id: .owner.id, owner_login: .owner.login}}'",
            ctx.gh(),
            info.repo
        ))
        .note(format!("USER: grant the Cloudflare Workers Builds GitHub App access to {} (GitHub → Settings → Integrations).", info.repo))
        .note("Cloudflare API: PUT /accounts/{account}/builds/repos/connections with the ids above, then POST /accounts/{account}/builds/triggers with build_command \"npm run ci\", deploy_command \"npx wrangler deploy\", branch_includes [\"main\"] (or PATCH the auto-created production trigger's build_command).")
        .note("Trigger the first build and confirm build_outcome is success.")
        .needs(&["cf.script", "github.repo"]),
    );
    drafts.push(
        Draft::new(
            "cf.deployed",
            System::Cloudflare,
            Executor::User,
            "Deploy HEAD through Workers Builds",
        )
        .gate(&["cf.deployed"], "new server: nothing deployed from GitHub yet")
        .cmd(format!("git -C {dir} push origin HEAD"))
        .note("Workers Builds deploys on push to main; a failed `npm run ci` blocks the deploy silently — check cf.build.")
        .needs(&["cf.build"]),
    );

    // ---- gateway
    if let Some(gw) = info
        .gateway
        .clone()
        .or_else(|| ctx.gateways.first().cloned())
    {
        let pick = if ctx.gateways.first() == Some(&gw) {
            String::new()
        } else {
            format!(" --gateway {}", sh(&gw))
        };
        if let Some(url) = &info.url {
            drafts.push(
                Draft::new(
                    "gateway.registered",
                    System::Gateway,
                    Executor::Studio,
                    format!("Register {url} on gateway {gw} as {gid}"),
                )
                .gate(&["gateway.registered"], "new server: not registered on the gateway")
                .cmd(ctx.studio(&format!(
                    "gateway register {} --id {}{pick}",
                    sh(url),
                    sh(&gid)
                )))
                .note("Register the custom-domain URL, never workers.dev (a Worker cannot fetch another on the same account: error 1042).")
                .needs(&["cf.script"]),
            );
        }
        drafts.push(
            Draft::new(
                "gateway.refresh",
                System::Gateway,
                Executor::Studio,
                format!("Re-read {gid}'s tools and scopes on the gateway"),
            )
            .gate(
                &["gateway.status", "gateway.health"],
                "new server: no gateway health yet",
            )
            .cmd(ctx.studio(&format!("gateway refresh {}{pick}", sh(&gid))))
            .needs(&["gateway.registered"]),
        );
    }

    // ---- marketplaces
    for (i, mp) in ctx.marketplaces.iter().enumerate() {
        let id = format!("marketplace.{mp}");
        let found = look.get(&[id.as_str()]);
        let warn = found.iter().any(|c| c.status == Status::Warn);
        // A new server is offered to the first marketplace only; a fleet
        // server is only steered where it is already (partly) listed.
        if mode == Mode::Repo && i > 0 {
            continue;
        }
        let cmd = if warn {
            ctx.studio(&format!(
                "marketplace update {} {} --version {}",
                sh(mp),
                sh(&info.marketplace_slug),
                sh(info.version.as_deref().unwrap_or("0.1.0"))
            ))
        } else {
            ctx.studio(&format!(
                "marketplace add {} {} --from-repo {dir}",
                sh(mp),
                sh(&info.marketplace_slug)
            ))
        };
        drafts.push(
            Draft::new(
                &format!("marketplace.{mp}"),
                System::Marketplace,
                Executor::Studio,
                if warn {
                    format!(
                        "Bring {}'s listing in {mp} up to date",
                        info.marketplace_slug
                    )
                } else {
                    format!("List {} in the {mp} marketplace", info.marketplace_slug)
                },
            )
            .gate(&[id.as_str()], format!("new server: not listed in {mp}"))
            .cmd(cmd)
            .note("Add --dry-run to preview; --publish pushes (or opens a PR)."),
        );
    }

    // ---- resolve states, in order
    let mut steps: Vec<Step> = Vec::new();
    let gated: Vec<String> = drafts.iter().flat_map(|d| d.gates.clone()).collect();
    for d in drafts {
        let gates: Vec<&str> = d.gates.iter().map(String::as_str).collect();
        let (mut state, why) = look.state(&gates, &d.new_why);
        if state == StepState::Unknown
            && d.depends.iter().any(|dep| {
                steps
                    .iter()
                    .any(|s| s.id == *dep && s.state == StepState::Todo)
            })
        {
            state = StepState::Todo;
        }
        steps.push(Step {
            id: d.id,
            system: d.system,
            title: d.title,
            why,
            commands: d.commands,
            executor: d.executor,
            destructive: d.destructive,
            verify: d.verify,
            state,
            notes: d.notes,
        });
    }

    // ---- anything failing that no step covers
    let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
    for c in checks {
        if !matches!(c.status, Status::Fail | Status::Warn)
            || gated.contains(&c.id)
            || seen.insert(c.id.as_str(), ()).is_some()
        {
            continue;
        }
        steps.push(generic(info, c, mode, ctx));
    }
    steps
}

fn generic(info: &ServerInfo, c: &Check, mode: Mode, ctx: &PlanContext) -> Step {
    let prefix = c.id.split('.').next().unwrap_or("");
    let system = match prefix {
        "okta" => System::Okta,
        "cf" | "http" => System::Cloudflare,
        "github" => System::Github,
        "gateway" => System::Gateway,
        "marketplace" => System::Marketplace,
        _ => System::Local,
    };
    let mut commands = Vec::new();
    let mut notes = Vec::new();
    match prefix {
        "pattern" if mode == Mode::Fleet => {
            commands.push(ctx.studio(&format!("pattern lint {}", sh(&info.name))))
        }
        "http" => {
            if let Some(u) = &info.url {
                commands.push(format!("curl -si -X POST {} | head -n 20", sh(u)));
            }
        }
        _ => {}
    }
    if let Some(e) = &c.evidence {
        notes.push(e.clone());
    }
    Step {
        id: c.id.clone(),
        system,
        title: format!("Resolve {}: {}", c.id, c.summary),
        why: Why {
            check: c.id.clone(),
            status: Some(c.status),
            summary: c.summary.clone(),
        },
        commands,
        executor: Executor::User,
        destructive: false,
        verify: Some(c.id.clone()),
        state: StepState::Todo,
        notes,
    }
}

fn d1_name(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("wrangler.toml")).ok()?;
    text.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("database_name"))
        .and_then(|r| r.split('"').nth(1))
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use studio_fleet::Column;

    fn example() -> Instance {
        Instance::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/instance"))
            .unwrap()
    }

    fn info(inst: &Instance) -> ServerInfo {
        ServerInfo {
            repo: "acme/weather-mcp-worker".into(),
            name: "weather-mcp-worker".into(),
            local_dir: PathBuf::from("/nonexistent/weather-mcp-worker"),
            local_exists: false,
            origins: vec!["include".into()],
            worker: Some("weather-mcp-worker".into()),
            hosts: vec!["weather.mcp.acme.example".into()],
            url: Some("https://weather.mcp.acme.example/mcp".into()),
            url_source: Some("PUBLIC_MCP_URL".into()),
            scopes: vec!["weather:read".into(), "weather:write".into()],
            version: Some("0.1.0".into()),
            gateway: inst.config.gateway(None).map(|g| g.id.clone()),
            gateway_id: Some("weather".into()),
            marketplace_slug: "weather".into(),
            environments: vec![],
            problems: vec![],
        }
    }

    fn status(inst: &Instance, checks: Vec<Check>) -> ServerStatus {
        let i = info(inst);
        ServerStatus {
            repo: i.repo.clone(),
            name: i.name.clone(),
            url: i.url.clone(),
            gateway_id: i.gateway_id.clone(),
            local_dir: i.local_dir.clone(),
            checks,
            columns: Vec::<Column>::new(),
            facts: BTreeMap::new(),
            info: i,
        }
    }

    fn green() -> Vec<Check> {
        [
            "github.repo",
            "github.topic",
            "pattern.version",
            "pattern.hashes",
            "pattern.drift",
            "okta.scopes",
            "okta.redirect_uri",
            "cf.script",
            "cf.domain",
            "cf.build",
            "cf.deployed",
            "gateway.registered",
            "gateway.status",
            "gateway.health",
            "marketplace.acme",
        ]
        .into_iter()
        .map(|id| Check::pass(id, "ok"))
        .collect()
    }

    fn set(checks: &mut [Check], id: &str, c: Check) {
        let at = checks.iter().position(|x| x.id == id).unwrap();
        checks[at] = c;
    }

    #[test]
    fn a_green_server_has_an_empty_plan() {
        let inst = example();
        let ctx = PlanContext::from_instance(&inst);
        let p = plan_for(&status(&inst, green()), &ctx);
        assert_eq!(p.mode, Mode::Fleet);
        assert!(p.steps.is_empty(), "{:#?}", p.steps);
        assert!(p.unverified.is_empty(), "{:#?}", p.unverified);
    }

    #[test]
    fn a_rule_lacking_a_scope_plans_okta_writes_for_that_rule() {
        let inst = example();
        let mut ctx = PlanContext::from_instance(&inst);
        let o = ctx.okta.as_mut().unwrap();
        o.rules[0].policy = Some("00pEXAMPLEPOLICY1".into());
        o.rules[0].name = Some("Allow Authorization Code".into());
        o.rules[1].name = Some("Bearer Token Access Rule".into());
        let mut checks = green();
        set(
            &mut checks,
            "okta.scopes",
            Check::fail(
                "okta.scopes",
                "Allow Authorization Code lacks weather:write",
            ),
        );
        let p = plan_for(&status(&inst, checks), &ctx);
        let ids: Vec<&str> = p.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["okta.scopes.create", "okta.scopes.rules"]);
        let rules = &p.steps[1];
        assert_eq!(rules.executor, Executor::OktaAdmin);
        assert_eq!(rules.verify.as_deref(), Some("okta.scopes"));
        assert_eq!(rules.why.status, Some(Status::Fail));
        assert!(rules.why.summary.contains("lacks weather:write"));
        assert!(!rules.destructive);
        // Only the named rule, with its resolved policy: one exact PUT.
        assert_eq!(rules.commands.len(), 1, "{:#?}", rules.commands);
        assert_eq!(
            rules.commands[0],
            "okta-api GET /api/v1/authorizationServers/default/policies/00pEXAMPLEPOLICY1/rules/0prEXAMPLERULE1 --org acme \
             | jq --argjson add '[\"weather:read\",\"weather:write\"]' 'del(._links) | .conditions.scopes.include |= (. + $add | unique)' \
             | okta-api PUT /api/v1/authorizationServers/default/policies/00pEXAMPLEPOLICY1/rules/0prEXAMPLERULE1 @- --org acme"
                .replace(" \\\n             ", " ")
        );
        let create = &p.steps[0];
        assert_eq!(create.commands.len(), 2);
        assert!(create.commands[1].contains("--arg n weather:write"));
        assert!(
            create.commands[1].contains(
                "okta-api POST /api/v1/authorizationServers/default/scopes @- --org acme"
            )
        );
    }

    #[test]
    fn an_unresolved_policy_is_looked_up_in_the_command() {
        let inst = example();
        let ctx = PlanContext::from_instance(&inst);
        let mut checks = green();
        set(
            &mut checks,
            "okta.scopes",
            Check::fail("okta.scopes", "x lacks weather:read"),
        );
        let p = plan_for(&status(&inst, checks), &ctx);
        let rules = p
            .steps
            .iter()
            .find(|s| s.id == "okta.scopes.rules")
            .unwrap();
        // both rules (no names known), each a lookup + a PUT
        assert_eq!(rules.commands.len(), 4);
        assert!(rules.commands[0].starts_with(
            "P=$(okta-api GET /api/v1/authorizationServers/default/policies --org acme"
        ));
        assert!(rules.commands[1].contains(
            "\"/api/v1/authorizationServers/default/policies/$P/rules/0prEXAMPLERULE1\""
        ));
    }

    #[test]
    fn skipped_checks_are_unverified_not_steps() {
        let inst = example();
        let ctx = PlanContext::from_instance(&inst);
        let mut checks = green();
        set(
            &mut checks,
            "gateway.registered",
            Check::skip("gateway.registered", "not signed in"),
        );
        let p = plan_for(&status(&inst, checks), &ctx);
        assert!(p.steps.is_empty(), "{:#?}", p.steps);
        assert_eq!(p.unverified.len(), 1);
        assert_eq!(p.unverified[0].check, "gateway.registered");
    }

    #[test]
    fn a_missing_worker_pulls_in_its_dependents() {
        let inst = example();
        let ctx = PlanContext::from_instance(&inst);
        let mut checks = green();
        set(
            &mut checks,
            "cf.script",
            Check::fail("cf.script", "no Worker named weather-mcp-worker"),
        );
        set(
            &mut checks,
            "cf.build",
            Check::skip("cf.build", "no Worker"),
        );
        let p = plan_for(&status(&inst, checks), &ctx);
        let ids: Vec<&str> = p.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["cf.script", "cf.secrets", "cf.build"]);
        let secrets = &p.steps[1];
        assert_eq!(secrets.verify, None);
        assert!(
            secrets
                .commands
                .iter()
                .all(|c| !c.contains("op read") || c.contains("op://Acme/weather-mcp-worker/"))
        );
        assert!(secrets.commands[2].starts_with("cd /nonexistent/weather-mcp-worker && printf '%s' \"$(op read op://Acme/weather-mcp-worker/OKTA_CLIENT_SECRET)\" | npx wrangler secret put OKTA_CLIENT_SECRET --name weather-mcp-worker"));
    }

    #[test]
    fn uncovered_failures_become_generic_steps() {
        let inst = example();
        let ctx = PlanContext::from_instance(&inst);
        let mut checks = green();
        checks.push(Check::fail("http.unauth_401", "unreachable").with_evidence("dns"));
        checks.push(Check::warn("pattern.pins", "hono is 4.0.0"));
        let p = plan_for(&status(&inst, checks), &ctx);
        let ids: Vec<&str> = p.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["http.unauth_401", "pattern.pins"]);
        assert_eq!(p.steps[0].system, System::Cloudflare);
        assert_eq!(p.steps[0].notes, vec!["dns"]);
        assert!(p.steps[1].commands[0].ends_with("pattern lint weather-mcp-worker"));
    }

    #[test]
    fn a_new_repo_plans_everything_in_order() {
        let inst = example();
        let ctx = PlanContext::from_instance(&inst);
        let p = plan_for_repo(&info(&inst), &[], &ctx);
        assert_eq!(p.mode, Mode::Repo);
        let ids: Vec<&str> = p.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "github.repo",
                "github.topic",
                "local.fleet",
                "local.bless",
                "okta.scopes.create",
                "okta.scopes.rules",
                "okta.redirect_uri",
                "cf.script",
                "cf.secrets",
                "cf.domain",
                "cf.build",
                "cf.deployed",
                "gateway.registered",
                "gateway.refresh",
                "marketplace.acme",
            ]
        );
        let reg = p
            .steps
            .iter()
            .find(|s| s.id == "gateway.registered")
            .unwrap();
        assert!(
            reg.commands[0]
                .ends_with("gateway register https://weather.mcp.acme.example/mcp --id weather")
        );
        let topic = p.steps.iter().find(|s| s.id == "github.topic").unwrap();
        assert_eq!(
            topic.commands[0],
            "GH_TOKEN=\"$(gh auth token -u acme-bot)\" gh repo edit acme/weather-mcp-worker --add-topic mcp-server"
        );
        let redirect = p
            .steps
            .iter()
            .find(|s| s.id == "okta.redirect_uri")
            .unwrap();
        assert!(redirect.commands[0].contains("--arg u https://weather.mcp.acme.example/callback"));
        assert!(
            redirect.commands[0]
                .contains("okta-api PUT /api/v1/apps/0oaEXAMPLEINTERACTIVE @- --org acme")
        );
        let mp = p.steps.iter().find(|s| s.id == "marketplace.acme").unwrap();
        assert!(
            mp.commands[0].contains(
                "marketplace add acme weather --from-repo /nonexistent/weather-mcp-worker"
            )
        );
        assert!(p.steps.iter().all(|s| s.state == StepState::Todo));
        // JSON shape the skills read
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["steps"][4]["executor"], "okta-admin skill");
        assert_eq!(v["steps"][4]["system"], "okta");
        assert_eq!(v["steps"][7]["executor"], "cloudflare skill");
    }

    #[test]
    fn pending_binding_ids_are_steps() {
        let inst = example();
        let ctx = PlanContext::from_instance(&inst);
        let dir = std::env::temp_dir().join(format!("studio-plan-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("wrangler.toml"),
            format!(
                "name = \"weather-mcp-worker\"\n[[d1_databases]]\nbinding = \"DB\"\ndatabase_name = \"weather-db\"\ndatabase_id = \"{PENDING_D1_ID}\"\n[[kv_namespaces]]\nbinding = \"OAUTH_KV\"\nid = \"{PENDING_KV_ID}\"\n[vars]\nPUBLIC_MCP_URL = \"https://weather.mcp.acme.example/mcp\"\nOKTA_M2M_SCOPE = \"weather:read weather:write\"\n"
            ),
        )
        .unwrap();
        let i = repo_info(&inst, "acme/weather-mcp-worker", &dir);
        assert_eq!(i.gateway_id.as_deref(), Some("weather"));
        assert_eq!(i.scopes, vec!["weather:read", "weather:write"]);
        let mut checks = green();
        checks.retain(|c| !c.id.starts_with("cf.") || c.id == "cf.domain");
        let mut st = status(&inst, checks);
        st.info = i;
        let p = plan_for(&st, &ctx);
        std::fs::remove_dir_all(&dir).ok();
        let kv = p.steps.iter().find(|s| s.id == "cf.kv").unwrap();
        assert!(kv.commands[0].ends_with("npx wrangler kv namespace create OAUTH_KV"));
        let d1 = p.steps.iter().find(|s| s.id == "cf.d1").unwrap();
        assert!(d1.commands[0].ends_with("npx wrangler d1 create weather-db"));
        // the Worker's state is unknown, but its bindings are missing: deploy is planned
        assert!(p.steps.iter().any(|s| s.id == "cf.script"));
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(sh("acme/weather"), "acme/weather");
        assert_eq!(sh("a b"), "'a b'");
        assert_eq!(sh("it's"), "'it'\\''s'");
        assert_eq!(sh(""), "''");
    }
}
