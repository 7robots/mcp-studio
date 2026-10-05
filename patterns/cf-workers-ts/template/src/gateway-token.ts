// OPT-IN — for a server that does no OAuth at all and trusts mcp-gateway to
// authenticate its callers (the gateway's `gateway_jwt` auth mode).
//
// The gateway signs a 5-minute RFC 9068 access token for this server alone and
// sends it as `Authorization: Bearer`. Verifying it is this server's whole
// authentication: no Okta app, no scopes on an authorization server, no login
// flow. Register the server with
//
//   register_server({ url, auth_mode: "gateway_jwt", scopes: ["<slug>:use"] })
//
// and call verifyGatewayToken() on every MCP request. Anything it refuses gets
// a 401. The server is then reachable ONLY through the gateway, which is the
// point: the gateway is its authority (mcp-gateway ruling 2026-09-26).
//
// What each check stops:
//   - `typ: at+jwt` — the gateway's X-MCP-Actor assertion is signed by a key in
//     the same JWKS but is `typ: JWT`; without this check it would pass as a bearer.
//   - `aud` = this server's own URL — a token minted for another gateway_jwt
//     server is refused here.
//   - `iss` = the gateway — a token from any other issuer, however well signed.
//   - `scope` — the scope this server was registered with.
//   - EdDSA only, 30s clock skew. jose refetches the JWKS on an unknown `kid`,
//     so a gateway key rotation needs no change here.
//
// The claims to use afterwards: `sub` (an Okta user id for a person, a client
// id for a service, `gateway:system` for the gateway's own catalogue refresh),
// `sub_type` ("user" | "service" | "system") and `client_id` (the MCP client
// that called the gateway).

import { createRemoteJWKSet, jwtVerify, type JWTPayload, type JWTVerifyGetKey } from "jose";

export interface GatewayTokenClaims {
  sub: string;
  subType: "user" | "service" | "system";
  clientId: string;
  scopes: string[];
  jti: string;
}

export type GatewayTokenVerdict = { ok: true; claims: GatewayTokenClaims } | { ok: false; reason: string };

export interface GatewayTokenOptions {
  /** The gateway, e.g. https://{{gateway_host}} */
  issuer: string;
  /** This server's MCP endpoint exactly as registered, e.g. https://x.{{domain_suffix}}/mcp */
  audience: string;
  /** The scope this server was registered with. */
  requiredScope: string;
  /** Defaults to `${issuer}/.well-known/jwks.json`. Injectable for tests. */
  jwks?: JWTVerifyGetKey;
}

const remote = new Map<string, JWTVerifyGetKey>();

function jwksFor(issuer: string): JWTVerifyGetKey {
  let set = remote.get(issuer);
  if (!set) {
    set = createRemoteJWKSet(new URL(`${issuer}/.well-known/jwks.json`));
    remote.set(issuer, set);
  }
  return set;
}

/** The bearer from an Authorization header, or null. */
export function bearerOf(request: Request): string | null {
  return request.headers.get("authorization")?.match(/^Bearer ([^\s]+)$/)?.[1] ?? null;
}

export async function verifyGatewayToken(
  token: string | null,
  opts: GatewayTokenOptions,
): Promise<GatewayTokenVerdict> {
  if (!token) return { ok: false, reason: "no bearer token" };
  let payload: JWTPayload;
  try {
    ({ payload } = await jwtVerify(token, opts.jwks ?? jwksFor(opts.issuer), {
      issuer: opts.issuer,
      audience: opts.audience,
      typ: "at+jwt",
      algorithms: ["EdDSA"],
      clockTolerance: 30,
      requiredClaims: ["sub", "exp", "iat", "jti", "client_id", "scope"],
    }));
  } catch (e) {
    return { ok: false, reason: `token did not verify: ${(e as Error).message}` };
  }
  const scopes = typeof payload.scope === "string" ? payload.scope.split(/\s+/).filter(Boolean) : [];
  if (!scopes.includes(opts.requiredScope)) return { ok: false, reason: `token lacks scope ${opts.requiredScope}` };
  const subType = payload.sub_type;
  if (subType !== "user" && subType !== "service" && subType !== "system") {
    return { ok: false, reason: "token has no recognised sub_type" };
  }
  return {
    ok: true,
    claims: {
      sub: payload.sub as string,
      subType,
      clientId: String(payload.client_id),
      scopes,
      jti: payload.jti as string,
    },
  };
}
