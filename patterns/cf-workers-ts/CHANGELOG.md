# Changelog — cf-workers-ts

Pattern versions are date-based, `YYYY-MM-DD.N`, and live in `pattern.toml`
`[pack] version`. A new version is cut when a change to a security file (the
`[security] files` list) or to the exact pins is ported fleet-wide; each fleet
repo's `conformance.json` records the version it was last blessed at, and
`mcp-studio pattern check` fails a repo that is behind. Documentation-only
changes to `skill/` do not need a version.

## Unreleased

### Moved into MCP Studio

- The pack now lives in MCP Studio as `patterns/cf-workers-ts`: `template/`,
  `skill/`, `pattern.toml` (version, exact pins, major floors, overrides,
  forbidden dependencies, security files, required files and scripts, wrangler
  and lockfile rules, placeholder patterns) and this changelog. Previously the
  template and the skill lived together in a personal skills repository, and
  the pattern version was a constant in a Python script.
- Organization values (Okta org, issuer, interactive client id, M2M audience,
  domain suffix, gateway host, GitHub org, 1Password vault, test email, operator
  name) are templated as double-brace variables declared in `[variables]` and
  supplied by each instance's `pattern-values.toml`. `mcp-studio pattern render`
  renders the template; `mcp-studio pattern install-skill` renders the skill.
- Conformance tooling ported to `mcp-studio pattern check | status | bless |
  lint | show`. The blessed per-repo diffs moved to the instance's conformance
  store (`<instance>/conformance/<repo>/<file>.diff`) and are reviewed in the
  instance repo's history. `bless` rewrites a repo's `conformance.json` only when
  hashes or the version changed, and has a `--store-only` mode. `lint` is new: it
  checks pins, the lockfile's `@img/` entry count, wrangler flags/vars/bindings,
  surviving placeholders and legacy (`McpAgent`) markers statically. The fleet
  dependency audit is now described as a procedure (`npm audit --omit=dev` in
  every fleet repo and on a rendered template) rather than a script with a
  hardcoded repo list.
- Skill rewritten org-neutral. Removed: the fleet inventory table and its
  per-server verification dates, per-server scope and reference-repo lists, and
  incident accounts naming individual servers — replaced by pointers to
  `mcp-studio fleet status`, `mcp-studio pattern status | lint` and the
  instance's `studio.toml`. Lessons kept, phrased generically.
- Corrections carried in the move (template README and skill):
  - The template ships **229 tests** in 11 test files; the README still said 171.
  - `src/actor.ts` **is wired** into `src/index.ts`'s M2M path: a present
    `X-MCP-Actor` assertion is always verified; one is required only when
    `REQUIRE_GATEWAY_ACTOR` is set (off by default; `GATEWAY_ISSUER` /
    `GATEWAY_JWKS_URL` commented in `wrangler.toml`). The skill had described
    it as shipped-but-unwired and optional.
  - The README's file table gained a `src/jwt.ts` row, and lines describing
    M2M introspection and its KV cache as current were corrected: M2M bearers
    are verified locally against the issuer's JWKS, and introspection was
    removed on 2026-08-24. The skill's gotchas, the "wrong tool" revocation note
    (formerly a 1-hour cache lag), the M2M configuration and verification
    sections, and the architecture diagram now describe local verification;
    the KV-write-budget lesson from the introspection era is kept as history.
  - The skill now states the template's actual interactive access-token TTL
    (the library default, one hour); several references still said eight hours.
  - The gateway's own provider version was dropped from the skill; it is not on
    this pattern and its pins are its own.

## 2026-10-05.1

- `@cloudflare/workers-oauth-provider` 0.10.3 → **1.2.1**: `allowPlainPKCE`
  dropped (throws if set), `resourceMetadata.scopes_supported` replaced by
  `requiredScopes`, `AuthorizationError.redirectTo` used for error redirects;
  route tests for the library-refused `/authorize` paths, negative control
  verified. 1.x serves protected-resource metadata only on the canonical
  resource's host, so a `*.workers.dev` URL no longer supports interactive OAuth
  discovery.
