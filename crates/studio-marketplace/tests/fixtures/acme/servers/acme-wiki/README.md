# Acme Wiki

Read and search the Acme wiki — every space, page, and attachment the signed-in user can see, with the wiki's own permissions applied.

## Install

**Claude Code**
```
/plugin marketplace add acme/acme-plugin-marketplace
/plugin install acme-wiki@acme-plugin-marketplace
```

**Codex**
```
codex plugin marketplace add acme/acme-plugin-marketplace
```

## Server

- **Endpoint:** `https://wiki.mcp.acme.example/mcp`
- **Transport:** `http`
- **Auth:** `oauth`

## Authentication

This server uses OAuth. Your MCP client will prompt you to authenticate on first use.

---

_Generated from `server.yaml`. Edit that file and open a PR to change this plugin — do not hand-edit generated files._
