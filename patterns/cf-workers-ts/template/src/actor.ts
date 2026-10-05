// OPT-IN — wired into src/index.ts's M2M path, but enforcement is off until
// REQUIRE_GATEWAY_ACTOR is set (and GATEWAY_ISSUER / GATEWAY_JWKS_URL are
// uncommented in wrangler.toml).
//
// It verifies mcp-gateway's `X-MCP-Actor` assertion. An M2M bearer proves only
// that *a platform workload* is calling; it says nothing about which human asked.
// The gateway therefore also sends a 60-second EdDSA assertion, signed with a key
// only it holds and published at its JWKS endpoint, naming who the call is for.
//
// WHAT "VERIFY" HAS TO MEAN. Two claims used to be declared here and never
// checked, which is worse than not carrying them: downstream code reads
// `on_behalf_of`/`gateway_purpose` off this verdict and would have been trusting
// values nothing had validated. (Review finding F-1: the checks existed only in
// Solar while the fleet documents called them fleet-wide.)
//
//   - `act.client_id` says which client the gateway was fronting. Unchecked, any
//     platform workload holding a valid bearer could present an assertion naming
//     a DIFFERENT client and have this server record the call against it. It is
//     bound to the calling token's own client id (see checkClaims).
//   - `jti` makes an assertion single-use. Unchecked, a captured assertion was
//     replayable for as long as this server would accept it — the minter's 60s
//     lifetime plus the skew below — by anyone who also held a valid bearer. It
//     is consumed in KV on first successful verification.
//
// Both are enforced whenever an assertion is PRESENT. REQUIRE_GATEWAY_ACTOR
// governs whether an assertion is REQUIRED, never whether a present one is
// checked — a flag that turned verification off would make the header a
// decoration.
//
// A verified assertion is reported ALONGSIDE the token's subject, never
// replacing it: `extra.sub` stays the workload, `extra.on_behalf_of` carries the
// human.
//
// TWO THINGS TO KNOW BEFORE RELYING ON IT:
//
//  1. With REQUIRE_GATEWAY_ACTOR unset or "false", a JWKS fetch failure degrades
//     SILENTLY to "not required" — indistinguishable from no assertion at all,
//     because nothing here logs. Verify the fetch actually works: with the gate
//     temporarily ON, a claim-valid assertion carrying a WRONG signature must be
//     refused with "signature did not verify". That reason is downstream of the
//     JWKS fetch and the kid match, so it proves the whole chain. A refusal
//     saying "could not retrieve the gateway's JWKS" means the subrequest failed.
//  2. This pattern retires once M2M tokens carry a per-server `aud` (RFC 7523,
//     verified locally against the IdP's JWKS). At that point the token itself
//     identifies the target server and the actor assertion is the only thing
//     still carrying the human — keep it for that, or drop it if the IdP starts
//     asserting the user directly.

import { resourceUrls } from "./resource";

const MAX_CLOCK_SKEW_SECONDS = 30;
const JWKS_CACHE_SECONDS = 3600;

/**
 * Storage prefix for actor replay markers. Distinct from the library's
 * `enterprise-jti:` namespace (EMA ID-JAGs): different issuer, different
 * lifetime, different trust decision, so a collision between the two would be a
 * cross-path bug rather than a coincidence.
 */
const ACTOR_JTI_KV_PREFIX = "gateway-actor-jti:";

/**
 * KV's floor for `expirationTtl`, verified against miniflare rather than assumed:
 * a put with `expirationTtl: 5` fails with
 * `400 Invalid expiration_ttl of 5. Expiration TTL must be at least 60.`
 *
 * This is why the marker TTL is clamped instead of being exactly the assertion's
 * remaining life, the way the library's EMA jti store does it (`Math.max(1, exp
 * - now)`). The gateway mints 60-second assertions
 * (mcp-gateway-worker/src/actor.ts: ASSERTION_TTL_SECONDS = 60), so an unclamped
 * copy would be under this floor for essentially every assertion, not just old
 * ones — turning replay protection into a 500. Erring LONG is the safe
 * direction: a marker outliving the assertion only refuses replays that were
 * already too old to accept.
 */
const KV_MIN_TTL_SECONDS = 60;

export interface ActorClaims {
  iss: string;
  sub: string;
  aud: string;
  act?: { client_id?: string };
  purpose?: string;
  run_id?: string;
  iat: number;
  exp: number;
  jti: string;
}