- `hono` → **4.13.13** (open advisories below 4.13.7), server SDK 2.3.1,
  zod 4.6.5, jose 6.2.12, **TypeScript 7.0.2**, wrangler 4.147.0,
  workers-types 5.20261005.1.
- `miniflare` override `5.20261001.0-alpha`: vitest-pool-workers 0.22.0 pins a
  miniflare with high-severity undici and sharp advisories. `npm audit`: 0 on
  the full tree. 229/229 tests.
- Lockfile lessons the same day: Workers Builds (npm 10.9.2) refuses a lockfile
  missing any `@img/sharp-*` platform entry or its version. The cause was
  root-owned entries in `~/.npm/_cacache`, which npm silently skips — not an
  npm 10 vs 11 difference, as first diagnosed. Pre-push check: 27 `@img/`
  entries and a clean `npm@10.9.2 ci --os=linux --cpu=x64`. A from-scratch
  resolution currently crashes in arborist on vitest 4's optional
  `@vitest/browser-*` peers, so bump on top of the existing lockfile.
- Workers Builds runs Node 24.18; `@types/node` stays on the 22 line.
- Lesson recorded: an un-blessed edit to a conformance file fails every
  subsequent build, and a failed build is silent in production — one audience
  change left four servers a week behind. Confirm `build_outcome` after a push.

## Between 2026-08-24.3 and 2026-10-05.1 (no version bump)

- 2026-09-28: the shared authorization server's audience changed (from a dead
  URL to the fleet's own); the `src/jwt.ts` comment and the sample `aud` in the
  M2M and matrix tests followed. (This was the edit that went un-blessed in four
  repos — see above.)
- 2026-09-28: `src/gateway-token.ts` + test added — `verifyGatewayToken()` for a
  server that does no OAuth and is reached only through the gateway's
  `gateway_jwt` auth mode (EdDSA, `iss`, `aud`, `typ: at+jwt`, scope). The skill
  lists every gateway `auth_mode`. 227 tests.
- 2026-09-28: all Okta configuration (scopes, policy rules, redirect URIs,
  System Log diagnosis) routed through the `okta-admin` skill instead of
  USER ACTIONs in the Admin Console.
- 2026-10-02: process narrative and incident history trimmed from the skill;
  technical content unchanged.

## 2026-08-24.3

- The behavioral matrix suite, `test/matrix.workerd.test.ts`, becomes the
  eleventh security file: one spec, byte-identical fleet-wide, driving metadata,
  M2M, both scope layers, the actor chain and EMA provenance through every
  server's real routes; per-repo data lives in the untracked
  `test/matrix.params.ts`.
- The template's vitest config gains a fake gateway Ed25519 JWKS arm and
  `GATEWAY_*` test bindings so the full actor chain runs.
- `whoami` surfaces `enterprise_issuer` on EMA grants — the suite's first catch:
  provenance said "enterprise" but never said which IdP. 223 tests.
- Same day, no version bump: **lockfile policy reversed** — the template commits
  `package-lock.json` and pins `@modelcontextprotocol/server` and `hono`
  exactly; the fleet runs `npm audit --omit=dev` across every target.

## 2026-08-24.2

- `whoami` surfaces `on_behalf_of` / `gateway_purpose` / `gateway_run_id` when
  a gateway assertion verified — a one-call liveness check for the whole
  verification chain.
- `check-placeholders.sh` gains a structural check: it also fails a wrangler
  route whose host disagrees with `PUBLIC_MCP_URL`. Generated repos must run it
  (and the conformance gate) in their own `ci`; the first-deploy checklist says
  to rewrite `ci` at generation time.
- Manifests re-blessed, recording the batch's approved diffs (guard and cap
  ports shrank several; `whoami`'s scope entries deliberately widened the
  `scopes.ts` diffs). 210 tests.

