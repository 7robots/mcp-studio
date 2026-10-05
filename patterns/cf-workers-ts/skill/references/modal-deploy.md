# Container-Host Deployment: Modal & Friends (cold storage)

> **⚠️ Not the current path.** This pattern deploys MCP servers exclusively to Cloudflare Workers (see [../SKILL.md](../SKILL.md) and [agent_guide.md](./agent_guide.md)); nothing is deployed to Modal. This file preserves the container-host lessons — scale-to-zero persistence, generic smoke testing, reverse-proxy fronting — in case a Python/container deployment returns. The recurring theme: **whatever scales to zero must persist its auth state somewhere durable and encrypted** — that, plus the smoke test, is most of what MCP deployment on a container host comes down to.

The server-side stack these examples assume is FastMCP — see [python-fastmcp.md](./python-fastmcp.md).

## Principles that transfer to any container platform

### A deploy isn't done until it's smoke-verified

Every deploy should end by hitting the **live public URL** — local tests can't see proxy, DNS, TLS, or host-header problems. Script it; don't re-derive it per project. A generic smoke test for any OAuth-fronted MCP server:

```bash
#!/usr/bin/env bash
# smoke.sh <base_url> — run automatically at the end of every deploy.
set -euo pipefail
base="${1%/}"
retries="${SMOKE_RETRIES:-10}"   # cold-start hosts need retries; set 1 for edge platforms

# 1. OAuth metadata reachable and issuer matches the public URL
#    (catches base-URL / vanity-domain misconfiguration)
for i in $(seq 1 "$retries"); do
  body="$(curl -sf --max-time 20 "$base/.well-known/oauth-authorization-server")" && break
  [ "$i" -eq "$retries" ] && { echo "FAIL: metadata unreachable"; exit 1; }
  sleep 5
done
grep -q "\"issuer\":[[:space:]]*\"$base/\{0,1\}\"" <<<"$body" || { echo "FAIL: issuer mismatch"; exit 1; }

# 2. MCP endpoint gated: unauthenticated POST → 401 (and specifically NOT 421,
#    which means host-origin protection is rejecting your TLS proxy)
code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 20 -X POST \
  -H 'Content-Type: application/json' -d '{}' "$base/mcp")"
[ "$code" = "421" ] && { echo "FAIL: 421 — host-origin protection blocking the proxy"; exit 1; }
[ "$code" = "401" ] || echo "WARN: expected 401, got $code"
echo "smoke: PASS"
```

Source the base URL from a single place in the repo (grep it from the deploy config) so there's exactly one spot where the public URL is written down.

Beyond the scripted smoke: after any change to **auth or storage code**, do one **cold-start persistence check** — authenticate, wait for the platform to scale to zero (typically 5–10 min idle), call a tool, and confirm no re-auth prompt. Skip it on routine redeploys; the scripted smoke covers those.

### Other transferable rules

- **Dependency bumps are deploy-relevant changes.** Deploy and smoke in the same session as any dependency change — framework minor versions have changed security-relevant behavior (fastmcp 3.4.3's host-origin protection turned a routine redeploy into a full outage behind a proxy: every request answered 421). Pin versions so local == deployed.
- **Secrets** enter as platform secrets (encrypted, write-only), piped from the secrets-manager CLI with the trailing newline stripped — the full convention lives in the `cloudflare` skill, if installed, and [../SKILL.md](../SKILL.md)'s gotchas, and applies verbatim to container hosts. To verify a deployed IdP client secret without logging in (on a host whose callback still accepts a caller-supplied state, as FastMCP's OIDCProxy does): hit the OAuth callback with a bogus code — `invalid_grant` (400) means the IdP accepted the secret; `invalid_client` (401) means it's corrupt. The Workers pattern's server-side state record deliberately closes this path.
- **OAuth cutovers need an ordered checklist, written before you start.** Any move that changes the public URL (new domain, proxy front, platform migration) touches the base-URL config, the proxy origin, and the IdP's registered redirect URIs. Order: (1) register the **new** redirect URI in the IdP first, both URIs live during the transition; (2) flip the base URL / proxy origin; (3) run the smoke test **and** a real end-to-end OAuth login with an actual MCP client — smoke tests cannot see the redirect flow; (4) remove the **old** redirect URI **last** — it is your rollback path.
- **Make deploy flows executable, not prose.** Ship a `deploy.sh` (env prep → build → deploy → auto-smoke). Docs describe; commands do — a README paragraph gets paraphrased and skipped, a script gets run.

