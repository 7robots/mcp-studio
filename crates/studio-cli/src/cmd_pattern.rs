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
        Cmd::Todo => anyhow::bail!("pattern: not implemented yet"),
    }
}
