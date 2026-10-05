import OAuthProvider from "@cloudflare/workers-oauth-provider";
import type { AuthInfo } from "@modelcontextprotocol/server";

import { authApp } from "./auth";
import { resourceUrls, type ResourceUrls } from "./resource";
import { createHandler, type ServerProps, type Handler } from "./mcp";
import { gateOnActor } from "./actor";
import { extractBearer, looksWorkerIssued } from "./m2m";
import { tryOktaJwt } from "./jwt";
import {
  SUPPORTED_SCOPES,
  hasScope,
  insufficientScopeResponse,
  mcpToolName,
  scopeForTool,
} from "./scopes";
import { mapEmaClaims, parseTrustedIssuers, trustedIssuerResolver } from "./ema";

// Access tokens are Worker-issued and the client refreshes them silently; every
// refresh writes token state to OAUTH_KV.
//
// This was 28800 (8h) in all seven repos, justified by Cloudflare's FREE-tier
// budget of 1,000 KV writes/day account-wide: at the library's 1h default an
// idle-but-connected client burns ~24 writes/day doing nothing, so 8h cut that
// ~8x. Both halves of that reasoning turned out not to apply here, measured
// 2026-08-23 rather than assumed:
//
//   * This account is on Workers PAID — Dynamic Workers are paid-plan only and
//     mcp-gateway runs a worker_loaders binding in production. The budget is
//     1 million writes/month, not 1,000/day.
//   * Actual account-wide KV writes over the busiest week were 20-150/day
//     (peak 150 on 2026-08-21, the heaviest development day of the project).
//     Even on the free tier that is 2-15% of the cap.
//
// So the trade was real in one direction only: tokens stayed valid eight times
// longer than necessary to protect headroom that was never approached. Back to
// the library default. A leaked or stolen token is now valid for an hour rather
// than a working day, and the 30-day refreshTokenTTL default is untouched.
const ACCESS_TOKEN_TTL_SECONDS = 3600; // 1 hour, the library default

// This server's canonical resource identifier (RFC 8707 / RFC 9728). Config,
// never derived from the Host header — a caller controls that.
//
// Derived from the PUBLIC_MCP_URL var, not duplicated as a literal: a second
// copy of this identifier silently desynchronises resource pinning from the
// consent keys, which src/auth.ts derives from the same var.
//
// The derivation itself lives in src/resource.ts so it can be unit-tested. This
// file transitively imports `cloudflare:` modules and will not load under the
// plain-Node pool, and the path arithmetic here is exactly where a malformed
// PUBLIC_MCP_URL turns into a corrupted metadata URL and a poisoned
// WWW-Authenticate auth-param.
let urls: ResourceUrls | undefined;
function canonical(env: Env): ResourceUrls {
  return (urls ??= resourceUrls(env.PUBLIC_MCP_URL));
}

// The MCP handler is stateless (2026-07-28 protocol): its factory builds a fresh
// McpServer for every request, so a single handler per isolate is safe. Memoized
// lazily because bindings only arrive with the first request's env.
//
// Nothing here uses the handler's notify/bus pair. If a cross-request stream is
// ever added (subscriptions/listen), stop memoizing and build per request —
// writing to it from another request's context hits Workers' cross-request I/O
// restriction.
let handler: Handler | undefined;
function getHandler(env: Env): Handler {
  return (handler ??= createHandler(env));
}

const resourceMetadataUrl = (env: Env) => canonical(env).resourceMetadataUrl;

function isMcpPath(req: Request): boolean {
  const p = new URL(req.url).pathname;
  return p === "/mcp" || p.startsWith("/mcp/");
}

// Pre-handler scope gate — layer 1 of two (see src/scopes.ts). Returns a
// 403 + WWW-Authenticate challenge when the header names a tool whose scope the
// caller lacks, so a conforming client can step up. Returns null when it cannot
// decide from headers alone; the in-handler guard is the real boundary.
function gateOnScope(request: Request, env: Env, granted: readonly string[]): Response | null {
  const tool = mcpToolName(request);
  if (!tool) return null;
  const needed = scopeForTool(tool);
  if (!needed) return null; // unknown tool: the handler refuses it
  if (hasScope(granted, needed)) return null;
  return insufficientScopeResponse(
    [needed],
    resourceMetadataUrl(env),
    `'${tool}' requires the ${needed} scope`,
  );
}

