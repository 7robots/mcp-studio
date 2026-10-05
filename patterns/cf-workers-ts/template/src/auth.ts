import { Hono } from "hono";
import { AuthorizationError, CimdFetchError } from "@cloudflare/workers-oauth-provider";
import type { AuthRequest } from "@cloudflare/workers-oauth-provider";

import { READ_SCOPE, SUPPORTED_SCOPES } from "./scopes";
import { clientIdAsUrl, redirectDisplay, renderConsent, renderDenied } from "./consent";
import {
  CONSENT_COOKIE_PREFIX,
  PENDING_TTL_SECONDS,
  stateCookieName,
  STATE_TTL_SECONDS,
  clearCookie,
  consentCovers,
  forgetConsent,
  consentKey,
  deletePending,
  getPending,
  isNonce,
  putPending,
  putState,
  readCookie,
  rememberConsent,
  setCookie,
  takeState,
  timingSafeEqualStr,
  type PendingRecord,
} from "./oauth-state";

type Bindings = Env;
const app = new Hono<{ Bindings: Bindings }>();

// Least privilege: a client that names no scope gets the minimal one and must
// step up for anything more. Defaulting to every supported scope would mean
// "asked for nothing, received everything" the moment a server has two.
const DEFAULT_GRANT_SCOPES = [READ_SCOPE];

const RESOURCE_NAME = "REPLACE-WITH-RESOURCE-NAME";

function callbackUrl(req: Request): string {
  const url = new URL(req.url);
  return `${url.protocol}//${url.host}/callback`;
}

/** The resource a consent decision is scoped to — this server, not the fleet. */
function resourceKey(env: Env): string {
  return env.PUBLIC_MCP_URL ?? "REPLACE-WITH-SERVER-SLUG";
}

