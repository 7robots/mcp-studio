# Acme Tickets

File and track Acme support tickets.

## Install

**Claude Code**
```
/plugin marketplace add acme/acme-plugin-marketplace
/plugin install acme-tickets@acme-plugin-marketplace
```

**Codex**
```
codex plugin marketplace add acme/acme-plugin-marketplace
```

## Server

- **Endpoint:** `https://tickets.mcp.acme.example/mcp`
- **Transport:** `http`
- **Auth:** `bearer`
- **Tags:** support

## Authentication

This server requires a credential. After installing, replace `<YOUR_API_KEY>` with your own key in `.mcp.json` (Claude Code) or `.codex-plugin/mcp.json` (Codex) — credentials are never distributed through the marketplace.

---

_Generated from `server.yaml`. Edit that file and open a PR to change this plugin — do not hand-edit generated files._