// Interactive path: OAuthProvider has already validated its Worker-issued token
// and attached the stored Okta identity to ctx.props. Map it onto the SDK's
// pass-through AuthInfo so tools can read the caller as ctx.http.authInfo.
//
// The upstream Okta access token deliberately stays in props and is not copied
// into AuthInfo — no tool here calls Okta-protected APIs on the user's behalf.
const apiHandler = {
  fetch(request: Request, env: Env, ctx: ExecutionContext & { props?: ServerProps }): Promise<Response> {
    // OAuthProvider matches apiRoute by prefix (so /mcpfoo would land here);
    // keep the served surface identical to the M2M path's isMcpPath().
    if (!isMcpPath(request)) return Promise.resolve(new Response("Not found", { status: 404 }));

    const props = ctx.props;
    // Fail closed. Previously a grant without props forwarded the request with
    // NO AuthInfo, so "authInfo is present on the interactive path" was not an
    // invariant and any tool reading it would throw. A grant that predates
    // props, or one whose props failed to decode, is now refused.
    if (!props?.okta_sub) {
      return Promise.resolve(
        new Response(
          JSON.stringify({
            error: "invalid_token",
            error_description:
              "This grant carries no stored identity. Re-authorize to obtain a current one.",
          }),
          {
            status: 401,
            headers: {
              "content-type": "application/json",
              "www-authenticate":
                `Bearer realm="OAuth", error="invalid_token", scope="${SUPPORTED_SCOPES.join(" ")}", resource_metadata="${resourceMetadataUrl(env)}"`,
            },
          },
        ),
      );
    }

    const scopes = props.scopes ?? [];
    const refusal = gateOnScope(request, env, scopes);
    if (refusal) return Promise.resolve(refusal);

    const authInfo: AuthInfo = {
      token: extractBearer(request) ?? "",
      // The OAuth client that holds this grant — NOT the user. The user is
      // extra.sub. These were previously the same value, which meant anything
      // keying rate limits or audit records off clientId attributed a human to
      // a client identity.
      clientId: props.client_id ?? "unknown-client",
      // Real granted scopes, carried through props by src/auth.ts. `[]` here
      // would deny every human caller once tools enforce scopes.
      scopes,
      extra: {
        // "enterprise" when this grant came from an ID-JAG assertion. Both land
        // here because both are props-based grants, but they are NOT the same
        // thing: an interactive grant means a human read the consent page and
        // approved; an enterprise grant means the IdP approved on their behalf
        // and no page was ever shown. Anything auditing or rate-limiting on
        // "did a human agree to this" needs the difference.
        auth_path: props.enterprise ? "enterprise" : "interactive",
        ...(props.enterprise_issuer ? { enterprise_issuer: props.enterprise_issuer } : {}),
        sub: props.okta_sub,
        email: props.email,
        name: props.name,
      },
    };
    return getHandler(env).fetch(request, { authInfo });
  },
};

// Interactive OAuth (Claude Desktop / Code / Web): OAuthProvider handles
// /authorize, /token, /register, /.well-known/oauth-authorization-server,
// /.well-known/oauth-protected-resource, and the Bearer-gated /mcp endpoint
// using its own KV-stored tokens.
function buildOauth(env: Env) {
  const emaIssuers = parseTrustedIssuers(env.EMA_TRUSTED_ISSUERS);
  // Named `res` rather than `urls`: `urls` is the module-level memo above, and
  // shadowing it here reads like an assignment to it.
  const res = canonical(env);
  return new OAuthProvider({
    apiRoute: "/mcp",
    apiHandler: apiHandler as never,
    defaultHandler: authApp as never,
    authorizeEndpoint: "/authorize",
    tokenEndpoint: "/token",
    clientRegistrationEndpoint: "/register",
    accessTokenTTL: ACCESS_TOKEN_TTL_SECONDS,
    // OAuth 2.1 hardening is the library default from 1.2: the implicit grant
    // and plain PKCE are gone (the old allowPlainPKCE / allowImplicitFlow options
    // now THROW at construction if set true), so code_challenge_method=plain is
    // refused with invalid_request. PKCE itself is REQUIRED for public clients —
    // DCR and CIMD clients are always public, so every MCP client that matters
    // here must send an S256 challenge. A confidential client (holding a secret)
    // may still omit it; there is no option covering them.
    // Advertise the scopes on the AUTHORIZATION SERVER metadata too, not just
    // the protected-resource metadata — a client that reads only the former
    // would otherwise see no scopes and request none.
    scopesSupported: SUPPORTED_SCOPES,
    // Client ID Metadata Documents (Phase 2). DCR is DEPRECATED as of MCP
    // 2026-07-28; CIMD replaces it. A CIMD client id is an HTTPS URL whose
    // document declares its own redirect_uris, which the AS fetches and
    // validates — so the client's identity is a domain someone demonstrably
    // controls rather than an arbitrary registered string. It does NOT remove the
    // need for the consent screen (a hostile domain can publish a document too),
    // but it makes that screen informative.
    //
    // Advertisement is gated on the `global_fetch_strictly_public` compatibility
    // flag; without it in wrangler.toml the library reports
    // client_id_metadata_document_supported: false and clients fall back to DCR.
    clientIdMetadataDocumentEnabled: true,
    // RFC 9728 protected-resource metadata. Since 1.0 `resource` is REQUIRED
    // and is the audience every Worker-issued token is bound to (RFC 8707):
    // an interactive token is accepted only for this exact URI. Without it 0.x
    // derived the resource from the request origin, which advertised
    // "https://replace.{{domain_suffix}}" rather than the canonical /mcp URI.
    resourceMetadata: {
      resource: res.resource,
      authorization_servers: [res.authorizationServer],
      bearer_methods_supported: ["header"],
      resource_name: "REPLACE-WITH-RESOURCE-NAME",
    },
    // Published as the protected-resource metadata's `scopes_supported` and
    // named in the 401 challenge — the MINIMUM for basic use per MCP
    // 2026-07-28; anything beyond it is requested via step-up. Replaced
    // resourceMetadata.scopes_supported in 1.2 (setting both throws).
    // Advertised, NOT enforced by the library: the pre-handler gate and the
    // in-handler requireScope guards are what enforce it.
    requiredScopes: SUPPORTED_SCOPES,
    // Phase 3 — Enterprise-Managed Authorization. Presence of this option
    // enables the ID-JAG grant; there is deliberately no `enabled` flag.
    // Configured only when EMA_TRUSTED_ISSUERS names at least one issuer, so an
    // unconfigured deploy does not advertise a grant it cannot honour.
    ...(emaIssuers.length
      ? {
          enterpriseManagedAuthorization: {
            trustedIssuers: trustedIssuerResolver(emaIssuers),
            mapClaims: mapEmaClaims,
            // CIMD clients are ALWAYS public (token_endpoint_auth_method:
            // "none") and cannot present a secret, while the EMA grant requires
            // client authentication by default. Doing CIMD and EMA together
            // therefore forces this on. The trade, per the library's own
            // documentation: trust rests on the IdP-issued, signature-verified,
            // short-lived, single-use, audience- and client-bound assertion plus
            // resource pinning, rather than on a separately presented secret.
            allowPublicClients: true,
          },
        }
      : {}),
  });
}

