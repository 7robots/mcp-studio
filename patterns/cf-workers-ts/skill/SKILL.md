---
name: mcp-server-dev
description: "Develop and deploy MCP servers — LLM-facing tool design, schemas, testing, and two-path auth (interactive OAuth + M2M bearers). The default stack is TypeScript on Cloudflare Workers (stateless MCP spec 2026-07-28, @modelcontextprotocol/server v2, Okta), with a runnable template. Use for any MCP server work: building, adding tools or auth, converting or migrating, reviewing, deploying, or troubleshooting OAuth. Python/FastMCP and Modal knowledge is retained in cold references."
---

# mcp-server-dev

One skill for MCP server development. **The default stack — assume it unless told otherwise — is TypeScript on Cloudflare Workers**: `@modelcontextprotocol/server` v2, stateless MCP spec 2026-07-28, KV + D1, Okta OAuth for interactive clients and Okta-issued M2M bearer JWTs, verified LOCALLY against the issuer's JWKS, for the MCP Marketplace and agents. Token introspection is gone from the pattern (removed 2026-08-24). This skill is the documentation half of the MCP Studio pattern pack `cf-workers-ts`; the template, the exact dependency pins and the conformance rules are the other half, and **MCP Studio is where the live state lives** — which servers exist, what they run, and whether they conform (see [§ Fleet state](#fleet-state-ask-mcp-studio-not-this-file)). Check it rather than assuming. Cross-cutting Cloudflare conventions (secrets via 1Password, wrangler-vs-MCP tool split, stack defaults) live in the `cloudflare` skill, if installed; this one adds the MCP-server specifics. Python/FastMCP and Modal are **not current paths** — their lessons are preserved in cold references (see the map below).

**The pattern is stateless (MCP spec 2026-07-28).** `createMcpHandler()` builds a fresh `McpServer` per HTTP request. There are no sessions (`Mcp-Session-Id` is gone), no `initialize` handshake (each request carries its own `_meta` envelope), and **no Durable Object anywhere in the MCP layer** — the `McpAgent`/`agents`-SDK pattern this skill used to teach is retired. Caller identity travels per-request as SDK `AuthInfo` on every auth path. 2025-era clients still work: the handler's default `legacy: 'stateless'` posture serves them per-request from the same factory (legacy `GET`/`DELETE` answer 405; stale session IDs are ignored). Design stateless-first regardless of host: fresh server object per request, zero cross-request state in process, every store behind an external binding.

## Tool design — the part the LLM actually sees

Tools are an API for a language model, not for a programmer. The model chooses tools by reading names and descriptions, so that text is load-bearing:

- **Names**: stable, `snake_case`, verb-led where natural (`list_orders`, `get_invoice`). Prefix with a short domain word if the client may aggregate many servers (`weather_get_forecast` beats a bare `get_forecast`).
- **Descriptions**: say what the tool does *and when to call it*. "Search customers by name or email; use when the user refers to a customer you don't have an ID for" outperforms "Searches customers."
- **Server instructions**: set the server-level `instructions` string — one or two sentences on what the server is for. Clients surface it to the model when deciding whether to use your server at all.
- **Schemas**: give every parameter a type, constraints (`min`/`max`, enums), and a default where one is sensible. Let the schema apply defaults — a call with `{}` arguments should come out the other side with defaults filled in; that's worth one test.
- **Response format**: return `{ content: [{ type: "text", text: ... }] }` per the spec. When output benefits from human-readable rendering, offer a `response_format: "json" | "markdown"` parameter — default `"json"` for agentic consumers.
- **Keep payloads bounded.** Paginate (`limit`/`offset`) instead of returning unbounded lists; a tool that dumps megabytes into the model's context is a worse tool than one that returns a page and a count.
- **Errors**: return `{ content: [...], isError: true }` rather than throwing. Distinguish user errors (not found, validation failed — concrete message the model can react to) from infrastructure errors (database down — generic message, log the detail). Don't retry inside the tool; the client owns retry policy.
- **Ship a usage guide as a resource** (`skill://<server-name>`, markdown): the server's data model, tools, and worked examples — the README the LLM reads. In this stack it's `src/skill.ts`, registered via `server.registerResource`.

Minimum test set for any server (statelessness makes this in-process, no deployment needed): `tools/list` returns the expected tools; schema defaults reach the handler (call with `{}`); argument validation rejects bad input; one test per data-layer function containing real logic (parsing, field mapping, index math — where bugs concentrate; export pure helpers so tests can import them); if auth-dependent, a test that injects a caller identity and asserts the tool sees it.

## Operating model: agentic, remote-only

**This work is done by an agent (Claude Code) with the Cloudflare MCP server connected, not by a human running `wrangler` locally.** Builds and deploys run on Cloudflare Workers Builds (remote CI). When this skill is triggered:

- Use `mcp__cloudflare__execute` for all Cloudflare API operations (Worker uploads, secrets, KV, D1, build triggers, log queries).
- Use `mcp__cloudflare__search` to discover endpoints in Cloudflare's OpenAPI spec.
- Use `gh` CLI for GitHub operations (repo create, push, PRs).
- Use `mcp-studio` for anything about the pattern or the fleet as a whole: rendering a new server, conformance, live fleet status.
- Push code; Workers Builds auto-deploys on `main`.
- **Do not** suggest `wrangler dev` or `wrangler deploy` from the user's terminal.
- **Do not** suggest `npm install` on the user's machine — the build runs on Cloudflare's infrastructure.

