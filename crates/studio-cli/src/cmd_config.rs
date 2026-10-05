use anyhow::Result;
use clap::Subcommand;

use crate::Ctx;

#[derive(Subcommand)]
pub enum Cmd {
    /// Load and validate the instance; print where everything resolves.
    Check {
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(cmd: Cmd, ctx: &Ctx) -> Result<()> {
    match cmd {
        Cmd::Check { json } => {
            let inst = ctx.instance()?;
            let c = &inst.config;
            let summary = serde_json::json!({
                "instance": inst.name(),
                "root": inst.root,
                "gateways": c.gateways.iter().map(|g| &g.url).collect::<Vec<_>>(),
                "fleet_servers": c.fleet.servers.len(),
                "fleet_include": c.fleet.include.len(),
                "marketplaces": c.marketplaces.iter().map(|m| &m.repo).collect::<Vec<_>>(),
                "pattern": inst.pattern_dir(),
                "conformance_dir": inst.conformance_dir(),
                "cache_dir": inst.cache_dir(),
            });
            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                println!("ok: {} ({})", inst.display_name(), inst.root.display());
                for (k, v) in summary.as_object().into_iter().flatten() {
                    println!("  {k:16} {v}");
                }
            }
            Ok(())
        }
    }
}