let oauthInstance: OAuthProvider | undefined;
function getOauth(env: Env): OAuthProvider {
  return (oauthInstance ??= buildOauth(env));
}

export default {
  async fetch(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
    // M2M bypass: an Okta-issued bearer carrying one of this server's scopes
    // skips OAuthProvider entirely and goes straight to the stateless handler,
    // with the verified claims passed through as AuthInfo.
    //
    // Verified LOCALLY against the issuer's JWKS — no introspection round trip,
    // no KV read, no KV write. Okta's default authorization server issues JWT
    // access tokens, so the signature tells us everything introspection did. A
    // Worker-issued interactive token is not a JWT, so it is short-circuited
    // before verification rather than failing it on every interactive request.
    if (isMcpPath(request) && request.headers.has("authorization")) {
      const bearer = extractBearer(request);
      const verdict =
        bearer && !looksWorkerIssued(bearer) ? await tryOktaJwt(bearer, env) : null;
      if (verdict?.active && verdict.claims) {
        // Gateway assertion FIRST: "did this come through the gateway" is a
        // precondition for being served at all, and reordering hands a scope
        // verdict to a caller you have already decided not to talk to.
        // Enforcement is opt-in (REQUIRE_GATEWAY_ACTOR); a PRESENT assertion is
        // always checked, and only a VERIFIED one becomes an actor.
        const actorGate = await gateOnActor(request, env, {
          clientId: verdict.claims.client_id,
        });
        if (actorGate.refusal) return actorGate.refusal;
        const actor = actorGate.actor;
        const scopes = verdict.claims.scope.split(/\s+/).filter(Boolean);
        const refusal = gateOnScope(request, env, scopes);
        if (refusal) return refusal;
        const authInfo: AuthInfo = {
          token: extractBearer(request) ?? "",
          clientId: verdict.claims.client_id,
          scopes,
          expiresAt: verdict.claims.exp,
          extra: {
            // `sub` stays the token's own subject — the platform workload. The
            // human the gateway is acting for is reported separately rather
            // than silently replacing it, so a tool can tell the difference
            // between "the gateway says this is {{operator_name}}" and "the IdP says
            // this is {{operator_name}}".
            auth_path: actor ? "m2m+actor" : "m2m",
            sub: verdict.claims.sub,
            ...(actor
              ? {
                  on_behalf_of: actor.sub,
                  gateway_purpose: actor.purpose,
                  gateway_run_id: actor.run_id,
                }
              : {}),
          },
        };
        return getHandler(env).fetch(request, { authInfo });
      }
      // Bearer didn't pass Okta M2M checks (wrong scope, wrong issuer or
      // audience, inactive, malformed): fall through to OAuthProvider, which
      // will accept it if it's a Worker-issued interactive token or reject
      // with 401.
    }
    return getOauth(env).fetch(request, env, ctx);
  },
} satisfies ExportedHandler<Env>;
