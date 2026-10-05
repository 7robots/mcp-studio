# Bearer Token Auth (M2M) on Cloudflare Workers MCP Servers

This document describes the M2M (machine-to-machine) authentication pattern for MCP servers on Cloudflare Workers — the Cloudflare counterpart of FastMCP's `MultiAuth(server=OIDCProxy, verifiers=[…])`.

It enables the MCP Marketplace, `mcp-gateway`, scheduled agents, and other backend callers to authenticate to your MCP server using Okta-issued bearer tokens obtained via `client_credentials` (or Cross App Access) — alongside interactive OAuth for human users.

## The shape of the problem

`@cloudflare/workers-oauth-provider` is designed around tokens **it issues**. After an upstream OAuth handshake (e.g., with Okta), the library issues its own opaque bearer tokens (`${userId}:${grantId}:${secret}`) that it validates locally against KV-stored records. This is the right model for interactive clients (Claude Desktop, Claude Code) but doesn't accommodate clients that hold Okta-issued tokens directly — and the library has no `client_credentials` grant of its own.

The fix: a **dispatcher in front of OAuthProvider** that inspects incoming `Authorization: Bearer` headers and chooses an auth path:

- **Okta-issued M2M JWT** (signature verifies against the issuer's JWKS, issuer matches, not expired, carries one of this server's scopes, has `cid` and `sub`) → bypass OAuthProvider, run the actor gate and the pre-handler scope gate, then call the stateless MCP handler directly with the verified claims as `AuthInfo`.
- **Worker-issued interactive token, or anything else** → delegate to OAuthProvider, which validates against KV or returns 401; its `apiHandler` then calls the same handler with an `AuthInfo` built from the stored Okta props.

## Architecture

```
                                            ┌─ M2M path ─→ actor gate → scope gate → handler.fetch(req, {authInfo}) → tools
                                            │   (verify JWT locally against the issuer's JWKS; claims → AuthInfo)
Authorization: Bearer <token>  ──→ Dispatcher
   on POST /mcp                             │
                                            └─ interactive fallback ─→ OAuthProvider ─→ apiHandler
                                                                       (KV-stored token check     │
                                                                        or 401 + WWW-Authenticate) │
                                                                                                   ↓
                                                                            handler.fetch(req, {authInfo})
                                                                                    (props → AuthInfo)
```

The dispatcher lives in `src/index.ts`; local verification in `src/jwt.ts`; bearer extraction and the verdict type in `src/m2m.ts`; the gateway assertion in `src/actor.ts`. All are drop-in from the template and under conformance.

## Why local verification, and not introspection

The pattern introspected every M2M bearer against Okta's `/v1/introspect` until 2026-08-21 (fleet) / 2026-08-24 (template), caching the verdict in KV. It was replaced for two reasons, one of which is decisive:

- **Introspection cannot validate a Cross App Access token at all.** Okta's `/v1/introspect` returns `{"active": false}` when the client making the request is not the client the token was issued to. Under Cross App Access the token's client is the AI agent's *workload principal* (`wlp…`), and a Worker cannot authenticate as that — the identity signs with the agent's private key. No configuration fixes it; it was verified empirically against a live tenant with a token confirmed valid by decoding it.
- **It was expensive and hazardous.** One Okta round trip per distinct token on the hot path, a KV read, and a KV write per token against an account-wide write budget — and while the cache stored inactive verdicts, 1,000 unauthenticated requests with random bearers could exhaust the whole account's KV writes and break OAuth for every Worker on it.

Okta's custom authorization servers issue JWT access tokens (RS256, an `scp` array, `cid`), so the signature tells the server everything introspection did. `src/jwt.ts` verifies with `jose`'s `createRemoteJWKSet` against `${OKTA_ISSUER}/v1/keys`, cached per issuer for the isolate's lifetime: no shared secret, no network on the hot path beyond the cached key set, no KV.

