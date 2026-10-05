// Bearer-token helpers for the M2M path.
//
// HISTORY, because what is gone matters more than what is left. This module used
// to introspect every M2M bearer against Okta's /v1/introspect and cache the
// verdict in KV. That cost a round trip per distinct token on the hot path, a KV
// read, and — until the active-only gate was added on 2026-08-18 — a KV WRITE per
// token, against a ceiling of 1,000 writes/day shared ACCOUNT-WIDE across every
// Worker. 1,000 unauthenticated requests with random bearers could exhaust the
// whole account's budget.
//
// Verification is local now (src/jwt.ts): Okta's default authorization server
// issues JWT access tokens — confirmed by decoding a live one, RS256 with an
// `scp` array — so the signature tells us everything introspection did, with no
// network on the hot path and no KV at all.
//
// OKTA_CLIENT_SECRET is still needed, by the INTERACTIVE path in src/auth.ts for
// the Okta code exchange. Only the M2M path stopped using it.

/** What an M2M verification concluded. src/jwt.ts produces these. */
export interface M2MVerdict {
  active: boolean;
  claims?: {
    scope: string;
    client_id: string;
    sub: string;
    exp?: number;
  };
}

/** The bearer token from an Authorization header, or null. */
export function extractBearer(req: Request): string | null {
  const raw = req.headers.get("authorization");
  if (!raw) return null;
  const m = raw.match(/^Bearer\s+(.+)$/i);
  return m ? m[1].trim() : null;
}

/**
 * True for a token this Worker's own OAuthProvider issued.
 *
 * Its format is `${userId}:${grantId}:${secret}` — three colon-separated parts,
 * which no Okta JWT can be. An interactive token sent to jwtVerify is a
 * guaranteed failure on EVERY authenticated interactive request, so this is the
 * difference between a cheap reject and a wasted verification, plus a JWKS fetch,
 * per call.
 *
 * COST, NOT CORRECTNESS. Removing this check breaks no test, and that is the
 * right outcome rather than a coverage gap: without it a Worker-issued token
 * fails verification and falls through to OAuthProvider — the same place it ends
 * up now, just more expensively. Do not add a test that implies otherwise.
 */
export function looksWorkerIssued(token: string): boolean {
  return token.split(":").length === 3;
}
