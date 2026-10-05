# Platform facts

Behaviour of `@cloudflare/workers-oauth-provider`, `@modelcontextprotocol/server`
and the Workers runtime that cost time to discover and is not obvious from the
documentation. Read before designing around any of these.

Versions these were measured against: provider `0.10.3`, server SDK `2.0.0`,
`@cloudflare/vitest-pool-workers` `0.22.0`, `vitest` `4.1.11`,
`@cloudflare/workers-types` `5.20260821.1`, `zod` `4.4.3`. Re-verified on
2026-10-05 against provider `1.2.1`, server SDK `2.3.1`, `typescript` `7.0.2`;
the 1.x deltas are in [§ workers-oauth-provider 1.x](#workers-oauth-provider-1x).

## workers-oauth-provider

**There is no `requirePKCE` option and no `client_credentials` grant.** The grant
types are exactly `authorization_code`, `refresh_token`, `token-exchange` and
`jwt-bearer`. Access tokens are opaque, formatted `${userId}:${grantId}:${random}`
— the library cannot issue JWTs. Any design that assumes "every resource server
verifies a JWT from our AS" is unavailable.

**PKCE is mandatory for public clients anyway.** `validateAuthorizationPkce`
throws for `response_type=code` with `token_endpoint_auth_method: "none"`, and DCR
and CIMD clients are always public. The residual gap is a *confidential* client
omitting PKCE, for which no option exists.

**`allowPublicClients` defaults to `false`, which makes CIMD and EMA mutually
exclusive until you flip it.** The EMA grant otherwise demands client
authentication, and a CIMD client has no secret to present. Turning it on is
defensible — trust then rests on the IdP-issued, signature-verified, short-lived,
single-use, audience- and client-bound assertion — but make it a decision, not an
accident.

**CIMD advertisement is gated on the compatibility flag.** The library reports
`client_id_metadata_document_supported: false` unless
`global_fetch_strictly_public` is set, with no error explaining why.

**The `aud` comparison is conditional.** A grant that recorded no `resource` skips
the check entirely and can be exchanged naming any server, so audience validation
cannot be delegated to the library.

**`jwksUri` is an SSRF primitive.** The library fetches it and enforces only the
`https:` scheme. Require the `jwksUri` origin to equal the issuer's, and make the
resolver a static closure.

**`mapEmaClaims` must read `claims.scope`, never `input.requestedScope`.** The
library downscopes the request to the assertion only inside
`if (assertionScopes.length > 0)`, so an ID-JAG carrying **no** `scope` claim lets
the client name its own scopes — and with `allowPublicClients: true` the assertion
is the only credential. Deny an assertion naming nothing the server implements
rather than defaulting to read.

**EMA claim validation specifics.** `aud` is compared against
`trustedIssuer.audience ?? getAuthorizationServerIssuer(url)` — the *authorization
server's* issuer, not the resource. `resource` is a separate optional claim. `jti`
is required. `authorization_details` and `cnf` are rejected outright. `typ` must be
`oauth-id-jag+jwt` (RFC 8725 §3.11), and a trusted issuer configured without an
`algorithms` list accepts **RS256 only**.

**`COOKIE_ENCRYPTION_KEY` is unnecessary in 0.10.3.** The provider derives
props-encryption keys from the tokens themselves (`unwrapKeyWithToken`). It was a
mandatory deploy step that did nothing.

**Redirect-URI matching ignores the port for loopback** (RFC 8252), and
`isValidRedirectUri` compares `hostname` **verbatim** while `isLoopbackUri`
lowercases only for its own `localhost` comparison. WHATWG `URL` case-normalizes
the host only for *special* schemes, so under a custom native-app scheme
`myapp://LOCALHOST/cb` and `myapp://localhost/cb` are distinct to the provider. Any
key you derive from a redirect URI must be **exactly as loose as the matcher and no
looser** — a stricter key does not fail closed, it fails NOISY, and noise is what gets
clicked through.

**Setting `resourceMetadata.resource` switches on exact RFC 8707 audience
pinning**, so the first request after that deploy 401s for any token minted before
it. Refresh recovers, so a retrying client self-heals and a non-retrying one looks
broken until re-authorization. Servers whose `OAUTH_KV` holds no grants are
unaffected.

**Per-token downscoping is not visible to the API handler.** The library
downscopes per token and stores `scope` on the token record, but hands the handler
only the grant's props — so a token narrowed on refresh or through RFC 8693
exchange still carries the grant's full scope. A `tokenExchangeCallback` is the fix.

## workers-oauth-provider 1.x

The pattern moved 0.10.3 → 1.2.1 on 2026-10-05 (pattern version 2026-10-05.1).
The package ships its own guide, `docs/migration-1.0.md`; what mattered here:

- **`resourceMetadata.resource` is required at construction**, and it becomes
  every Worker-issued token's audience (exact match, RFC 8707). The template
  already set it from `PUBLIC_MCP_URL`. A server that spread `resourceMetadata`
  in conditionally (one did) must instead refuse every route when the var is
  missing — there is no provider to build without it.
- **`requiredScopes` replaces `resourceMetadata.scopes_supported`**; setting both
  throws. Same wire output. It is *advertised, not enforced* — scope enforcement
  stays with the pre-handler gate and `requireScope`.
- **`allowPlainPKCE` / `allowImplicitFlow` are removed** (throw if `true`); plain
  is refused as `invalid_request`.
- **`AuthorizationError.redirectTo`** is the ready-made error redirect, set only
  when the redirect URI validated. `src/auth.ts` uses it instead of building the
  URL by hand; absent means render locally.
- **Protected-resource metadata is served only on the canonical resource's
  origin.** A request for `/.well-known/oauth-protected-resource/mcp` under any
  other Host — including the Worker's own `*.workers.dev` name — gets 404. 0.x
  served the configured document on every Host. The 401 challenge follows suit
  (RFC 9728 §3.3): on the canonical host it carries `resource_metadata=…`; on
  any other host it carries only `scope=`. So **a `*.workers.dev` URL no longer
  supports interactive OAuth discovery** — point connectors at the custom
  domain. M2M callers (the gateway) bypass OAuthProvider and are unaffected.
- **User IDs may not contain `:`** — Okta `sub` values never do; `src/ema.ts`
  already refused them.
- **No KV migration.** Stored 0.x grants and tokens keep working; an
  audience-less access token is treated as bound to the sole resource and is
  rebound on refresh.

## @modelcontextprotocol/server 2.0.0

**`createMcpHandler(createServer)` takes a factory, not an instance**, and
`export default createMcpHandler(...)` breaks: Wrangler reads a function export as
a `WorkerEntrypoint` class.

**Era selection is either-signal.** A request is served as 2026-07-28 if it carries
**either** the `MCP-Protocol-Version` header **or** the per-request `_meta` envelope
(`io.modelcontextprotocol/protocolVersion` +
`io.modelcontextprotocol/clientCapabilities`). Neither means legacy. A *partial*
envelope returns `-32602` naming the missing keys. Probing with neither makes the
entire 2026 surface look unimplemented.

**Header validation is modern-path only.** `Mcp-Method`/`Mcp-Name` are
cross-checked against the body only for modern-classified requests, so a legacy
POST whose header names a different tool than its body is never checked, and a
legacy JSON-RPC batch cannot be described by one header set at all. Header-derived
policy may refuse early; it must never admit early.

**`x-mcp-header` is mirror-and-validate, not injection.** It is a raw JSON Schema
extension key on a tool property; the client sends `Mcp-Param-<literal value>`
with no derivation from the property name. The SDK never merges the header into the
arguments — the body stays authoritative and complete, a header-only argument is
silently dropped, and a header disagreeing with the body is rejected `-32020`. Only
`string`/`integer`/`boolean`/`number` are permitted, and **an invalid declaration
anywhere in a tool's schema makes the SDK skip the cross-check for that entire
tool** rather than erroring. Verified that zod v4 `.meta({"x-mcp-header": "Name"})`
reaches the emitted schema.

**MRTR `requestState` has no integrity protection by default.** The handler
receives whatever string came back. `createRequestStateCodec` (HMAC-SHA256, key ≥32
bytes, `ttlSeconds`, `bind`) plus `ServerOptions.requestState.verify` is the
supported pair; signed, not encrypted, so nothing secret goes in the payload. Bind
it to the caller — signing alone makes a valid confirmation a bearer token for that
operation. `requestState` and `inputResponses` ride **params**, not `arguments`.
The same key must reach every isolate, so never a per-instance random.

**Embedded MRTR requests are gated on client capabilities.** Returning an
`elicitation/create` to a client that declared no `elicitation.form` capability
fails with `-32021` before the result reaches the wire. Only `tools/call`,
`prompts/get` and `resources/read` may return `InputRequiredResult`.

**The tasks extension has no runtime.** `tasks/get`, `tasks/result`, `tasks/list`,
`tasks/cancel` and `notifications/tasks/status` exist as wire types, every one
marked `@deprecated`, and `TaskRequestMethod` is *subtracted* from the usable
method surface. No handler is registered and the SDK stores nothing. MRTR is the
supported path for long-running work; building tasks means building the whole
extension against a type layer that refuses those method names.

**`cacheHints` is an `McpServer` option, not a `createMcpHandler` one.** The SDK
default is `ttlMs: 0, cacheScope: "private"` — every field present, nothing
cacheable, which reads as configured. `cacheScope: "public"` is sound **only** if
the server does not filter its catalogue by caller scope; assert that invariant
(two tokens, different scopes, identical tool lists) rather than commenting it.

**`server/discover` is a MUST for servers.** Cloudflare's post calls it optional,
which is true only of the client's decision to call it.

**Decode the `=?base64?…?=` sentinel before comparing a header to the body**, and
use the **canonical** base64 alphabet — the SDK rejects base64url with `-32020`, so
accepting it means your gate and the dispatcher disagree about the request.

**`traceparent` is constants only.** `TRACEPARENT_META_KEY` and friends are
exported strings with zero runtime; reading, generating and propagating a trace is
entirely yours.

**Roots and sampling are CLIENT capabilities a server must not declare**; logging
IS a server capability and `sendLoggingMessage` **silently no-ops** if undeclared.
All three are deprecated as of 2026-07-28 (SEP-2577). On a stateless Worker the
per-request `_meta` log level is the only one that works — a session map does not
survive.

**Other SDK v2 notes.** `McpAgent` is deprecated and feature-frozen. A dual lane
needs `legacy: 'reject'` on the stateless handler or its compatibility lane eats
requests before the sessionful route sees them. `skipIssuerMetadataValidation`
weakens mix-up protection. MRTR elicitation handlers are in-memory, so isolate
restart or transport loss rejects an active interactive call. A custom transport
must preserve `MCP-Protocol-Version`, `Mcp-Method`, `Mcp-Name` and `Mcp-Param-*`.

## Cloudflare runtime

**Never use `caches.default` for a token cache.** It is shared across every Worker
on the zone, so a co-resident workload could pre-plant an entry. An in-isolate
`Map` with positive results only, `min(60s, expires_in)` TTL with a 1s floor,
absolute per-entry expiry and a FIFO cap works instead.

**A pre-authentication KV write keyed on request input is a DoS primitive
against the whole account.** The pattern's removed introspection path cached
*inactive* verdicts keyed on the presented token: 1,000 unauthenticated requests
with random bearers would exhaust the free tier's account-wide 1,000-writes/day
KV budget and take OAuth token issuance down for every Worker on the account.
Fixing it without a worker-token prefilter made every interactive tool call pay a
blocking introspection — the two fixes had to ship together. Local JWT
verification removed the write entirely; keep the shape in mind anywhere else a
request can choose a storage key before it has authenticated.

**The KV write budget is 1,000/day account-wide** on the free tier, shared across
every Worker; the paid plan gives 1 million/month. It shapes token TTLs,
introspection caching and `jti` replay markers in ways that are invisible until
they bite — but **measure before conserving**. One fleet ran an 8-hour
access-token TTL in every server to protect that budget and, when finally
measured, was on the paid plan with a peak of 150 writes/day. One GraphQL query
answers it:

```graphql
{ viewer { accounts(filter: {accountTag: "<id>"}) {
    kvOperationsAdaptiveGroups(limit: 50,
      filter: {date_geq: "<date>", actionType: "write"}) {
      sum { requests } dimensions { date }
    } } } }
```

Note also that **Dynamic Workers (`worker_loaders`) are paid-plan only**, so a
Worker using one is proof the account is not on the free tier.

**KV `get`+`delete` is not atomic and reads are eventually consistent**, so
single-use consumption of a KV record is not absolute. A Durable Object is the only
hard guarantee.

**A Worker cannot fetch another Worker on the same account** without
`global_fetch_strictly_public` — the failure is Cloudflare error 1042, and it looks
like an auth problem.

**`@cloudflare/vitest-pool-workers` reads `wrangler.toml`.** Shipping it as
`wrangler.toml.example` means miniflare never sees `main` and every route test
fails with `requires poolOptions.workers.main to be set`.

**An uppercase wrangler `name` makes wrangler refuse the config**, which surfaces
as the workerd project reporting "no tests" rather than an error.

**TOML type coercion fails open.** `REQUIRE_GATEWAY_ACTOR = true` unquoted arrives
as a boolean, so a `=== "true"` check leaves the gate **off** while the operator
believes it is on. Quote booleans meant to be read as strings.

**Splitting `tsconfig.test.json` gates `node:*` imports, `require` and
`__dirname` — but not `process`, `Buffer` or `setImmediate`**, which
`@cloudflare/workers-types` declares itself.

**Header values are normalized by the Fetch spec when set**, so `Headers.get()`
never returns leading or trailing whitespace. A gate does not need to strip it, in
undici or in workerd.

**Rate limiting is abuse dampening, not accounting.** Cloudflare documents the
`ratelimit` binding as "permissive, eventually consistent, and intentionally
designed to not be used as an accurate accounting system"; counters are per
location and `simple.period` must be exactly 10 or 60. One limit per binding, so
distinct limits need distinct bindings.

**An unsigned identity header is only safe behind private routing.** A publicly
addressable Worker must have the identity signed.

## Okta

**Okta sets `aud` per authorization server, not per request.** Every token from one
custom AS carries the same audience regardless of target, so audience pinning
defends against another *issuer* and never against another server on the same
issuer. Scopes carry the whole burden of cross-server separation.

**A default scope distinguishes nothing.** A scope marked default on the AS is
added to `client_credentials` tokens whether or not it was requested, so every
caller of every server holds it.

**Access-policy rules match only when EVERY requested scope is in the rule's
list**, and a rule whose "client acting on behalf of itself" box is checked matches
a `client_credentials` request on grant type alone — the user/group condition
cannot apply, because such a request has no user. Both mistakes hand a privileged
scope to a service account.

**A server's scopes must exist on the AS before its deploy**, or a hard cutover
leaves it with no working M2M path.

**Interactive-login cutover order:** register the new AS URI with the connectors
first, flip, verify with a real client login, remove the old URI last.
