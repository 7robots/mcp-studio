//! `mcp-studio`: the TUI when run with no subcommand, otherwise a scriptable
//! CLI. Every subcommand that reads state takes `--json` so skills and agents
//! drive the same engine as the TUI.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use studio_core::Instance;

mod cmd_config;
mod cmd_fleet;
mod cmd_gateway;
mod cmd_marketplace;
mod cmd_pattern;
mod wiring;

#[derive(Parser)]
#[command(name = "mcp-studio", version, about)]
struct Cli {
    /// Instance directory (holds studio.toml). Default: $MCP_STUDIO_INSTANCE,
    /// then default_instance in ~/.config/mcp-studio/config.toml.
    #[arg(long, global = true)]
    instance: Option<PathBuf>,
    /// Open the TUI against an in-process fake gateway (no instance, no sign-in).
    #[arg(long)]
    demo: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Instance configuration.
    #[command(subcommand)]
    Config(cmd_config::Cmd),
    /// The gateway: sign-in and registry administration.
    #[command(subcommand)]
    Gateway(cmd_gateway::Cmd),
    /// The pattern pack: render, conformance, bless.
    #[command(subcommand)]
    Pattern(cmd_pattern::Cmd),
    /// Fleet status across every probe source.
    #[command(subcommand)]
    Fleet(cmd_fleet::Cmd),
    /// Claude Code and Codex plugin marketplaces.
    #[command(subcommand)]
    Marketplace(cmd_marketplace::Cmd),
}

/// What every subcommand gets. The instance loads lazily so commands that
/// don't need one (e.g. `--demo`) work without it.
pub struct Ctx {
    instance_arg: Option<PathBuf>,
}

impl Ctx {
    pub fn instance(&self) -> Result<Instance> {
        let dir = studio_core::instance::locate(self.instance_arg.as_deref())?;
        Ok(Instance::load(&dir)?)
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let ctx = Ctx {
        instance_arg: cli.instance,
    };
    let result = match cli.cmd {
        None => cmd_gateway::tui(&ctx, cli.demo).await,
        Some(Cmd::Config(c)) => cmd_config::run(c, &ctx).await,
        Some(Cmd::Gateway(c)) => cmd_gateway::run(c, &ctx).await,
        Some(Cmd::Pattern(c)) => cmd_pattern::run(c, &ctx).await,
        Some(Cmd::Fleet(c)) => cmd_fleet::run(c, &ctx).await,
        Some(Cmd::Marketplace(c)) => cmd_marketplace::run(c, &ctx).await,
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mcp-studio: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
