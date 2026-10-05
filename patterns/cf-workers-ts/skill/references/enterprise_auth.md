# Enterprise-Managed Authorization (ID-JAG)

The third auth path. Alongside interactive OAuth and M2M bearers, an MCP server can accept an **ID-JAG assertion** — a token the *enterprise's* IdP mints, saying "this client may act for this user at this resource." The user never sees a consent screen, because the enterprise already decided.

The MCP extension is `io.modelcontextprotocol/enterprise-managed-authorization`. The underlying exchange is RFC 8693 token exchange with an RFC 7523 JWT bearer assertion; Anthropic shipped support in beta (Claude Team/Enterprise, Okta-only) on 2026-08-03, gated on Okta Cross App Access.

`@cloudflare/workers-oauth-provider` implements the server half from **0.10.3** (the pattern pins 1.2.1; the EMA option surface is unchanged). You configure it; you do not implement the grant.

## What it looks like on the wire

1. The client asks its enterprise IdP for an ID-JAG naming *your* server as the resource.
2. The client presents that assertion at your `/token` endpoint with `grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer`.
3. The library verifies the assertion against the issuer's JWKS, calls your `mapClaims`, and — if you return a mapping — mints its own access token.
4. The client uses that token exactly like an interactive one.

Claude's discovery signal is `grant_types_supported` containing `urn:ietf:params:oauth:grant-type:jwt-bearer`, **not** the `authorization_grant_profiles_supported` field the MCP spec names. The library emits both, so this only matters when debugging why a client did not attempt the grant.

## Wiring it

```typescript
import { mapEmaClaims, parseTrustedIssuers, trustedIssuerResolver } from "./ema";

// Parse ONCE, outside the request path: parseTrustedIssuers throws on a
// partially-unparseable value, and you want that at startup rather than on some
// unlucky request.
const emaIssuers = parseTrustedIssuers(env.EMA_TRUSTED_ISSUERS);

new OAuthProvider({
  // …
  allowTokenExchangeGrant: true,
  // Unset EMA_TRUSTED_ISSUERS leaves the whole option off, which DISABLES the
  // grant rather than advertising one this server cannot honour.
  ...(emaIssuers.length > 0
    ? {
        enterpriseManagedAuthorization: {
          // Takes the PARSED list, not env.
          trustedIssuers: trustedIssuerResolver(emaIssuers),
          // Passed by reference: it takes a single object argument and reads
          // SUPPORTED_SCOPES from module scope.
          mapClaims: mapEmaClaims,
          // CIMD clients are public — they hold no secret — so this must be true
          // or every real MCP client is refused.
          allowPublicClients: true,
        },
      }
    : {}),
});
```

`src/ema.ts` in the template holds both helpers. It is inert until `EMA_TRUSTED_ISSUERS` is set, which is deliberate: an unset value leaves the grant **disabled** rather than advertising one the server cannot honour.

## The three things that are easy to get wrong

### 1. Scopes must come from the assertion, never from the request

This is the one that bit hardest. `mapClaims` receives both the assertion's claims and the client's `requestedScope`. Granting `requestedScope` — or falling back to it when the assertion carries no `scope` claim — lets a client grant itself anything:

```typescript
// WRONG. A scope-less ID-JAG grants whatever the client asked for.
const granted = assertionScopes.length > 0 ? intersect(assertionScopes, requestedScope) : requestedScope;

// RIGHT. No scope claim means no grant.
const asserted = typeof claims.scope === "string" ? claims.scope.split(" ").filter(Boolean) : null;
if (!asserted || asserted.length === 0) return null;
const granted = asserted.filter((s) => supported.has(s)).filter((s) => requestedScope.includes(s));
if (granted.length === 0) return null;
```

`requestedScope` may only ever **narrow** what the assertion already granted. Returning `null` refuses the exchange, which is the correct answer for an assertion that authorizes nothing.

Note also that Okta strips the OIDC system scopes (`openid`, `profile`, `email`) during the ID-JAG exchange, so define **custom** scopes only — a policy expressed in system scopes will arrive empty.

### 2. `trustedIssuers` is operator config, and `jwksUri` is an SSRF primitive

Each entry pairs an issuer with the JWKS URL used to verify its signatures, and **your server fetches that URL**. So:

- Read it from an env var only. Never from KV, D1, a request, or anything a client can influence.
- Require each `jwksUri`'s origin to equal its issuer's origin. Otherwise a trusted issuer name can point verification at an attacker-controlled key set.
- Refuse the **whole list** if any pair fails to parse, rather than silently trusting the subset that did. A partially-parsed allowlist is an allowlist nobody has read.

```
EMA_TRUSTED_ISSUERS = "https://<enterprise-idp>/oauth2/default=https://<enterprise-idp>/oauth2/default/v1/keys"
```

### 3. DCR is unusable under EMA — you need CIMD

The IdP stamps a **fixed** `client_id` into every assertion, so your server must recognise that client *before* the first assertion arrives. Dynamic registration cannot satisfy that: the client id would be one your server minted at registration time, not the one the enterprise knows.

So EMA requires Client ID Metadata Documents, which requires the `global_fetch_strictly_public` compatibility flag — without it the library reports `client_id_metadata_document_supported: false` and clients fall back to DCR, at which point EMA cannot work. Set `clientIdMetadataDocumentEnabled: true` **and** the flag.

