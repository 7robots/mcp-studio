// The M2M path through the real dispatcher, with a real signed access token.
//
// WHY THIS EXISTS. The entire M2M verification path was replaced — introspection
// plus a KV cache swapped for local JWT verification — and **not one test
// failed**. test/m2m.test.ts exercised `decide()` and `tryOktaM2M()` in isolation,
// so it kept passing against code the dispatcher no longer called. Nothing
// anywhere drove a bearer token through `src/index.ts`.
//
// That is the same shape as the gap the workerd project exists to close for the
// OAuth flow: helpers well covered, the wiring untested. A unit test on a verifier
// says nothing about whether the dispatcher calls it.

import { SELF, env } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";

const ORIGIN = "https://replace.{{domain_suffix}}";

// The private half of the key whose public half vitest.config.ts serves at
// Okta's /v1/keys. Same key as the fake enterprise IdP, on purpose: it means one
// key could sign either an access token or an ID-JAG, which is exactly the
// confusion the typ guard in src/jwt.ts refuses.
const PRIVATE_JWK = {
  "kty": "RSA",
  "n": "rMZAa31krtgsQWkLjzOcLN9q_cRzdvNidE_yIN3m4dCYxWrGLk84XMTTx6OalWAQnNnXgqSVCMf4xJvUZs4ERBIDXiILX_G5xlFUODYMOOkuDzhSFS_0AxdTk4ada9hwO2MKDlY0RqQcz7Pw1wdZLmjeh-4WpP3EFaAEdBr8c7ZBXKbJ0lhCmtmHd6uC_Nl4v8EakutnbO-ANr4GPV9H8rxiBtQkax2s6htSwOGSj93AGGCKuZ9aUHapZB8p7u45xJuUvr6Q-1SUVI5e7z8m1fZZefkULKEnn8SRxUoMJRQZ0T-YiQU9Ijg55qT8ae8G-03pbhq8k_sKoaopp7xWiQ",
  "e": "AQAB",
  "d": "MDAEIdx0BK8qTM8d11ve3P2xjT6gxGQvebkXYecFUPruMXBIWuIrb5N_3OV4NxIYIHkus5-k4RcgGQSozxp9l70IqzeqjR2sM5Q4FJmISjmkttNwjgpI0khmN2kRuFirPPi3AmsaqxKVvsSuqEP3SMNre2IcsgBnGgAfI5t6MdupmF-cScF_JOrSrxd2Y2qKEJDNdByvX2Ayb94ewxDgU0nl91NIW9eSNGA-YCd33HkkE1nTLNRF-V6ClU4omeICLi2H4JPsgllunCRQvFQWTJWshYkvPjKO_osTPv7keEoVNgbDzourzB3EZP60Vv0sgVRlscvYbIGzZVFIBdRB",
  "p": "4T99nZZf38-lkfg2gtHPzEq8FKUO1R2MAje5fUrYhRS62YlIKA4XVPIQFe-92KIIeSRNNi1_opyZkYhMZeJZoBhh9TvhiVFB0oFvxHtd9eXwlkfppBe7xMJeaUX7NfywI1sawXNWjkGvfmXV9I9Wqb47SQZdNW2EXbIiKvoNj8k",
  "q": "xFzJpuRAONzdLTPrBhnAuDZzTVDHIA6_jiiGCVyYCBqJ8R3P87SF-vLpvbO1PgQ-vXP2FaESg73VRD_RtwUWtOlBc0cKAkjZ17l1ODZ7JgXH6z3-s8SCcSaah4fAzAH6EsChv8S2rht0s3ddeKyRKt5YgzLW_-5xikU6Y3tDcME",
  "dp": "WUEgVH6OtRAB6qpxZzseXTRL_N4-12Hi5coQ_T3YODuzopmMdxrGUgmtKBQcpSfntaEV218CEXx-ObXJmCGuJAslXdiBkTkanQBfOnssC1E3GUWbpkMlS109rfdmCMl9PjVOj9NVO_95O9u8gTD_RTm1IkWcT5x68-mvMlptX0k",
  "dq": "WIs-ovLpwrpVQbzXjbivHmHvPD3gjKQZ3JCJYE9Qftb4vLEkxE_y2mYO4GvYnk5rvCI-JSKspptDP7NHba_tvUYxLTorWTxgftYx9Vcb0NlqfLlH0OgbqcouhE7CsTty-GHEjiS1-2yGAycgDvpBu4LnhsG2EVIEAMWWvqUmlAE",
  "qi": "kXDGdjMwWBI13cguBrseSkZF1MrkT54iOt2UCzPleXH8YenjKgOGs5P51XWoAS4beWetEdXIfIunf9eUMq_0t8K7xwsUQcp_Qk_JIrWEp7fJ1pZ6qfqTvH3Sxv9Q-y57cNaIKyLQt6c9cDJiV11Otefe98_OHpeEa1PJZxva3Rk",
  "alg": "RS256"
} as unknown as JsonWebKey;

