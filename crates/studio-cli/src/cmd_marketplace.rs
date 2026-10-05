//! `mcp-studio marketplace …`: thin wrappers over `studio_marketplace`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use studio_core::check::{Check, Status, rollup};
use studio_marketplace::model::{AuthType, Installation, Kind, Transport};
use studio_marketplace::ops::{self, ChangeSet, EntryPatch};
use studio_marketplace::provision::{self, ProvisionOptions};
use studio_marketplace::publish::{self, Gh, GithubApi};
use studio_marketplace::reconcile::DiffKind;
use studio_marketplace::{
    Configured, Location, Marketplace, draft, git, overview, tree, validate, yaml,
};

use crate::Ctx;

#[derive(Subcommand)]
pub enum Cmd {
    /// Configured marketplaces and their entries.
    List {
        #[arg(long)]
        json: bool,
    },
    /// One marketplace: identity, clone, entries.
    Show {
        marketplace: String,
        #[arg(long)]
        json: bool,
    },
    /// Schema + invariant gate, for a configured marketplace or any directory.
    Validate {
        /// A marketplace id from studio.toml, or a path to a marketplace tree.
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// Compare every server.yaml with its generated files and both catalogs.
    Reconcile {
        marketplace: String,
        #[arg(long)]
        json: bool,
        /// Also list passing checks.
        #[arg(long)]
        all: bool,
    },
    /// Regenerate the whole marketplace in a temp dir and diff it with the tree.
    Verify {
        marketplace: String,
        #[arg(long)]
        json: bool,
        /// Only name the differing files.
        #[arg(long)]
        quiet: bool,
    },
    /// Add an entry.
    Add {
        marketplace: String,
        slug: String,
        #[command(flatten)]
        fields: Box<Fields>,
        /// Start from a server.yaml (flags override its values).
        #[arg(long, conflicts_with = "from_repo")]
        from_file: Option<PathBuf>,
        /// Start from a draft of a fleet server repo (see draft-entry).
        #[arg(long)]
        from_repo: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = KindArg::McpServer)]
        kind: KindArg,
        #[command(flatten)]
        run: RunOpts,
    },
    /// Change an entry's fields (and regenerate its files).
    Update {
        marketplace: String,
        slug: String,
        #[command(flatten)]
        fields: Box<Fields>,
        #[command(flatten)]
        run: RunOpts,
    },
    /// Mark an entry deprecated: still listed and installable.
    Deprecate {
        marketplace: String,
        slug: String,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        run: RunOpts,
    },
    /// Undo a deprecation.
    Reinstate {
        marketplace: String,
        slug: String,
        #[command(flatten)]
        run: RunOpts,
    },
    /// Remove an entry from the repo and both catalogs (breaks installs).
    Remove {
        marketplace: String,
        slug: String,
        #[command(flatten)]
        run: RunOpts,
    },
    /// Seed a new marketplace repo in a local directory.
    Provision {
        marketplace: String,
        #[arg(long)]
        dir: PathBuf,
        /// Also seed the weekly endpoint probe workflow.
        #[arg(long)]
        health: bool,
        /// Then create the GitHub repo with gh and push (private by default).
        #[arg(long)]
        create_repo: bool,
        /// With --create-repo: make it public. Everything in it becomes world-readable.
        #[arg(long, requires = "create_repo")]
        public: bool,
    },
    /// Adopt an existing clone: seed it if empty, else add missing schemas/CI.
    Adopt {
        marketplace: String,
        #[arg(long)]
        dir: PathBuf,
        #[arg(long)]
        health: bool,
    },
    /// Draft a server.yaml from a fleet server repo (prints it; writes nothing).
    DraftEntry {
        #[arg(long)]
        from_repo: PathBuf,
        /// Marketplace slug (default: the fleet config's marketplace_slug, else the repo name).
        #[arg(long)]
        slug: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args, Default)]
