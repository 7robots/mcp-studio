//! `mcp-studio server …`: scaffold a new server, and plan or verify what a
//! server still needs. Scaffolding lives in `studio-pattern`; plans are pure
//! functions in `crate::plan` over fleet probe results.
//!
//! Naming: a server's slug (`weather`) is its scope prefix, host label and
//! gateway id; its repo and Worker are `<slug>-mcp-worker` unless `--worker`
//! says otherwise, cloned at `<repos_dir>/<repo>` like every fleet repo.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use serde::Serialize;
use studio_core::Instance;
use studio_core::check::Status;
use studio_fleet::{Endpoints, GatewaySource, Providers, RepoChecker, ServerStatus, StatusOptions};
use studio_pattern::scaffold::WORKER_SUFFIX;
use studio_pattern::{Pack, ScaffoldSpec, ScaffoldSummary};

use crate::Ctx;
use crate::plan::{self, Plan, PlanContext, Step, StepState, sh};

#[derive(Subcommand)]
pub enum Cmd {
    /// Scaffold a new server from the pattern pack (git init + first commit).
    New(NewArgs),
    /// What a server still needs, as exact commands for the okta-admin and
    /// cloudflare skills, mcp-studio, or you. Writes nothing.
    Plan {
        /// A fleet server (repo name, owner/repo or slug), or a repo directory.
        target: String,
        #[arg(long)]
        json: bool,
        /// Ignore the cached fleet status.
        #[arg(long)]
        refresh: bool,
    },
    /// Re-probe a fleet server and show which plan steps are now satisfied.
    Verify {
        /// A fleet server (repo name, owner/repo or slug).
        server: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
pub struct NewArgs {
    /// Lowercase slug: scope prefix, host label (<slug>.<domain_suffix>) and gateway id.
    slug: String,
    /// Display name (consent page); default: the slug in title case.
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    description: Option<String>,
    /// A scope, twice: the read scope then the write scope (default <slug>:read, <slug>:write).
    #[arg(long = "scope", value_name = "SCOPE")]
    scopes: Vec<String>,
    /// Keep the template's D1 binding.
    #[arg(long)]
    d1: bool,
    /// Repo / Worker name (default <slug>-mcp-worker).
    #[arg(long)]
    worker: Option<String>,
    /// Target directory (default <repos_dir>/<worker>); must be absent or empty.
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Create the GitHub repo with gh, push, and add the discovery topic.
    #[arg(long)]
    create_repo: bool,
    /// With --create-repo: make it public.
    #[arg(long, requires = "create_repo")]
    public: bool,
    #[arg(long)]
    json: bool,
}

pub async fn run(cmd: Cmd, ctx: &Ctx) -> Result<()> {
    let inst = ctx.instance()?;
    match cmd {
        Cmd::New(a) => new(&inst, a),
        Cmd::Plan {
            target,
            json,
            refresh,
        } => {
            let p = plan_target(&inst, &target, refresh).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&p)?);
            } else {
                print_plan(&p);
            }
            Ok(())
        }
        Cmd::Verify { server, json } => verify(&inst, &server, json).await,
    }
}

// ---------------------------------------------------------------- new

#[derive(Serialize)]
struct NewOutcome {
    #[serde(flatten)]
    summary: ScaffoldSummary,
    repo: String,
    dir: PathBuf,
    committed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    github_url: Option<String>,
    next: Vec<String>,
}

fn github_org(inst: &Instance) -> Result<String> {
    if let Some(o) = inst.config.github.orgs.first() {
        return Ok(o.clone());
    }
    inst.pattern_values()?
        .get("github_org")
        .cloned()
        .ok_or_else(|| anyhow!("no github.orgs in studio.toml and no github_org pattern value"))
}

fn load_pack(inst: &Instance) -> Result<Pack> {
    let dir = inst
        .pattern_dir()
        .ok_or_else(|| anyhow!("{}: no [pattern] section", inst.root.display()))?;
    Pack::load(&dir).with_context(|| format!("loading pattern pack {}", dir.display()))
}