// Entry into the OAuth flow. OAuthProvider hands us the MCP client's
// authorization request; we stash it SERVER-SIDE and send only an opaque nonce
// to Okta as `state`, bound to a __Host- cookie. See src/oauth-state.ts for the
// attack this closes.
app.get("/authorize", async (c) => {
  // parseAuthRequest THROWS AuthorizationError for expected validation failures; the app
  // must turn it into an OAuth error redirect. Without this a bad `resource`
  // parameter — rejected now that resourceMetadata.resource is configured —
  // surfaces as a bare 500 instead of a redirect.
  let oauthReqInfo: AuthRequest;
  try {
    oauthReqInfo = await c.env.OAUTH_PROVIDER.parseAuthRequest(c.req.raw);
  } catch (e) {
    // A CIMD client whose metadata document cannot be fetched throws
    // CimdFetchError, NOT AuthorizationError. Rethrowing it gave a bare 500;
    // there is no validated redirect to send an OAuth error to, so render.
    if (e instanceof CimdFetchError) {
      return c.text(
        `Could not resolve the client ID metadata document for this client: ${e.message}`,
        400,
      );
    }
    if (!(e instanceof AuthorizationError)) throw e;
    // `redirectTo` (1.2) is the library's ready-made error redirect (error,
    // error_description, state, iss), set ONLY when the redirect URI was
    // validated. Absent means client/redirect validation did not pass, so we
    // MUST render locally rather than redirect anywhere the caller named.
    if (!e.redirectTo) {
      return c.text(`Invalid authorization request: ${e.code} — ${e.description}`, 400);
    }
    return Response.redirect(e.redirectTo, 302);
  }

  // parseAuthRequest will hand back an empty clientId rather than refusing, and
  // everything downstream — the consent key, the CIMD document lookup,
  // completeAuthorization — would then operate on "". Fail fast instead of
  // reasoning about what an empty client identity means at each of those points.
  if (!oauthReqInfo.clientId) {
    return c.text("Invalid request: missing client_id", 400);
  }

  // parseAuthRequest does NOT validate the scope grammar or check it against
  // scopesSupported — it just splits the query parameter. Unknown scopes would
  // otherwise be recorded on the grant, stored in the remembered-consent record,
  // and rendered on the consent page, which is now the control that actually
  // stops a hostile client. Reject them here, as invalid_scope, through the
  // error-redirect path above.
  const unknown = (oauthReqInfo.scope ?? []).filter((s) => !SUPPORTED_SCOPES.includes(s));
  if (unknown.length) {
    const back = new URL(oauthReqInfo.redirectUri);
    back.searchParams.set("error", "invalid_scope");
    back.searchParams.set(
      "error_description",
      `Unsupported scope(s): ${unknown.slice(0, 5).join(" ")}. This server supports: ${SUPPORTED_SCOPES.join(" ")}`,
    );
    if (oauthReqInfo.state) back.searchParams.set("state", oauthReqInfo.state);
    return Response.redirect(back.toString(), 302);
  }

  // OIDC `prompt=consent` forces the page even when a decision is remembered —
  // the only way to reach it before the 90-day TTL. Captured HERE because
  // /callback sees Okta's query string, not the client's.
  const forceConsent = (c.req.query("prompt") ?? "").split(/\s+/).includes("consent");

  // The EFFECTIVE scope set, resolved once so /callback cannot disagree. Resolving
  // it twice — forwarding the client's request here, applying the default there —
  // means a client that omits `scope` gets a grant Okta was never shown.
  const effectiveScopes = oauthReqInfo.scope?.length
    ? [...oauthReqInfo.scope]
    : [...DEFAULT_GRANT_SCOPES];

  const { state, csrf } = await putState(
    c.env.OAUTH_KV,
    { ...oauthReqInfo, scope: effectiveScopes },
    { forceConsent },
  );

  const authorizeUrl = new URL(`${c.env.OKTA_ISSUER}/v1/authorize`);
  authorizeUrl.searchParams.set("client_id", c.env.OKTA_CLIENT_ID);
  authorizeUrl.searchParams.set("response_type", "code");
  authorizeUrl.searchParams.set("redirect_uri", callbackUrl(c.req.raw));
  // The MCP scopes go UPSTREAM, not just onto the local grant. Sending only
  // OKTA_SCOPES leaves every MCP scope SELF-ASSERTED for a human: the client asks,
  // nothing consults the IdP, and this server records it. Forwarding them is what
  // makes an Okta access-policy rule a boundary for people rather than only for
  // workloads.
  //
  // Okta matches a policy rule only when EVERY requested scope is in that rule's
  // list, so any rule meant to grant a privileged scope must also carry the OIDC
  // scopes sent here. A user matching no rule gets `access_denied` rather than a
  // downgraded token — which is why ADVERTISED_SCOPES is narrower than
  // SUPPORTED_SCOPES (src/scopes.ts).
  authorizeUrl.searchParams.set(
    "scope",
    [...new Set([...c.env.OKTA_SCOPES.split(/\s+/).filter(Boolean), ...effectiveScopes])].join(" "),
  );
  authorizeUrl.searchParams.set("state", state);

  return new Response(null, {
    status: 302,
    headers: {
      location: authorizeUrl.toString(),
      "set-cookie": setCookie(stateCookieName(state), csrf, STATE_TTL_SECONDS),
      "cache-control": "no-store",
    },
  });
});

