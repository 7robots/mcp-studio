// Local JWT validation against the authorization server's JWKS.
//
// WHY THIS EXISTS, and why introspection cannot replace it:
//
// Okta's /v1/introspect returns `{"active": false}` when the client making the
// request is not the client the token was issued to. Under Okta Cross App
// Access the token's client is the AI agent's *workload principal* (`wlp…`),
// and a Worker cannot authenticate as a workload principal — that identity
// signs with the agent's private key, held by the agent, not by us.
//
// The consequence: no amount of configuration makes introspection validate an
// ID-JAG-derived token. It always reports inactive. Verified empirically
// against the live tenant with a token confirmed valid by decoding it.
//
// So the resource server validates the JWT itself: verify the signature
// against the issuer's published JWKS, then check issuer, audience, expiry,
// and scope. No shared secret, no network round-trip on the hot path (jose
// caches the key set in-isolate), and no KV writes.
//
// This is now the PRIMARY M2M path, not an opt-in one. Introspection is gone:
// Okta's default authorization server issues JWT access tokens (verified by
// decoding a live one — RS256, `scp` array, `cid`), so there is nothing an
// introspection round trip can tell us that the signature does not.
//
// AUDIENCE, honestly: `aud` is pinned when OKTA_M2M_AUDIENCE is set, but on a
// SHARED authorization server it separates nothing. Every token this AS issues
// carries the same `aud` — today `{{okta_audience}}`, the fleet-wide
// audience (set 2026-09-28; formerly a dead Modal URL). It is defence against a token from a
// different ISSUER, not against a token for a different server on the same issuer.
// Scopes do that job until each server has its own custom authorization server.

import { createRemoteJWKSet, jwtVerify, type JWTPayload } from "jose";

import type { M2MVerdict } from "./m2m";

const DEFAULT_REQUIRED_SCOPE = "REPLACE:read REPLACE:write";

// Cache the key set per issuer for the isolate's lifetime. jose handles the
// fetch, cooldown, and rotation; re-creating it per request would defeat that.
const jwksCache = new Map<string, ReturnType<typeof createRemoteJWKSet>>();

function jwksFor(issuer: string): ReturnType<typeof createRemoteJWKSet> {
  let set = jwksCache.get(issuer);
  if (!set) {
    set = createRemoteJWKSet(new URL(`${issuer}/v1/keys`));
    jwksCache.set(issuer, set);
  }
  return set;
}

// Okta puts scopes in `scp` (array). Some issuers use `scope` (space-delimited
// string). Accept either so this isn't brittle across authorization servers.
//
// Exported for unit testing — see test/jwt.test.ts.
export function extractScopes(payload: JWTPayload): string[] {
  const scp = (payload as { scp?: unknown }).scp;
  if (Array.isArray(scp)) return scp.filter((s): s is string => typeof s === "string");
  const scope = (payload as { scope?: unknown }).scope;
  if (typeof scope === "string") return scope.split(/\s+/).filter(Boolean);
  return [];
}

// Build the verdict from a verified payload. Signature, issuer, audience, and
// expiry are already checked by jwtVerify before this runs — this applies only
// the scope policy.
//
// Exported for unit testing.
export function decideFromClaims(
  payload: JWTPayload,
  requiredScope: string,
): M2MVerdict {
  const scopes = extractScopes(payload);
  // ANY-OF. `requiredScope` is a space-separated list of the scopes this server
  // implements, so asking whether the token's array contains that whole string
  // would never be true on a multi-scope server — which is exactly the bug this
  // replaced: ported from a single-scope server, where the two readings coincide.
  //
  // Matching the introspection path this supersedes: the door admits a caller
  // holding at least one of this server's scopes, and per-tool enforcement in
  // src/scopes.ts decides what that caller may actually do.
  const accepted = requiredScope.split(/\s+/).filter(Boolean);
  if (accepted.length === 0) return { active: false };
  if (!accepted.some((s) => scopes.includes(s))) return { active: false };

  // `cid` is the agent's workload principal; `sub` is the human it acts for.
  // Both are carried through so downstream code can attribute an action to the
  // user/agent pair rather than to just one of them — which is the entire
  // point of Cross App Access. A token without a cid is unattributable —
  // reject it, mirroring decide()'s client_id requirement on the
  // introspection path (claims.client_id becomes AuthInfo.clientId).
  if (typeof payload.cid !== "string" || payload.cid === "") return { active: false };

  // Same argument as `cid`: a token with no subject is unattributable, and it
  // would reach AuthInfo.extra.sub as undefined — the audit gap the cid check
  // above exists to close. For client_credentials, Okta sets sub to the client id.
  if (typeof payload.sub !== "string" || payload.sub === "") return { active: false };

  return {
    active: true,
    claims: {
      client_id: payload.cid,
      scope: scopes.join(" "),
      sub: payload.sub,
      exp: typeof payload.exp === "number" ? payload.exp : undefined,
    },
  };
}

/**
 * Validate a bearer token locally. Returns null when this path isn't
 * configured or the token isn't a verifiable JWT for this resource, so the
 * caller can fall through to other auth paths.
 */
export async function tryOktaJwt(
  token: string,
  env: Env,
): Promise<M2MVerdict | null> {
  // Optional. See the audience note in the file header: unset is a reasonable
  // production posture while the fleet shares one authorization server, because
  // the value it would pin is the same for every server on it.
  const audience = env.OKTA_M2M_AUDIENCE;

  // One issuer here. solar carries an OKTA_M2M_ISSUER override because it
  // deploys twice against two tenants; adding it where it is unused would be
  // inventing configuration.
  const issuer = env.OKTA_ISSUER;
  const requiredScope = env.OKTA_M2M_SCOPE || DEFAULT_REQUIRED_SCOPE;

  try {
    const { payload, protectedHeader } = await jwtVerify(token, jwksFor(issuer), {
      issuer,
      ...(audience ? { audience } : {}),
      // This is the one auth path with no introspection backstop: a signed
      // token without `exp` would otherwise validate forever.
      requiredClaims: ["exp"],
    });
    // TOKEN-TYPE CONFUSION GUARD (RFC 8725 §3.11). This function accepts an
    // ACCESS TOKEN. Since Phase 3 the same Worker is also an EMA authorization
    // server (src/ema.ts), which accepts an ID-JAG — a JWT that is an
    // authorization GRANT, not a credential for calling this resource. The two
    // trust anchors are disjoint today (EMA_TRUSTED_ISSUERS is unset, and would
    // name an enterprise IdP rather than OKTA_M2M_ISSUER), so an ID-JAG cannot
    // reach here. But nothing structurally prevents an operator from pointing
    // both at one issuer, and an ID-JAG minted for that issuer carrying `cid`
    // and a matching `aud` would otherwise be accepted as a bearer token — the
    // exact confusion the library guards on its own path by requiring
    // `typ=oauth-id-jag+jwt`. Refuse it here by name: a deny-list of one value
    // cannot reject a legitimate access token, whose `typ` is `at+jwt` or absent.
    if (protectedHeader.typ === "oauth-id-jag+jwt") return { active: false };
    return decideFromClaims(payload, requiredScope);
  } catch {
    // Bad signature, wrong issuer/audience, expired, or not a JWT at all.
    // Return null rather than an inactive verdict so a token meant for a
    // different auth path still gets its chance.
    return null;
  }
}