pub struct Fields {
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    description: Option<String>,
    #[arg(long)]
    url: Option<String>,
    #[arg(long, value_enum)]
    transport: Option<TransportArg>,
    #[arg(long, value_enum)]
    auth: Option<AuthArg>,
    #[arg(long)]
    header_name: Option<String>,
    /// Repeat for several tags (replaces the existing list).
    #[arg(long = "tag")]
    tags: Vec<String>,
    #[arg(long)]
    category: Option<String>,
    #[arg(long)]
    homepage: Option<String>,
    #[arg(long)]
    version: Option<String>,
    #[arg(long)]
    owner: Option<String>,
    /// Codex catalog policy.installation.
    #[arg(long, value_enum)]
    installation: Option<InstallationArg>,
    /// Claude catalog `strict`.
    #[arg(long)]
    strict: Option<bool>,
    #[arg(long)]
    license: Option<String>,
    #[arg(long)]
    repository: Option<String>,
}

#[derive(Args)]
pub struct RunOpts {
    /// Print the file changes; write nothing.
    #[arg(long)]
    dry_run: bool,
    /// After committing, push (or open a PR, per the marketplace's publish mode).
    #[arg(long, conflicts_with = "dry_run")]
    publish: bool,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum KindArg {
    McpServer,
    Skill,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum TransportArg {
    Http,
    Sse,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum AuthArg {
    None,
    Bearer,
    #[value(name = "api_key", alias = "api-key")]
    ApiKey,
    Oauth,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum InstallationArg {
    #[value(name = "AVAILABLE", alias = "available")]
    Available,
    #[value(name = "INSTALLED_BY_DEFAULT", alias = "installed-by-default")]
    InstalledByDefault,
    #[value(name = "NOT_AVAILABLE", alias = "not-available")]
    NotAvailable,
}

impl Fields {
    fn patch(&self) -> EntryPatch {
        EntryPatch {
            name: self.name.clone(),
            description: self.description.clone(),
            url: self.url.clone(),
            transport: self.transport.map(|t| match t {
                TransportArg::Http => Transport::Http,
                TransportArg::Sse => Transport::Sse,
            }),
            auth: self.auth.map(|a| match a {
                AuthArg::None => AuthType::None,
                AuthArg::Bearer => AuthType::Bearer,
                AuthArg::ApiKey => AuthType::ApiKey,
                AuthArg::Oauth => AuthType::Oauth,
            }),
            header_name: self.header_name.clone(),
            tags: (!self.tags.is_empty()).then(|| self.tags.clone()),
            category: self.category.clone(),
            homepage: self.homepage.clone(),
            version: self.version.clone(),
            owner: self.owner.clone(),
            installation: self.installation.map(|i| match i {
                InstallationArg::Available => Installation::Available,
                InstallationArg::InstalledByDefault => Installation::InstalledByDefault,
                InstallationArg::NotAvailable => Installation::NotAvailable,
            }),
            strict: self.strict,
            license: self.license.clone(),
            repository: self.repository.clone(),
        }
    }
}

pub async fn run(cmd: Cmd, ctx: &Ctx) -> Result<()> {
    match cmd {
        Cmd::List { json } => list(ctx, json),
        Cmd::Show { marketplace, json } => show(ctx, &marketplace, json),
        Cmd::Validate { target, json } => validate_cmd(ctx, &target, json),
        Cmd::Reconcile {
            marketplace,
            json,
            all,
        } => reconcile_cmd(ctx, &marketplace, json, all),
        Cmd::Verify {
            marketplace,
            json,
            quiet,
        } => verify_cmd(ctx, &marketplace, json, quiet),
        Cmd::Add {
            marketplace,
            slug,
            fields,
            from_file,
            from_repo,
            kind,
            run,
        } => {
            let kind = match kind {
                KindArg::McpServer => Kind::McpServer,
                KindArg::Skill => Kind::Skill,
            };
            let mut entry = match (&from_file, &from_repo) {
                (Some(f), _) => {
                    let text = std::fs::read_to_string(f)
                        .with_context(|| format!("reading {}", f.display()))?;
                    yaml::parse_entry(&text).with_context(|| f.display().to_string())?
                }
                (None, Some(r)) => draft::entry_from_server(r, Some(&slug))?.entry,
                (None, None) => fields.patch().into_entry(kind),
            };
            if from_file.is_some() || from_repo.is_some() {
                fields.patch().apply_to(&mut entry);
                if kind == Kind::Skill {
                    entry.kind = Some(Kind::Skill);
                }
            }
            mutate(ctx, &marketplace, &run, |dir, m| {
                ops::plan_add(dir, m, entry, Some(&slug))
            })
            .await
        }
        Cmd::Update {
            marketplace,
            slug,
            fields,
            run,
        } => {
            let patch = fields.patch();
            mutate(ctx, &marketplace, &run, |dir, m| {
                ops::plan_update(dir, m, &slug, &patch)
            })
            .await
        }
        Cmd::Deprecate {
            marketplace,
            slug,
            reason,
            run,
        } => {
            mutate(ctx, &marketplace, &run, |dir, m| {
                ops::plan_deprecate(dir, m, &slug, &reason)
            })
            .await
        }
        Cmd::Reinstate {
            marketplace,
            slug,
            run,
        } => {
            mutate(ctx, &marketplace, &run, |dir, m| {
                ops::plan_reinstate(dir, m, &slug)
            })
            .await
        }
        Cmd::Remove {
            marketplace,
            slug,
            run,
        } => {
            mutate(ctx, &marketplace, &run, |dir, m| {
                ops::plan_remove(dir, m, &slug)
            })
            .await
        }
        Cmd::Provision {
            marketplace,
            dir,
            health,
            create_repo,
            public,
        } => provision_cmd(ctx, &marketplace, &dir, health, create_repo, public),
        Cmd::Adopt {
            marketplace,
            dir,
            health,
        } => {
            let c = resolve(ctx, &marketplace)?;
            let out = provision::adopt(&dir, &c.market, ProvisionOptions { health })?;
            println!("{}", serde_json::to_string_pretty(&out)?);
            println!("committed locally in {}; nothing was pushed", dir.display());
            Ok(())
        }
        Cmd::DraftEntry {
            from_repo,
            slug,
            json,
        } => draft_cmd(ctx, &from_repo, slug, json),
    }
}

fn resolve(ctx: &Ctx, id: &str) -> Result<Configured> {
    studio_marketplace::resolve(&ctx.instance()?, id)
}

fn list(ctx: &Ctx, json: bool) -> Result<()> {
    let inst = ctx.instance()?;
    let all: Vec<_> = studio_marketplace::configured(&inst)
        .iter()
        .map(overview)
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(());
    }
    if all.is_empty() {
        println!("no [[marketplace]] configured");
    }
    for o in &all {
        print_overview(o);
        println!();
    }
    Ok(())
}

fn print_overview(o: &studio_marketplace::Overview) {
    println!("{}  {}  (@{})", o.id, o.repo, o.catalog_name);
    if !o.cloned {
        println!("  not cloned at {}", o.dir);
        return;
    }
    println!("  clone: {}", o.dir);
    if o.entries.is_empty() {
        println!("  (no entries)");
    }
    for e in &o.entries {
        let dep = if e.deprecated { "  [deprecated]" } else { "" };
        println!(
            "  {:<40} {:<8} {:<7} {}{dep}",
            e.slug,
            e.version,
            e.auth.as_str(),
            e.url.as_deref().unwrap_or(e.kind.as_str())
        );
    }
}

fn show(ctx: &Ctx, id: &str, json: bool) -> Result<()> {
    let c = resolve(ctx, id)?;
    let o = overview(&c);
    if json {
        #[derive(serde::Serialize)]
        struct Show<'a> {
            marketplace: &'a Marketplace,
            #[serde(flatten)]
            overview: &'a studio_marketplace::Overview,
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Show {
                marketplace: &c.market,
                overview: &o
            })?
        );
        return Ok(());
    }
    let m = &c.market;
    println!("owner:        {}", m.owner_name);
    println!("author:       {}", m.author_name);
    let targets: Vec<&str> = m
        .targets
        .iter()
        .map(|t| match t {
            studio_core::config::Target::Claude => "claude",
            studio_core::config::Target::Codex => "codex",
        })
        .collect();
    println!("targets:      {}", targets.join(", "));
    println!("branch:       {}", m.branch);
    println!("publish:      {:?}", m.publish);
    println!(
        "gh account:   {}",
        m.github_account.as_deref().unwrap_or("(active)")
    );
    print_overview(&o);
    Ok(())
}

fn validate_cmd(ctx: &Ctx, target: &str, json: bool) -> Result<()> {
    let path = Path::new(target);
    let (dir, market) = if path.is_dir() {
        (path.to_path_buf(), Marketplace::from_tree(path))
    } else {
        let c = resolve(ctx, target)?;
        match &c.location {
            Location::NotCloned(d) => {
                let skip = studio_marketplace::not_cloned(&c.market, d);
                return print_skip(&skip, json);
            }
            Location::Cloned(d) => (d.clone(), c.market),
        }
    };
    let r = validate::validate_dir(&dir, &market);
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        let schema: Vec<_> = r.schema_errors().collect();
        let data: Vec<_> = r.data_errors().collect();
        if !schema.is_empty() {
            println!("schema problems (the schemas themselves are broken):");
            for f in &schema {
                println!("  {}: {}", f.file, f.message);
            }
        }
        if !data.is_empty() {
            println!("data problems:");
            for f in &data {
                let kind = match f.kind {
                    validate::FindingKind::Schema => "schema",
                    _ => "invariant",
                };
                println!("  [{kind}] {}: {}", f.file, f.message);
            }
        }
        for n in &r.notes {
            println!("note: {n}");
        }
        println!(
            "{}: {} files checked, {} schema problem(s), {} data problem(s)",
            dir.display(),
            r.files_checked,
            schema.len(),
            data.len()
        );
    }
    if !r.is_ok() {
        bail!("marketplace invalid");
    }
    Ok(())
}