// Okta redirects back here with ?code=...&state=...
app.get("/callback", async (c) => {
  const code = c.req.query("code");
  const error = c.req.query("error");

  if (error) {
    return c.text(
      `Okta returned an error: ${error} — ${c.req.query("error_description") ?? ""}`,
      400,
    );
  }
  if (!code) return c.text("Missing code on callback", 400);

  // Single-use, cookie-bound, deleted only after the CSRF check passes.
  const stateNonce = c.req.query("state") ?? "";
  const taken = await takeState<AuthRequest>(
    c.env.OAUTH_KV,
    stateNonce,
    c.req.header("cookie") ?? null,
  );
  if (!taken.ok) return c.text(taken.reason, 400);
  const oauthReqInfo = taken.req;

  const tokenRes = await fetch(`${c.env.OKTA_ISSUER}/v1/token`, {
    method: "POST",
    headers: {
      "Content-Type": "application/x-www-form-urlencoded",
      Authorization: "Basic " + btoa(`${c.env.OKTA_CLIENT_ID}:${c.env.OKTA_CLIENT_SECRET}`),
    },
    body: new URLSearchParams({
      grant_type: "authorization_code",
      code,
      redirect_uri: callbackUrl(c.req.raw),
    }).toString(),
  });
  if (!tokenRes.ok) {
    const body = await tokenRes.text();
    return c.text(`Token exchange failed (${tokenRes.status}): ${body.slice(0, 500)}`, 502);
  }
  const tokens = (await tokenRes.json()) as { access_token: string; scope?: string };

  const userinfoRes = await fetch(`${c.env.OKTA_ISSUER}/v1/userinfo`, {
    headers: { Authorization: `Bearer ${tokens.access_token}` },
  });
  if (!userinfoRes.ok) {
    const body = await userinfoRes.text();
    return c.text(`Userinfo lookup failed (${userinfoRes.status}): ${body.slice(0, 500)}`, 502);
  }
  const userinfo = (await userinfoRes.json()) as {
    sub: string;
    email?: string;
    name?: string;
    preferred_username?: string;
  };
  // Fail closed: `sub` keys the remembered-consent record, so an empty one
  // would collapse every user onto the same key.
  if (!userinfo.sub) return c.text("Okta returned no subject", 502);
  const email = userinfo.email ?? userinfo.preferred_username ?? userinfo.sub;

  // ONE value for both the grant and the props, so the recorded scope, the
  // /token response and what the caller can actually do never disagree.
  const requestedScopes = oauthReqInfo.scope?.length
    ? [...oauthReqInfo.scope]
    : [...DEFAULT_GRANT_SCOPES];

  // What Okta ACTUALLY granted, when it says. RFC 6749 §3.3 lets an authorization
  // server issue a NARROWER scope than requested and report it here instead of
  // erroring. Okta's custom-AS rules currently deny outright, but relying on that
  // means one policy rule edited to "any scope" silently restores the
  // self-asserted behaviour this change removes, with nothing able to notice. So
  // intersect: the grant can never exceed what came back.
  const oktaGranted = (tokens.scope ?? "").split(/\s+/).filter(Boolean);
  const grantedScopes = oktaGranted.length
    ? requestedScopes.filter((s) => oktaGranted.includes(s))
    : requestedScopes;

  // Every MCP scope intersected away leaves a grant that can call nothing. Fail
  // closed and say so, rather than issuing a token whose every tool call refuses.
  if (grantedScopes.length === 0) {
    return c.text("Okta granted none of the requested scopes.", 403);
  }

  const key = consentKey({
    sub: userinfo.sub,
    clientId: oauthReqInfo.clientId,
    resource: resourceKey(c.env),
    // In the key so moving the destination re-prompts — see consentKey.
    redirectUri: oauthReqInfo.redirectUri,
  });

  // A remembered decision that already covers these scopes completes without
  // interrupting. Remembering is UX only — nothing about authorization is
  // skipped, and a request for MORE re-prompts.
  if (!taken.forceConsent && (await consentCovers(c.env.OAUTH_KV, key, grantedScopes))) {
    const done = await completeAndRedirect(c.env, oauthReqInfo, {
      userId: userinfo.sub,
      email,
      name: userinfo.name,
      oktaAccessToken: tokens.access_token,
      scopes: grantedScopes,
    });
    // The consent-page branch cleared this; the remembered branch did not, so
    // every silent login left an orphan __Host- cookie in the jar until its own
    // Max-Age expired. The KV record is already gone (takeState deleted it), so
    // this is hygiene rather than a replayable state — but a cookie that outlives
    // the thing it authenticates has no business still being there.
    done.headers.append("set-cookie", clearCookie(stateCookieName(stateNonce)));
    return done;
  }

  const { nonce, csrf } = await putPending<AuthRequest>(c.env.OAUTH_KV, {
    req: oauthReqInfo,
    userId: userinfo.sub,
    email,
    name: userinfo.name,
    oktaAccessToken: tokens.access_token,
    scopes: grantedScopes,
    consentKey: key,
    forceConsent: taken.forceConsent,
  });

  const client = await c.env.OAUTH_PROVIDER.lookupClient(oauthReqInfo.clientId).catch(() => null);
  const page = renderConsent({
    nonce,
    csrf,
    clientName: client?.clientName ?? null,
    redirectHost: redirectDisplay(oauthReqInfo.redirectUri),
    clientIdUrl: clientIdAsUrl(oauthReqInfo.clientId),
    email,
    scopes: grantedScopes,
    resourceName: RESOURCE_NAME,
  });
  // Per-flow cookie name, so two concurrent authorizations cannot clobber each
  // other's CSRF value.
  page.headers.append(
    "set-cookie",
    setCookie(CONSENT_COOKIE_PREFIX + nonce, csrf, PENDING_TTL_SECONDS),
  );
  page.headers.append("set-cookie", clearCookie(stateCookieName(stateNonce)));
  return page;
});

