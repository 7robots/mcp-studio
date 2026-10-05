// The fleet behavioral matrix — ONE spec, byte-identical in every repo.
//
// The conformance mechanism (my-skills/mcp-server-dev/scripts/
// fleet-conformance.py) proves the security FILES are identical or
// approved-different; it says nothing about what a repo actually DOES. This
// suite is the other half: the same scenarios driven through every server's
// real front door — SELF.fetch against src/index.ts under this repo's own
// wrangler.toml and outbound stubs — so "the control exists" and "the control
// is wired" can never diverge again the way F-1 did (a verifier exported and
// never imported), or F-3 (provenance set and then overwritten).
//
// This FILE is conformance-tracked, so it cannot drift per repo. What may
// differ per repo lives in ./matrix.params: the scope strings, what the
// metadata advertises, and which tool (if any) needs a second scope. Scenarios
// whose configuration a repo does not carry — EMA without EMA_TRUSTED_ISSUERS,
// actor without GATEWAY_ISSUER — skip LOUDLY, naming the reason, rather than
// passing vacuously.
//
// Consent-flow behavior is deliberately absent: test/flow.workerd.test.ts
// already drives register→authorize→consent→token per repo, and duplicating a
// four-step browser flow here would double the maintenance for no new proof.

import { SELF, env } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";

import { MATRIX } from "./matrix.params";

// ---------------------------------------------------------------------------
// Fixtures. Fixed keys, not generated: vitest.config.ts's outboundService runs
// in the config's realm and this file in its own isolate, so a generated key
// cannot be shared between them.
// ---------------------------------------------------------------------------

/** RS256 — the fake Okta/IdP key, public half served by the stub at /v1/keys
 *  (and as the fake enterprise IdP's JWKS). Authenticates nothing real. */
const RSA_PRIVATE_JWK = {
  kty: "RSA",
  n: "rMZAa31krtgsQWkLjzOcLN9q_cRzdvNidE_yIN3m4dCYxWrGLk84XMTTx6OalWAQnNnXgqSVCMf4xJvUZs4ERBIDXiILX_G5xlFUODYMOOkuDzhSFS_0AxdTk4ada9hwO2MKDlY0RqQcz7Pw1wdZLmjeh-4WpP3EFaAEdBr8c7ZBXKbJ0lhCmtmHd6uC_Nl4v8EakutnbO-ANr4GPV9H8rxiBtQkax2s6htSwOGSj93AGGCKuZ9aUHapZB8p7u45xJuUvr6Q-1SUVI5e7z8m1fZZefkULKEnn8SRxUoMJRQZ0T-YiQU9Ijg55qT8ae8G-03pbhq8k_sKoaopp7xWiQ",
  e: "AQAB",
  d: "MDAEIdx0BK8qTM8d11ve3P2xjT6gxGQvebkXYecFUPruMXBIWuIrb5N_3OV4NxIYIHkus5-k4RcgGQSozxp9l70IqzeqjR2sM5Q4FJmISjmkttNwjgpI0khmN2kRuFirPPi3AmsaqxKVvsSuqEP3SMNre2IcsgBnGgAfI5t6MdupmF-cScF_JOrSrxd2Y2qKEJDNdByvX2Ayb94ewxDgU0nl91NIW9eSNGA-YCd33HkkE1nTLNRF-V6ClU4omeICLi2H4JPsgllunCRQvFQWTJWshYkvPjKO_osTPv7keEoVNgbDzourzB3EZP60Vv0sgVRlscvYbIGzZVFIBdRB",
  p: "4T99nZZf38-lkfg2gtHPzEq8FKUO1R2MAje5fUrYhRS62YlIKA4XVPIQFe-92KIIeSRNNi1_opyZkYhMZeJZoBhh9TvhiVFB0oFvxHtd9eXwlkfppBe7xMJeaUX7NfywI1sawXNWjkGvfmXV9I9Wqb47SQZdNW2EXbIiKvoNj8k",
  q: "xFzJpuRAONzdLTPrBhnAuDZzTVDHIA6_jiiGCVyYCBqJ8R3P87SF-vLpvbO1PgQ-vXP2FaESg73VRD_RtwUWtOlBc0cKAkjZ17l1ODZ7JgXH6z3-s8SCcSaah4fAzAH6EsChv8S2rht0s3ddeKyRKt5YgzLW_-5xikU6Y3tDcME",
  dp: "WUEgVH6OtRAB6qpxZzseXTRL_N4-12Hi5coQ_T3YODuzopmMdxrGUgmtKBQcpSfntaEV218CEXx-ObXJmCGuJAslXdiBkTkanQBfOnssC1E3GUWbpkMlS109rfdmCMl9PjVOj9NVO_95O9u8gTD_RTm1IkWcT5x68-mvMlptX0k",
  dq: "WIs-ovLpwrpVQbzXjbivHmHvPD3gjKQZ3JCJYE9Qftb4vLEkxE_y2mYO4GvYnk5rvCI-JSKspptDP7NHba_tvUYxLTorWTxgftYx9Vcb0NlqfLlH0OgbqcouhE7CsTty-GHEjiS1-2yGAycgDvpBu4LnhsG2EVIEAMWWvqUmlAE",
  qi: "kXDGdjMwWBI13cguBrseSkZF1MrkT54iOt2UCzPleXH8YenjKgOGs5P51XWoAS4beWetEdXIfIunf9eUMq_0t8K7xwsUQcp_Qk_JIrWEp7fJ1pZ6qfqTvH3Sxv9Q-y57cNaIKyLQt6c9cDJiV11Otefe98_OHpeEa1PJZxva3Rk",
} as const;

