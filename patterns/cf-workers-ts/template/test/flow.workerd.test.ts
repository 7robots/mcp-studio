// Route-level tests for the interactive OAuth flow, inside workerd.
//
// WHY THESE EXIST. `src/auth.ts` and `src/index.ts` are importable in no Node
// test — Hono and @cloudflare/workers-oauth-provider pull in `cloudflare:`
// modules. So before this file the helpers in src/oauth-state.ts were well
// covered while the FLOW was not, and a regression that stopped calling
// `takeState` altogether would not have failed a single test. Reverting the
// source made those Node suites fail to RESOLVE, which is not coverage.
//
// Everything here drives `SELF.fetch()`, so it exercises the deployed
// wrangler.toml bindings, vars and compatibility flags. Okta is answered by the
// `outboundService` stub in vitest.config.ts.

import { SELF, env } from "cloudflare:test";
import { beforeEach, describe, expect, it } from "vitest";

// Imported, not restated. These were hard-coded at 12 sites, so retargeting
// the prefixes in src/oauth-state.ts and missing one produced a confusing
// route-test failure instead of a clear one.
import { CONSENT_COOKIE_PREFIX, STATE_COOKIE_PREFIX } from "../src/oauth-state";

const ORIGIN = "https://replace.{{domain_suffix}}";
const REDIRECT = "https://probe.invalid/cb";
// S256 of the verifier below, which is the one from RFC 7636's example.
const CHALLENGE = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

/** Register a public client the way an MCP client does, via DCR. */
async function registerClient(name = "Flow Test Client"): Promise<string> {
  const res = await SELF.fetch(`${ORIGIN}/register`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      client_name: name,
      redirect_uris: [REDIRECT],
      token_endpoint_auth_method: "none",
      grant_types: ["authorization_code"],
      response_types: ["code"],
    }),
  });
  expect(res.status).toBeLessThan(300);
  return ((await res.json()) as { client_id: string }).client_id;
}

function authorizeUrl(clientId: string, scope = "REPLACE:read"): string {
  const u = new URL(`${ORIGIN}/authorize`);
  u.searchParams.set("response_type", "code");
  u.searchParams.set("client_id", clientId);
  u.searchParams.set("redirect_uri", REDIRECT);
  u.searchParams.set("scope", scope);
  u.searchParams.set("code_challenge", CHALLENGE);
  u.searchParams.set("code_challenge_method", "S256");
  u.searchParams.set("state", "client-state-abc");
  return u.toString();
}

/** Start a flow and return the state nonce plus the cookie the browser holds. */
async function beginFlow(clientId: string, scope?: string) {
  const res = await SELF.fetch(authorizeUrl(clientId, scope), { redirect: "manual" });
  expect(res.status).toBe(302);
  const setCookie = res.headers.get("set-cookie") ?? "";
  const m = setCookie.match(new RegExp(`(${STATE_COOKIE_PREFIX}([0-9a-f-]+))=([^;]+)`));
  expect(m, `no state cookie in: ${setCookie}`).not.toBeNull();
  return {
    location: res.headers.get("location") ?? "",
    cookieName: m![1],
    state: m![2],
    cookieValue: m![3],
    cookieHeader: `${m![1]}=${m![3]}`,
  };
}

const callback = (state: string, cookie: string | null, code = "good-code") =>
  SELF.fetch(`${ORIGIN}/callback?code=${code}&state=${state}`, {
    redirect: "manual",
    headers: cookie ? { cookie } : {},
  });