fn print_skip(c: &Check, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&[c])?);
    } else {
        println!("skip  {}  {}", c.id, c.summary);
    }
    Ok(())
}

fn mark(s: Status) -> &'static str {
    match s {
        Status::Pass => "ok  ",
        Status::Warn => "warn",
        Status::Fail => "FAIL",
        Status::Skip => "skip",
    }
}

fn reconcile_cmd(ctx: &Ctx, id: &str, json: bool, all: bool) -> Result<()> {
    let c = resolve(ctx, id)?;
    let checks = c.checks();
    let worst = rollup(&checks);
    if json {
        println!("{}", serde_json::to_string_pretty(&checks)?);
    } else {
        let mut passed = 0;
        for ch in &checks {
            if ch.status == Status::Pass && !all {
                passed += 1;
                continue;
            }
            println!("{}  {}  {}", mark(ch.status), ch.id, ch.summary);
            if let Some(ev) = &ch.evidence {
                for line in ev.lines() {
                    println!("        {line}");
                }
            }
        }
        if !all && passed > 0 {
            println!("({passed} passing checks hidden; --all shows them)");
        }
    }
    if worst == Status::Fail {
        bail!("{id}: drift found");
    }
    Ok(())
}

fn verify_cmd(ctx: &Ctx, id: &str, json: bool, quiet: bool) -> Result<()> {
    let c = resolve(ctx, id)?;
    let dir = match &c.location {
        Location::NotCloned(d) => {
            return print_skip(&studio_marketplace::not_cloned(&c.market, d), json);
        }
        Location::Cloned(d) => d.clone(),
    };
    let r = studio_marketplace::verify(&dir, &c.market)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        for d in &r.diffs {
            let k = match d.kind {
                DiffKind::Changed => "changed",
                DiffKind::Missing => "missing",
                DiffKind::Extra => "extra  ",
            };
            println!("{k}  {}", d.path);
            if !quiet && !d.diff.is_empty() {
                for line in d.diff.lines() {
                    println!("    {line}");
                }
            }
        }
        for (f, e) in &r.broken {
            println!("broken  {f}: {e}");
        }
        println!(
            "{}: regenerated {} files, {} byte-identical, {} differ",
            dir.display(),
            r.generated,
            r.identical,
            r.diffs.len()
        );
    }
    if !r.is_clean() {
        bail!("{id}: the tree differs from what its server.yaml files generate");
    }
    Ok(())
}