/** Ed25519 — the fake GATEWAY signing key. Public half served by the stub at
 *  {{gateway_host}}/.well-known/jwks.json. */
const GW_PRIVATE_JWK = {
  crv: "Ed25519",
  d: "HhRHIheXapPNSfNiIREjALomWTaoR6jAepw8Xdk8qfY",
  x: "jeYDokDz3cqVljYqaBtW3gDZx5C2Fjef8qeYAI7kVfs",
  kty: "OKP",
} as const;

const CALLER_CLIENT_ID = "0oaMatrixProbe";
const PROTOCOL = "2026-07-28";

const b64u = (bytes: ArrayBuffer | Uint8Array): string => {
  const b = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let s = "";
  for (const byte of b) s += String.fromCharCode(byte);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
};
const seg = (o: unknown): string => b64u(new TextEncoder().encode(JSON.stringify(o)));

/** The canonical resource, computed the way src/resource.ts computes it. */
function canonicalResource(): string {
  const u = new URL(env.PUBLIC_MCP_URL);
  return `${u.origin}${u.pathname === "/" ? "" : u.pathname.replace(/\/+$/, "")}`;
}
const ORIGIN = new URL(env.PUBLIC_MCP_URL).origin;

let rsaKey: CryptoKey;
let gwKey: CryptoKey;
beforeAll(async () => {
  rsaKey = await crypto.subtle.importKey(
    "jwk",
    RSA_PRIVATE_JWK as unknown as JsonWebKey,
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["sign"],
  );
  gwKey = await crypto.subtle.importKey(
    "jwk",
    GW_PRIVATE_JWK as unknown as JsonWebKey,
    { name: "Ed25519" },
    false,
    ["sign"],
  );
});

/** A locally-verifiable Okta-shaped M2M access token. */
async function mintBearer(scopes: string[], over: Record<string, unknown> = {}): Promise<string> {
  const now = Math.floor(Date.now() / 1000);
  const input = `${seg({ alg: "RS256", kid: "ema-test-key" })}.${seg({
    iss: env.OKTA_ISSUER,
    aud: "{{okta_audience}}",
    cid: CALLER_CLIENT_ID,
    sub: CALLER_CLIENT_ID,
    scp: scopes,
    iat: now,
    exp: now + 3600,
    ...over,
  })}`;
  const sig = await crypto.subtle.sign(
    { name: "RSASSA-PKCS1-v1_5" },
    rsaKey,
    new TextEncoder().encode(input),
  );
  return `${input}.${b64u(sig)}`;
}