describe("/authorize", () => {
  it("redirects to Okta and issues a per-flow __Host- state cookie", async () => {
    const flow = await beginFlow(await registerClient());
    expect(flow.location).toContain(`${env.OKTA_ISSUER}/v1/authorize`);
    // The nonce carries no information about the request.
    expect(flow.location).toContain(`state=${flow.state}`);
    expect(flow.location).not.toContain(REDIRECT);
    expect(flow.cookieName).toBe(`${STATE_COOKIE_PREFIX}${flow.state}`);
  });

  it("refuses an unsupported scope as invalid_scope without starting a flow", async () => {
    const clientId = await registerClient();
    const res = await SELF.fetch(authorizeUrl(clientId, "REPLACE:read admin:everything"), {
      redirect: "manual",
    });
    expect(res.status).toBe(302);
    const loc = new URL(res.headers.get("location")!);
    expect(loc.origin + loc.pathname).toBe(REDIRECT);
    expect(loc.searchParams.get("error")).toBe("invalid_scope");
    // The client's own state must come back so it can correlate the failure.
    expect(loc.searchParams.get("state")).toBe("client-state-abc");
    expect(res.headers.get("set-cookie")).toBeNull();
  });

  // The library's own AuthorizationError path (src/auth.ts, `redirectTo`), as
  // distinct from the app-level invalid_scope refusal above. 1.2 removed plain
  // PKCE, so a plain challenge is refused by parseAuthRequest itself.
  it("redirects a library-refused request (plain PKCE) back with error and state", async () => {
    const u = new URL(authorizeUrl(await registerClient()));
    u.searchParams.set("code_challenge_method", "plain");
    const res = await SELF.fetch(u.toString(), { redirect: "manual" });
    expect(res.status).toBe(302);
    const loc = new URL(res.headers.get("location")!);
    expect(loc.origin + loc.pathname).toBe(REDIRECT);
    expect(loc.searchParams.get("error")).toBe("invalid_request");
    expect(loc.searchParams.get("state")).toBe("client-state-abc");
    expect(res.headers.get("set-cookie")).toBeNull();
  });

  it("renders locally, never redirects, when the redirect URI is not registered", async () => {
    const u = new URL(authorizeUrl(await registerClient()));
    u.searchParams.set("redirect_uri", "https://attacker.invalid/cb");
    const res = await SELF.fetch(u.toString(), { redirect: "manual" });
    expect(res.status).toBe(400);
    expect(res.headers.get("location")).toBeNull();
  });

  it("gives two concurrent flows independent cookies", async () => {
    const clientId = await registerClient();
    const a = await beginFlow(clientId);
    const b = await beginFlow(clientId);
    expect(a.state).not.toBe(b.state);
    expect(a.cookieName).not.toBe(b.cookieName);
    // Both must still get PAST the state check — a single fixed cookie name
    // meant the second flow overwrote the first's cookie and the first then
    // failed. 200 here is the consent page (a first-time client), which is
    // exactly what "got past the state check" looks like.
    const both = `${a.cookieHeader}; ${b.cookieHeader}`;
    expect([200, 302]).toContain((await callback(a.state, both)).status);
    expect([200, 302]).toContain((await callback(b.state, both)).status);
  });
});

describe("/callback state binding", () => {
  let clientId: string;
  beforeEach(async () => {
    clientId = await registerClient();
  });

  it("REJECTS a state that was never issued", async () => {
    const res = await callback(crypto.randomUUID(), `${STATE_COOKIE_PREFIX}x=y`);
    expect(res.status).toBe(400);
    expect(await res.text()).toContain("invalid or expired state");
  });

  it("REJECTS a real state presented without the cookie", async () => {
    const flow = await beginFlow(clientId);
    const res = await callback(flow.state, null);
    expect(res.status).toBe(400);
  });

  it("REJECTS a real state presented with the wrong cookie value", async () => {
    const flow = await beginFlow(clientId);
    const res = await callback(flow.state, `${flow.cookieName}=${crypto.randomUUID()}`);
    expect(res.status).toBe(400);
  });

  it("REJECTS one flow's cookie value under another flow's name", async () => {
    const a = await beginFlow(clientId);
    const b = await beginFlow(clientId);
    const res = await callback(a.state, `${a.cookieName}=${b.cookieValue}`);
    expect(res.status).toBe(400);
  });

  it("ACCEPTS the matching pair — the positive control the rejections need", async () => {
    const flow = await beginFlow(clientId);
    const res = await callback(flow.state, flow.cookieHeader);
    // 302 to the client OR a consent page; either way it got past the state
    // check, which is what distinguishes this from the rejections above.
    expect([200, 302]).toContain(res.status);
  });

  it("leaves the record usable after a wrong-cookie attempt", async () => {
    const flow = await beginFlow(clientId);
    expect((await callback(flow.state, `${flow.cookieName}=nope`)).status).toBe(400);
    expect([200, 302]).toContain((await callback(flow.state, flow.cookieHeader)).status);
  });

  it("is single-use: the same state cannot be replayed", async () => {
    const flow = await beginFlow(clientId);
    expect([200, 302]).toContain((await callback(flow.state, flow.cookieHeader)).status);
    expect((await callback(flow.state, flow.cookieHeader)).status).toBe(400);
  });

  it("rejects a malformed state", async () => {
    const res = await callback("not-a-uuid", `${STATE_COOKIE_PREFIX}x=y`);
    expect(res.status).toBe(400);
    expect(await res.text()).toContain("malformed state");
  });

  it("fails closed when Okta returns no subject", async () => {
    const flow = await beginFlow(clientId);
    const res = await callback(flow.state, flow.cookieHeader, "no-sub");
    expect(res.status).toBe(502);
    expect(await res.text()).toContain("no subject");
  });

  it("surfaces an Okta token-exchange failure without minting anything", async () => {
    const flow = await beginFlow(clientId);
    const res = await callback(flow.state, flow.cookieHeader, "bad-code");
    expect(res.status).toBe(502);
    expect(await res.text()).toContain("Token exchange failed");
  });
});