fn new(inst: &Instance, a: NewArgs) -> Result<()> {
    let pack = load_pack(inst)?;
    let values = inst.pattern_values()?;
    let mut spec = ScaffoldSpec::new(&a.slug);
    if let Some(n) = a.name {
        spec.display_name = n;
    }
    spec.description = a
        .description
        .unwrap_or_else(|| format!("MCP server for {}", spec.display_name));
    spec.scopes = a.scopes;
    spec.with_d1 = a.d1;
    spec.worker = a.worker;
    let repo = format!("{}/{}", github_org(inst)?, spec.worker_name());
    let dir = a.dir.unwrap_or_else(|| inst.repo_dir(&repo));

    let summary = studio_pattern::scaffold(&pack, &values, &spec, &dir)?;
    let committed = git_commit(
        &dir,
        &format!(
            "feat: scaffold {} from the {} pattern ({})",
            summary.worker, summary.pack, summary.version
        ),
    );
    let mut github_url = None;
    if a.create_repo {
        github_url = Some(create_repo(inst, &repo, &dir, a.public)?);
    }
    let dir_arg = sh(&dir.display().to_string());
    let next = vec![
        format!("mcp-studio server plan {dir_arg}"),
        format!(
            "mcp-studio server plan {}   (once it is in the fleet)",
            summary.worker
        ),
    ];
    let out = NewOutcome {
        summary,
        repo,
        dir,
        committed: committed.is_ok(),
        github_url,
        next,
    };
    if a.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    let s = &out.summary;
    println!(
        "scaffolded {} ({} files) from {} {} into {}",
        s.worker,
        s.files,
        s.pack,
        s.version,
        out.dir.display()
    );
    println!("  url     {}", s.url);
    println!("  scopes  {}", s.scopes.join(" "));
    if let Some(db) = &s.d1_database {
        println!("  d1      {db}");
    }
    match &committed {
        Ok(()) => println!("  git     initialised, first commit on main"),
        Err(e) => println!("  git     not committed: {e}"),
    }
    if let Some(u) = &out.github_url {
        println!("  github  {u}");
    }
    println!("\nStill yours:");
    for f in &s.follow_ups {
        println!("  - {f}");
    }
    println!("\nNext: what it needs before it serves:\n  {}", out.next[0]);
    Ok(())
}

fn git_commit(dir: &Path, message: &str) -> Result<()> {
    use studio_core::exec::run;
    run("git", &["init", "-q", "-b", "main"], Some(dir))?;
    run("git", &["add", "-A"], Some(dir))?;
    run("git", &["commit", "-q", "-m", message], Some(dir))?;
    Ok(())
}

