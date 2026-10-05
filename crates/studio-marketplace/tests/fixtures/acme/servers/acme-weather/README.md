# Acme Weather

> **DEPRECATED.** Replaced by the Acme Climate server.

Forecasts for Acme sites.

## Install

**Claude Code**
```
/plugin marketplace add acme/acme-plugin-marketplace
/plugin install acme-weather@acme-plugin-marketplace
```

**Codex**
```
codex plugin marketplace add acme/acme-plugin-marketplace
```

## Server

- **Endpoint:** `https://weather.mcp.acme.example/sse`
- **Transport:** `sse`
- **Auth:** `api_key`

## Authentication

This server requires a credential. After installing, replace `<YOUR_API_KEY>` with your own key in `.mcp.json` (Claude Code) or `.codex-plugin/mcp.json` (Codex) — credentials are never distributed through the marketplace.

---

_Generated from `server.yaml`. Edit that file and open a PR to change this plugin — do not hand-edit generated files._