describe("the consent gate", () => {
  it("shows consent to a first-time client, and does NOT issue a code", async () => {
    const clientId = await registerClient("Totally Legit App");
    const flow = await beginFlow(clientId);
    const res = await callback(flow.state, flow.cookieHeader);
    expect(res.status).toBe(200);
    const html = await res.text();
    // The redirect host is the anti-phishing signal and must always appear.
    expect(html).toContain("probe.invalid");
    expect(html).toContain("Totally Legit App");
    expect(html).toContain('action="/consent"');
    expect(res.headers.get("content-security-policy")).toContain("frame-ancestors 'none'");
    // Nothing was issued: no redirect to the client.
    expect(res.headers.get("location")).toBeNull();
  });

  it("requires the per-flow consent cookie — a leaked nonce alone is not enough", async () => {
    const clientId = await registerClient();
    const flow = await beginFlow(clientId);
    const page = await callback(flow.state, flow.cookieHeader);
    const html = await page.text();
    const nonce = html.match(/name="nonce" value="([^"]+)"/)![1];
    const csrf = html.match(/name="csrf" value="([^"]+)"/)![1];

    // Same nonce and csrf, but from a browser holding no consent cookie.
    const res = await SELF.fetch(`${ORIGIN}/consent`, {
      method: "POST",
      redirect: "manual",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({ nonce, csrf, decision: "approve" }).toString(),
    });
    expect(res.status).toBe(400);
    expect(res.headers.get("location")).toBeNull();
  });

  it("approve issues exactly one code, and the record cannot be reused", async () => {
    const clientId = await registerClient();
    const flow = await beginFlow(clientId);
    const page = await callback(flow.state, flow.cookieHeader);
    const html = await page.text();
    const nonce = html.match(/name="nonce" value="([^"]+)"/)![1];
    const csrf = html.match(/name="csrf" value="([^"]+)"/)![1];
    const consentCookie = (page.headers.get("set-cookie") ?? "").match(
      new RegExp(`(${CONSENT_COOKIE_PREFIX}[0-9a-f-]+=[^;]+)`),
    )![1];

    const approve = () =>
      SELF.fetch(`${ORIGIN}/consent`, {
        method: "POST",
        redirect: "manual",
        headers: {
          "content-type": "application/x-www-form-urlencoded",
          cookie: consentCookie,
        },
        body: new URLSearchParams({ nonce, csrf, decision: "approve" }).toString(),
      });

    const first = await approve();
    expect(first.status).toBe(302);
    const loc = new URL(first.headers.get("location")!);
    expect(loc.origin + loc.pathname).toBe(REDIRECT);
    expect(loc.searchParams.get("code")).toBeTruthy();
    expect(loc.searchParams.get("state")).toBe("client-state-abc");

    // Sequential replay must not mint a second code.
    const second = await approve();
    expect(second.status).toBe(400);
  });

  it("deny issues nothing", async () => {
    const clientId = await registerClient();
    const flow = await beginFlow(clientId);
    const page = await callback(flow.state, flow.cookieHeader);
    const html = await page.text();
    const nonce = html.match(/name="nonce" value="([^"]+)"/)![1];
    const csrf = html.match(/name="csrf" value="([^"]+)"/)![1];
    const consentCookie = (page.headers.get("set-cookie") ?? "").match(
      new RegExp(`(${CONSENT_COOKIE_PREFIX}[0-9a-f-]+=[^;]+)`),
    )![1];

    const res = await SELF.fetch(`${ORIGIN}/consent`, {
      method: "POST",
      redirect: "manual",
      headers: { "content-type": "application/x-www-form-urlencoded", cookie: consentCookie },
      body: new URLSearchParams({ nonce, csrf, decision: "deny" }).toString(),
    });
    expect(res.status).toBe(200);
    expect(await res.text()).toContain("Access denied");
    expect(res.headers.get("location")).toBeNull();
  });

  it("remembers the decision, so the same client and scope is not re-prompted", async () => {
    const clientId = await registerClient();

    // First pass: consent, approve.
    const f1 = await beginFlow(clientId);
    const page = await callback(f1.state, f1.cookieHeader);
    const html = await page.text();
    const nonce = html.match(/name="nonce" value="([^"]+)"/)![1];
    const csrf = html.match(/name="csrf" value="([^"]+)"/)![1];
    const consentCookie = (page.headers.get("set-cookie") ?? "").match(
      new RegExp(`(${CONSENT_COOKIE_PREFIX}[0-9a-f-]+=[^;]+)`),
    )![1];
    await SELF.fetch(`${ORIGIN}/consent`, {
      method: "POST",
      redirect: "manual",
      headers: { "content-type": "application/x-www-form-urlencoded", cookie: consentCookie },
      body: new URLSearchParams({ nonce, csrf, decision: "approve" }).toString(),
    });

    // Second pass, same client and scope: straight through.
    const f2 = await beginFlow(clientId);
    const res = await callback(f2.state, f2.cookieHeader);
    expect(res.status).toBe(302);
    expect(new URL(res.headers.get("location")!).searchParams.get("code")).toBeTruthy();
  });

  it("RE-PROMPTS when the same client asks for a broader scope", async () => {
    const clientId = await registerClient();

    // Approve REPLACE:read only.
    const f1 = await beginFlow(clientId, "REPLACE:read");
    const page = await callback(f1.state, f1.cookieHeader);
    const html = await page.text();
    const nonce = html.match(/name="nonce" value="([^"]+)"/)![1];
    const csrf = html.match(/name="csrf" value="([^"]+)"/)![1];
    const consentCookie = (page.headers.get("set-cookie") ?? "").match(
      new RegExp(`(${CONSENT_COOKIE_PREFIX}[0-9a-f-]+=[^;]+)`),
    )![1];
    await SELF.fetch(`${ORIGIN}/consent`, {
      method: "POST",
      redirect: "manual",
      headers: { "content-type": "application/x-www-form-urlencoded", cookie: consentCookie },
      body: new URLSearchParams({ nonce, csrf, decision: "approve" }).toString(),
    });

    // Now ask for write as well — this must NOT be silent.
    const f2 = await beginFlow(clientId, "REPLACE:read REPLACE:write");
    const res = await callback(f2.state, f2.cookieHeader);
    expect(res.status).toBe(200);
    expect(await res.text()).toContain("REPLACE:write");
  });

  it("does NOT re-prompt a loopback client that reconnects on a different port", async () => {
    // RFC 8252: a native app cannot reserve a port, so the provider's
    // isValidRedirectUri compares only protocol, hostname, pathname and search
    // for loopback URIs — any port is valid. The remembered decision has to be
    // canonicalized the same way, or every launch shows a consent page whose
    // destination reads "127.0.0.1:52222", a different unjudgeable number each
    // time. Consent fatigue is the failure mode consent actually has.
    const LOOPBACK = "http://127.0.0.1:5000/cb";
    const reg = await SELF.fetch(`${ORIGIN}/register`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        client_name: "Native Client",
        redirect_uris: [LOOPBACK],
        token_endpoint_auth_method: "none",
        grant_types: ["authorization_code"],
        response_types: ["code"],
      }),
    });
    expect(reg.status).toBeLessThan(300);
    const clientId = ((await reg.json()) as { client_id: string }).client_id;

    const begin = async (port: number) => {
      const u = new URL(`${ORIGIN}/authorize`);
      u.searchParams.set("response_type", "code");
      u.searchParams.set("client_id", clientId);
      u.searchParams.set("redirect_uri", `http://127.0.0.1:${port}/cb`);
      u.searchParams.set("scope", "REPLACE:read");
      u.searchParams.set("code_challenge", CHALLENGE);
      u.searchParams.set("code_challenge_method", "S256");
      u.searchParams.set("state", "client-state-abc");
      const res = await SELF.fetch(u.toString(), { redirect: "manual" });
      // The positive control for the whole test: if the provider ever stopped
      // accepting an unregistered port, this test would be meaningless.
      expect(res.status, `port ${port} was not accepted`).toBe(302);
      const m = (res.headers.get("set-cookie") ?? "").match(
        new RegExp(`(${STATE_COOKIE_PREFIX}([0-9a-f-]+))=([^;]+)`),
      );
      expect(m, `no state cookie for port ${port}`).not.toBeNull();
      return { state: m![2], cookieHeader: `${m![1]}=${m![3]}` };
    };

    // Approve once, on the port the client happened to get this launch.
    const f1 = await begin(51111);
    const page = await callback(f1.state, f1.cookieHeader);
    expect(page.status).toBe(200);
    const html = await page.text();
    const nonce = html.match(/name="nonce" value="([^"]+)"/)![1];
    const csrf = html.match(/name="csrf" value="([^"]+)"/)![1];
    const consentCookie = (page.headers.get("set-cookie") ?? "").match(
      new RegExp(`(${CONSENT_COOKIE_PREFIX}[0-9a-f-]+=[^;]+)`),
    )![1];
    await SELF.fetch(`${ORIGIN}/consent`, {
      method: "POST",
      redirect: "manual",
      headers: { "content-type": "application/x-www-form-urlencoded", cookie: consentCookie },
      body: new URLSearchParams({ nonce, csrf, decision: "approve" }).toString(),
    });

    // Next launch, new ephemeral port. Straight through.
    const f2 = await begin(52222);
    const res = await callback(f2.state, f2.cookieHeader);
    expect(res.status, "re-prompted on a new ephemeral port").toBe(302);
    const loc = new URL(res.headers.get("location")!);
    expect(loc.port).toBe("52222");
    expect(loc.searchParams.get("code")).toBeTruthy();

    // ...but the canonicalization is loopback-only and drops nothing else: a
    // different PATH on the same loopback host still re-prompts.
    const u = new URL(`${ORIGIN}/authorize`);
    u.searchParams.set("response_type", "code");
    u.searchParams.set("client_id", clientId);
    u.searchParams.set("redirect_uri", "http://127.0.0.1:52222/other");
    u.searchParams.set("scope", "REPLACE:read");
    u.searchParams.set("code_challenge", CHALLENGE);
    u.searchParams.set("code_challenge_method", "S256");
    u.searchParams.set("state", "client-state-abc");
    const other = await SELF.fetch(u.toString(), { redirect: "manual" });
    // 400, not a redirect: an unregistered redirect_uri cannot be redirected TO
    // without making this an open redirector for the very value it just refused
    // to trust. So the loopback exemption really is port-only — a different path
    // does not get in at all, which is stricter than a re-prompt.
    expect(other.status).toBe(400);
    expect(other.headers.get("location")).toBeNull();
  });

  it("RE-PROMPTS when an approved client moves the redirect destination", async () => {
    // Route-level proof that the redirect URI in the consent key is actually
    // consulted by /callback. Reverting the consentKey() call in src/auth.ts
    // leaves every unit test green — this is the one that fails.
    //
    // Modelled with a DCR client holding two registered URIs, because that is
    // what a route test can do. The real exposure is CIMD: those redirect_uris
    // come from a document re-fetched on every authorization, so whoever
    // controls it can add a destination after the approval was given.
    const MOVED = "https://moved.invalid/cb";
    const reg = await SELF.fetch(`${ORIGIN}/register`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        client_name: "Two Destinations",
        redirect_uris: [REDIRECT, MOVED],
        token_endpoint_auth_method: "none",
        grant_types: ["authorization_code"],
        response_types: ["code"],
      }),
    });
    expect(reg.status).toBeLessThan(300);
    const clientId = ((await reg.json()) as { client_id: string }).client_id;

    const begin = async (redirectUri: string) => {
      const u = new URL(`${ORIGIN}/authorize`);
      u.searchParams.set("response_type", "code");
      u.searchParams.set("client_id", clientId);
      u.searchParams.set("redirect_uri", redirectUri);
      u.searchParams.set("scope", "REPLACE:read");
      u.searchParams.set("code_challenge", CHALLENGE);
      u.searchParams.set("code_challenge_method", "S256");
      u.searchParams.set("state", "client-state-abc");
      const res = await SELF.fetch(u.toString(), { redirect: "manual" });
      expect(res.status).toBe(302);
      const m = (res.headers.get("set-cookie") ?? "").match(
        new RegExp(`(${STATE_COOKIE_PREFIX}([0-9a-f-]+))=([^;]+)`),
      );
      expect(m, `no state cookie for ${redirectUri}`).not.toBeNull();
      return { state: m![2], cookieHeader: `${m![1]}=${m![3]}` };
    };

    // Approve at the original destination.
    const f1 = await begin(REDIRECT);
    const page = await callback(f1.state, f1.cookieHeader);
    expect(page.status).toBe(200);
    const html = await page.text();
    expect(html).toContain("probe.invalid");
    const nonce = html.match(/name="nonce" value="([^"]+)"/)![1];
    const csrf = html.match(/name="csrf" value="([^"]+)"/)![1];
    const consentCookie = (page.headers.get("set-cookie") ?? "").match(
      new RegExp(`(${CONSENT_COOKIE_PREFIX}[0-9a-f-]+=[^;]+)`),
    )![1];
    const approved = await SELF.fetch(`${ORIGIN}/consent`, {
      method: "POST",
      redirect: "manual",
      headers: { "content-type": "application/x-www-form-urlencoded", cookie: consentCookie },
      body: new URLSearchParams({ nonce, csrf, decision: "approve" }).toString(),
    });
    expect(new URL(approved.headers.get("location")!).origin).toBe(new URL(REDIRECT).origin);

    // Same user, same client, same scope — but delivered somewhere else. The
    // remembered decision must NOT cover it.
    const f2 = await begin(MOVED);
    const res = await callback(f2.state, f2.cookieHeader);
    expect(res.status).toBe(200);
    const moved = await res.text();
    expect(moved).toContain("moved.invalid");
    expect(moved).toContain('action="/consent"');
    expect(res.headers.get("location")).toBeNull();

    // ...while the original destination is still remembered, so the binding
    // narrows the decision rather than discarding it.
    const f3 = await begin(REDIRECT);
    const again = await callback(f3.state, f3.cookieHeader);
    expect(again.status).toBe(302);
    expect(new URL(again.headers.get("location")!).searchParams.get("code")).toBeTruthy();
  });
});