// Settles a pending decision. The form's csrf must match BOTH the per-flow
// cookie and the value stored server-side, so a leaked nonce cannot be settled
// from a different browser presenting its own self-consistent pair.
app.post("/consent", async (c) => {
  const form = await c.req.formData().catch(() => null);
  if (!form) return c.text("Malformed form submission", 400);

  const nonce = String(form.get("nonce") ?? "");
  const csrf = String(form.get("csrf") ?? "");
  const decision = String(form.get("decision") ?? "");
  // Shape-check before the value becomes a KV key or a cookie name.
  if (!isNonce(nonce)) return c.text("Malformed consent nonce", 400);

  const pending = await getPending<AuthRequest>(c.env.OAUTH_KV, nonce);
  const cookie = readCookie(c.req.header("cookie") ?? null, CONSENT_COOKIE_PREFIX + nonce);
  if (
    !pending ||
    !cookie ||
    !timingSafeEqualStr(pending.csrf, cookie) ||
    !timingSafeEqualStr(pending.csrf, csrf)
  ) {
    // Validation precedes consumption: a failed attempt leaves the record
    // intact so the legitimate browser can still finish.
    return c.text("Invalid or expired consent request", 400);
  }

  if (decision !== "approve") {
    await deletePending(c.env.OAUTH_KV, nonce);
    if (pending.forceConsent) {
      await forgetConsent(c.env.OAUTH_KV, pending.consentKey);
    }
    const denied = renderDenied();
    denied.headers.append("set-cookie", clearCookie(CONSENT_COOKIE_PREFIX + nonce));
    return denied;
  }

  // Delete before minting. NOTE this is not an absolute single-use guarantee:
  // KV get+delete is not atomic and reads are eventually consistent, so two
  // truly concurrent POSTs with the same nonce could both pass validation. The
  // blast radius is bounded — same user, same client, same scopes — and
  // completeAuthorization revokes prior grants for that client by default. A
  // Durable Object would be needed for a hard guarantee.
  await deletePending(c.env.OAUTH_KV, nonce);

  // Mint FIRST, remember second. completeAuthorization can still throw here —
  // an invalid redirect URI, or a CIMD document that has become unfetchable
  // since the page was rendered — and remembering before that point leaves a
  // 90-day approval on record for a grant that was never issued, which the user
  // would then never be asked about again.
  const res = await completeAndRedirect(c.env, pending.req, pending);
  await rememberConsent(c.env.OAUTH_KV, pending.consentKey, pending.scopes);
  res.headers.append("set-cookie", clearCookie(CONSENT_COOKIE_PREFIX + nonce));
  return res;
});

async function completeAndRedirect(
  env: Env,
  req: AuthRequest,
  grant: Pick<
    PendingRecord<AuthRequest>,
    "userId" | "email" | "name" | "oktaAccessToken" | "scopes"
  >,
): Promise<Response> {
  const { redirectTo } = await env.OAUTH_PROVIDER.completeAuthorization({
    request: req,
    userId: grant.userId,
    metadata: { label: grant.email },
    scope: grant.scopes,
    props: {
      okta_sub: grant.userId,
      email: grant.email,
      name: grant.name,
      okta_access_token: grant.oktaAccessToken,
      // OAuthProvider hands apiHandler only ctx.props — the grant's scopes are
      // not on the ExecutionContext — so they travel here to reach AuthInfo.
      scopes: grant.scopes,
      // The OAuth client holding this grant, so AuthInfo.clientId means
      // "client" on both auth paths rather than a user subject.
      client_id: req.clientId,
    },
  });
  return new Response(null, {
    status: 302,
    headers: { location: redirectTo, "cache-control": "no-store" },
  });
}

app.get("/", (c) =>
  c.text(
    "Cloudflare MCP server — MCP endpoint at /mcp (Streamable HTTP). " +
      "See /.well-known/oauth-protected-resource/mcp for resource metadata.",
  ),
);

export { app as authApp };