fn print_changes(cs: &ChangeSet) {
    println!("{}", cs.title());
    for c in &cs.changes {
        println!("  {:<6}  {}", c.action(), c.path);
    }
    for n in &cs.notes {
        println!("  note: {n}");
    }
}

async fn mutate<F>(ctx: &Ctx, id: &str, run: &RunOpts, plan: F) -> Result<()>
where
    F: FnOnce(&Path, &Marketplace) -> Result<ChangeSet>,
{
    let c = resolve(ctx, id)?;
    let m = &c.market;
    let ws = git::open_workspace(
        c.location.path(),
        &git::github_clone_url(&m.repo),
        &m.branch,
        run.publish || run.dry_run,
    )?;
    let cs = plan(&ws.dir, m)?;
    if cs.is_empty() {
        println!("{}: nothing to change", cs.title());
        return Ok(());
    }
    print_changes(&cs);
    if run.dry_run {
        println!();
        print!("{}", cs.diff());
        println!("(dry run: nothing written)");
        return Ok(());
    }
    ops::apply_checked(&ws.dir, m, &cs)?;
    let commit = ops::commit(&ws.dir, &cs)?;
    println!(
        "committed {} in {}",
        &commit.head[..commit.head.len().min(12)],
        ws.dir.display()
    );
    if !run.publish {
        println!("not published (pass --publish to push or open a PR)");
        return Ok(());
    }
    let token = studio_core::exec::github_token(m.github_account.as_deref()).ok();
    let api = token.clone().map(GithubApi::new);
    let route = publish::decide(m.publish, api.as_ref(), &m.repo, &m.branch).await?;
    let out = publish::publish(
        &ws.dir,
        &m.branch,
        &m.repo,
        &cs,
        &commit,
        &route,
        &Gh::new(token),
    )?;
    match out {
        publish::Published::Pushed { branch } => println!("pushed to {}:{branch}", m.repo),
        publish::Published::PullRequest { branch, url } => {
            println!("opened a PR from {branch}: {url}")
        }
    }
    Ok(())
}