const b64u = (b: ArrayBuffer | Uint8Array): string => {
  const u = b instanceof Uint8Array ? b : new Uint8Array(b);
  let s = "";
  for (const x of u) s += String.fromCharCode(x);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
};
const seg = (o: unknown) => b64u(new TextEncoder().encode(JSON.stringify(o)));

let key: CryptoKey;

beforeAll(async () => {
  key = await crypto.subtle.importKey(
    "jwk",
    PRIVATE_JWK,
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["sign"],
  );
});

/** Mint an Okta-shaped access token. Claims match a real one, decoded from the
 *  live tenant: RS256, `scp` as an array, `cid`, `sub` = the client id. */
async function accessToken(over: Record<string, unknown> = {}, header: Record<string, unknown> = {}) {
  const now = Math.floor(Date.now() / 1000);
  const h = { alg: "RS256", kid: "ema-test-key", ...header };
  const c = {
    iss: env.OKTA_ISSUER,
    aud: "{{okta_audience}}",
    cid: "0oaServiceApp",
    sub: "0oaServiceApp",
    scp: ["REPLACE:read"],
    iat: now,
    exp: now + 3600,
    ...over,
  };
  const input = `${seg(h)}.${seg(c)}`;
  const sig = await crypto.subtle.sign({ name: "RSASSA-PKCS1-v1_5" }, key, new TextEncoder().encode(input));
  return `${input}.${b64u(sig)}`;
}

const callMcp = (token: string, method = "tools/list") =>
  SELF.fetch(`${ORIGIN}/mcp`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${token}`,
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
      "Mcp-Method": method,
      "Mcp-Name": method,
    },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method }),
  });

describe("M2M path through the dispatcher", () => {
  it("serves a locally-verified access token, with no introspection call", async () => {
    const res = await callMcp(await accessToken());
    expect(res.status, await res.clone().text()).toBe(200);
  });

  it("REFUSES a token signed by the wrong key", async () => {
    const good = await accessToken();
    const parts = good.split(".");
    const mid = Math.floor(parts[2].length / 2);
    const ch = parts[2][mid];
    parts[2] = parts[2].slice(0, mid) + (ch === "A" ? "B" : "A") + parts[2].slice(mid + 1);
    const res = await callMcp(parts.join("."));
    expect(res.status).toBe(401);
  });

  it("REFUSES a token from another issuer", async () => {
    const res = await callMcp(await accessToken({ iss: "https://evil.okta.com/oauth2/default" }));
    expect(res.status).toBe(401);
  });

  it("REFUSES an expired token", async () => {
    const now = Math.floor(Date.now() / 1000);
    const res = await callMcp(await accessToken({ iat: now - 7200, exp: now - 3600 }));
    expect(res.status).toBe(401);
  });

  it("REFUSES a token with no exp at all", async () => {
    // The one auth path with no introspection backstop: a signed token without
    // `exp` would otherwise validate forever.
    const res = await callMcp(await accessToken({ exp: undefined }));
    expect(res.status).toBe(401);
  });

  it("REFUSES a token carrying no scope this server implements", async () => {
    const res = await callMcp(await accessToken({ scp: ["someone:else"] }));
    expect(res.status).toBe(401);
  });

  it("REFUSES a token with no cid — an unattributable caller", async () => {
    const res = await callMcp(await accessToken({ cid: undefined }));
    expect(res.status).toBe(401);
  });

  it("REFUSES a token with no sub, for the same reason", async () => {
    const res = await callMcp(await accessToken({ sub: undefined }));
    expect(res.status).toBe(401);
  });

  it("REFUSES an ID-JAG presented as an access token (RFC 8725 §3.11)", async () => {
    // Token-type confusion. The same key signs both here, which is the point:
    // without the typ guard, an authorization GRANT would be accepted as a
    // CREDENTIAL for calling this resource.
    const res = await callMcp(await accessToken({}, { typ: "oauth-id-jag+jwt" }));
    expect(res.status).toBe(401);
  });

  it("enforces per-tool scope on the M2M path, not just at the door", async () => {
    // A read-only token reaching a write tool. The door accepts it — REPLACE:read is
    // one of this server's scopes — and the tool must still refuse.
    const token = await accessToken({ scp: ["REPLACE:read"] });
    const res = await SELF.fetch(`${ORIGIN}/mcp`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${token}`,
        "content-type": "application/json",
        accept: "application/json, text/event-stream",
        "Mcp-Method": "tools/call",
        "Mcp-Name": "REPLACE_write_tool",
      },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "tools/call",
        params: { name: "REPLACE_write_tool", arguments: {} },
      }),
    });
    // 403 from the pre-handler gate, or a tool-level refusal — either is correct;
    // what must not happen is a 200 that wrote something.
    expect([403, 200]).toContain(res.status);
    if (res.status === 200) {
      const body = await res.text();
      expect(body).toMatch(/scope|Error/i);
    }
  });

  it("still lets an interactive Worker-issued token through its own path", async () => {
    // A Worker-issued token is not a JWT. It must be short-circuited before
    // verification and handed to OAuthProvider, which will 401 an invented one —
    // the point is that it is not mistaken for a malformed M2M token.
    const res = await callMcp("00uAbc:grantXyz:invented");
    expect(res.status).toBe(401);
  });
});
