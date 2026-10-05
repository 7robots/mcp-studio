# Python / FastMCP MCP Servers (cold storage)

> **⚠️ Not the current path.** This pattern builds MCP servers in TypeScript on Cloudflare Workers (see [../SKILL.md](../SKILL.md)); the FastMCP/Python deployment path was retired 2026-07. This file preserves the hard-won FastMCP lessons in case the stack returns. **Currency note:** it describes **FastMCP 3.x**, which predates the stateless 2026-07-28 MCP spec — it still has the sessionful model, the `initialize` handshake, and the `stateless_http=True` opt-in. FastMCP 4 was expected to incorporate the new spec natively; if reviving this stack, first check whether FastMCP 4 has shipped — the `<4` pin, the `stateless_http` flag, and possibly the auth/`client_storage` wiring will need updating.

Building MCP servers with **FastMCP** (`fastmcp`), managed with **uv**. General tool-design guidance (names, descriptions, schemas, pagination, errors) lives in [../SKILL.md](../SKILL.md) and applies unchanged here.

## Dependencies

```toml
# pyproject.toml
dependencies = [
    "fastmcp>=3.4.3,<4",
    # Starlette is transitive via fastmcp, but pin it explicitly:
    # CVE-2026-48710 (BadHost, Host-header auth bypass) is fixed in 1.0.1,
    # and older fastmcp lower bounds can drift below it.
    "starlette>=1.0.1",
]
```

After editing dependencies run `uv lock` and verify the resolution:

```bash
uv lock --upgrade-package starlette
grep -A1 '^name = "starlette"' uv.lock   # must be >= 1.0.1
```