/** A gateway-shaped X-MCP-Actor assertion, optionally signed by the WRONG key. */
async function mintActor(over: Record<string, unknown> = {}, signer?: CryptoKey): Promise<string> {
  const now = Math.floor(Date.now() / 1000);
  const key =
    signer ??
    gwKey;
  const input = `${seg({ alg: "EdDSA", typ: "JWT", kid: "matrix-gw-key" })}.${seg({
    iss: env.GATEWAY_ISSUER,
    sub: "00uMatrixHuman",
    aud: canonicalResource(),
    act: { client_id: CALLER_CLIENT_ID },
    purpose: "matrix",
    run_id: "run-matrix",
    iat: now,
    exp: now + 60,
    jti: crypto.randomUUID(),
    ...over,
  })}`;
  const sig = await crypto.subtle.sign({ name: "Ed25519" }, key, new TextEncoder().encode(input));
  return `${input}.${b64u(sig)}`;
}

/** One tools/call (or other method) through the real dispatcher. */
async function rpc(
  method: string,
  params: Record<string, unknown>,
  opts: { token: string; headers?: Record<string, string>; modern?: boolean } ,
) {
  const headers: Record<string, string> = {
    authorization: `Bearer ${opts.token}`,
    "content-type": "application/json",
    accept: "application/json, text/event-stream",
    ...opts.headers,
  };
  const body: Record<string, unknown> = { jsonrpc: "2.0", id: 1, method, params };
  if (opts.modern) {
    headers["MCP-Protocol-Version"] = PROTOCOL;
    body.params = {
      _meta: {
        "io.modelcontextprotocol/protocolVersion": PROTOCOL,
        "io.modelcontextprotocol/clientCapabilities": {},
      },
      ...params,
    };
  }
  const res = await SELF.fetch(`${ORIGIN}/mcp`, { method: "POST", headers, body: JSON.stringify(body) });
  const text = await res.text();
  const line = text.split("\n").filter((l) => l.startsWith("data:")).pop();
  let json: Record<string, any> = {};
  try {
    json = JSON.parse(line ? line.slice(5) : text);
  } catch {
    /* non-JSON is asserted by status */
  }
  return { status: res.status, json, headers: res.headers };
}

const whoami = async (token: string, extraHeaders: Record<string, string> = {}) => {
  const { status, json } = await rpc(
    "tools/call",
    { name: "whoami", arguments: {} },
    { token, headers: extraHeaders },
  );
  expect(status, JSON.stringify(json)).toBe(200);
  return JSON.parse(json.result.content[0].text) as Record<string, unknown>;
};

// ---------------------------------------------------------------------------
// A. Resource metadata and the challenge
// ---------------------------------------------------------------------------

describe("matrix: metadata", () => {
  it("serves RFC 9728 metadata whose resource is the canonical PUBLIC_MCP_URL", async () => {
    const u = new URL(env.PUBLIC_MCP_URL);
    const res = await SELF.fetch(`${u.origin}/.well-known/oauth-protected-resource${u.pathname}`);
    expect(res.status).toBe(200);
    const body = (await res.json()) as Record<string, unknown>;
    expect(body.resource).toBe(canonicalResource());
    expect(body.scopes_supported).toEqual(MATRIX.advertisedScopes);
  });

  it("401s an unauthenticated /mcp with a challenge naming the metadata", async () => {
    const res = await SELF.fetch(`${ORIGIN}/mcp`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }),
    });
    expect(res.status).toBe(401);
    expect(res.headers.get("www-authenticate") ?? "").toContain("resource_metadata=");
  });
});

