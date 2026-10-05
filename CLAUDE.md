# MCP Studio — working notes

- Cargo is at `/opt/homebrew/opt/rustup/bin` (not on the default PATH).
- **This repo is public and org-neutral.** Org values (identity provider ids, hostnames, GitHub orgs, account ids, emails, 1Password paths) belong in an instance repo, never here. `scripts/check-neutral.sh <instance>` builds its deny list from the instance's own values; the pre-push hook runs it. Tests and examples use the fictional `acme` instance and `*.example` hosts.
- Every module's logic lives in a library crate; `studio-cli` and `studio-tui` are thin. Every read command has `--json`.
- Every check reports through `studio_core::check::Check` (stable dotted id, status, summary, evidence).
- Studio writes only to the gateway (admin tools), marketplaces (git commits / PRs), the instance's conformance store, and new repos it scaffolds. Okta and Cloudflare are read-only: Studio plans and verifies those changes, and the cloudflare and okta-admin skills carry them out.
- Gate: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