Pin `fastmcp` with an upper bound and keep local and deployed versions identical — fastmcp has changed security-relevant behavior in minor releases (3.4.3 enabled host-origin protection by default, which can 421 every request behind a TLS proxy; see [modal-deploy.md](./modal-deploy.md#fronting-a-container-host-with-a-reverse-proxy--vanity-domain)). Dependency bumps are deploy-relevant changes: deploy and smoke-test in the same session as the bump.

## Server shape

```python
import os
from fastmcp import FastMCP

mcp = FastMCP(
    "my-server",
    instructions="Short description of what this server does and when to use its tools.",
    auth=_create_auth(),  # None → unauthenticated (local dev); see below
)


@mcp.tool()
async def list_things(limit: int = 20, offset: int = 0, response_format: str = "json") -> str:
    """List things, paginated. Use when the user asks what things exist."""
    ...
```

- Type-hint every parameter and give defaults where sensible — FastMCP derives the JSON schema from the signature, so the hints *are* the contract the LLM sees.
- The docstring is the tool description; write it for the model (what it does + when to call it).
- Ship an LLM-facing usage guide via FastMCP's `SkillProvider` (or a plain resource) — the markdown README the model reads to use the server well.

## HTTP entry point

For any deployment behind OAuth, build the ASGI app with `http_app()` — `mcp.run()` does not mount the OAuth routes:

```python
if __name__ == "__main__":
    import uvicorn
    app = mcp.http_app(transport="streamable-http", stateless_http=True)
    uvicorn.run(app, host="0.0.0.0", port=8000)
```

`stateless_http=True` is the stateless-first posture: no in-process session state, which is what lets scale-to-zero hosting work at all.

## Auth: `MultiAuth` = interactive OAuth + bearer tokens

FastMCP composes the two credential types cleanly:

```python
def _create_auth():
    """Okta shown as the worked example; any OIDC IdP with an introspection
    endpoint (Auth0, Entra ID, ...) slots in the same way."""
    client_secret = os.environ.get("OIDC_CLIENT_SECRET")
    if not client_secret:
        return None  # auth is opt-in: unset env vars → unauthenticated local dev

    from fastmcp.server.auth import MultiAuth
    from fastmcp.server.auth.oidc_proxy import OIDCProxy
    from fastmcp.server.auth.providers.introspection import IntrospectionTokenVerifier

    client_id = os.environ["OIDC_CLIENT_ID"]
    issuer = os.environ["OIDC_ISSUER"]          # e.g. https://<org>.okta.com/oauth2/default
    base_url = os.environ["MCP_BASE_URL"]       # public URL of the deployed server

    oidc_proxy = OIDCProxy(
        config_url=f"{issuer}/.well-known/openid-configuration",
        client_id=client_id,
        client_secret=client_secret,
        base_url=base_url,
        jwt_signing_key=os.environ.get("JWT_SIGNING_KEY") or None,
        extra_authorize_params={"scope": "openid profile email offline_access"},
        allowed_client_redirect_uris=[
            "http://localhost:*",
            "http://127.0.0.1:*",
            # plus the hosted MCP clients you support, e.g. "https://claude.ai/*"
        ],
        client_storage=_build_client_storage(prefix="my-server"),  # see next section
    )

    introspection_verifier = IntrospectionTokenVerifier(
        introspection_url=f"{issuer}/v1/introspect",
        client_id=client_id,
        client_secret=client_secret,
        cache_ttl_seconds=300,  # revocation lag; 0 = real-time at higher IdP load
    )

    return MultiAuth(server=oidc_proxy, verifiers=[introspection_verifier])
```

- `server=` provides the OAuth routes **and** verifies tokens; `verifiers=` only verify. Verification order: server first, then each verifier — a failure falls through to the next.
- `OIDCProxy` issues its **own** JWTs to MCP clients; each carries a JTI claim mapping to the encrypted upstream IdP token in `client_storage`. Set a fixed `JWT_SIGNING_KEY` in production — the auto-generated key on Linux is ephemeral, so issued tokens die on every restart.
- `IntrospectionTokenVerifier` validates IdP-issued bearers (RFC 7662) — works with opaque tokens, supports revocation within the cache TTL, needs no extra config beyond the OAuth credentials. Know its limit before reviving it: Okta introspection reports a token **inactive** unless the caller is the client it was issued to, so it cannot validate Cross App Access tokens at all — the Workers pattern moved to local JWT verification for that reason (see [bearer_token_auth.md](./bearer_token_auth.md#why-local-verification-and-not-introspection)); FastMCP's `JWTVerifier` is the equivalent here. M2M callers get tokens from the IdP's client-credentials flow (a separate "API Services"/service application, with an access policy that allows that grant).

## The cold-start re-auth problem (the big FastMCP gotcha)

Serverless hosts scale containers to zero on idle. `OIDCProxy` defaults `client_storage` to an **in-memory** store, so every cold start wipes the JTI → upstream-token mapping and **every user re-authenticates on every cold start**. The log signature is `JTI mapping not found (token may have expired)` right after a restart.

Two independent fixes, **both required**:

1. **Request `offline_access`** (in `extra_authorize_params`) *and* enable the **Refresh Token grant** in the IdP's access policy — otherwise no upstream refresh token is ever issued and users re-auth every ~1 hour regardless of storage.
2. **Persistent, encrypted `client_storage`**, via the `py-key-value-aio` stack:

```python
def _build_client_storage(prefix: str):
    """Encrypted, namespaced persistent client_storage; None if not configured."""
    backing_url = os.environ.get("STORAGE_URL")  # e.g. rediss://...
    if not backing_url:
        return None  # safe: falls back to in-memory (fine for local dev)

    encryption_key = os.environ.get("STORAGE_ENCRYPTION_KEY")
    if not encryption_key:
        raise ValueError(
            "STORAGE_URL is set but STORAGE_ENCRYPTION_KEY is missing. "
            "Refusing to store OAuth tokens unencrypted."
        )

    from cryptography.fernet import Fernet
    from key_value.aio.stores.redis import RedisStore
    from key_value.aio.wrappers.encryption import FernetEncryptionWrapper
    from key_value.aio.wrappers.prefix_collections import PrefixCollectionsWrapper

    return FernetEncryptionWrapper(
        key_value=PrefixCollectionsWrapper(
            key_value=RedisStore(url=backing_url),
            prefix=prefix,
        ),
        fernet=Fernet(encryption_key.encode()),
    )
```

Deps: `py-key-value-aio[redis]>=0.2.0`, `cryptography>=42.0.0`.

Notes on the stack:

- The **bottom store is swappable** — Redis is the common choice, but any `py-key-value-aio` `BaseStore` works (e.g. a `modal.Dict`-backed store on Modal; see [modal-deploy.md](./modal-deploy.md)). The encryption and prefix wrappers stay identical.
- The **prefix** namespaces one server's OAuth state, so multiple servers can share a single Redis database (each passes a unique prefix — the repo name is a good choice). The Fernet key may be shared across servers.
- Losing the Fernet key is annoying, not catastrophic: stored tokens become unreadable and every user re-authenticates once. Generate and store it in a secrets manager:
  ```bash
  python -c "from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())"
  ```
- Bearer tokens don't need any of this — introspection is stateless. Only the interactive flow has state to persist.

## Environment variables (auth-enabled deployment)

| Variable | Purpose |
|---|---|
| `OIDC_CLIENT_ID` / `OIDC_CLIENT_SECRET` | Used by both the proxy and the introspection verifier |
| `OIDC_ISSUER` | Authorization-server issuer URL |
| `MCP_BASE_URL` | Public URL of the deployed server (OAuth redirects are built from it) |
| `JWT_SIGNING_KEY` | Fixed key for the proxy's own JWTs — required in production |
| `STORAGE_URL` | Persistent client_storage backend (production) |
| `STORAGE_ENCRYPTION_KEY` | Fernet key; required whenever `STORAGE_URL` is set |

## Testing

Run everything through uv (`uv run pytest`). Use FastMCP's in-process client against the server object — no network, no deployment:

```python
import pytest
from fastmcp import Client
from server import mcp


@pytest.mark.asyncio
async def test_list_things_defaults():
    async with Client(mcp) as client:
        result = await client.call_tool("list_things", {})
        assert result  # defaults applied, no validation error
```

Cover tool listing, defaults, validation failures, and the pure data-layer functions directly. Auth wiring is best verified against a deployed instance (see [modal-deploy.md](./modal-deploy.md)'s smoke test and cold-start check) — the interesting failures are all environmental.

## Troubleshooting quick table

| Symptom | Likely cause |
|---|---|
| Users re-auth every ~1 hour | `offline_access` missing from scopes, or Refresh Token grant not allowed in the IdP policy |
| Users re-auth on every cold start | `client_storage` not persistent (check `STORAGE_URL`/`STORAGE_ENCRYPTION_KEY`); look for `JTI mapping not found` after restarts; or ephemeral `JWT_SIGNING_KEY` |
| IdP 404s on the authorize URL | Wrong IdP hostname — with Okta, the `-admin` console domain has no OAuth endpoints; use the org domain. Verify: `curl <issuer>/.well-known/openid-configuration` |
| Every request 421s behind a proxy | fastmcp ≥3.4.3 host-origin protection — see [modal-deploy.md](./modal-deploy.md#fronting-a-container-host-with-a-reverse-proxy--vanity-domain) |
| Bearer tokens rejected | Service app's access policy doesn't allow client-credentials; token expired; or introspection caching masking a fix (`cache_ttl_seconds=0` to rule out) |
| OAuth routes 404 | Server started with `mcp.run()` instead of `http_app()` |
