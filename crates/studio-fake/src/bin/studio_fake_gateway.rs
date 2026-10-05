//! `studio-fake-gateway`: the in-memory gateway on a local port, for poking at
//! the client by hand. Prints its base URL on the first line.

use std::io::Write;
use std::path::PathBuf;

use clap::Parser;
use studio_fake::gateway::{Options, serve};

#[derive(Parser)]
#[command(
    name = "studio-fake-gateway",
    about = "An in-memory MCP gateway for tests and demos"
)]
struct Cli {
    /// port to listen on (0 picks a free one)
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// JSON file the servers persist in (seeded when missing)
    #[arg(long)]
    state: Option<PathBuf>,
    /// refuse logins that ask for the admin scope, as an identity provider
    /// does for a user outside the admin group
    #[arg(long)]
    reader: bool,
    /// leave the user off the gateway's admin allowlist
    #[arg(long)]
    not_admin: bool,
    /// answer /mcp with plain JSON instead of an event stream
    #[arg(long)]
    json: bool,
    /// behave like an older gateway: no update_server, list_scope_owners or
    /// list_connections, no server versions and no build in health
    #[arg(long)]
    legacy: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut opts = Options {
        idp_grants_admin: !cli.reader,
        sse: !cli.json,
        state_path: cli.state,
        legacy: cli.legacy,
        ..Options::default()
    };
    if cli.not_admin {
        opts.admin_subs.clear();
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", cli.port)).await?;
    println!("http://{}", listener.local_addr()?);
    std::io::stdout().flush()?;
    serve(listener, opts).await
}
