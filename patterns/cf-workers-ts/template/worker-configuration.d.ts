import type { KVNamespace } from "@cloudflare/workers-types";
import type { OAuthHelpers } from "@cloudflare/workers-oauth-provider";

// ONE shape, exposed twice below. `cloudflare:test` types its `env` as
// `Cloudflare.Env`, a different interface from the global `Env` this Worker's
// source uses, so declaring both from this single interface is what lets
// test/flow.workerd.test.ts read `env.OKTA_ISSUER` and typecheck against the
// real bindings instead of needing a cast.
interface WorkerEnv {
  // Drop-in bindings. No Durable Object: the stateless 2026-07-28 protocol
  // has no per-session state to hold.
  OAUTH_KV: KVNamespace;
  OAUTH_PROVIDER: OAuthHelpers;

  // This server's canonical public MCP endpoint (RFC 8707 resource identifier).
  // src/index.ts derives the RFC 9728 protected-resource metadata and its
  // well-known URL from this, so the advertised identifiers cannot drift from
  // the deployed hostname. src/auth.ts derives the remembered-consent record's
  // resource key from the same value. Set in wrangler.toml [vars] — never
  // derived from the Host header, which a caller controls.
  PUBLIC_MCP_URL: string;

  // Okta config (plain text).
  OKTA_CLIENT_ID: string;
  /** Only `OKTA_ISSUER` is read by src/. OKTA_DOMAIN is kept because the org
   *  domain is what you check when an issuer looks wrong (and because using the
   *  `-admin` console hostname here is a classic 404), but nothing consumes it. */
  OKTA_DOMAIN?: string;
  OKTA_ISSUER: string;
  OKTA_SCOPES: string;
  /** Space-separated ANY-OF list; a token needs at least one to be accepted. */
  OKTA_M2M_SCOPE: string;
  /** Optional expected `aud`. Unset = issuer pinning only; see wrangler.toml. */
  OKTA_M2M_AUDIENCE?: string;
  // Optional: M2M introspection cache TTL in seconds (default 300). Raise to
  // cut KV writes; capped per-token at the token's own `exp`.

  /** EMA (Phase 3): space/comma-separated `issuer=jwksUri` pairs, both https
   *  and same-origin. UNSET disables the ID-JAG grant rather than advertising
   *  one this server cannot honour; a partially unparseable value throws. See
   *  src/ema.ts and the note in wrangler.toml. */
  EMA_TRUSTED_ISSUERS?: string;

  /** Optional — only used by the opt-in src/actor.ts. See its header comment
   *  and the commented block in wrangler.toml. All three are absent unless this
   *  server sits behind mcp-gateway. */
  // string | boolean: TOML `= true` arrives as a boolean, `= "true"` as a
  // string, and actorRequired() accepts both.
  REQUIRE_GATEWAY_ACTOR?: string | boolean;
  GATEWAY_ISSUER?: string;
  GATEWAY_JWKS_URL?: string;

  // Okta secrets (set via `wrangler secret put`, not in wrangler.toml).
  OKTA_CLIENT_SECRET: string;
  /** VESTIGIAL — nothing reads this. `workers-oauth-provider` 0.10.3 derives its
   *  props-encryption keys from the tokens themselves (`unwrapKeyWithToken`), so
   *  there is no configured key any more. Kept optional and stubbed in
   *  vitest.config.ts so an older provider, or a future one that reintroduces
   *  it, does not fail closed — but do NOT document it as a required deploy
   *  step. It was one. */
  COOKIE_ENCRYPTION_KEY?: string;
  /**
   * HMAC key for the MRTR confirmation state on destructive tools
   * (src/confirm.ts). At least 32 bytes. OPTIONAL in the type because a deploy
   * without it must degrade to "confirmation unavailable" rather than to an
   * unprotected state — and the destructive tool then REFUSES rather than acting
   * unconfirmed. The same value must reach every isolate, so never a
   * per-instance random.
   *
   *   openssl rand -base64 48 | tr -d '\n'   # then store in 1Password
   */
  REQUEST_STATE_KEY?: string;

  // REPLACE — data-layer bindings. The template's `list_examples` tool and
  // src/data.ts use a D1 database bound as `DB`. Delete this (and the
  // [[d1_databases]] block in wrangler.toml, src/data.ts, and the tool) if your
  // server has a different data layer, or none.
  DB: D1Database;
}

declare global {
  interface Env extends WorkerEnv {}

  // This must live INSIDE `declare global`: the file is a module (it has
  // `export {}`), so a top-level `declare namespace` would be file-local and
  // `cloudflare:test`'s `env` would stay untyped.
  namespace Cloudflare {
    interface Env extends WorkerEnv {}
  }
}

export {};