describe("resource metadata and the MCP endpoint", () => {
  it("serves RFC 9728 metadata derived from PUBLIC_MCP_URL", async () => {
    const res = await SELF.fetch(`${ORIGIN}/.well-known/oauth-protected-resource/mcp`);
    expect(res.status).toBe(200);
    const body = (await res.json()) as Record<string, unknown>;
    expect(body.resource).toBe(env.PUBLIC_MCP_URL);
    expect(body.scopes_supported).toEqual(["REPLACE:read", "REPLACE:write"]);
  });

  it("advertises CIMD, which requires the global_fetch_strictly_public flag", async () => {
    const res = await SELF.fetch(`${ORIGIN}/.well-known/oauth-authorization-server`);
    const body = (await res.json()) as Record<string, unknown>;
    // This is the only place the compatibility flag's effect is observable.
    expect(body.client_id_metadata_document_supported).toBe(true);
    expect(body.code_challenge_methods_supported).toEqual(["S256"]);
  });

  it("does not advertise the ID-JAG grant while EMA_TRUSTED_ISSUERS is unset", async () => {
    const res = await SELF.fetch(`${ORIGIN}/.well-known/oauth-authorization-server`);
    const body = (await res.json()) as { grant_types_supported: string[] };
    expect(body.grant_types_supported).not.toContain(
      "urn:ietf:params:oauth:grant-type:jwt-bearer",
    );
  });

  it("401s an unauthenticated /mcp with a challenge naming the scopes", async () => {
    const res = await SELF.fetch(`${ORIGIN}/mcp`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }),
    });
    expect(res.status).toBe(401);
    const challenge = res.headers.get("www-authenticate") ?? "";
    expect(challenge).toContain('scope="REPLACE:read REPLACE:write"');
    expect(challenge).toContain("resource_metadata=");
  });
});

