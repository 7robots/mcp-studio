use anyhow::Result;
use clap::Subcommand;

use crate::Ctx;

#[derive(Subcommand)]
pub enum Cmd {
    /// Not implemented yet.
    Todo,
}

pub async fn run(cmd: Cmd, _ctx: &Ctx) -> Result<()> {
    match cmd {
        Cmd::Todo => anyhow::bail!("gateway: not implemented yet"),
    }
}

/// The TUI (default when no subcommand is given).
pub async fn tui(_ctx: &Ctx, _demo: bool) -> Result<()> {
    anyhow::bail!("the TUI is not implemented yet")
}