Manual user steps are limited to: (a) Cloudflare Workers Builds GitHub App install (one-time per Cloudflare account) + per-repo grant in GitHub for each new repo — skip the grant ask if the App has **All repositories** access (test by attempting `PUT /builds/repos/connections`), (b) Okta admin work only where the API can't reach: creating the admin service app when onboarding a new org, and anything needing an interactive admin login. Scopes, policy rules, redirect URIs, app settings, and System Log diagnosis are the agent's job through the **`okta-admin`** skill, if installed (the instance's `studio.toml` `[identity.okta]` names the org and its object ids); every Okta write prompts {{operator_name}}, so it is not a USER ACTION, (c) secrets entry — **preferred: no user action.** The user stores values in 1Password (the `{{op_vault}}` vault); when the 1Password CLI (`op`) and a wrangler login are available the agent injects them itself, per the `cloudflare` skill: `printf '%s' "$(op read "op://{{op_vault}}/<item>/<FIELD>")" | npx wrangler secret put <FIELD>` from the repo dir (the `printf '%s'` is load-bearing — see gotchas). Fallback is the Cloudflare dashboard: paste with **Type: Secret**, not Variable (a Variable stores the value in cleartext readable via the bindings API). (d) review/approval at decision points. Flag remaining manual steps as **USER ACTION** when you reach them.

## Fleet state: ask MCP Studio, not this file

This skill deliberately carries no fleet inventory. An inventory written into a skill goes stale silently and then tells every agent that reads it there is nothing to check — which is exactly how a "the whole fleet is on the new pattern" claim once survived three weeks while three servers were still sessionful. Ask the tools instead:

| Question | Command / source |
| --- | --- |
| Which servers exist, where they are deployed, at which commit and pattern version, whether they answer, whether the gateway and marketplaces know them | `mcp-studio fleet status --json` (`mcp-studio fleet list` for membership only) |
| Does each repo's security seam still match the template and its blessed diff? | `mcp-studio pattern status [--json]` (per repo × file drift line counts); `mcp-studio pattern check [repo…]` exits 1 on any drift |
| Pins, lockfile, wrangler config, leftover placeholders, legacy (`McpAgent`) markers | `mcp-studio pattern lint [repo…] [--json]` |
| The pattern version, exact pins, security files and rules | `mcp-studio pattern show` (the pack's `pattern.toml`) |
| Fleet members, Okta org / authorization server / app / policy-rule ids, the gateway | the instance's `studio.toml` |

**Verifying a server against its running deployment** still means a real request: a bare `tools/list` with a bearer carrying that server's own scope. A sessionful server answers `400 … Mcp-Session-Id header is required`.

**An un-blessed edit to a conformance file stops deploys silently.** A 2026-09-28 audience change edited `src/jwt.ts` and the matrix test in four repos without a re-bless; every build after it failed the conformance gate, and production stayed on the previous commit for a week with nothing alerting. After pushing a change to a fleet repo, confirm the build's `build_outcome` (and the deployed commit in `mcp-studio fleet status`) rather than assuming it deployed.

**Resolve a server from the account, not from a repo found by name** — superseded copies of a repo commonly sit next to the live one, and custom domains can be attached outside `wrangler.toml`:

1. `GET /accounts/{id}/workers/scripts` — deployed Worker names.
2. `GET /accounts/{id}/workers/domains` — the real hostname→service map.
3. `GET /accounts/{id}/workers/scripts/{name}/bindings` — what the Worker has.
4. Confirm the deployed build's `commit_hash` exists in the repo
   (`git cat-file -t <sha>`); a hand-deployed Worker has no builds, so match on
   the repo description and push time.

A protocol or pattern change applies to every server in the fleet: migrate each one (and bless it), or record that it wasn't. `mcp-studio pattern status` is the place a laggard shows up.

## Core workflow

0. **Working in a server that already exists? Identify its pattern before
   touching anything.** `grep -l "McpAgent\|agents/mcp" src/*.ts` (or
   `mcp-studio pattern lint <repo>`, which flags the same legacy markers) — a
   hit means the server predates the 2026-07-28 stateless spec, and *any* work
   in `src/mcp.ts` or `src/index.ts` should start by migrating it (see the
   gotcha below and [agent_guide.md § Migrating an existing sessionful worker](references/agent_guide.md#migrating-an-existing-sessionful-worker)).
1. Read [references/agent_guide.md](references/agent_guide.md) for the full step-by-step. It maps every operation to a specific tool and has a checklist at the bottom.
2. For M2M (MCP Marketplace, scheduled agents) auth details, read [references/bearer_token_auth.md](references/bearer_token_auth.md). The dispatcher pattern is non-obvious if you're new to this stack.
3. Before designing around the OAuth provider, the server SDK, the Workers runtime or Okta, read [references/platform_facts.md](references/platform_facts.md), and before writing a security test read [references/test_methodology.md](references/test_methodology.md). Both are distilled from things that cost real time — the tasks extension having no runtime, `cacheHints` sitting on the wrong constructor, a test suite that passed 211 cases against a database with no schema.
4. For the **third** auth path — Enterprise-Managed Authorization / ID-JAG, where the enterprise's IdP consents on the user's behalf — read [references/enterprise_auth.md](references/enterprise_auth.md). Read it before touching `src/ema.ts`: the scope-escalation trap in `mapClaims` is not obvious and once shipped to six servers.
5. For the storage story (KV only — no session state anywhere), best practices, gotchas, and the FastMCP-vs-Cloudflare comparison, read [references/architecture.md](references/architecture.md).
6. Scaffold from the pack's template: `mcp-studio server new <slug> [--name …] [--description …] [--scope <slug>:read --scope <slug>:write] [--d1]` renders it with this instance's values, fills every placeholder (package and Worker names, route and `PUBLIC_MCP_URL`, scopes in source and tests, cookie prefixes, consent text), adds `conformance.json`, and makes the first commit in `<repos_dir>/<slug>-mcp-worker` — 229 tests green before you add a tool. (`mcp-studio pattern render --out <dir>` renders the bare template, placeholders intact, for reading.) Then `mcp-studio server plan <slug>` lists every remaining provisioning step (GitHub repo and topic, Okta scopes and policy rules, redirect URI, KV/D1, secrets, custom domain, Workers Builds, gateway, marketplace) as exact commands: Okta steps run through the okta-admin skill, Cloudflare steps through the cloudflare skill; `mcp-studio server verify <slug>` re-probes and shows what's done. Then customize `src/mcp.ts` (tools, each guarded by `requireScope`) and `src/data.ts` (data layer) → retarget `test/mcp.test.ts` and add tests for the data layer's pure functions → push. It ships a mandatory consent gate, CIMD, per-server scopes and the `workerd` route suite; none of that is optional scaffolding you can strip. The first deploy follows [references/first_deploy_cutover.md](references/first_deploy_cutover.md).

**Testing is part of the deploy.** The build trigger's `build_command` is `npm run ci` — which in a fleet repo is the conformance gate first (`npm run conformance`), then the placeholder check, then **two** tsc projects (`tsc --noEmit && tsc --noEmit -p tsconfig.test.json`, keeping Node globals out of `src/`) plus `vitest run` over **two** vitest projects (`unit` in Node, `workerd` via `@cloudflare/vitest-pool-workers`). A conformance failure, type error or failing test blocks the deploy — all in the cloud, no local install. The template ships 229 tests across 11 test files: the auth layer (`oauth-state`, `consent-ema`, `flow.workerd`, `jwt`, `m2m`, `m2m.workerd`, `resource`, `actor`, `gateway-token`), the behavioral `matrix.workerd` suite, and `test/mcp.test.ts` for the handler. The stateless handler is directly testable in-process: POST JSON-RPC requests to `handler.fetch()` with a stub binding, optionally passing `{ authInfo }`, and parse the response as JSON or a single-message SSE stream.

## Calls that arrive through mcp-gateway

**Registering a new server with the gateway is a default deploy step** — see
the checklist in [references/agent_guide.md](references/agent_guide.md). One
admin `register_server{url}` call against `https://{{gateway_host}}`: the
gateway probes the server, reads its scopes from its RFC 9728 metadata, indexes
its tools, and it is callable at once. Nothing else keeps the registry and the
fleet in step (`mcp-studio fleet status` shows any server the gateway does not
know).

An M2M bearer proves only that *a platform workload* is calling. Per-server
scopes (`<slug>:read` / `<slug>:write`) replaced one coarse fleet-wide
`mcp-access` scope in 2026-08, so a token no longer works on every server — but a
credential is still shared among everything holding it, so it cannot distinguish
the gateway from any other caller with the same scope, and says nothing about
which human asked.

`mcp-gateway` therefore also sends **`X-MCP-Actor`**: a 60-second EdDSA
assertion, signed with a key only the gateway holds, published at
`https://{{gateway_host}}/.well-known/jwks.json`. `src/actor.ts` verifies
it — EdDSA only, issuer and audience pinned, expiry with 30s skew, `act.client_id`
bound to the calling token's own client id, a single-use `jti` consumed in KV,
JWKS cached at the edge rather than in KV.

**It is wired into the template's M2M path, with enforcement off by default.**
`src/index.ts` calls `gateOnActor()` before the scope gate on every M2M request:
a **present** `X-MCP-Actor` assertion is always verified, but an assertion is
**required** only when `REQUIRE_GATEWAY_ACTOR` is set. The flag governs whether
one is required, never whether a present one is checked: with it off, an
assertion that fails verification (forged, expired, naming another client,
replayed) is ignored and the caller is served as the plain M2M caller it is —
never as an actor; with it on, the request is refused 403. To use it, uncomment
`GATEWAY_ISSUER` / `GATEWAY_JWKS_URL` in `wrangler.toml`; until they are set, no
assertion can verify.

- A verified assertion is reported **alongside** the token's subject, never
  replacing it: `extra.sub` stays the workload, `extra.on_behalf_of` carries the
  human (plus `gateway_purpose` / `gateway_run_id`), and `extra.auth_path`
  becomes `"m2m+actor"`. "The gateway says this is {{operator_name}}" and "Okta
  says this is {{operator_name}}" are different claims and are kept apart. The
  template's `whoami` tool surfaces these fields — the one-call liveness check
  for the whole verification chain.
- **With enforcement off, a JWKS fetch failure degrades silently** to "no
  actor". Prove the chain works before relying on the enrichment: with the gate
  temporarily on, a claim-valid assertion with a wrong signature must be refused
  as "signature did not verify" — that reason is downstream of the JWKS fetch.
- **`REQUIRE_GATEWAY_ACTOR = "true"` is a cutover, not a default — and quote it.** wrangler.toml is TOML, so unquoted `true` arrives as a real boolean; the template's `actorRequired` accepts both, but a hand-rolled `=== "true"` compare would leave the gate silently OFF while you believe it is on. It refuses
  any M2M caller without a valid assertion, so every direct caller of that
  server — the marketplace included — breaks the moment it is flipped. Verify
  with it on, then decide.
- It is an *assertion*, not a grant: the gateway states the subject it
  authenticated, and the downstream trusts the gateway to be honest. A token
  minted by Okta carrying both identities (Cross App Access / RFC 8693) is the
  stronger thing this stands in for.

### How the gateway authenticates to a server (`auth_mode`)

`register_server` takes an `auth_mode`; pick by what the server can verify.
The gateway repo's `docs/server-auth.md` is the full guide.

- **`okta_m2m`** (default) — the fleet pattern above: the gateway's own M2M token,
  per-server scopes. The server keeps its interactive OAuth for direct clients.
- **`okta_user`** — same server, same Okta checks, but each call carries the
  *calling person's* Okta token, narrowed to this server's scopes (`sub` = login
  email, `uid` = Okta user id, `cid` = the gateway app). The person connects once
  through a link the first call returns. The server must accept both user and M2M
  tokens, as every server on this pattern does, because the catalogue refresh uses M2M.
- **`gateway_jwt`** — for a server that does **no OAuth**. The gateway signs a
  5-minute RFC 9068 token for that server alone; the template's `src/gateway-token.ts`
  `verifyGatewayToken()` is the whole of its authentication (issuer, audience =
  its own URL, `typ: at+jwt`, its registered scope). Register with
  `auth_mode: "gateway_jwt", scopes: ["<slug>:use"]`. The server is then reachable
  only through the gateway.
- **`bearer` / `api_key`** — a static secret the gateway holds encrypted; for
  servers you cannot change. **`none`** — nothing.

## Authorization servers: one, or one per server

**The pattern as shipped assumes one** Okta custom authorization server
(`{{okta_issuer}}`) shared by every MCP server, with per-server scopes
(`weather:read`, `weather:write`, `tickets:read`, …) and issuer pinning doing the
separation.

**Know what that forfeits.** Okta sets `aud` per authorization server, not per
request, so every token from a shared AS carries the same audience
(`{{okta_audience}}`) whatever it targets — and a strict RFC 8707 audience check
would then refuse every token.
MCP 2026-07-28 makes validating the token's audience a **MUST** for a resource
server, so a shared AS means knowingly not meeting it. A token ends up scoped to
a server but not bound to it.

**Recommend one AS per MCP server for anything production or enterprise**, each
with its audience set to that server's canonical resource URI. That makes `aud`
a real binding, satisfies the MUST, keeps scopes as defence in depth rather than
the only line, and confines a compromised AS to one server. The template is ready
for it: set `OKTA_M2M_AUDIENCE` and audience pinning is config, not code.

**One app is a separate question from one AS.** Scopes live on the authorization
server; apps are granted subsets. Keeping a single Worker app plus a single M2M
app is fine either way — it is the *authorization server* that multiplies, not
the app. Note that a shared M2M app granted every server's scopes can mint a
token for any of them; separating that is a per-caller decision (a second M2M
app), not a per-server one.

## Reference map — load on demand

| Read | When |
|---|---|
| [references/agent_guide.md](references/agent_guide.md) | Building, converting, or deploying a server on Cloudflare — the step-by-step |
| [references/bearer_token_auth.md](references/bearer_token_auth.md) | Adding/troubleshooting M2M bearer auth; the two-path dispatcher |
| [references/architecture.md](references/architecture.md) | Stateless pattern internals, storage, best practices, full gotcha list |
| [references/platform_facts.md](references/platform_facts.md) | Before designing around the OAuth provider, the server SDK, the Workers runtime or Okta — behaviour that cost time to discover and is not in the docs |
| [references/test_methodology.md](references/test_methodology.md) | Before writing or trusting a security test — the six ways one lies, and why mutation-testing every control is not optional |
| [references/conformance.md](references/conformance.md) | Before editing any of the eleven security files in a fleet repo — how the drift gate works, how a change gets blessed, and the dependency/lockfile policy |
| [references/first_deploy_cutover.md](references/first_deploy_cutover.md) | Deploying a generated server for the first time — ordered, with rollback at each step and the Okta objects created before anything ships |
| [references/enterprise_auth.md](references/enterprise_auth.md) | Enterprise-Managed Authorization (ID-JAG) — the third auth path |
| `okta-admin` skill, if installed | Any Okta read or change — the new-server recipe (scopes + both policy rules + redirect URI), and the System Log recipe for a failed login. Object ids are in the instance's `studio.toml` |
| [references/python-fastmcp.md](references/python-fastmcp.md) | **Cold** — only if Python/FastMCP genuinely returns; MultiAuth, cold-start token persistence, encrypted client_storage |
| [references/modal-deploy.md](references/modal-deploy.md) | **Cold** — only if deploying to Modal or another container host; scale-to-zero lessons, reverse-proxy fronting, generic smoke test |

## Quick orientation

- **Reference implementation**: the pack's template. Every auth-layer change is meant to land there first, be ported to the fleet, and be blessed (see [conformance.md](references/conformance.md)); a fleet repo is never "the canonical version". Render it with `mcp-studio pattern render --out <dir>` to read it. It ships **two** scopes so the scope-bound consent, step-up and re-prompt branches are non-vacuous — keep two even if the server has one obvious permission level.
- **Library versions**: the authoritative list is the pack's `pattern.toml` (`[pins]`, `[dev_pins]`, `[overrides]`, `[majors]`; `mcp-studio pattern show`), enforced by `mcp-studio pattern lint`. At pattern version 2026-10-05.1 (`npm audit` clean on the full tree): `@cloudflare/workers-oauth-provider@1.2.1` (exact — 1.x binds every token to the canonical resource and drops plain PKCE; the migration from 0.10.x is in [platform_facts.md](references/platform_facts.md#workers-oauth-provider-1x)), `@modelcontextprotocol/server@2.3.1` and `hono@4.13.13` (both exact — security-load-bearing; hono < 4.13.7 carries open advisories), `zod@^4.6.5`, `jose@^6.2.12`; dev: `@cloudflare/vitest-pool-workers@0.22.0` and `vitest@4.1.11` (both exact — the pool requires vitest `^4.1`, so vitest 5 is out until the pool moves), `@types/node@^22.20.5` (test project only; the Builds image runs Node 24.18, which is fine), `@cloudflare/workers-types@^5.20261005.1`, `typescript@^7.0.2` (the native compiler; both tsc projects pass unchanged), `wrangler@^4.147.0`, plus an `overrides` entry `"miniflare": "5.20261001.0-alpha"` — pool 0.22.0 pins wrangler 4.124 / miniflare 5.20260815, which carry high-severity undici and sharp advisories. `@modelcontextprotocol/server` v2 replaces **both** `@modelcontextprotocol/sdk` v1 and the `agents` SDK — neither belongs in a new server (lint forbids them, and Ajv). zod must be v4, imported as `import * as z from "zod/v4"`. workers-types must be v5 (wrangler ≥4.116's peer); v4 now ERESOLVE-fails on Workers Builds. The gateway is a different codebase with its own pins; it is not on this pattern.
- **API surface**: `createMcpHandler(factory)` → `{ fetch, close, notify, bus }`, where `fetch(request, options?: { authInfo })`. The factory runs **once per HTTP request** and must return a fresh `McpServer`; it receives `{ era, authInfo, requestInfo }`. Tools register via `server.registerTool(name, { description, inputSchema: z.object({…}) }, handler)` and read the caller as `ctx.http.authInfo`. Resources are unchanged: `server.registerResource(name, uri, metadata, callback)`.
- **Drop-in files** (keep exactly as rendered — the first eight are under conformance): `src/index.ts` (dispatcher, actor gate, pre-handler scope gate, CIMD + EMA wiring), `src/auth.ts` (the Okta dance plus `/consent`), `src/oauth-state.ts` (server-side state and the remembered decision — edit only the two cookie prefixes), `src/ema.ts`, `src/jwt.ts` (local M2M JWT verification), `src/m2m.ts` (bearer extraction and the verdict type), `src/resource.ts`, `src/actor.ts` (wired in; enforcement off until `REQUIRE_GATEWAY_ACTOR` is set), `vitest.config.ts`, `tsconfig.json`, `tsconfig.test.json`, `.gitignore`, and the sample suites `test/oauth-state.test.ts`, `test/consent-ema.test.ts`, `test/flow.workerd.test.ts`, `test/jwt.test.ts`, `test/m2m.test.ts`, `test/m2m.workerd.test.ts`, `test/resource.test.ts`, `test/actor.test.ts`, and the behavioral `test/matrix.workerd.test.ts` (identical fleet-wide; its per-repo variation lives in `test/matrix.params.ts`). `src/gateway-token.ts` + its test are only for a `gateway_jwt` server.
- **Customize per server**: `src/scopes.ts` (the two scope names and one `TOOL_SCOPES` entry per tool), `SCOPE_HELP` in `src/consent.ts` (what each scope permits, in the user's words), the two `__Host-` cookie prefixes in `src/oauth-state.ts`, `src/mcp.ts` (tools inside `buildServer()`, each opening with a `requireScope` guard), `src/data.ts` (your data layer), `src/skill.ts` (the LLM-facing guide, exposed as `skill://<server-name>`), `test/mcp.test.ts` (retarget the tool names), `test/matrix.params.ts`, `wrangler.toml` (name, bindings, IDs, Okta vars), Env types in `worker-configuration.d.ts`. The full find-and-replace checklist is in the rendered repo's `README.md` — note that placeholders in a hostname and the wrangler `name` **must stay lowercase**. `npm run check:placeholders` (and `mcp-studio pattern lint`) catch what survives.

## When this skill is the wrong tool

- **If the target is FastMCP/Python**, the default move is converting it to this stack (this skill covers conversion). Only build *new* Python MCP servers on explicit request — then read [references/python-fastmcp.md](references/python-fastmcp.md).
- **If the user wants Cloudflare Access in front of the Worker** (zero trust gating), this skill doesn't cover that — the Okta OAuth happens inside the Worker, not via Access.
- **If real-time token revocation is a hard requirement**, the pattern won't meet it as shipped. M2M bearers are verified locally against the issuer's JWKS, so a token revoked in Okta stays valid until its own `exp` (the access-token lifetime set on the Okta access-policy rule — one hour by default); and a Worker-issued interactive token stays valid for `ACCESS_TOKEN_TTL_SECONDS` (the library default, one hour) with no fast revocation short of deleting its grant from KV. Shorten those lifetimes, or add an introspection check for the callers that need it — knowing that introspection cannot validate Cross App Access tokens at all (see [bearer_token_auth.md](references/bearer_token_auth.md#why-local-verification-and-not-introspection)).

## Key gotchas the agent should know up-front

- **GitHub App install is one-time per Cloudflare account, but each new repo needs an explicit grant in GitHub.** First Worker fails with `8000008: This project is disconnected from your Git account`; subsequent Workers fail with `8000012: The project is linked to a repository that no longer exists` until the repo is added to the App's repository access list. Flag as USER ACTION on every Worker.
- **Build-trigger config auto-creation only happens for the very first Worker on the account.** All Workers auto-deploy on push to `main` once a trigger exists; the difference is just whether you create the config yourself via `POST /builds/triggers` (every subsequent Worker) or Cloudflare creates it for you (first Worker only).
- **Secrets go in via the 1Password pipe.** Preferred path (per the `cloudflare` skill): `printf '%s' "$(op read "op://{{op_vault}}/<item>/<FIELD>")" | npx wrangler secret put <NAME> --name <worker>` — the value never enters the transcript. The `printf '%s'` is required, not stylistic; see the trailing-newline gotcha below. If the dashboard fallback is ever used, the user must choose **Type: Secret** (stored encrypted, masked in bindings API); **Variable** stores it in cleartext, readable via `GET /workers/services/{name}/environments/production/bindings`. Verify after upload by confirming the binding shows `secret_text`, not `plain_text`.
- **PKCE: say which client type.** Through 0.8.x the provider only rejected `code_challenge_method=plain`; a client sending no challenge got no PKCE. From **0.10.3** `validateAuthorizationPkce` requires PKCE whenever `responseType` is `"code"` and the client is public (`token_endpoint_auth_method: "none"`) — which DCR and CIMD clients always are. From **1.2** plain PKCE and the implicit grant are gone entirely (`allowPlainPKCE` / `allowImplicitFlow` now throw at construction if set `true` — delete them). A *confidential* client may still omit PKCE. Do not write "PKCE is not enforced" (wrong from 0.10.3) or "requires S256 PKCE" without "for public clients".
- **A remembered consent decision must be keyed on the redirect URI, not just (user, client, resource).** Enabling CIMD changes what a `client_id` *is*: a URL whose metadata document is re-fetched on every authorization, so its `redirect_uris` are mutable by whoever controls that URL later — a document edit, an expired domain, a subdomain takeover. A decision keyed without the destination therefore keeps minting codes to a destination the user never approved, for the whole remaining TTL (90 days in the template). DCR clients are safe (registration is `POST`-only, no update endpoint) which is exactly why this reads as fine until CIMD lands. Key on all four. Accept that the page shows the redirect *host* while the key uses the full URI, so a path change re-prompts on a visually identical page: over-prompting, never under-prompting. Deploying the fix orphans every existing 3-component record — unreachable, non-colliding, self-expiring — so each current user sees exactly one extra prompt and there is no migration.
- **A memo key derived from a value the framework matches LOOSELY must match it exactly as loosely.** `isValidRedirectUri` in `workers-oauth-provider` deliberately ignores the **port** for loopback redirect URIs — RFC 8252, because a native app cannot reserve one — comparing only protocol, hostname, pathname and search. So a client registered at `http://127.0.0.1:5000/cb` is validly authorized at *any* port. Key a remembered consent on the full URI and it is stricter than the rule that admitted the request: an ephemeral-port local client is re-prompted on **every launch**, and the destination it asks the user to judge reads `127.0.0.1:52222`, a different unjudgeable number each time. A key stricter than its matcher does not fail closed, it fails **noisy** — and noise is what gets clicked through, which on a consent-gated server is the whole control. Canonicalize for loopback (`127.x.x.x`, `::1`, `localhost`, case-insensitive) and only for loopback. Non-loopback stays exact, because exact is what the provider requires there too.
- **When a change widens what an approval covers, the consent page has to say so — the disclosure IS part of the change.** Canonicalizing loopback redirect URIs (above) made one approval cover every port on that host, but `redirectHostOf` still returned `URL.host`, which includes the port. Users were shown `127.0.0.1:51111` and granted all ports: the approval was broader than the page stated, which is the one thing a consent page cannot be. Show `127.0.0.1 (any port)` for loopback, `host:port` elsewhere where the key is exact. The general trap is that a key change gets reasoned about, tested and documented entirely in key terms, and nobody follows it out to the sentence the user actually reads.
- **Mint the grant BEFORE recording the consent decision.** `completeAuthorization` can still throw at that point — it re-validates the redirect URI against a *freshly fetched* CIMD document, which may have changed since the page was rendered. Remembering first leaves a 90-day approval on record for a grant that was never issued, and the user is never asked about that client again. This ordering has no observable signature, so it once silently diverged across five of six servers; a source-text guard (`indexOf("rememberConsent") > indexOf("completeAndRedirect")`) is worth the ugliness, because `src/auth.ts` cannot be imported under the Node pool at all.
- **`LOOKUP[key] ?? fallback` is a 500 waiting to happen when `key` comes from a request.** A scope named `toString`, `constructor`, `valueOf`, `hasOwnProperty` or `__proto__` resolves to the *inherited* member, so `??` never fires and the non-string reaches the escaper: `s.replace is not a function`, page 500s. Use `Object.hasOwn(LOOKUP, key)` **and** a `typeof === "string"` guard. Validating the input upstream is not a substitute — this is the belt to that braces, and a belt that throws is worse than no belt.
- **Strip invisible characters BEFORE truncating a self-reported name, not after.** Slicing first lets padding *suppress* a real name: 80 zero-widths followed by `Evil Corp` is cut at the cap, stripped to nothing, and renders no row at all — hiding a name the user should have seen. Beyond the usual bidi controls, include U+00AD, U+061C, U+180E, U+2028 and U+2029: all render as nothing, all survive HTML escaping, and the last two terminate a line in some readers and log viewers. Know what a deny-list cannot reach — U+3164, U+2800, U+00A0 still render a visibly blank row — and that closing it needs a "contains a visible character" test instead.
- **Grant scope and props scope must be the same array.** If `completeAuthorization` records one value and props carry another, a caller can hold scopes the grant does not report — the `/token` response and the audit trail then disagree with reality. And default an omitted `scope` to the MINIMAL scope, never to everything supported: "asked for nothing, received everything" is invisible until a server has two scopes.
- **Route-level tests need the workerd pool, and the Node pool cannot substitute.** `src/auth.ts` and `src/index.ts` are importable in NO Node test — Hono and `workers-oauth-provider` pull in `cloudflare:` modules. That means helper tests can look thorough while the FLOW is untested: reverting the source makes such a suite fail to **resolve**, not fail an assertion, which is not coverage. Add a second vitest project on `@cloudflare/vitest-pool-workers` and drive `SELF.fetch()`. Prove it bites: defeat the check under test and confirm a specific number of route tests fail (in the template, accepting a state record without checking the CSRF cookie fails 8).
- **Pool 0.22 API notes**, which cost real time to discover: it requires **vitest ^4.1** (upgrading from 3.2.4 broke none of ~130 existing tests); the `./config` subpath export and `defineWorkersProject` are **gone** — use `cloudflareTest()` from the package root as a Vite **plugin**; and `fetchMock` is **gone** from `cloudflare:test`. Intercept outbound calls with miniflare's `outboundService` instead — which is the correct mechanism anyway, because `SELF.fetch` runs the Worker in its own isolate, so a `globalThis.fetch` stub applies to the wrong one. Type `env` by declaring one shape exposed as both global `Env` and `Cloudflare.Env`, and put the `namespace Cloudflare` INSIDE `declare global` (a `worker-configuration.d.ts` with `export {}` is a module, so a top-level `declare namespace` is local and silently does nothing).
- **Prove each new security test with a negative control, and prove the control applied.** Write the test, then make the source edit it is supposed to catch and confirm *that* test — by name, and ideally only that test — fails. Two traps: (1) a test can pass against the pre-change code and prove nothing, which is how a regression test for strip-before-slice got written asserting a property that held under *both* orders; (2) a control that silently fails to apply looks exactly like a vacuous test — one edited the doc comment listing the characters instead of the regex, and the suite stayed green. Assert the control landed before believing the green. And note where the control must be aimed: reverting a four-argument `consentKey()` **call** in `src/auth.ts` leaves every unit test green, because the helper still hashes four arguments correctly. Only a route test through `/authorize` → `/callback` → `/consent` catches it.
- **Stub every outbound host in `outboundService`, not just the IdP.** A route test that reaches a paid API spends real money and a write test can mutate real data. Assert the interception itself — one server's suite asserts an OpenAI-blocked marker rather than merely hoping.
- **`workers-oauth-provider 0.6+` strictly validates client_id.** Synthetic smoke-test client_ids get 500; use DCR (`POST /register`) first. (0.6.x *does* serve `/.well-known/oauth-protected-resource` — the old "404 on 0.0.5" note is obsolete.)
- **Trigger listing wants the script tag** (32-char hex), not the script name. Get it from `/workers/services/{name}.default_environment.script_tag`.
- **Caller identity rides `AuthInfo`, not `props`.** Every auth path builds an `AuthInfo` in `src/index.ts` and passes it to `handler.fetch(request, { authInfo })` — M2M from the locally verified JWT claims (`src/jwt.ts`), interactive and enterprise from OAuthProvider's stored props. Tools read `ctx.http.authInfo`, and `extra.auth_path` (`"m2m"` | `"m2m+actor"` | `"interactive"` | `"enterprise"`) says which path they came from. The old "no props on the M2M path" limitation is gone.
- **Migrating an existing sessionful (McpAgent/DO) Worker needs a `deleted_classes` migration.** Deleting the DO class from the code isn't enough: append a new `[[migrations]]` tag with `deleted_classes = ["OldClass"]`, keeping the old tags. It's one-way — `wrangler rollback` does **not** work across a DO migration; recovery requires yet another tag that re-creates the class. See [references/agent_guide.md § Migrating an existing sessionful worker](references/agent_guide.md#migrating-an-existing-sessionful-worker).
- **No manual JSON-schema validator wiring.** The SDK's `workerd` package-export condition selects an eval-free validator (`CfWorkerJsonSchemaValidator`) automatically. Don't add Ajv — it needs `eval`, which Workers forbid.
- **`wrangler` ≥4.116 disables the workers.dev subdomain when `routes` are present.** Adding a custom domain silently kills the `*.workers.dev` URL that connectors and Okta redirect URIs are usually registered against. Set `workers_dev = true` explicitly to keep both, and treat the change as an OAuth cutover: register the new redirect URI in the IdP first, flip the URL, verify with a real client login, remove the old URI last (it's the rollback path). (Since provider 1.x a `*.workers.dev` URL no longer supports interactive OAuth discovery anyway — see [platform_facts.md](references/platform_facts.md#workers-oauth-provider-1x).)
- **DO commit `package-lock.json`** (policy reversed 2026-08-24 — see [conformance.md § Dependency policy](references/conformance.md#dependency-policy)). **The lockfile must list every `@img/sharp-*` platform package with a version**, or Workers Builds' `npm clean-install` (Linux x64, npm 10.9.2) fails with `Invalid Version:` or `Missing: … from lock file`. A root-owned `~/.npm/_cacache` silently drops them; the recipe and the pre-push check are in [conformance.md § Dependency policy](references/conformance.md#dependency-policy), and `mcp-studio pattern lint` checks the entry count. Bump dependencies *on top of* the existing lockfile, not from a deleted one: as of 2026-10-05 a fresh resolution crashes in arborist under both npm 10.9.2 and 11 (`Cannot read properties of null (reading 'edgesOut')`) on vitest 4's optional `@vitest/browser-*` peers, which now resolve to vitest 5.
- **The 2-triggers-per-Worker limit** — Cloudflare auto-creates production + preview triggers on first GitHub App authorization. Don't try to add more.
- **Anything keyed on a caller-chosen value must not write KV before authentication.** History, from the removed introspection path: `tryOktaM2M` ran before any authentication and keyed its cache on sha256(the presented token), so the caller chose the key. KV allows 1,000 writes/day *account-wide* on the free tier: caching inactive results let 1,000 unauthenticated requests with random bearers exhaust the whole account's budget and break OAuth token issuance for every Worker on it. The fix was gating the write on `result.active`; one server had the guard and the template and three others had shipped without it. Local JWT verification writes no KV at all, so the hazard is gone from the M2M path — but the shape recurs anywhere a pre-auth code path writes storage keyed on request input (the actor `jti` marker is consumed only *after* signature verification for this reason).
- **`OKTA_DOMAIN` must be the org domain, never the `-admin` console hostname** — `https://<org>-admin.okta.com` 404s on `/oauth2/default/v1/authorize`. Drop the `-admin`.
- **`op read | wrangler secret put` stores a trailing newline** — Okta rejects the Basic auth as `invalid_client`, and Bearer headers built from the value throw. Always `printf '%s' "$(op read ...)" | npx wrangler secret put NAME`. Verifying a deployed Okta secret **without a login no longer works the old way**: the old recipe (`GET /callback?code=bogus&state=<base64 …>`) relied on `/callback` trusting a caller-supplied `state`, which is exactly the vulnerability the server-side state record closed — `takeState` now 400s on an unknown nonce before any token exchange happens. Verify by completing a real interactive login instead, or by checking the binding reports `secret_text`.
- **OAuth completes but the client says it never authenticated** (grants + tokens visible in `OAUTH_KV`, authed `POST /mcp` hangs ~10s until the `Claude-User` client times out): suspect stale claude.ai/Desktop **connector state** (e.g., the connector was previously pointed at another server or Okta tenant), not server code. Fix: delete and re-add the connector — forces fresh DCR and a freshly issued token.

Full gotcha list in [references/agent_guide.md](references/agent_guide.md#common-gotchas) and [references/architecture.md](references/architecture.md#gotchas).