fn provision_cmd(
    ctx: &Ctx,
    id: &str,
    dir: &Path,
    health: bool,
    create_repo: bool,
    public: bool,
) -> Result<()> {
    let c = resolve(ctx, id)?;
    let files = provision::provision(dir, &c.market, ProvisionOptions { health })?;
    for f in &files {
        println!("  create  {f}");
    }
    let r = validate::validate_dir(dir, &c.market);
    if !r.is_ok() {
        bail!("seeded tree does not validate: {:?}", r.findings);
    }
    println!(
        "seeded and committed in {} (validates clean)",
        dir.display()
    );
    if create_repo {
        let token = studio_core::exec::github_token(c.market.github_account.as_deref())?;
        let url = provision::create_repo(dir, &c.market, &token, public)?;
        println!("created {} and pushed ({url})", c.market.repo);
    } else {
        println!(
            "local only: --create-repo creates {} on GitHub and pushes",
            c.market.repo
        );
    }
    Ok(())
}

fn draft_cmd(ctx: &Ctx, repo: &Path, slug: Option<String>, json: bool) -> Result<()> {
    // The fleet config may name a marketplace slug for this repo.
    let slug = slug.or_else(|| {
        let inst = ctx.instance().ok()?;
        let want = repo.canonicalize().ok()?;
        inst.config.fleet.servers.iter().find_map(|s| {
            let dir = inst.repo_dir(&s.repo).canonicalize().ok()?;
            (dir == want).then(|| s.marketplace_slug.clone()).flatten()
        })
    });
    let d = draft::entry_from_server(repo, slug.as_deref())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    print!("{}", yaml::emit(&d.entry));
    for (field, from) in &d.sources {
        eprintln!("# {field}: {from}");
    }
    if !tree::is_marketplace(repo) {
        eprintln!(
            "# review, then: mcp-studio marketplace add <marketplace> <slug> --from-file <this file>"
        );
    }
    Ok(())
}