describe("consent hygiene and re-consent", () => {
  /** Approve once, returning the consent cookie used, so a second pass can be compared. */
  async function approveOnce(clientId: string, query = "") {
    const res = await SELF.fetch(`${ORIGIN}/authorize?${new URLSearchParams({
      response_type: "code",
      client_id: clientId,
      redirect_uri: REDIRECT,
      scope: "REPLACE:read",
      code_challenge: CHALLENGE,
      code_challenge_method: "S256",
      state: "client-state-abc",
    })}${query}`, { redirect: "manual" });
    const m = (res.headers.get("set-cookie") ?? "").match(
      /(__Host-REPLACE_oauth_csrf-([0-9a-f-]+))=([^;]+)/,
    )!;
    const page = await callback(m[2], `${m[1]}=${m[3]}`);
    expect(page.status).toBe(200);
    const html = await page.text();
    const nonce = html.match(/name="nonce" value="([^"]+)"/)![1];
    const csrf = html.match(/name="csrf" value="([^"]+)"/)![1];
    const cookie = (page.headers.get("set-cookie") ?? "").match(
      new RegExp(`(${CONSENT_COOKIE_PREFIX}[0-9a-f-]+=[^;]+)`),
    )![1];
    return { nonce, csrf, cookie, html };
  }

  const settle = (nonce: string, csrf: string, cookie: string, decision: string) =>
    SELF.fetch(`${ORIGIN}/consent`, {
      method: "POST",
      redirect: "manual",
      headers: { "content-type": "application/x-www-form-urlencoded", cookie },
      body: new URLSearchParams({ nonce, csrf, decision }).toString(),
    });

  it("CLEARS the per-flow state cookie on the remembered path too", async () => {
    // The consent-page branch always cleared it; the remembered branch did not,
    // so every silent login left an orphan __Host- cookie in the jar until its
    // own Max-Age ran out.
    const clientId = await registerClient();
    const first = await approveOnce(clientId);
    await settle(first.nonce, first.csrf, first.cookie, "approve");

    const f2 = await beginFlow(clientId);
    const res = await callback(f2.state, f2.cookieHeader);
    expect(res.status).toBe(302);
    const cleared = res.headers.get("set-cookie") ?? "";
    expect(cleared).toContain(f2.cookieName);
    expect(cleared).toMatch(/Max-Age=0|Expires=Thu, 01 Jan 1970/i);
  });

  it("shows the FULL destination, path included, not just the host", async () => {
    // The key is computed from the full URI, so two paths on one host are two
    // records — and both used to render as the same string, making the re-prompt
    // for a moved destination indistinguishable from a first prompt.
    const clientId = await registerClient();
    const { html } = await approveOnce(clientId);
    expect(html).toContain(REDIRECT);
  });

  it("re-prompts on prompt=consent even though the decision is remembered", async () => {
    const clientId = await registerClient();
    const first = await approveOnce(clientId);
    await settle(first.nonce, first.csrf, first.cookie, "approve");

    // Without the parameter this completes silently — asserted above.
    const forced = await approveOnce(clientId, "&prompt=consent");
    expect(forced.html).toContain("Authorize access");
  });

  it("DENY forgets the remembered decision, so the next login asks again", async () => {
    // Approving was durable and denying was momentary: a user who changed their
    // mind had no move short of the 90-day TTL.
    const clientId = await registerClient();
    const first = await approveOnce(clientId);
    await settle(first.nonce, first.csrf, first.cookie, "approve");

    const reconsider = await approveOnce(clientId, "&prompt=consent");
    const denied = await settle(reconsider.nonce, reconsider.csrf, reconsider.cookie, "deny");
    expect(denied.status).toBe(200);

    // No prompt=consent this time: it must stop at the page on its own.
    const after = await approveOnce(clientId);
    expect(after.html).toContain("Authorize access");
  });
});