## Worked example: Modal

The FastMCP stack on Modal, a Python serverless container platform. Modal's value here: the persistent-storage backend can be a **named `modal.Dict`** — no external Redis to run. Swap the bottom store of the `client_storage` stack ([python-fastmcp.md](./python-fastmcp.md#the-cold-start-re-auth-problem-the-big-fastmcp-gotcha)) for a `modal.Dict`-backed `py-key-value-aio` adapter; the Fernet-encryption and prefix wrappers stay identical. (Default `modal.Dict` TTL is 7 days of inactivity — fine for OAuth state, since expiry just means one re-login.)

The app wrapper:

```python
from pathlib import Path
import modal

app = modal.App("my-server")
secrets = [modal.Secret.from_name("my-server-oidc")]
_root = Path(__file__).resolve().parent

image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install_from_pyproject(str(_root / "pyproject.toml"))
    .add_local_file(str(_root / "server.py"), "/app/server.py", copy=True)
    .add_local_file(str(_root / "modal_storage.py"), "/app/modal_storage.py", copy=True)
)


@app.function(image=image, secrets=secrets, timeout=300)
@modal.asgi_app()
def web():
    import sys
    sys.path.insert(0, "/app")
    from server import mcp   # import INSIDE the function — see gotcha below
    return mcp.http_app(transport="streamable-http", stateless_http=True)
```

Deploy: `find . -type d -name __pycache__ -exec rm -rf {} +` then `uv run modal deploy modal_app.py`, then run the smoke test.

Gotchas:

- **`from server import mcp` must live inside `web()`.** Modal injects secrets after the module imports but before the function runs; a top-level import evaluates `_create_auth()` before the secrets exist, and auth silently comes up disabled.
- **`copy=True` on every `add_local_file`/`add_local_dir`**, and clean `__pycache__` before each deploy — lazy mounts can otherwise serve stale bytecode.
- **`MCP_BASE_URL` is the root platform URL, no `/mcp` suffix**, and it must be a *valid* URL or unset — a whitespace value crashes Pydantic's URL validator with a misleading error. First deploy prints the assigned URL; set the secret then redeploy.
- Register `https://<assigned-host>/auth/callback` (OIDCProxy's default callback path) as a redirect URI in the IdP.

## Fronting a container host with a reverse proxy / vanity domain

Pattern: an edge worker 1:1 reverse-proxies every path from the vanity domain to the platform origin. Hard-won rules:

- **fastmcp ≥3.4.3 answers 421 to every request behind a TLS proxy** — its default host-origin protection sees a Host that never matches. Build the ASGI app via `create_streamable_http_app(..., host_origin_protection=False)`, feature-detecting the kwarg with `inspect.signature` so other fastmcp versions don't crash. Disabling is safe *here*: rebinding protection defends localhost servers; this is a public, IdP-authenticated endpoint.
- With a proxy front, `MCP_BASE_URL` is the **public** domain and the IdP must register the public callback — run the OAuth-cutover checklist.
- **Lock the origin**: require proxy credentials on the platform origin (e.g. Modal's `requires_proxy_auth=True` + key/secret on the proxy) so direct origin hits 401 and traffic must pass the proxy.
- The proxy must **set** (not append) `X-Forwarded-*`/`X-Original-Host` headers, overwriting anything the client sent — appended headers are a spoofing vector.

## Choosing a platform (if you get to choose)

| Concern | Container host (Modal, fastmcp.cloud, Cloud Run, ...) | Edge platform (Cloudflare Workers, ...) |
|---|---|---|
| Language | Python (or anything) | JS/TS (V8 isolates) |
| OAuth state persistence | **You must engineer it** (encrypted store + `offline_access` + fixed signing key) | Durable by default (KV) |
| Cold starts | Seconds — smoke tests need retries; users notice | Milliseconds |
| Data access | Network calls to your DB | Platform bindings (D1/R2/KV) are zero-config |
| Real-time token revocation | Introspection TTL = 0 works | Bounded by token `exp` (local JWT verification); shorten token lifetimes |