// ---------------------------------------------------------------------------
// B. The M2M dispatcher
// ---------------------------------------------------------------------------

describe("matrix: M2M", () => {
  it("serves a locally-verified bearer carrying a supported scope", async () => {
    const { status, json } = await rpc("tools/list", {}, { token: await mintBearer([MATRIX.readScope]) });
    expect(status).toBe(200);
    expect(json.result.tools.length).toBeGreaterThan(0);
  });

  it("refuses a bearer from another issuer", async () => {
    const token = await mintBearer([MATRIX.readScope], { iss: "https://evil.example" });
    const { status } = await rpc("tools/list", {}, { token });
    expect(status).toBe(401);
  });

  it("refuses a bearer carrying no supported scope", async () => {
    const { status } = await rpc("tools/list", {}, { token: await mintBearer(["other:nothing"]) });
    expect(status).toBe(401);
  });

  it("stamps auth_path m2m with the token's own client and subject", async () => {
    const who = await whoami(await mintBearer([MATRIX.readScope]));
    expect(who.auth_path).toBe("m2m");
    expect(who.client_id).toBe(CALLER_CLIENT_ID);
    expect(who.on_behalf_of).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// C. The two scope layers (needs a second scope to be expressible)
// ---------------------------------------------------------------------------

describe("matrix: scope layers", () => {
  if (!MATRIX.stepUp) {
    it("SKIPPED — single-scope server cannot express a step-up (recorded in ROADMAP Carried)", () => {
      expect(MATRIX.stepUp).toBeNull();
    });
    return;
  }
  const stepUp = MATRIX.stepUp;

  it("layer 1 refuses from the Mcp-Name header alone, with a step-up challenge", async () => {
    const { status, headers } = await rpc(
      "tools/call",
      { name: stepUp.tool, arguments: stepUp.args },
      {
        token: await mintBearer([MATRIX.readScope]),
        modern: true,
        headers: { "Mcp-Method": "tools/call", "Mcp-Name": stepUp.tool },
      },
    );
    expect(status).toBe(403);
    const challenge = headers.get("www-authenticate") ?? "";
    expect(challenge).toContain("insufficient_scope");
    expect(challenge).toContain(stepUp.scope);
  });

  it("layer 2 refuses inside the handler when no header names the tool", async () => {
    const { status, json } = await rpc(
      "tools/call",
      { name: stepUp.tool, arguments: stepUp.args },
      { token: await mintBearer([MATRIX.readScope]) },
    );
    expect(status).toBe(200);
    expect(json.result.isError).toBe(true);
    expect(json.result.content[0].text).toContain(stepUp.scope);
  });
});

// ---------------------------------------------------------------------------
// D. The gateway actor chain, through the route
// ---------------------------------------------------------------------------

describe("matrix: X-MCP-Actor", () => {
  if (!env.GATEWAY_ISSUER || !env.GATEWAY_JWKS_URL) {
    it("SKIPPED — GATEWAY_ISSUER/GATEWAY_JWKS_URL are not configured in this repo's test env", () => {
      expect(env.GATEWAY_ISSUER || env.GATEWAY_JWKS_URL).toBeFalsy();
    });
    return;
  }

  it("a VERIFIED assertion surfaces as m2m+actor with the human attached", async () => {
    const who = await whoami(await mintBearer([MATRIX.readScope]), {
      "x-mcp-actor": await mintActor(),
    });
    expect(who.auth_path, JSON.stringify(who)).toBe("m2m+actor");
    expect(who.on_behalf_of).toBe("00uMatrixHuman");
    expect(who.gateway_purpose).toBe("matrix");
  });

  it("a FORGED assertion degrades to plain m2m — never to an actor", async () => {
    const wrongKey = (await crypto.subtle.generateKey({ name: "Ed25519" }, false, [
      "sign",
      "verify",
    ])) as CryptoKeyPair;
    const who = await whoami(await mintBearer([MATRIX.readScope]), {
      "x-mcp-actor": await mintActor({}, wrongKey.privateKey),
    });
    expect(who.auth_path).toBe("m2m");
    expect(who.on_behalf_of).toBeUndefined();
  });

  it("a REPLAYED assertion is single-use: the second presentation is not an actor", async () => {
    const assertion = await mintActor();
    const first = await whoami(await mintBearer([MATRIX.readScope]), { "x-mcp-actor": assertion });
    expect(first.auth_path).toBe("m2m+actor");
    const second = await whoami(await mintBearer([MATRIX.readScope]), { "x-mcp-actor": assertion });
    expect(second.auth_path).toBe("m2m");
  });

  it("an assertion naming ANOTHER client is not this caller's actor", async () => {
    const who = await whoami(await mintBearer([MATRIX.readScope]), {
      "x-mcp-actor": await mintActor({ act: { client_id: "0oaSomebodyElse" } }),
    });
    expect(who.auth_path).toBe("m2m");
  });
});

// ---------------------------------------------------------------------------
// E. EMA provenance, end to end — the route-level test F-3 was owed
// ---------------------------------------------------------------------------

describe("matrix: EMA provenance", () => {
  if (!env.EMA_TRUSTED_ISSUERS) {
    it("SKIPPED — EMA_TRUSTED_ISSUERS is not configured in this repo's test env", () => {
      expect(env.EMA_TRUSTED_ISSUERS).toBeFalsy();
    });
    return;
  }
  const idpIssuer = String(env.EMA_TRUSTED_ISSUERS).split("=")[0];

  async function registerClient(): Promise<string> {
    const res = await SELF.fetch(`${ORIGIN}/register`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        client_name: "Matrix Enterprise Client",
        redirect_uris: ["https://enterprise.invalid/cb"],
        token_endpoint_auth_method: "none",
        grant_types: ["authorization_code", "urn:ietf:params:oauth:grant-type:jwt-bearer"],
        response_types: ["code"],
      }),
    });
    expect(res.status).toBeLessThan(300);
    return ((await res.json()) as { client_id: string }).client_id;
  }

  async function mintIdJag(clientId: string): Promise<string> {
    const now = Math.floor(Date.now() / 1000);
    const input = `${seg({ alg: "RS256", typ: "oauth-id-jag+jwt", kid: "ema-test-key" })}.${seg({
      iss: idpIssuer,
      sub: "00uEnterpriseHuman",
      aud: ORIGIN,
      client_id: clientId,
      jti: crypto.randomUUID(),
      iat: now,
      exp: now + 300,
      scope: MATRIX.readScope,
      email: "human@corp.example",
    })}`;
    const sig = await crypto.subtle.sign(
      { name: "RSASSA-PKCS1-v1_5" },
      rsaKey,
      new TextEncoder().encode(input),
    );
    return `${input}.${b64u(sig)}`;
  }

  it("an ID-JAG grant reaches a tool as auth_path=enterprise with its issuer", async () => {
    const clientId = await registerClient();
    const res = await SELF.fetch(`${ORIGIN}/token`, {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer",
        assertion: await mintIdJag(clientId),
        client_id: clientId,
        scope: MATRIX.readScope,
      }).toString(),
    });
    const body = (await res.json()) as { access_token?: string };
    expect(res.status, JSON.stringify(body)).toBe(200);

    // THE point of this scenario: six leaves used to hard-code "interactive"
    // here, so an IdP-approved grant was indistinguishable from a human who
    // read the consent page (review finding F-3). Type-checked mapping proved
    // the fix compiles; this proves it is WIRED.
    const who = await whoami(body.access_token!);
    expect(who.auth_path).toBe("enterprise");
    expect(who.enterprise_issuer).toBe(idpIssuer);
    expect(who.sub).toBe("00uEnterpriseHuman");
  });
});