## 2026-08-24.1

The first versioned scaffold: the conformance mechanism for the copied security
files, introduced after an independent review found three real defects living in
the gap between "fixed in one repo" and "documented as fleet-wide".

- Two layers: a stored, reviewed unified diff per repo per security file against
  the template (the manifest of allowed differences), and a per-repo CI hash gate
  (`scripts/check-conformance.mjs` + `conformance.json`) that runs first in
  `npm run ci` and fails when a security file changes without a bless.
  Mutation-checked: an un-blessed one-line edit fails that repo's CI.
- Ten files under conformance (`actor`, `auth`, `consent`, `ema`, `index`,
  `jwt`, `m2m`, `oauth-state`, `resource`, `scopes`).
- Landing it converged the fleet first, so the first manifests recorded
  decisions rather than accidents: the hardened, wired actor verifier everywhere
  with zero drift, the template's `src/resource.ts` adopted fleet-wide, and
  custom-domain servers stopped serving `workers.dev` beside their domains.

## Before versioning (2026-07-30 to 2026-08-24)

- **2026-07-30 — stateless rewrite.** Servers moved from the `McpAgent` /
  Durable Object / `agents` SDK pattern to `@modelcontextprotocol/server` v2's
  `createMcpHandler` under MCP spec 2026-07-28: no sessions, no `initialize`,
  no DO, a fresh `McpServer` per request, caller identity as `AuthInfo`. Not
  every server moved that day; a dated inventory replaced an unverified
  "all servers" claim on 2026-08-18.
- **2026-08-05** — MCP guidance consolidated into one skill; Python/FastMCP and
  Modal kept as cold references.
- **2026-08-18** — never cache an inactive Okta introspection (a pre-auth KV
  write keyed on the presented token was an account-wide DoS primitive);
  `src/actor.ts` added to verify the gateway's `X-MCP-Actor` EdDSA assertion,
  reported alongside the token's subject (`auth_path: "m2m+actor"`).
- **2026-08-21** — per-server scopes replaced the single fleet-wide
  `mcp-access` scope; RFC 9728 resource metadata, issuer pinning, `AuthInfo`
  fixes (`clientId` is the OAuth client, not the user; interactive scopes copied
  from the grant). PKCE finding corrected (mandatory for public clients from
  provider 0.10.3). The one-shared-authorization-server decision and its cost
  (no per-server `aud`) recorded. Fleet M2M verification moved from
  introspection to local JWT verification. Consent-key lessons recorded
  (redirect URI in the key, loopback port canonicalization, mint-before-remember).
- **2026-08-21** — template rewritten up to the fleet's auth layer: server-side
  OAuth state bound to a `__Host-` cookie, the **mandatory consent gate** with a
  four-part remembered decision, CIMD, EMA (ID-JAG), two-layer scope
  enforcement, and the `@cloudflare/vitest-pool-workers` route suite; provider
  → 0.10.3, vitest → 4.1.11, split `tsconfig.test.json`. `wrangler.toml.example`
  became a real `wrangler.toml` (the pool reads it). 171 tests.
- **2026-08-23** — template caught up with the fleet's consent fixes and gained a
  WRITE-scoped destructive tool with an HMAC-sealed MRTR confirmation, so the
  two-scope branches are exercised; `check:placeholders` added and its patterns
  fixed after its first real use; access-token TTL returned to the library
  default once KV writes were measured rather than assumed; the first-deploy
  checklist (no new Okta app per server) written.
- **2026-08-24** — **local JWT verification replaced introspection in the
  template** (`src/jwt.ts`; introspection cannot validate Cross App Access
  tokens, and cost a round trip and a KV write per token), caught by the first
  server generated from the template. The hardened actor verifier
  (`act.client_id` binding, single-use `jti`) was ported and actually wired into
  the dispatcher. 188 → 210 tests.
