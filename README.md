# MCP Studio

A terminal workbench for running an MCP server program end to end. One Rust/Ratatui TUI, plus a scriptable `mcp-studio` CLI, that manages:

- **The pattern:** the versioned recipe for building an MCP server. That covers the template, the skill docs, the dependency pins and the conformance rules.
- **The fleet:** every server repo built from the pattern. For each one Studio shows whether it is deployed and at which commit, whether it is live, whether it is registered on the gateway, which marketplaces list it, and whether it conforms to the current pattern version.
- **The gateway:** registry administration for an MCP gateway (servers, tool classes, access, usage and policy events).
- **Plugin marketplaces:** Claude Code and Codex catalogs, generated per server from one `server.yaml`.

Nothing org-specific lives in this repo. An organization is an **instance**: a directory holding `studio.toml` (identity provider, Cloudflare account, gateway, fleet, marketplaces), `pattern-values.toml` (the values the pattern template is rendered with), and `conformance/` (blessed per-repo diffs). See [`examples/instance`](examples/instance) for a fully commented example.

## Install

```sh
cargo install --path crates/studio-cli      # installs `mcp-studio`
mkdir -p ~/.config/mcp-studio
echo 'default_instance = "~/path/to/your-instance"' > ~/.config/mcp-studio/config.toml
mcp-studio config check
```

`mcp-studio` with no subcommand opens the TUI. `mcp-studio --demo` opens it against an in-process fake gateway, so it needs no instance and no sign-in.

## CLI

Every view has a CLI form, and every read takes `--json`, so skills and agents drive the same engine the TUI does.

| Command | What it does |
|---|---|
| `config check` | Load and validate the instance |
| `gateway login\|whoami\|servers\|register\|refresh\|…` | Gateway sign-in and administration |
| `pattern show\|render\|check\|status\|bless\|lint\|install-skill` | Pattern pack and fleet conformance |
| `fleet list\|status` | Fleet discovery and live status across every probe source |
| `marketplace list\|validate\|reconcile\|verify\|add\|update\|deprecate\|remove\|provision` | Plugin marketplaces |

## Secrets

Config holds references only: `op://vault/item/field` (1Password) or `env:NAME`. Studio reads a value when it needs it and keeps it in memory. It never prints, logs or caches a secret. The one exception is the gateway sign-in, which Studio mints itself on each machine; that sign-in is kept in the macOS Keychain (service `mcp-studio`).

## Layout

```
crates/studio-core         instance config, secret refs, shared Check type
crates/studio-gateway      MCP client, OAuth (DCR + PKCE), token stores, admin actions
crates/studio-pattern      pattern packs, template rendering, conformance (check/status/bless/lint)
crates/studio-fleet        discovery and probes: git, GitHub, Cloudflare, HTTP, Okta, gateway, marketplaces
crates/studio-marketplace  server.yaml model, Claude + Codex generators, schemas, publish
crates/studio-tui          the TUI framework and modules
crates/studio-cli          the `mcp-studio` binary
crates/studio-fake         fake gateway for tests and --demo
patterns/                  pattern packs (cf-workers-ts: TypeScript MCP servers on Cloudflare Workers)
marketplace/               marketplace JSON schemas, entry templates, CI workflows
examples/instance/         an example instance (fictional "acme")
```

## Development

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
scripts/check-neutral.sh <instance-dir>   # no instance value may appear in this repo
ln -s ../../scripts/pre-push .git/hooks/pre-push
```

## License

MIT
