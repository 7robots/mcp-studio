use std::collections::BTreeSet;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use studio_core::check::Status;
use studio_fleet::{
    Endpoints, FleetReport, GatewaySource, Providers, RepoChecker, Source, StatusOptions,
};

use crate::Ctx;

#[derive(Subcommand)]
pub enum Cmd {
    /// Fleet members and what their repos say about them.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Probe every server across every enabled source.
    Status(StatusArgs),
}

#[derive(Args)]
pub struct StatusArgs {
    #[arg(long)]
    json: bool,
    /// Only this server (repo name or owner/repo); repeatable.
    #[arg(long = "server", value_name = "NAME")]
    servers: Vec<String>,
    /// Only these sources: git,github,cloudflare,http,okta,gateway,marketplace,pattern.
    #[arg(long = "source", value_delimiter = ',', value_name = "SOURCES")]
    sources: Vec<String>,
    /// Ignore the cached result.
    #[arg(long)]
    refresh: bool,
    /// `git fetch` each clone first (read-only on the remote).
    #[arg(long)]
    fetch: bool,
    /// Exit non-zero when any check fails.
    #[arg(long)]
    strict: bool,
}

pub async fn run(cmd: Cmd, ctx: &Ctx) -> Result<()> {
    let checker = crate::wiring::PatternChecker::from_instance(&ctx.instance()?)?
        .map(|c| Box::new(c) as Box<dyn RepoChecker>);
    run_with(cmd, ctx, None, checker).await
}

/// The hook point for wiring the gateway registry and pattern conformance.
pub async fn run_with(
    cmd: Cmd,
    ctx: &Ctx,
    gateway: Option<Box<dyn GatewaySource>>,
    repo_checker: Option<Box<dyn RepoChecker>>,
) -> Result<()> {
    let inst = ctx.instance()?;
    match cmd {
        Cmd::List { json } => {
            let d = studio_fleet::discover(&inst, &Endpoints::default()).await;
            if json {
                println!("{}", serde_json::to_string_pretty(&d)?);
                return Ok(());
            }
            let rows: Vec<[String; 6]> = d
                .servers
                .iter()
                .map(|s| {
                    let envs: Vec<_> = s
                        .environments
                        .iter()
                        .map(|e| {
                            if e.deployed {
                                format!("+{}", e.name)
                            } else {
                                e.name.clone()
                            }
                        })
                        .collect();
                    [
                        format!(
                            "{}{}",
                            s.name,
                            if s.local_exists { "" } else { " (no clone)" }
                        ),
                        s.worker.clone().unwrap_or_else(|| "–".into()),
                        s.url.clone().unwrap_or_else(|| "–".into()),
                        s.gateway_id.clone().unwrap_or_else(|| "–".into()),
                        s.scopes.join(" "),
                        format!(
                            "{}{}",
                            s.version.as_deref().unwrap_or("–"),
                            if envs.is_empty() {
                                String::new()
                            } else {
                                format!("  env {}", envs.join(","))
                            }
                        ),
                    ]
                })
                .collect();
            print_table(
                &["SERVER", "WORKER", "URL", "GATEWAY ID", "SCOPES", "VERSION"],
                &rows,
            );
            for s in &d.servers {
                for p in &s.problems {
                    println!("! {}: {p}", s.name);
                }
            }
            for n in &d.notes {
                println!("note: {n}");
            }
            Ok(())
        }
        Cmd::Status(a) => {
            let sources = if a.sources.is_empty() {
                None
            } else {
                let mut set = BTreeSet::new();
                for s in &a.sources {
                    match Source::parse(s) {
                        Some(x) => {
                            set.insert(x);
                        }
                        None => bail!(
                            "unknown source {s:?}; want one of {}",
                            Source::ALL.map(|s| s.as_str()).join(",")
                        ),
                    }
                }
                Some(set)
            };
            let opts = StatusOptions {
                sources,
                servers: a.servers.clone(),
                refresh: a.refresh,
                fetch: a.fetch,
                endpoints: Endpoints::default(),
            };
            let providers = Providers::new(gateway, repo_checker);
            let report = studio_fleet::fleet_status(&inst, &opts, &providers).await;
            if !a.servers.is_empty() && report.servers.is_empty() {
                bail!("no fleet server matches {}", a.servers.join(", "));
            }
            if a.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_status(&report);
            }
            let fails = report.count(Status::Fail);
            if a.strict && fails > 0 {
                bail!("{fails} failing check{}", if fails == 1 { "" } else { "s" });
            }
            Ok(())
        }
    }
}

fn symbol(s: Status) -> &'static str {
    match s {
        Status::Pass => "✓",
        Status::Warn => "!",
        Status::Fail => "✗",
        Status::Skip => "–",
    }
}

const CELL: usize = 18;

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn print_status(r: &FleetReport) {
    let mut header = vec!["SERVER".to_string()];
    header.extend(r.sources.iter().map(|s| s.as_str().to_uppercase()));
    let rows: Vec<Vec<String>> = r
        .servers
        .iter()
        .map(|s| {
            let mut row = vec![format!("{} {}", symbol(s.rollup()), s.name)];
            for src in &r.sources {
                row.push(match s.column(*src) {
                    Some(c) => truncate(&format!("{} {}", symbol(c.status), c.text), CELL),
                    None => "–".into(),
                });
            }
            row
        })
        .collect();
    let h: Vec<&str> = header.iter().map(String::as_str).collect();
    print_table(&h, &rows);

    let problems: Vec<_> = r.problems().collect();
    if !problems.is_empty() {
        println!();
        let w = problems
            .iter()
            .map(|(n, _)| n.chars().count())
            .max()
            .unwrap_or(0);
        let iw = problems
            .iter()
            .map(|(_, c)| c.id.chars().count())
            .max()
            .unwrap_or(0);
        for (name, c) in problems {
            println!(
                "{} {name:w$}  {:iw$}  {}",
                symbol(c.status),
                c.id,
                c.summary
            );
        }
    }
    println!();
    let age = studio_fleet::time::now_unix() - r.generated_at_unix;
    println!(
        "{} pass, {} warn, {} fail, {} skip — {}{}",
        r.count(Status::Pass),
        r.count(Status::Warn),
        r.count(Status::Fail),
        r.count(Status::Skip),
        r.generated_at,
        if r.from_cache {
            format!(
                " (cached, {} old; --refresh to re-probe)",
                studio_fleet::time::age(age)
            )
        } else {
            String::new()
        }
    );
    for n in &r.notes {
        println!("note: {n}");
    }
}

fn print_table<R: AsRef<[String]>>(header: &[&str], rows: &[R]) {
    let n = header.len();
    let mut w: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.as_ref().iter().enumerate().take(n) {
            w[i] = w[i].max(c.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let s: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let pad = w[i].saturating_sub(c.chars().count());
                format!("{c}{}", " ".repeat(pad))
            })
            .collect();
        println!("{}", s.join("  ").trim_end());
    };
    line(header.to_vec());
    for r in rows {
        line(r.as_ref().iter().map(String::as_str).collect());
    }
}