What it gives up is revocation: a revoked token stays valid until its `exp`. See [architecture.md § Token lifetime and revocation](./architecture.md#token-lifetime-and-revocation).

## Mapping to FastMCP

| FastMCP concept | Cloudflare equivalent |
| --- | --- |
| `MultiAuth(server, verifiers)` | The dispatcher in `src/index.ts` |
| `IntrospectionTokenVerifier(introspection_url=…, cache_ttl_seconds=300)` | `tryOktaJwt()` in `src/jwt.ts` — local JWT verification, no introspection, no cache to tune |
| `JWTVerifier(jwks_uri=…, issuer=…, audience=…)` | The closest FastMCP analogue to what this pattern actually does |
| Scope check (often implicit in FastMCP) | Explicit `OKTA_M2M_SCOPE` plain-text var, a space-separated ANY-OF list of this server's own scopes, then per-tool `requireScope` |
| `client_id`/`client_secret` for introspection | None needed on the M2M path. `OKTA_CLIENT_SECRET` remains, for the interactive code exchange in `src/auth.ts` |

## Implementation (drop-in)

**The code is not duplicated here.** Read it in a rendered template (`mcp-studio pattern render --out <dir>`), which is type-checked and covered by 229 tests:

| File | What to read it for |
| --- | --- |
| `src/jwt.ts` | `tryOktaJwt` (issuer pinning, optional `OKTA_M2M_AUDIENCE` pinning, `exp` required, the ID-JAG type-confusion guard), `decideFromClaims` (any-of scope check, `cid` and `sub` required), `extractScopes` (`scp` array or `scope` string) |
| `src/m2m.ts` | `extractBearer`, `looksWorkerIssued`, the `M2MVerdict` type — and a header recording what used to live there |
| `src/index.ts` | the dispatcher: M2M-bearer-first, the actor gate, the pre-handler scope gate, the fail-closed props check, and the `AuthInfo` mapping for each path |
| `test/jwt.test.ts`, `test/m2m.test.ts`, `test/m2m.workerd.test.ts` | what each of those is supposed to do, stated as assertions — the workerd suite drives real routes against a fake JWKS served by `outboundService` |

An earlier version of this document inlined the M2M code. That copy drifted and
ended up teaching things the template had already fixed, which is worth
recording because each is a live trap:

- **A single required-scope string.** The scope check is an **any-of list**
  (`OKTA_M2M_SCOPE = "<slug>:read <slug>:write"`): one server accepts
  `<slug>:read` *or* `<slug>:write` at the door and enforces per-tool scopes
  further in. Comparing the token's scope array against the whole
  space-separated string is never true on a multi-scope server — a bug that
  hides on a single-scope server, where the two readings coincide.
- **No issuer pinning.** A signature is only as trustworthy as the key set it
  was checked against; pinning `issuer` (and fetching the JWKS from that
  issuer only) is what stops a token minted by another authorization server
  being accepted.
- **Audience pinning is deliberately optional.** `OKTA_M2M_AUDIENCE` is usually
  unset — while a fleet shares one Okta authorization server, `aud` is identical
  on every server (`{{okta_audience}}`), so pinning it separates nothing. It
  becomes load-bearing the moment each server gets its own custom AS.
- **`exp` must be required.** On a path with no introspection backstop, a signed
  token without `exp` would otherwise validate forever.
- **An ID-JAG is not an access token.** The same Worker is an EMA authorization
  server; an ID-JAG (`typ: oauth-id-jag+jwt`) from the same issuer carrying `cid`
  and a matching `aud` would otherwise verify as a bearer. `tryOktaJwt` refuses
  that `typ` by name (RFC 8725 §3.11).
- **The `looksWorkerIssued` short-circuit is cost, not correctness.** A
  Worker-issued interactive token is not a JWT, so sending it to `jwtVerify` is a
  guaranteed failure on every interactive request. Removing the check breaks no
  test — the token falls through to OAuthProvider either way — and that is the
  right outcome; do not add a test implying otherwise.

The dispatcher also **fails closed**: a request that reaches `apiHandler` with no
usable props gets a 401 rather than being forwarded with `authInfo === undefined`.

Note the `isMcpPath()` guard inside `apiHandler`: OAuthProvider matches `apiRoute` by **prefix**, so `/mcpfoo` would otherwise reach the handler. The guard keeps the served surface identical on both paths.

## Configuration

In `wrangler.toml` (the rendered template fills the Okta values from the instance):

```toml
[vars]
OKTA_CLIENT_ID = "{{okta_client_id}}"
OKTA_DOMAIN = "{{okta_domain}}"
OKTA_ISSUER = "{{okta_issuer}}"
OKTA_SCOPES = "openid profile email offline_access"  # for the interactive flow
OKTA_M2M_SCOPE = "<slug>:read <slug>:write"  # per-server ANY-OF list
# OKTA_M2M_AUDIENCE = "…"  # only once this server has its own authorization server
```

Secrets (injected from 1Password via `printf '%s' "$(op read …)" | npx wrangler secret put`, never committed):

| Secret | Purpose |
| --- | --- |
| `OKTA_CLIENT_SECRET` | The interactive Okta code exchange in `src/auth.ts`. **Not used on the M2M path** |
| `REQUEST_STATE_KEY` | MRTR confirmation sealing (≥32 bytes) |

`COOKIE_ENCRYPTION_KEY` is not needed with provider 0.10.3+, and there is no M2M cache TTL to set any more (an `OKTA_M2M_CACHE_TTL_SECONDS` left in an old `wrangler.toml` is read by nothing — delete it).

`worker-configuration.d.ts` declares these on one shape exposed as both global `Env` and `Cloudflare.Env` — keep the template's dual declaration; it is load-bearing for the workerd test pool.

## Okta side: no changes if you reuse the existing service app

The Worker verifies signatures against the issuer's public JWKS, so it needs **no Okta credential at all** for M2M — only the issuer URL. **No Okta admin change is required** to wire up M2M to a new MCP server beyond:

1. This server's scopes exist on the authorization server, and the M2M access-policy rule grants them (a server whose scopes do not exist has no working M2M path at all).
2. The M2M service app is allowed those scopes, so it can mint tokens carrying at least one of them.

Neither scope may be a **default** scope — a default scope is added to every `client_credentials` token whether requested or not, so it distinguishes nothing. The ordered recipe is [first_deploy_cutover.md § 1](./first_deploy_cutover.md#1-okta-objects-before-any-deploy); the `okta-admin` skill, if installed, carries it out over the API.

If you need a separate M2M service app per caller (uncommon), create it as an "API Services" app with the `client_credentials` grant and the relevant scopes; the Worker doesn't need its credentials — only the caller that mints tokens does.

## Verifying the M2M path was actually taken

Call `whoami` (the template's stub tool — keep a version of it) with the M2M bearer. `extra.auth_path` reads:

- `"m2m"` — the dispatcher took the M2M branch;
- `"m2m+actor"` — M2M, and a gateway `X-MCP-Actor` assertion verified (with `on_behalf_of`, `gateway_purpose`, `gateway_run_id`);
- `"interactive"` / `"enterprise"` — the bearer was Worker-issued, so OAuthProvider handled it.

There is no KV trace to inspect: local verification writes nothing. If the Okta System Log shows `token.introspect` events for this server, something is still running the removed design.

## Caller identity: `AuthInfo` on every path

The old limitation here — "the M2M path can't tell tools who's calling, because it bypasses the machinery that injects `props`" — is **resolved**. With the stateless handler there is nothing to inject into: `fetch(request, { authInfo })` takes the caller's identity as an argument, so the dispatcher supplies it on whichever path it took.

```typescript
interface AuthInfo {
  token: string;          // the bearer the caller actually presented
  clientId: string;
  scopes: string[];
  expiresAt?: number;
  resource?: URL;
  extra?: Record<string, unknown>;
}
```

Conventions the template follows:

| Field | M2M path | Interactive path | Enterprise path (ID-JAG) |
| --- | --- | --- | --- |
| `token` | the Okta-issued bearer | the Worker-issued bearer | the Worker-issued bearer |
| `clientId` | the token's `cid` (the service app, or under Cross App Access the agent's workload principal) | **the OAuth client's id**, copied into props at `completeAuthorization` time | the `client_id` the IdP stamped into the assertion |
| `scopes` | from the verified token's `scp` (`<slug>:read`, …) | the **grant's** scopes, copied into props — see the correction below | from the assertion's `scope` claim, intersected with the supported scopes |
| `expiresAt` | the token's `exp` | omitted | assertion `exp` |
| `extra.auth_path` | `"m2m"`, or `"m2m+actor"` with a verified gateway assertion | `"interactive"` | `"enterprise"` |
| `extra.sub` | the token's `sub` (for `client_credentials`, Okta sets it to the client id) | Okta `sub` | assertion `sub` |
| `extra.email` / `extra.name` | — | from `/userinfo`, stored in props | from the assertion, when present |
| other `extra` | `on_behalf_of`, `gateway_purpose`, `gateway_run_id` (actor only) | — | `enterprise_issuer` |

**Two corrections to earlier versions of this table**, both of which shipped to a fleet before being caught:

- `clientId` on the interactive path was documented (and implemented) as the user's Okta `sub`. That conflates the OAuth **client** with the **human**, and makes per-client consent and per-client revocation meaningless — every client looks like the same principal. It must be the client id, which means copying it into props at `completeAuthorization` time.
- `scopes` on the interactive path was `[]`. That is harmless only while nothing enforces scopes; the moment a tool calls `requireScope`, every human caller is denied. OAuthProvider exposes only `ctx.props` to the `apiHandler`, never the grant's scopes, so the grant's scopes must be copied into props too — and the value recorded on the grant and the value in props must be the **same array**, or the `/token` response and the audit trail disagree with what the caller can actually do.

Tools read it as `ctx.http.authInfo`:

```typescript
server.registerTool("whoami", { description: "…", inputSchema: z.object({}) }, async (_args, ctx) => {
  const auth = ctx.http?.authInfo;
  const path = (auth?.extra as { auth_path?: string } | undefined)?.auth_path;
  // path === "m2m" | "m2m+actor" | "interactive" | "enterprise" | undefined (unauthenticated)
});
```

**Branch on `extra.auth_path` before trusting `clientId` or `scopes`** — they mean different things per path (a service app, an OAuth client acting for a human, or a client the enterprise IdP vouched for). That's the whole point of stamping it.

On the enterprise path in particular, **scopes come from the assertion, never from the request**. Reading a requested `scope` parameter there lets a client grant itself anything: an ID-JAG carrying no scope claim would otherwise be honoured at whatever the client asked for. See [enterprise_auth.md](enterprise_auth.md).

The upstream Okta access token deliberately stays in OAuthProvider's props and is **not** copied into `AuthInfo`. If a tool needs to call Okta-protected APIs on the user's behalf, map it into `extra` in `apiHandler` yourself, knowing it then travels with the AuthInfo.

## Smoke testing M2M

```bash
# 1. Mint a token via client_credentials (run on a host with the service app secret,
#    held in 1Password — never paste it into a transcript).
TOKEN=$(curl -s -X POST "{{okta_issuer}}/v1/token" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -u "$OKTA_M2M_CLIENT_ID:$OKTA_M2M_CLIENT_SECRET" \
  -d "grant_type=client_credentials" \
  --data-urlencode "scope=<slug>:read" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['access_token'])")

# 2. Call the MCP server.
curl -X POST "https://<slug>.{{domain_suffix}}/mcp" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","method":"tools/list","id":1}'

# Expect 200 with the server's tool list.
```

Then the negative checks that make the positive one mean something: a token carrying only **another** server's scope is refused here (401); a read-only token calling a write tool is refused with the required scope named (403 from the pre-handler gate when `Mcp-Name` is sent, an `isError` result from `requireScope` when it is not).

## Cross-references

- [agent_guide.md](./agent_guide.md) — full step-by-step build/conversion guide; M2M verification is Step 7 there.
- [architecture.md](./architecture.md) — the stateless model, storage story, token lifetimes, and where `AuthInfo` fits.
- [platform_facts.md](./platform_facts.md#okta) — Okta's per-AS `aud`, default scopes, and policy-rule matching.