You also need `resourceMetadata.resource` set, which is required to enable EMA at all and pins resource policy for the ordinary interactive flow at the same time.

## What EMA does not do

- **It does not reconcile identity.** The template records the assertion's `sub` and `email` but does not match them against an existing local identity, so the same human arriving interactively and via EMA is two principals. Invisible while reads are unfiltered; it matters the moment anything is scoped to an owner.
- **It does not replace the consent gate.** Interactive flows still consent. EMA bypasses consent *because the enterprise consented on the user's behalf* — which is only true if `trustedIssuers` really is the enterprise's IdP. That list is the whole trust boundary.
- **It does not narrow the audience.** While a fleet shares one Okta authorization server, `aud` is identical across every server on it (`{{okta_audience}}`), so scopes remain the only cross-server separation. Per-server audiences need per-server custom authorization servers.

## Access-token TTL

The template uses the library default, a 1h `accessTokenTTL`, which is also what Anthropic suggests for EMA. Each re-exchange is a token mint plus a `jti` replay-protection write, and on Cloudflare's free tier KV writes are capped at **1,000/day account-wide** — shared across every Worker on the account — so an hourly TTL with many connected clients can become a meaningful fraction of that budget. The template once ran 8h for exactly that reason, and dropped it when measurement showed the account on the paid plan with writes nowhere near any ceiling. Measure your account's KV writes ([platform_facts.md](platform_facts.md#cloudflare-runtime) has the query) before trading token lifetime for write headroom.

## Testing it without an EMA-enabled org

You do not need Claude Team/Enterprise to develop against this. Self-mint ID-JAGs:

1. Generate a key pair, serve a JWKS from anywhere reachable, and point `EMA_TRUSTED_ISSUERS` at it.
2. Sign an assertion carrying every claim the library requires: `iss`, `sub`,
   `aud`, `client_id`, `jti`, `exp`, `iat` — plus `scope`, without which
   `mapClaims` refuses the exchange. And **the JOSE header matters as much as the
   claims**:

   - **`typ` MUST be `oauth-id-jag+jwt`** (RFC 8725 §3.11). `validateIdJagHeader`
     rejects anything else before a single claim is read, so an otherwise perfect
     assertion with no `typ` never reaches `mapClaims`.
   - **Sign with RS256 unless you declare otherwise.** The global allowlist is
     `{RS256, ES256}` — but a trusted issuer that does not set `algorithms` gets
     `[EMA_DEFAULT_JWT_ALGORITHM]`, which is `["RS256"]` **alone**. An ES256
     assertion against such an issuer fails as `issuer_not_trusted`: an algorithm
     problem wearing a trust problem's name, and it is genuinely hard to diagnose
     because every claim is correct. Okta signs with RS256, so the default is also
     the realistic path; set `algorithms: ["ES256"]` on the issuer if you need it.
   - **EdDSA is not accepted at all**, which is easy to conflate with
     `mcp-gateway`'s `X-MCP-Actor` assertions — those *are* EdDSA. Different
     mechanism, different allowlist.
   - The assertion's **lifetime is capped at 300s** (`exp - iat`, plus clock
     skew) by default.

   Three claim-level traps:
   - **`aud` is the authorization server, not the resource.** It is compared
     against `trustedIssuer.audience ?? getAuthorizationServerIssuer(url)` — this
     Worker's own origin unless the trusted-issuer entry overrides it. The
     *resource* travels in a separate, optional `resource` claim that defaults to
     the configured one.
   - **`jti` is required**, and is the replay-protection key: each accepted one
     costs a KV write, keyed on `sha256(issuer + "\n" + jti)`.
   - **`authorization_details` and `cnf` are rejected outright** as unsupported,
     so do not include them speculatively.
   `client_id` must also equal the id of the client actually presenting the
   assertion.
3. `POST /token` with `grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer`.

Cover at minimum: a valid assertion mints a token with exactly the asserted
scopes **and that token is then accepted at `/mcp`** (a token that mints but is
refused downstream is a passing test and a broken feature); an assertion with
**no** `scope` claim is refused; a `requestedScope` broader than the assertion is
narrowed, not honoured; a request for more than the assertion granted is refused;
an untrusted issuer, a foreign `client_id`, a foreign `aud`, an expired assertion,
a replayed `jti`, a missing `typ`, a disallowed `alg` and a tampered signature are
all refused; and a `jwksUri` whose origin differs from its issuer is refused at
config-parse time.

A worked implementation of exactly that — 15 cases in a
`test/ema.workerd.test.ts`, driven through the real `/token` endpoint against a
fake IdP whose JWKS is served by the miniflare `outboundService` — was written
for a two-scope server before the template carried one. Two mechanical notes
from writing it: the key pair must be
**fixed**, not generated, because the JWKS handler runs in the vitest config's
realm while the test runs in its own isolate and they cannot share a generated key
(an attempt to bridge them via `globalThis` silently served 503, which made every
VALID assertion fail while every negative case passed for the wrong reason); and
the DCR client must list the `urn:ietf:params:oauth:grant-type:jwt-bearer` grant
type at registration.

## Cross-references

- [`bearer_token_auth.md`](bearer_token_auth.md) — the `AuthInfo` table, including the `"enterprise"` `auth_path`.
- [`architecture.md`](architecture.md) — storage and the KV write budget.
- `src/ema.ts` in a rendered template (`mcp-studio pattern render --out <dir>`) — the reference implementation of both helpers. It is under conformance.
