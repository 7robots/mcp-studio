//! `mcp-studio pattern …`: the pattern pack, its rendering, and fleet
//! conformance. All logic lives in `studio-pattern`.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use serde::Serialize;
use studio_core::Instance;
use studio_core::check::{Check, Status};
use studio_core::config::repo_name;
use studio_pattern::{BlessOptions, Conformance, Pack, Values};

use crate::Ctx;

#[derive(Subcommand)]
pub enum Cmd {
    /// The pack's version, pins, security files and variables.
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Render the template with the instance's values into a new directory.
    Render {
        /// Output directory; must be absent or empty.
        #[arg(long)]
        out: PathBuf,
    },
    /// Conformance: version, blessed hashes and blessed diffs. Exit 1 on drift.
    Check {
        /// Repos (name or owner/name); default: the whole fleet.
        repos: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Per repo × security file: differing lines vs the template, and whether
    /// that matches the blessed diff.
    Status {
        repos: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Re-bless: regenerate the stored diffs and each repo's conformance.json
    /// (left untouched when version and hashes are unchanged).
    Bless {
        repos: Vec<String>,
        /// Write only the instance's conformance store; never touch a repo.
        #[arg(long)]
        store_only: bool,
    },
    /// Static rules: pins, lockfile, wrangler.toml, placeholders, legacy markers.
    Lint {
        repos: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Render the pack's skill (SKILL.md + references/) into a directory.
    InstallSkill {
        #[arg(long)]
        dest: PathBuf,
    },
}

struct Env {
    instance: Instance,
    pack: Pack,
    values: Values,
}

impl Env {
    fn load(ctx: &Ctx) -> Result<Self> {
        let instance = ctx.instance()?;
        let dir = instance
            .pattern_dir()
            .ok_or_else(|| anyhow!("{}: no [pattern] section", instance.root.display()))?;
        let pack =
            Pack::load(&dir).with_context(|| format!("loading pattern pack {}", dir.display()))?;
        let values = instance.pattern_values()?;
        Ok(Self {
            instance,
            pack,
            values,
        })
    }

    fn store_dir(&self) -> PathBuf {
        self.instance
            .conformance_dir()
            .unwrap_or_else(|| self.instance.root.join("conformance"))
    }

    fn conformance(&self) -> Result<Conformance<'_>> {
        Ok(Conformance::new(
            &self.pack,
            &self.values,
            &self.store_dir(),
        )?)
    }

    /// Fleet repos (`include`, then `[[fleet.server]]`), filtered by `wanted`.
    fn repos(&self, wanted: &[String]) -> Result<Vec<(String, PathBuf)>> {
        let fleet = &self.instance.config.fleet;
        let mut all: Vec<&str> = Vec::new();
        for r in fleet
            .include
            .iter()
            .chain(fleet.servers.iter().map(|s| &s.repo))
        {
            if !all.iter().any(|a| a.eq_ignore_ascii_case(r)) {
                all.push(r);
            }
        }
        let mut out = Vec::new();
        for w in wanted {
            if !all
                .iter()
                .any(|r| r.eq_ignore_ascii_case(w) || repo_name(r).eq_ignore_ascii_case(w))
            {
                bail!(
                    "unknown repo {w}; the fleet is: {}",
                    all.iter()
                        .map(|r| repo_name(r))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        for r in all {
            let name = repo_name(r);
            if wanted.is_empty()
                || wanted
                    .iter()
                    .any(|w| r.eq_ignore_ascii_case(w) || name.eq_ignore_ascii_case(w))
            {
                let dir = self.instance.repo_dir(r);
                if !dir.is_dir() {
                    bail!("{name}: no checkout at {}", dir.display());
                }
                out.push((name.to_string(), dir));
            }
        }
        if out.is_empty() {
            bail!("the instance's fleet lists no repos");
        }
        Ok(out)
    }
}

#[derive(Serialize)]
struct RepoChecks {
    repo: String,
    path: PathBuf,
    status: Status,
    checks: Vec<Check>,
}

fn label(s: Status) -> &'static str {
    match s {
        Status::Pass => "pass",
        Status::Warn => "WARN",
        Status::Fail => "FAIL",
        Status::Skip => "skip",
    }
}

fn print_json<T: Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

pub async fn run(cmd: Cmd, ctx: &Ctx) -> Result<()> {
    match cmd {
        Cmd::Show { json } => show(&Env::load(ctx)?, json),
        Cmd::Render { out } => {
            let env = Env::load(ctx)?;
            let tree = env.pack.render_to(&env.values, &out)?;
            println!(
                "rendered {} {} ({} files) into {}",
                env.pack.name(),
                env.pack.version(),
                tree.len(),
                out.display()
            );
            Ok(())
        }
        Cmd::Check { repos, json } => check(&Env::load(ctx)?, &repos, json),
        Cmd::Status { repos, json } => status(&Env::load(ctx)?, &repos, json),
        Cmd::Bless { repos, store_only } => bless(&Env::load(ctx)?, &repos, store_only),
        Cmd::Lint { repos, json } => lint(&Env::load(ctx)?, &repos, json),
        Cmd::InstallSkill { dest } => {
            let env = Env::load(ctx)?;
            let tree = env.pack.install_skill(&dest, &env.values)?;
            println!(
                "installed the {} skill ({} files) into {}",
                env.pack.name(),
                tree.len(),
                dest.display()
            );
            Ok(())
        }
    }
}

fn show(env: &Env, json: bool) -> Result<()> {
    let m = &env.pack.manifest;
    if json {
        return print_json(&serde_json::json!({
            "name": m.pack.name,
            "version": m.pack.version,
            "description": m.pack.description,
            "dir": env.pack.dir,
            "pins": m.pins,
            "dev_pins": m.dev_pins,
            "overrides": m.overrides,
            "majors": m.majors,
            "security_files": m.security.files,
            "required_files": m.required.files,
            "variables": m.variables,
        }));
    }
    println!("{} {}", m.pack.name, m.pack.version);
    if let Some(d) = &m.pack.description {
        println!("{d}");
    }
    println!("pack: {}", env.pack.dir.display());
    println!("\npins:");
    for (k, v) in m.pins.iter().chain(&m.dev_pins) {
        println!("  {k} = {v}");
    }
    for (k, v) in &m.overrides {
        println!("  {k} = {v} (override)");
    }
    println!("\nsecurity files:");
    for f in &m.security.files {
        println!("  {f}");
    }
    println!("\nvariables:");
    for (k, v) in &m.variables {
        let set = if env.values.contains_key(k) {
            "set"
        } else {
            "MISSING"
        };
        println!("  {k} [{set}]: {v}");
    }
    Ok(())
}

fn check(env: &Env, wanted: &[String], json: bool) -> Result<()> {
    let c = env.conformance()?;
    let mut results = Vec::new();
    for (name, dir) in env.repos(wanted)? {
        let checks = c.check(&name, &dir);
        results.push(RepoChecks {
            status: studio_core::check::rollup(&checks),
            repo: name,
            path: dir,
            checks,
        });
    }
    let failures: usize = results
        .iter()
        .flat_map(|r| &r.checks)
        .filter(|c| c.status == Status::Fail)
        .count();
    if json {
        print_json(&results)?;
    } else {
        for r in &results {
            for ch in r.checks.iter().filter(|c| c.status == Status::Fail) {
                println!("{}: {}", r.repo, ch.summary);
                for line in ch.evidence.iter().flat_map(|e| e.lines()) {
                    println!("  {line}");
                }
            }
        }
        if failures == 0 {
            println!(
                "conformance: {} repo(s) clean at {} {}",
                results.len(),
                env.pack.name(),
                env.pack.version()
            );
        }
    }
    if failures > 0 {
        bail!("conformance: {failures} failing check(s)");
    }
    Ok(())
}

fn status(env: &Env, wanted: &[String], json: bool) -> Result<()> {
    let c = env.conformance()?;
    let mut all = Vec::new();
    for (name, dir) in env.repos(wanted)? {
        all.extend(c.status(&name, &dir));
    }
    if json {
        return print_json(&all);
    }
    for s in &all {
        let flag = if s.matches_store { "OK " } else { "DRIFT" };
        println!(
            "{flag} {}/{}: {} differing line(s) vs template",
            s.repo, s.file, s.differing_lines
        );
    }
    Ok(())
}

fn bless(env: &Env, wanted: &[String], store_only: bool) -> Result<()> {
    let c = env.conformance()?;
    for (name, dir) in env.repos(wanted)? {
        let o = c.bless(&name, &dir, BlessOptions { store_only })?;
        let manifest = if o.manifest_written {
            format!("{} rewritten", env.pack.manifest.security.conformance_file)
        } else if o.manifest_stale {
            format!(
                "{} is STALE (kept: --store-only)",
                env.pack.manifest.security.conformance_file
            )
        } else {
            format!("{} unchanged", env.pack.manifest.security.conformance_file)
        };
        println!(
            "{name}: blessed ({} files; {} stored diff(s) changed; {manifest})",
            o.files,
            o.diffs_changed.len()
        );
    }
    println!("store: {}", c.store_dir.display());
    Ok(())
}

fn lint(env: &Env, wanted: &[String], json: bool) -> Result<()> {
    let mut results = Vec::new();
    for (name, dir) in env.repos(wanted)? {
        let checks = studio_pattern::lint_repo(&env.pack, &dir);
        results.push(RepoChecks {
            status: studio_core::check::rollup(&checks),
            repo: name,
            path: dir,
            checks,
        });
    }
    if json {
        print_json(&results)?;
    } else {
        for r in &results {
            println!("{} [{}]", r.repo, label(r.status));
            for ch in &r.checks {
                println!("  {:<4} {:<34} {}", label(ch.status), ch.id, ch.summary);
                if ch.status != Status::Pass {
                    for line in ch.evidence.iter().flat_map(|e| e.lines()) {
                        println!("         {line}");
                    }
                }
            }
        }
    }
    let failing: Vec<&str> = results
        .iter()
        .filter(|r| r.status == Status::Fail)
        .map(|r| r.repo.as_str())
        .collect();
    if !failing.is_empty() {
        bail!("lint failed for {}", failing.join(", "));
    }
    Ok(())
}