describe("consent second pass — what a denial actually revokes", () => {
  const approveVia = async (clientId: string, scope: string, query = "") => {
    const res = await SELF.fetch(
      `${ORIGIN}/authorize?${new URLSearchParams({
        response_type: "code",
        client_id: clientId,
        redirect_uri: REDIRECT,
        scope,
        code_challenge: CHALLENGE,
        code_challenge_method: "S256",
        state: "client-state-abc",
      })}${query}`,
      { redirect: "manual" },
    );
    const m = (res.headers.get("set-cookie") ?? "").match(
      /(__Host-REPLACE_oauth_csrf-([0-9a-f-]+))=([^;]+)/,
    )!;
    const page = await callback(m[2], `${m[1]}=${m[3]}`);
    return { page, status: page.status };
  };

  const settleFrom = async (page: Response, decision: string) => {
    const html = await page.text();
    const nonce = html.match(/name="nonce" value="([^"]+)"/)![1];
    const csrf = html.match(/name="csrf" value="([^"]+)"/)![1];
    const cookie = (page.headers.get("set-cookie") ?? "").match(
      new RegExp(`(${CONSENT_COOKIE_PREFIX}[0-9a-f-]+=[^;]+)`),
    )![1];
    return SELF.fetch(`${ORIGIN}/consent`, {
      method: "POST",
      redirect: "manual",
      headers: { "content-type": "application/x-www-form-urlencoded", cookie },
      body: new URLSearchParams({ nonce, csrf, decision }).toString(),
    });
  };

  it("denying a scope ESCALATION leaves the narrower approval intact", async () => {
    // The bug this closes: the deny branch assumed reaching this page with a
    // decision already remembered meant `prompt=consent`. It also appears on an
    // escalation — so denying a request for MORE erased the approval the user had
    // already given, which they never asked to withdraw.
    const clientId = await registerClient();
    const first = await approveVia(clientId, "REPLACE:read");
    await settleFrom(first.page, "approve");

    // Ask for more, and refuse the increase.
    const escalated = await approveVia(clientId, "REPLACE:read REPLACE:write");
    expect(escalated.status).toBe(200);
    await settleFrom(escalated.page, "deny");

    // The original narrower approval must still stand: straight through, no page.
    const again = await approveVia(clientId, "REPLACE:read");
    expect(again.status).toBe(302);
    expect(new URL(again.page.headers.get("location")!).searchParams.get("code")).toBeTruthy();
  });

  it("denying after prompt=consent DOES erase the decision", async () => {
    // The other half: when the user deliberately came back, deny means revoke.
    const clientId = await registerClient();
    const first = await approveVia(clientId, "REPLACE:read");
    await settleFrom(first.page, "approve");

    const reconsider = await approveVia(clientId, "REPLACE:read", "&prompt=consent");
    await settleFrom(reconsider.page, "deny");

    const after = await approveVia(clientId, "REPLACE:read");
    expect(after.status).toBe(200);
  });
});