export interface ActorVerdict {
  ok: boolean;
  claims?: ActorClaims;
  reason?: string;
}

interface Jwk {
  kty?: string;
  crv?: string;
  x?: string;
  kid?: string;
  alg?: string;
}

export function extractActorHeader(request: Request): string | null {
  const raw = request.headers.get("x-mcp-actor");
  return raw && raw.trim() ? raw.trim() : null;
}

function decodeSegment(segment: string): Record<string, unknown> | null {
  try {
    const padded = segment.replace(/-/g, "+").replace(/_/g, "/");
    return JSON.parse(atob(padded + "=".repeat((4 - (padded.length % 4)) % 4)));
  } catch {
    return null;
  }
}

function decodeSignature(segment: string): Uint8Array | null {
  try {
    const padded = segment.replace(/-/g, "+").replace(/_/g, "/");
    const binary = atob(padded + "=".repeat((4 - (padded.length % 4)) % 4));
    return Uint8Array.from(binary, (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}

// Pure: everything decidable without the key. Separated so the policy is
// testable without generating a keypair, and so a malformed assertion is
// rejected before any network call to fetch a JWKS.
export function checkClaims(
  claims: Record<string, unknown>,
  expected: { issuer: string; audience: string; callerClientId: string },
  now: number,
): string | null {
  if (typeof claims.iss !== "string" || claims.iss !== expected.issuer) {
    return `issuer mismatch (expected ${expected.issuer})`;
  }
  if (typeof claims.aud !== "string" || claims.aud !== expected.audience) {
    // Audience binding is what stops an assertion minted for one server being
    // replayed against another.
    return `audience mismatch (expected ${expected.audience})`;
  }
  if (typeof claims.sub !== "string" || !claims.sub) return "missing subject";
  if (typeof claims.exp !== "number" || claims.exp + MAX_CLOCK_SKEW_SECONDS < now) return "assertion expired";
  if (typeof claims.iat !== "number" || claims.iat - MAX_CLOCK_SKEW_SECONDS > now) return "assertion issued in the future";

  // A jti is what makes the assertion single-use, so an assertion without one
  // cannot be accepted: there would be nothing to consume, and "no jti" would
  // become the way to opt out of replay protection. Checked here, before any
  // network call, so verifyActor can treat the value as a string.
  if (typeof claims.jti !== "string" || !claims.jti) return "assertion has no jti";

  // The acting client, bound to the client that actually presented the bearer
  // token. The gateway always sets `act.client_id`; ABSENT IS REFUSED rather
  // than waved through, because "unstated" and "matches" are not the same
  // statement and only one of them is something this server can record. An
  // optional binding is not a binding.
  const act = typeof claims.act === "object" && claims.act !== null
    ? (claims.act as { client_id?: unknown })
    : null;
  const actClientId = act?.client_id;
  if (typeof actClientId !== "string" || !actClientId) return "assertion names no acting client";
  if (actClientId !== expected.callerClientId) {
    // Both values are identifiers the caller already holds, so naming them costs
    // nothing and makes a real misconfiguration debuggable.
    return `acting-client mismatch (assertion names ${actClientId}, calling token is ${expected.callerClientId})`;
  }
  return null;
}

async function sha256Hex(input: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(input));
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/**
 * Consume an assertion's `jti`, returning false when it has already been used.
 *
 * Mirrors the KV jti store the OAuth provider library uses for EMA ID-JAGs
 * (`createKvJtiStore`, node_modules/@cloudflare/workers-oauth-provider): the key
 * is a hash of `issuer\njti` so neither value's length or content leaks into a
 * KV key, get-then-put, and the TTL covers the assertion's own acceptance window
 * so KV expiry does the cleanup and nothing accumulates.
 *
 * SAME CAVEAT AS THE LIBRARY'S: KV is eventually consistent and offers no
 * compare-and-set, so two truly concurrent requests carrying the same jti can
 * both read "not seen" and both succeed. That is the accepted trade — the
 * surrounding checks (signature, issuer, audience, exp/iat window, and the
 * client binding) bound the window to a few seconds of one client's own
 * assertion, which is a far smaller target than the full 60s-plus-skew replay
 * window this closes. A Durable Object would be needed for a hard guarantee.
 *
 * COST: one KV write per accepted assertion. Deliberate — the free tier's 1,000
 * writes/day is account-wide, but the gateway path is low volume, and replay
 * protection that is skipped to save a write is not replay protection.
 */
async function consumeJti(
  kv: KVNamespace,
  issuer: string,
  jti: string,
  exp: number,
  now: number,
): Promise<boolean> {
  const key = ACTOR_JTI_KV_PREFIX + (await sha256Hex(`${issuer}\n${jti}`));
  if (await kv.get(key)) return false;
  await kv.put(key, "1", {
    // The marker must outlive the window in which the assertion is still
    // ACCEPTED, which is not the same as the window in which it is valid:
    // checkClaims admits it while `exp + MAX_CLOCK_SKEW_SECONDS >= now`. A TTL
    // of `exp - now` expires the marker AT exp and leaves the skew allowance
    // uncovered — with the minter's 60s lifetime that is a 30-second hole in
    // which a captured assertion replays exactly once, i.e. most of what this
    // function exists to prevent. The skew is added on both sides or neither.
    expirationTtl: Math.max(KV_MIN_TTL_SECONDS, exp - now + MAX_CLOCK_SKEW_SECONDS),
  });
  return true;
}

async function fetchJwks(url: string, fetchImpl: typeof fetch): Promise<Jwk[] | null> {
  try {
    // Cached at the edge rather than in KV: the free tier allows 1,000 KV
    // writes/day account-wide, and a public document with its own cache-control
    // does not need to spend any of them.
    const res = await fetchImpl(url, { cf: { cacheTtl: JWKS_CACHE_SECONDS, cacheEverything: true } } as RequestInit);
    if (!res.ok) return null;
    const body = (await res.json()) as { keys?: Jwk[] };
    return Array.isArray(body.keys) ? body.keys : null;
  } catch {
    return null;
  }
}

/**
 * `caller` is the client that presented the BEARER token this assertion
 * accompanies — required, not optional, so a call site that cannot say who is
 * calling is a compile error rather than a silently unbound check. The calling
 * token's own `cid` claim (surfaced as `M2MVerdict.claims.client_id`) is exactly
 * the right value to bind to: it is what the IdP signed.
 */
export async function verifyActor(
  request: Request,
  env: Env,
  caller: { clientId: string },
  deps: { fetchImpl?: typeof fetch; now?: () => number } = {},
): Promise<ActorVerdict> {
  const fetchImpl = deps.fetchImpl ?? fetch;
  const now = Math.floor((deps.now?.() ?? Date.now()) / 1000);

  // This file is opt-in (see the header), so its two vars are optional in Env.
  // Unconfigured is NOT quietly treated as valid: with REQUIRE_GATEWAY_ACTOR off
  // this verdict is ignored, and with it on this refuses every caller — which is
  // the correct direction for a gate whose configuration is missing.
  const issuer = env.GATEWAY_ISSUER;
  const jwksUrl = env.GATEWAY_JWKS_URL;
  if (!issuer || !jwksUrl) {
    return { ok: false, reason: "gateway actor verification is not configured" };
  }

  const token = extractActorHeader(request);
  if (!token) return { ok: false, reason: "no X-MCP-Actor header" };

  const parts = token.split(".");
  if (parts.length !== 3) return { ok: false, reason: "malformed assertion" };

  const header = decodeSegment(parts[0]);
  const claims = decodeSegment(parts[1]);
  const signature = decodeSignature(parts[2]);
  if (!header || !claims || !signature) return { ok: false, reason: "malformed assertion" };

  // Only EdDSA. Accepting whatever the header names is how JWT verification
  // gets broken, "none" included.
  if (header.alg !== "EdDSA") return { ok: false, reason: `unsupported algorithm ${String(header.alg)}` };

  // The CANONICAL resource, not the raw var. resourceUrls() trims a trailing
  // slash and drops query/fragment, and `new URL()` lowercases the host — so a
  // PUBLIC_MCP_URL of `…/mcp/` or `…/MCP.example.com/…` advertises one string in
  // the RFC 9728 metadata and would demand a different one here.
  const claimError = checkClaims(
    claims,
    {
      issuer,
      audience: resourceUrls(env.PUBLIC_MCP_URL).resource,
      callerClientId: caller.clientId,
    },
    now,
  );
  if (claimError) return { ok: false, reason: claimError };

  // Fail closed rather than throwing: without a namespace there is nowhere to
  // record single use, and an assertion that cannot be consumed must not be
  // accepted as if it had been.
  if (!env.OAUTH_KV) return { ok: false, reason: "no KV namespace for replay protection" };

  const keys = await fetchJwks(jwksUrl, fetchImpl);
  if (!keys) return { ok: false, reason: "could not retrieve the gateway's JWKS" };

  // Match on kid when the assertion names one; otherwise try every published
  // key, which keeps verification working across a rotation.
  const candidates = header.kid ? keys.filter((k) => k.kid === header.kid) : keys;
  if (candidates.length === 0) return { ok: false, reason: "no published key matches this assertion" };

  const signed = new TextEncoder().encode(`${parts[0]}.${parts[1]}`);
  let verified = false;
  for (const jwk of candidates) {
    try {
      const key = await crypto.subtle.importKey("jwk", jwk as JsonWebKey, { name: "Ed25519" }, false, ["verify"]);
      if (await crypto.subtle.verify({ name: "Ed25519" }, key, signature, signed)) {
        verified = true;
        break;
      }
    } catch {
      // A key that will not import is not a reason to stop trying the others.
    }
  }
  if (!verified) return { ok: false, reason: "signature did not verify" };

  // ORDER IS LOAD-BEARING: the jti is consumed only once the signature has
  // verified. Marking it earlier would let anyone who can guess or observe a jti
  // burn it with an unsigned assertion — denying the legitimate holder their one
  // use, and spending a KV write per forgery.
  //
  // Deliberately OUTSIDE the loop above rather than inside its try. That catch
  // exists for "this key would not import, try the next one", and a KV error
  // falling into it was swallowed and then reported as `signature did not
  // verify` — a storage outage that reads as key rotation, which is the wrong
  // thing to hand an operator debugging a 403. It still fails CLOSED, with its
  // own reason, and does not throw: with the flag off a KV outage must degrade
  // the caller to plain M2M, not 500 every direct M2M request.
  let fresh: boolean;
  try {
    fresh = await consumeJti(
      env.OAUTH_KV,
      issuer,
      (claims as { jti: string }).jti,
      (claims as { exp: number }).exp,
      now,
    );
  } catch (e) {
    return {
      ok: false,
      reason: `could not record single use of this assertion: ${e instanceof Error ? e.message : String(e)}`,
    };
  }
  if (!fresh) return { ok: false, reason: "assertion has already been used (jti replay)" };
  return { ok: true, claims: claims as unknown as ActorClaims };
}

export function actorRequired(env: Env): boolean {
  // Accepts the boolean too, not just the string. wrangler.toml is TOML, so
  // `REQUIRE_GATEWAY_ACTOR = true` (unquoted) is the natural thing to write and
  // arrives as a real boolean — against a `=== "true"` compare that is false,
  // and the gate is SILENTLY OFF while the operator believes it is on. Failing
  // open on a plausible typo is unacceptable in the one setting this file's
  // header calls a deliberate cutover.
  const raw: unknown = env.REQUIRE_GATEWAY_ACTOR;
  return raw === true || raw === "true";
}

/**
 * Enforcement point for the gateway assertion, called by src/index.ts on the
 * M2M path. Lives HERE rather than in index.ts so the policy is testable in
 * Node — index.ts imports `cloudflare:` modules and can only be exercised
 * through workerd. (F-1's sharpest lesson: this file used to be exported and
 * never imported, so nothing enforced anything.)
 *
 * THE FLAG GOVERNS ONE THING: whether an assertion is REQUIRED. It never
 * governs whether a present one is CHECKED. So with the flag off, an assertion
 * that does not verify — forged, expired, naming another client, or replayed —
 * falls through to `{}` and the caller is served as the plain M2M caller it is
 * entitled to be, WITHOUT an actor. It is never served as `{ actor }`, so a
 * bogus assertion can never turn into an `+actor` auth_path or an
 * `on_behalf_of` attribution that nothing validated.
 */
export async function gateOnActor(
  request: Request,
  env: Env,
  caller: { clientId: string },
  deps: { fetchImpl?: typeof fetch; now?: () => number } = {},
): Promise<{ refusal?: Response; actor?: ActorClaims }> {
  const verdict = await verifyActor(request, env, caller, deps);
  if (verdict.ok) return { actor: verdict.claims };
  if (!actorRequired(env)) return {};
  return {
    refusal: new Response(
      JSON.stringify({
        error: "forbidden",
        error_description: `this server requires a valid X-MCP-Actor assertion from ${env.GATEWAY_ISSUER}: ${verdict.reason}`,
      }),
      { status: 403, headers: { "content-type": "application/json" } },
    ),
  };
}
