# Template — Cloudflare Workers MCP server

Copy this directory into a new server's home, fill in the placeholders, replace the stub tools, and push.

Stack: `@modelcontextprotocol/server` v2 with `createMcpHandler` — MCP protocol **2026-07-28**, stateless. No sessions, no `initialize` handshake, no Durable Object. A fresh `McpServer` is built per HTTP request; 2025-era clients are served by the SDK's legacy stateless fallback.

Auth: Okta upstream, with a **mandatory consent gate**, CIMD client identification, per-server scopes enforced in two layers, and Enterprise-Managed Authorization (ID-JAG) available. `npm run ci` ships **229 tests** before you write a single tool of your own.

## How to use (agentic workflow)

Open a Claude Code session with the Cloudflare MCP server connected. Tell the agent: "use the mcp-server-dev skill to set up a new server at `~/GitHub/<server-name>`." The agent will:

1. `mcp-studio pattern render --out ~/GitHub/<server-name>` (renders this template with the instance's values)
2. Work the find-and-replace checklist below, then replace the stub tools in `src/mcp.ts` and the data layer in `src/data.ts`.
3. Create Cloudflare resources via `mcp__cloudflare__execute` (KV namespace, D1 database, placeholder Worker), then inject secrets from 1Password (`printf '%s' "$(op read …)" | npx wrangler secret put`).
4. Create the two Okta scopes, add them to both policy rules, and add the redirect URI — over the API with the `okta-admin` skill's new-server recipe (each write prompts you).
5. `git init`, `gh repo create --private`, push.
6. Walk you through the one-time GitHub App authorization in the Cloudflare dashboard.
7. Wire Workers Builds via API and trigger the first deploy.
8. Smoke test and verify.

See [`references/agent_guide.md`](../references/agent_guide.md) for the full step-by-step.

## What's in here

| File | Action |
| --- | --- |
| `src/index.ts` | **Drop-in.** Dispatcher: M2M bearer first, OAuth fallback; pre-handler scope gate; `resourceMetadata`, CIMD and EMA wiring. |
| `src/auth.ts` | **Drop-in.** `/authorize` → Okta → `/callback` → `POST /consent`. Server-side state, per-flow CSRF cookie, remembered-consent lookup. |
| `src/oauth-state.ts` | **Drop-in.** Opaque KV state bound to a `__Host-` cookie, the pending-consent record, and the remembered decision. **Edit only the two cookie prefixes** — keeping the `__Host-` prefix, which the state binding relies on. |
| `src/consent.ts` | **Customize `SCOPE_HELP` only.** The consent and denial pages, escaping, invisible-character stripping, strict CSP. |
| `src/scopes.ts` | **Customize.** `READ_SCOPE`/`WRITE_SCOPE`, the `TOOL_SCOPES` map (one entry per tool), and `mcpToolName`. The enforcement helpers are drop-in. |
| `src/ema.ts` | **Drop-in.** ID-JAG trusted-issuer parsing and claim mapping. Inert until `EMA_TRUSTED_ISSUERS` is set. |
| `src/m2m.ts` | **Drop-in.** Bearer extraction and the M2M verdict type. Introspection and its KV cache are gone. |
| `src/jwt.ts` | **Drop-in.** Local verification of M2M access tokens against the issuer's JWKS: signature, issuer and (optional) audience pinning, expiry. No network on the hot path beyond the cached JWKS, and no KV. |
| `src/resource.ts` | **Drop-in.** RFC 8707 / RFC 9728 identifiers derived from `PUBLIC_MCP_URL`. Edit `FALLBACK_PUBLIC_MCP_URL`. |
| `src/actor.ts` | **Drop-in; wired in, enforcement off.** `src/index.ts` calls it on the M2M path: a PRESENT `X-MCP-Actor` assertion from mcp-gateway is always verified, but one is REQUIRED only when `REQUIRE_GATEWAY_ACTOR` is set. Uncomment `GATEWAY_ISSUER` / `GATEWAY_JWKS_URL` in `wrangler.toml` to use it. |
| `src/mcp.ts` | **Customize.** Replace the stub tools (`echo`, `list_examples`, `whoami`) inside `buildServer()`. Keep the `requireScope` guard as the first statement of every handler, the `registerResource` skill block, and the `ServerProps` export. |
| `src/data.ts` | **Customize.** Replace the example D1 helpers with your data layer, or delete it. |
| `src/skill.ts` | **Customize.** The LLM-facing usage guide, exposed as `skill://<server-name>`. |
| `test/oauth-state.test.ts` | **Drop-in sample.** State, CSRF, consent keys, loopback canonicalization, the mint-order guard. Retarget the scope names. |
| `test/consent-ema.test.ts` | **Drop-in sample.** Consent-page rendering, escaping, stripping, EMA mapping. Retarget the scope names; the `SCOPE_HELP` prose needs no test change. |
| `test/flow.workerd.test.ts` | **Drop-in sample.** `/authorize` → `/callback` → `/consent` through `SELF.fetch()`. Retarget scope names. |
| `test/m2m.test.ts`, `test/resource.test.ts`, `test/actor.test.ts` | **Drop-in samples.** Keep; `actor.test.ts` covers the wired-in `src/actor.ts`. |
| `test/mcp.test.ts` | **Customize.** Retarget the tool names and add a `VALID_ARGS` entry per tool. Keep the `describe.each(TOOL_SCOPES)` guard block — it covers every tool automatically. |
| `vitest.config.ts` | **Drop-in.** Two projects (`unit`, `workerd`) plus the Okta `outboundService` stub. Add an arm per upstream host you call. |
| `wrangler.toml` | **Customize.** Worker name, KV/D1 ids, Okta vars. A real `wrangler.toml`, not `.example` — see below. |
| `worker-configuration.d.ts` | **Customize.** Env types. Keep the dual `Env` / `Cloudflare.Env` declaration — it is load-bearing. Note `COOKIE_ENCRYPTION_KEY` and `OKTA_DOMAIN` are declared **optional** because nothing reads them: `workers-oauth-provider` derives props-encryption keys from the tokens themselves, and `src/` uses only `OKTA_ISSUER`. |
| `tsconfig.json`, `tsconfig.test.json` | **Drop-in.** Two projects on purpose; don't merge them. |
| `package.json` | **Customize** `name` and `description`. Keep the pinned versions. |
| `.gitignore` | **Drop-in.** |

### Why `wrangler.toml` and not `wrangler.toml.example`

`@cloudflare/vitest-pool-workers` **reads `wrangler.toml`** to build the workerd test environment — bindings, vars and compatibility flags all come from it, so `test/flow.workerd.test.ts` exercises the same configuration you deploy. Pointing the pool at a `.example` file does not work: miniflare never picks up `main` and every route test fails with `requires poolOptions.workers.main to be set`. Deploying with the placeholders still fails fast, because the KV and D1 ids are not real.

## Find-and-replace checklist

**Case matters, in two places for real reasons.** A placeholder sitting in an `https://` *hostname* must be lowercase, because `new URL()` case-normalizes the host — an uppercase one silently changes value on the way through `resourceUrls()`, so the advertised RFC 8707 identifier stops matching the configured string. And the wrangler `name` must be lowercase-alphanumeric-with-dashes, or wrangler rejects the config outright and the whole workerd test project reports "no tests". (Two things this is *not* about: npm accepts a mixed-case `name` for a `private: true` package, and `skill://` is a non-special scheme so `new URL()` preserves its case exactly. Keeping those lowercase is consistency, not necessity.) Leave the lowercase placeholders lowercase.

- `replace-with-your-server-name` → your server name (Worker name, `package.json` name, the `skill://` resource id)
- `replace-with-tenant` → your Okta tenant subdomain (appears in `OKTA_DOMAIN` and `OKTA_ISSUER`)
- `replace.{{domain_suffix}}` → your server's public hostname (`PUBLIC_MCP_URL`, `FALLBACK_PUBLIC_MCP_URL`)
- `REPLACE:read` / `REPLACE:write` → your two scope names (`src/scopes.ts`, `SCOPE_HELP` in `src/consent.ts`, `OKTA_M2M_SCOPE` in `wrangler.toml`, and the test fixtures). The `SCOPE_HELP` *prose* needs no test change — `test/consent-ema.test.ts` asserts against the exported map, so rewriting the text cannot break CI.
- `__Host-REPLACE_oauth_csrf-` / `__Host-REPLACE_consent_csrf-` → per-server cookie prefixes, in `src/oauth-state.ts` **only** — `test/flow.workerd.test.ts` imports the constants rather than restating them. **Keep the `__Host-` prefix**: it is what makes the browser refuse the cookie unless it is secure, host-locked and path-`/`, which `takeState` relies on.
- `REPLACE-WITH-RESOURCE-NAME` → the human-readable server name shown on the consent page
- `REPLACE-WITH-SERVER-NAME` → the `McpServer` name string (also asserted in `test/mcp.test.ts`)
- `REPLACE-WITH-DESCRIPTION`, `REPLACE — short description…` → your description and MCP `instructions`
- `REPLACE-WITH-OKTA-CLIENT-ID` → your Okta app's client_id
- `REPLACE-WITH-D1-UUID`, `REPLACE-WITH-KV-NAMESPACE-ID` → binding ids
- `REPLACE-WITH-D1-DATABASE-NAME` → the D1 database's name (distinct from its uuid)
- **`routes` in `wrangler.toml`** → either uncomment it with the same host as `PUBLIC_MCP_URL`, or point `PUBLIC_MCP_URL` at your `*.workers.dev` URL. They must agree: `PUBLIC_MCP_URL` is the RFC 8707 resource identifier and the base of the RFC 9728 metadata URL, and OAuthProvider validates a client's `resource` parameter against it — so a custom host with no route attached means discovery resolves to a host this Worker does not serve, and every client fails at the first step.
- `REPLACE-WITH-SERVER-SLUG` → the `resourceKey()` fallback in `src/auth.ts`
- **`src/skill.ts`** → the LLM-facing description and usage guide. Listed
  explicitly because **no test asserts it**: a checklist-exact instantiation once
  passed its whole suite with this file untouched, shipping the words "REPLACE —
  the LLM-facing usage guide" to every model that read the server.
- **`REQUEST_STATE_KEY`** → a Worker secret, at least 32 bytes, for the MRTR
  confirmation on the destructive tool. Not a string to swap — generate it and
  store it:
  ```sh
  SECRET=$(openssl rand -base64 48 | tr -d '\n')
  op item create --category="API Credential" --title="<worker-name>" \
    --vault {{op_vault}} "REQUEST_STATE_KEY[concealed]=$SECRET"
  printf '%s' "$(op read 'op://{{op_vault}}/<worker-name>/REQUEST_STATE_KEY')" \
    | npx wrangler secret put REQUEST_STATE_KEY
  ```
  Without it `delete_example` **refuses** rather than deleting unconfirmed — a
  missing secret must not be more permissive than a present one.

Then run the mechanical check, which is not part of `npm run ci` because the
template's own CI has to pass while the placeholders are still in place:

```sh
npm run check:placeholders
```

Then: one entry per tool in `TOOL_SCOPES`, one `SCOPE_HELP` line per scope, and a `requireScope` guard opening every tool handler.

## Two scopes, not one

The template ships `REPLACE:read` and `REPLACE:write` deliberately. With a single scope, `consentCovers`, the step-up `insufficient_scope` response and the broader-scope re-prompt are all unreachable — the branches exist but nothing can exercise them, and the day a second scope is added they have never once run. Two scopes keeps them honest from the first commit.

Scopes are also the **only** separation between servers that share one Okta authorization server: Okta sets `aud` per authorization server, so every server on it sees the same audience. Until each server has its own custom AS, a token's scopes are what stop it working everywhere.

## The consent gate

Required, with no flag to switch it off — a flag that can switch it off will be off. What keeps it from being tedious is that a decision is remembered per **(user, client, resource, redirect URI)**, scope-bound: asking for more, or moving the destination, re-prompts.

Three things in there are less obvious than they look:

- **The redirect URI is part of the key** because CIMD makes `redirect_uris` mutable. A DCR client cannot change its own (registration is `POST`-only, no update endpoint), but a CIMD `client_id` is a URL whose document is re-fetched on every authorization — so whoever controls that URL later can move the destination, and without it in the key, codes keep being minted to the new one for the rest of the 90-day window with no re-prompt.
- **Loopback destinations are keyed without the port**, matching the provider's own RFC 8252 rule (a native app cannot reserve a port). Keying on it would re-prompt a local client on every launch with a destination reading `127.0.0.1:52222`, which no user can judge. The page says `127.0.0.1 (any port)` so the disclosure matches the grant.
- **`/consent` mints the grant before recording the decision.** `completeAuthorization` can still throw there — it re-validates the redirect URI against a freshly fetched CIMD document — and remembering first leaves a 90-day approval on record for a grant that was never issued.

## Reading the caller's identity

Both auth paths hand the SDK an `AuthInfo` (`{ token, clientId, scopes, expiresAt?, extra? }`), so tools see the caller regardless of how it authenticated:

- **Interactive**: `src/index.ts` maps OAuthProvider's stored Okta props → `extra.auth_path === "interactive"`, `extra.sub`/`email`/`name`.
- **M2M**: the dispatcher maps the locally verified JWT claims (`src/jwt.ts`) → `extra.auth_path === "m2m"`, `clientId` is the service app, `scopes` from the token.
- **Enterprise (ID-JAG)**: `src/ema.ts` maps the assertion's claims. Scopes come from `claims.scope`, **never** from the request.

Tools read it off the handler context: `ctx.http?.authInfo`. The `whoami` stub tool shows the shape.

Note that `ServerProps.client_id` is the **OAuth client**, not the user. Setting it from `okta_sub` conflates the two and makes per-client consent and revocation meaningless.

## Scope enforcement is two layers

1. **Pre-handler** (`src/index.ts` → `gateOnScope`): reads `Mcp-Method`/`Mcp-Name`, returns `403` + `WWW-Authenticate` naming the needed scope so a conforming client can step up. A caller can simply omit the header, so this is a courtesy, not a boundary.
2. **In-handler** (`requireScope` as the first statement of every tool): the boundary. It cannot be avoided.

`TOOL_SCOPES` fails closed in both — a tool with no entry gets `null` from `scopeForTool`, layer 1 declines to decide, and layer 2 refuses.

## Drop-in files: when to actually edit them

- **`src/index.ts`**: switch from `apiRoute`/`apiHandler` to `apiHandlers` if you mount more than one MCP endpoint. Add `tokenExchangeCallback` if your tools need fresh upstream Okta tokens (see [architecture.md § Upstream token refresh](../references/architecture.md#upstream-token-refresh-v2)). Tune `ACCESS_TOKEN_TTL_SECONDS` (3600, the library default) to trade token lifetime against KV-write volume — each client token refresh is a KV write, and the free tier allows 1,000/day **account-wide**. If you add a cross-request stream (`subscriptions/listen`, `notify.*`), stop memoizing the handler and build it per request.
- **`src/jwt.ts`** / **`src/m2m.ts`**: nothing to tune. The M2M path no longer introspects or caches, so the "M2M introspection cache TTL" still described in comments in `wrangler.toml` and `worker-configuration.d.ts` is read by nothing; revocation lag is bounded by the token's own `exp`.
- **`src/auth.ts`**: change the `OKTA_SCOPES` interpretation if your tenant uses non-standard scopes. Leave the redirect and state handling alone. The props set in `completeAuthorization` are what `src/index.ts` maps into `AuthInfo`.
- **`src/oauth-state.ts`**: the two cookie prefixes, and nothing else. In particular do not reorder the delete-after-check in `takeState`, and do not move `consentRedirectKey` out of `consentKey`.

## Testing

Tests run **in the cloud** as part of Workers Builds — no local install needed. The `ci` script is wired as the build trigger's `build_command`, so a type error or a failing test **blocks the deploy**. The pipeline is `npm install` → `npm run ci` → `npx wrangler deploy`.

- `npm run typecheck` — **two** tsc projects: `src/` (Workers types only) and `tsconfig.test.json` (adds `@types/node`). Keeping them apart is what stops `src/` importing `node:*` and still compiling.
- `npm run test` — both vitest projects.
- `npm run test:unit` / `npm run test:workerd` — one at a time.

**The `workerd` project is not optional.** `src/index.ts` and `src/auth.ts` are importable in **no** Node test — Hono and `workers-oauth-provider` pull in `cloudflare:` modules. Reverting either file makes a Node-pool suite fail to *resolve*, which is not the same as coverage. `test/flow.workerd.test.ts` drives the real routes through `SELF.fetch()` against the real `wrangler.toml`.

**Prove your tests bite.** Every security-relevant test here was checked with a negative control — the source edit it should catch, applied, confirming *that* test fails:

| Control | Tests that fail |
| --- | --- |
| drop the redirect URI from the `consentKey` call in `auth.ts` | 2 |
| `stripInvisible(name.slice(0, 80))` instead of the reverse | 1 |
| `SCOPE_HELP[scope] ?? fallback` instead of `Object.hasOwn` | 1 |
| remove the five characters from the `INVISIBLE` **character class** | 1 |
| `consentRedirectKey` → identity | 7 |
| `rememberConsent` before `completeAndRedirect` | 1 |
| accept a state record without checking the CSRF cookie | 8 |
| delete **any one** tool's `requireScope` guard | 2 (measured for all three) |

That last row was wrong in an earlier version of this file, and the way it was
wrong is worth keeping: it claimed 2, which held for `whoami` and was **0** for
`echo` and `list_examples`, because both refusal tests named `whoami`. The row
read as coverage of the pattern and was coverage of one instance. The tests now
iterate `TOOL_SCOPES`, so a tool added later is covered as soon as it has a policy
entry.

Two more traps found the same way, both now closed: calling every tool with
`arguments: {}` made the `echo` case vacuous (a required argument fails *schema*
validation before the guard runs, so "refused" held with the guard deleted —
hence `VALID_ARGS`), and asserting merely "some error occurred" made the
no-identity case vacuous once `whoami` stopped null-checking (deleting the guard
made it *throw* instead of refuse). The assertions now pin the refusal to the
required scope name.

Two traps worth knowing: a test can pass against the pre-change code and prove nothing, and a control that silently *fails to apply* looks exactly like a vacuous test. Assert the control landed before believing the green.

**Add tests for your data layer** — focus on the pure functions (parsing, field mapping, range math), where conversion bugs concentrate. Export those helpers so tests can import them.

**`package-lock.json` is committed** (fleet policy since 2026-08-24; Workers Builds installs from it). Bump dependencies on top of the existing lockfile rather than regenerating it from nothing — a fresh resolution currently crashes (npm 10 and 11) on vitest 4's optional `@vitest/browser-*` peers. The lockfile must carry all 27 `@img/sharp-*` platform entries or Workers Builds' `npm ci` fails; see `references/conformance.md` § Dependency policy for the cause (a root-owned npm cache) and the pre-push check. Run `npm audit` (full tree, not just `--omit=dev`) after any bump.

## After the deploy

- Test interactive OAuth from Claude Desktop / Code by adding the `/mcp` URL as a custom connector. **You will see the consent page** — that is the gate working.
- Test M2M with a real `client_credentials` token from your Okta service app.
- Confirm the stateless posture: `GET /mcp` with a valid bearer returns **405** (SDK-provided — there is no standalone SSE stream under the 2026-07-28 protocol, and nothing in this repo asserts it, so check it by hand), and `tools/call` with `{}` applies your schema defaults.
- Confirm CIMD is advertised: `/.well-known/oauth-authorization-server` must report `client_id_metadata_document_supported: true`. If it is `false`, the `global_fetch_strictly_public` compatibility flag is missing and every client silently falls back to deprecated DCR.
- Confirm scope separation: a token carrying only another server's scope must be refused here.
- Add `https://<server-name>.<account-subdomain>.workers.dev/callback` to the Okta app's allowed redirect URIs (`okta-admin` recipe, step 3, with `HOST` set to the workers.dev host).
- **Register with `mcp-gateway`** — the default for the {{github_org}} fleet, not
  optional. As an admin caller:
  `register_server{url: "https://<name>.{{domain_suffix}}/mcp"}`. The gateway probes
  the server, reads its scopes from `/.well-known/oauth-protected-resource/mcp`,
  indexes its tools, and it is callable immediately with no redeploy. Confirm with
  `list_servers` showing it at `health: "ok"`.
  A server that publishes no `scopes_supported` is **refused** — the gateway mints
  a token per server from that list, so check that endpoint first if registration
  fails. Skipping this step is how three of six registry rows came to be wrong:
  nothing else keeps the registry and the fleet in step.
- Register the server in the MCP Marketplace (separate registry-side step).

## Library versions

Pin to these or newer (verified 2026-10-05: full CI green, `npm audit` reports 0 vulnerabilities across the whole tree):

| Package | Version | Why pinned exactly |
| --- | --- | --- |
| `@cloudflare/workers-oauth-provider` | `1.2.1` | Security-load-bearing. 1.x requires `resourceMetadata.resource` and binds every token to it; `requiredScopes` replaces `resourceMetadata.scopes_supported`; plain PKCE is gone. See `references/platform_facts.md`. |
| `@modelcontextprotocol/server` | `2.3.1` | Security-load-bearing. |
| `hono` | `4.13.13` | Security-load-bearing; < 4.13.7 has open advisories (XSS in `hono/jsx`, `parseBody()` DoS, query-parser differential). |
| `vitest` (devDep) | `4.1.11` | `@cloudflare/vitest-pool-workers` 0.22 requires `^4.1`; vitest 5 is out until the pool moves. |
| `@cloudflare/vitest-pool-workers` (devDep) | `0.22.0` | The `./config` subpath export and `defineWorkersProject` are **gone** — use `cloudflareTest()` from the package root as a Vite plugin. `fetchMock` is gone too; intercept with miniflare's `outboundService`. |
| `miniflare` (`overrides`) | `5.20261001.0-alpha` | The pool pins wrangler 4.124 / miniflare 5.20260815, which carry high-severity undici and sharp advisories. Drop the override once a pool release moves past them. |
| `zod` | `^4.6.5` | Import as `zod/v4`. |
| `jose` | `^6.2.12` | |
| `wrangler` (devDep) | `^4.147.0` | |
| `@cloudflare/workers-types` (devDep) | `^5.20261005.1` | v4 ERESOLVE-fails against current wrangler's peer range. |
| `@types/node` (devDep) | `^22.20.5` | Test project only — never in `tsconfig.json`. Stays on the 22 line; the Builds image runs Node 24.18 and that is fine. |
| `typescript` (devDep) | `^7.0.2` | The native compiler; both tsc projects pass with no config change. |

`@modelcontextprotocol/server` v2 replaces both `@modelcontextprotocol/sdk` v1 and the `agents` SDK — remove both if you're migrating an older server. The SDK's `workerd` package-export condition automatically selects an eval-free JSON-schema validator (`CfWorkerJsonSchemaValidator`) — nothing to wire up, and no reason to add Ajv.