describe("MCP scopes reach Okta, and Okta's answer is honoured", () => {
  it("FORWARDS the requested MCP scopes to Okta's /authorize", async () => {
    // Until 2026-08-23 this server sent only OKTA_SCOPES, so every MCP scope was
    // self-asserted for a human: the client asked, nothing consulted Okta, and
    // this server recorded the grant. The access-policy rules governed workload
    // callers only.
    const clientId = await registerClient();
    const res = await SELF.fetch(authorizeUrl(clientId, "REPLACE:read REPLACE:write"), {
      redirect: "manual",
    });
    expect(res.status).toBe(302);
    const upstream = new URL(res.headers.get("location")!);
    expect(upstream.pathname).toContain("/v1/authorize");
    const sent = (upstream.searchParams.get("scope") ?? "").split(" ");
    expect(sent).toContain("REPLACE:read");
    expect(sent).toContain("REPLACE:write");
    // The OIDC scopes still go too — Okta matches a policy rule only when EVERY
    // requested scope is in that rule's list, so dropping these would silently
    // stop matching.
    expect(sent).toContain("openid");
    expect(sent).toContain("offline_access");
    expect(new Set(sent).size).toBe(sent.length);
  });

  it("sends the DEFAULT scope upstream when the client requests none", async () => {
    // The gap this closes: /authorize forwarded the client's request verbatim
    // while /callback substituted DEFAULT_GRANT_SCOPES, so omitting `scope`
    // produced a grant carrying a scope Okta was never shown.
    const clientId = await registerClient();
    const u = new URL(`${ORIGIN}/authorize`);
    u.searchParams.set("response_type", "code");
    u.searchParams.set("client_id", clientId);
    u.searchParams.set("redirect_uri", REDIRECT);
    u.searchParams.set("code_challenge", CHALLENGE);
    u.searchParams.set("code_challenge_method", "S256");
    u.searchParams.set("state", "client-state-abc");
    // No `scope` parameter at all.
    const res = await SELF.fetch(u.toString(), { redirect: "manual" });
    expect(res.status).toBe(302);
    const sent = (new URL(res.headers.get("location")!).searchParams.get("scope") ?? "").split(" ");
    expect(sent).toContain("REPLACE:read");
  });

  it("REFUSES the flow when Okta grants no MCP scope at all", async () => {
    // Okta's rules currently deny outright rather than downgrading, so this
    // guards against a policy rule drifting back to "any scope" with nothing in
    // the code able to notice.
    const clientId = await registerClient();
    const flow = await beginFlow(clientId);
    const res = await callback(flow.state, flow.cookieHeader, "downgrade-all");
    expect(res.status).toBe(403);
    expect(await res.text()).toContain("granted none");
  });
});