/// `gh repo create` as the instance's account, push, and add the discovery
/// topic. Private unless `public`.
fn create_repo(inst: &Instance, repo: &str, dir: &Path, public: bool) -> Result<String> {
    let token = studio_core::exec::github_token(inst.config.github.account.as_deref())?;
    let gh = |args: &[&str]| -> Result<()> {
        let out = std::process::Command::new("gh")
            .args(args)
            .env("GH_TOKEN", token.expose())
            .output()
            .map_err(|_| anyhow!("`gh` is not installed or not on PATH"))?;
        if !out.status.success() {
            bail!(
                "gh {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    };
    let visibility = if public { "--public" } else { "--private" };
    gh(&["repo", "create", repo, visibility])?;
    let url = format!("https://github.com/{repo}.git");
    studio_core::exec::run("git", &["remote", "add", "origin", &url], Some(dir))?;
    studio_core::exec::run("git", &["push", "-q", "-u", "origin", "main"], Some(dir))?;
    if let Some(topic) = inst
        .config
        .fleet
        .discover
        .as_ref()
        .and_then(|d| d.github_topic.as_deref())
    {
        gh(&["repo", "edit", repo, "--add-topic", topic])?;
    }
    Ok(format!("https://github.com/{repo}"))
}

// ---------------------------------------------------------------- plan

fn providers(inst: &Instance) -> Result<Providers> {
    let checker = crate::wiring::PatternChecker::from_instance(inst)?
        .map(|c| Box::new(c) as Box<dyn RepoChecker>);
    let gateway = (!inst.config.gateways.is_empty())
        .then(|| {
            crate::cmd_gateway::sessions(inst)
                .map(|s| Box::new(crate::wiring::GatewayRegistry::new(s)) as Box<dyn GatewaySource>)
        })
        .transpose()?;
    Ok(Providers::new(gateway, checker))
}

/// The fleet member `name` names (repo name, owner/repo, or slug), probed.
async fn probe_one(inst: &Instance, name: &str, refresh: bool) -> Result<Option<ServerStatus>> {
    let providers = providers(inst)?;
    let mut candidates = vec![name.to_string()];
    if !name.contains('/') && !name.ends_with(WORKER_SUFFIX) {
        candidates.push(format!("{name}{WORKER_SUFFIX}"));
    }
    // A slug that is a configured gateway id (`dnd` for its repo).
    candidates.extend(
        inst.config
            .fleet
            .servers
            .iter()
            .filter(|s| s.gateway_id.as_deref() == Some(name))
            .map(|s| s.repo.clone()),
    );
    for c in candidates {
        let opts = StatusOptions {
            servers: vec![c],
            refresh,
            endpoints: Endpoints::default(),
            ..Default::default()
        };
        let report = studio_fleet::fleet_status(inst, &opts, &providers).await;
        if let Some(s) = report.servers.into_iter().next() {
            return Ok(Some(s));
        }
    }
    Ok(None)
}

async fn plan_target(inst: &Instance, target: &str, refresh: bool) -> Result<Plan> {
    let as_dir = studio_core::config::expand_tilde(Path::new(target));
    let is_path = target.contains(std::path::MAIN_SEPARATOR) && as_dir.is_dir()
        || target.starts_with('.')
        || target.starts_with('~');
    let name = if is_path {
        as_dir
            .canonicalize()
            .unwrap_or(as_dir.clone())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        target.to_string()
    };
    let mut ctx = PlanContext::from_instance(inst);

    // A fleet member whose checkout is the one asked about: plan from probes.
    if let Some(status) = probe_one(inst, &name, refresh).await?
        && (!is_path || same_dir(&status.local_dir, &as_dir))
    {
        let mut p = plan::plan_for(&status, &ctx);
        if p.steps.iter().any(|s| s.id == "okta.scopes.rules") {
            match resolve_rules(&mut ctx).await {
                Ok(()) => p = plan::plan_for(&status, &ctx),
                Err(e) => p.notes.push(format!(
                    "policy ids not resolved ({e}); the rule commands look them up"
                )),
            }
        }
        return Ok(p);
    }

    // Otherwise a repo outside the fleet: plan from its files.
    let org = github_org(inst).unwrap_or_default();
    let dir = if is_path {
        as_dir
    } else {
        [name.clone(), format!("{name}{WORKER_SUFFIX}")]
            .into_iter()
            .map(|n| inst.repo_dir(&format!("{org}/{n}")))
            .find(|d| d.is_dir())
            .ok_or_else(|| {
                anyhow!("{target} is neither a fleet server nor a local repo; pass its directory")
            })?
    };
    let dir = dir.canonicalize().unwrap_or(dir);
    // The repo is named for its Worker (package.json name), whatever the
    // checkout directory is called.
    let repo_short = std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("name")?.as_str().map(String::from))
        .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or(name);
    let repo = format!("{org}/{repo_short}");
    let info = plan::repo_info(inst, &repo, &dir);
    let pack = load_pack(inst)?;
    let values = inst.pattern_values()?;
    let store = inst
        .conformance_dir()
        .unwrap_or_else(|| inst.root.join("conformance"));
    let lint =
        studio_pattern::Conformance::new(&pack, &values, &store)?.check_and_lint(&repo_short, &dir);
    let p = plan::plan_for_repo(&info, &lint, &ctx);
    if p.steps.iter().any(|s| s.id == "okta.scopes.rules") && resolve_rules(&mut ctx).await.is_ok()
    {
        return Ok(plan::plan_for_repo(&info, &lint, &ctx));
    }
    Ok(p)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Read-only: which policy holds each configured rule, and its name.
async fn resolve_rules(ctx: &mut PlanContext) -> Result<()> {
    let Some(o) = ctx.okta.as_mut() else {
        return Ok(());
    };
    let base = format!("/api/v1/authorizationServers/{}", o.auth_server);
    let policies = okta_get(&o.tool, o.org.as_deref(), &format!("{base}/policies")).await?;
    for p in policies.as_array().into_iter().flatten() {
        if o.rules.iter().all(|r| r.policy.is_some()) {
            break;
        }
        let Some(pid) = p.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let rules = okta_get(
            &o.tool,
            o.org.as_deref(),
            &format!("{base}/policies/{pid}/rules"),
        )
        .await?;
        for r in rules.as_array().into_iter().flatten() {
            let id = r.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            if let Some(rr) = o.rules.iter_mut().find(|x| x.id == id) {
                rr.policy = Some(pid.to_string());
                rr.name = r.get("name").and_then(|v| v.as_str()).map(String::from);
            }
        }
    }
    Ok(())
}

async fn okta_get(tool: &str, org: Option<&str>, path: &str) -> Result<serde_json::Value> {
    let mut c = tokio::process::Command::new(tool);
    c.args(["GET", path]);
    if let Some(o) = org {
        c.args(["--org", o]);
    }
    c.stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(std::time::Duration::from_secs(60), c.output())
        .await
        .map_err(|_| anyhow!("`{tool} GET {path}` timed out"))?
        .map_err(|_| anyhow!("`{tool}` is not installed or not on PATH"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "`{tool} GET {path}`: {}",
            err.lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("failed")
        );
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

// ---------------------------------------------------------------- verify

#[derive(Serialize)]
struct Verification {
    server: String,
    generated_at: String,
    satisfied: usize,
    remaining: usize,
    unknown: usize,
    steps: Vec<Step>,
}

async fn verify(inst: &Instance, server: &str, json: bool) -> Result<()> {
    let status = probe_one(inst, server, true).await?.ok_or_else(|| {
        anyhow!("{server} is not in the fleet yet: `mcp-studio server plan <its directory>`")
    })?;
    let ctx = PlanContext::from_instance(inst);
    let mut checks = status.checks.clone();
    checks.extend(plan::local_checks(&status.info));
    checks.push(studio_core::check::Check::pass(
        "fleet.member",
        "in the fleet",
    ));
    let steps = plan::catalog(&status.info, &checks, plan::Mode::Fleet, &ctx);
    let count = |s: StepState| steps.iter().filter(|x| x.state == s).count();
    let v = Verification {
        server: status.name.clone(),
        generated_at: studio_fleet::time::format_rfc3339(studio_fleet::time::now_unix()),
        satisfied: count(StepState::Done),
        remaining: count(StepState::Todo),
        unknown: count(StepState::Unknown),
        steps,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("{} — re-probed {}", v.server, v.generated_at);
    for s in &v.steps {
        let mark = match s.state {
            StepState::Done => "✓",
            StepState::Todo => "✗",
            StepState::Unknown => "?",
        };
        let st = s
            .why
            .status
            .map(|x| format!("{x:?}").to_lowercase())
            .unwrap_or_else(|| "not run".into());
        println!(
            "{mark} {:<20} {:<22} {st}: {}",
            s.id,
            s.verify.as_deref().unwrap_or("(no probe)"),
            s.why.summary
        );
    }
    println!(
        "\n{} satisfied, {} remaining, {} unknown",
        v.satisfied, v.remaining, v.unknown
    );
    Ok(())
}

// ---------------------------------------------------------------- output

fn print_plan(p: &Plan) {
    println!(
        "Plan for {} ({}, {:?} mode){}",
        p.server,
        p.repo,
        p.mode,
        p.url
            .as_ref()
            .map(|u| format!(" — {u}"))
            .unwrap_or_default()
    );
    if p.steps.is_empty() {
        println!("\nNothing to do: every check that ran is green.");
    }
    for (i, s) in p.steps.iter().enumerate() {
        println!(
            "\n{}. [{}] {}{}",
            i + 1,
            format!("{:?}", s.system).to_lowercase(),
            s.title,
            if s.destructive { "  (DESTRUCTIVE)" } else { "" }
        );
        let st = match s.why.status {
            Some(Status::Fail) => "fail",
            Some(Status::Warn) => "warn",
            Some(Status::Skip) => "skip",
            Some(Status::Pass) => "pass",
            None => "not run",
        };
        println!("   why:      {} ({st}): {}", s.why.check, s.why.summary);
        println!("   executor: {}", s.executor.as_str());
        println!(
            "   verify:   {}",
            s.verify.as_deref().unwrap_or("(no probe; see notes)")
        );
        for c in &s.commands {
            println!("   $ {c}");
        }
        for n in &s.notes {
            for (j, line) in n.lines().enumerate() {
                println!("   {} {line}", if j == 0 { "note:" } else { "     " });
            }
        }
    }
    if !p.unverified.is_empty() {
        println!("\nUnverified (checks skipped or not run):");
        for w in &p.unverified {
            println!("  ? {}: {}", w.check, w.summary);
        }
    }
    for n in &p.notes {
        println!("note: {n}");
    }
}