describe("the access token's lifetime", () => {
  it("is one hour — the library default, not the old 8-hour override", async () => {
    // Nothing tested this before, so the value could be raised back to 28800 in
    // any of seven repos with every suite still green. It was 8h fleet-wide to
    // conserve a FREE-tier budget of 1,000 KV writes/day; measured 2026-08-23,
    // this account is on Workers Paid and account-wide KV writes peaked at
    // 150/day. The trade only ever went one way — tokens valid for a working day
    // to protect headroom that was never approached.
    //
    // This is also the only test in this repo that drives /token at all.
    const clientId = await registerClient();
    const flow = await beginFlow(clientId);
    const page = await callback(flow.state, flow.cookieHeader);
    const html = await page.text();
    const nonce = html.match(/name="nonce" value="([^"]+)"/)![1];
    const csrf = html.match(/name="csrf" value="([^"]+)"/)![1];
    const cookie = (page.headers.get("set-cookie") ?? "").match(
      new RegExp(`(${CONSENT_COOKIE_PREFIX}[0-9a-f-]+=[^;]+)`),
    )![1];
    const approved = await SELF.fetch(`${ORIGIN}/consent`, {
      method: "POST",
      redirect: "manual",
      headers: { "content-type": "application/x-www-form-urlencoded", cookie },
      body: new URLSearchParams({ nonce, csrf, decision: "approve" }).toString(),
    });
    const code = new URL(approved.headers.get("location")!).searchParams.get("code")!;
    expect(code).toBeTruthy();

    const res = await SELF.fetch(`${ORIGIN}/token`, {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "authorization_code",
        code,
        client_id: clientId,
        redirect_uri: REDIRECT,
        // The verifier for CHALLENGE, from RFC 7636's own example.
        code_verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
      }).toString(),
    });
    expect(res.status, await res.clone().text()).toBe(200);
    const body = (await res.json()) as { expires_in: number; token_type: string };
    expect(body.expires_in).toBe(3600);
    expect(body.token_type).toBe("bearer");
  });
});

