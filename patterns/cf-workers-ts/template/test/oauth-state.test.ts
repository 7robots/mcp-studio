// Tests for the server-side OAuth state and the consent records.
//
// SCOPE OF WHAT THESE PROVE, stated precisely because the honest limit is easy
// to overstate. They test these helpers thoroughly: a forged state, a missing or
// wrong cookie, single use, per-flow cookie isolation, the delete-after-check
// ordering, and that the nonce carries no request data.
//
// They do NOT prove the vulnerability is closed. That vulnerability lived in
// src/auth.ts — which no test imports, because Hono plus workers-oauth-provider
// pull in `cloudflare:` modules the Node pool cannot load. Reverting src/auth.ts
// makes these files fail to RESOLVE, not fail an assertion. Nothing here
// exercises /authorize -> Okta -> /callback -> /consent end to end, so nothing
// here would catch a regression that stopped calling takeState at all.
//
// Closing that gap needs @cloudflare/vitest-pool-workers driving the real routes
// against a stubbed Okta. Recorded as a required followup in
// my-skills/docs/plans/mcp-2026-07-28-adoption.md.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import {
  CONSENT_COOKIE_PREFIX,
  stateCookieName,
  clearCookie,
  consentCovers,
  consentKey,
  consentRedirectKey,
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
} from "../src/oauth-state";

// Minimal in-memory KV honouring only what these functions use.
function makeKv() {
  const store = new Map<string, string>();
  const kv = {
    get: async (key: string, type?: string) => {
      const raw = store.get(key);
      if (raw === undefined) return null;
      return type === "json" ? JSON.parse(raw) : raw;
    },
    put: async (key: string, value: string) => void store.set(key, value),
    delete: async (key: string) => void store.delete(key),
  } as unknown as KVNamespace;
  return { kv, store };
}

const REQ = { clientId: "c1", redirectUri: "https://client.example/cb", scope: ["REPLACE:read"] };

describe("isNonce", () => {
  it("accepts a UUID and rejects anything that could break a KV key or cookie name", () => {
    expect(isNonce(crypto.randomUUID())).toBe(true);
    expect(isNonce("")).toBe(false);
    expect(isNonce(undefined)).toBe(false);
    expect(isNonce("../../etc/passwd")).toBe(false);
    expect(isNonce("a".repeat(600))).toBe(false);
    // A cookie name may not contain these; a KV key length limit is a 500.
    expect(isNonce("has spaces and=equals")).toBe(false);
  });
});

describe("timingSafeEqualStr", () => {
  it("compares equal and unequal values, and never throws on a length mismatch", () => {
    expect(timingSafeEqualStr("abc", "abc")).toBe(true);
    expect(timingSafeEqualStr("abc", "abd")).toBe(false);
    expect(timingSafeEqualStr("short", "much longer value")).toBe(false);
    expect(timingSafeEqualStr("", "")).toBe(true);
  });

  it("does not short-circuit on the first differing byte", () => {
    // Differing at the start and at the end must both simply be false; the
    // point of the XOR accumulator is that neither is cheaper to discover.
    const base = "a".repeat(36);
    expect(timingSafeEqualStr(base, "b" + base.slice(1))).toBe(false);
    expect(timingSafeEqualStr(base, base.slice(0, 35) + "b")).toBe(false);
  });

  it("is not fooled by multi-byte characters of differing byte length", () => {
    expect(timingSafeEqualStr("é", "e")).toBe(false);
    expect(timingSafeEqualStr("é", "é")).toBe(true);
  });
});

describe("cookies", () => {
  it("sets __Host--compatible attributes", () => {
    const c = setCookie(stateCookieName(crypto.randomUUID()), "v", 600);
    // __Host- requires Secure and Path=/ and forbids Domain.
    expect(c).toContain("Secure");
    expect(c).toContain("Path=/");
    expect(c).toContain("HttpOnly");
    expect(c).not.toContain("Domain");
  });

  it("clears with Max-Age=0", () => {
    expect(clearCookie(stateCookieName(crypto.randomUUID()))).toContain("Max-Age=0");
  });

  it("reads one cookie out of many without prefix-matching a similar name", () => {
    const n = stateCookieName(crypto.randomUUID());
    const header = `other=1; ${n}=wanted; ${n}x=decoy`;
    expect(readCookie(header, n)).toBe("wanted");
    expect(readCookie(null, n)).toBeNull();
    expect(readCookie("nope=1", n)).toBeNull();
  });
});

describe("state records", () => {
  it("round-trips the request when the cookie matches", async () => {
    const { kv } = makeKv();
    const { state, csrf } = await putState(kv, REQ);
    const out = await takeState<typeof REQ>(kv, state, `${stateCookieName(state)}=${csrf}`);
    expect(out).toEqual({ ok: true, req: REQ });
  });

  it("NEVER puts the request itself in the state parameter", async () => {
    const { kv } = makeKv();
    const { state } = await putState(kv, REQ);
    // The whole point: the nonce carries no information about the request, so
    // there is nothing for an attacker to forge.
    expect(state).not.toContain("client");
    expect(state).not.toContain("https");
    expect(isNonce(state)).toBe(true);
  });

  it("rejects a forged state that was never issued", async () => {
    const { kv } = makeKv();
    const forged = crypto.randomUUID();
    const out = await takeState(kv, forged, `${stateCookieName(forged)}=anything`);
    expect(out.ok).toBe(false);
  });

  it("uses a PER-FLOW cookie, so concurrent authorizations cannot clobber each other", async () => {
    const { kv } = makeKv();
    const a = await putState(kv, REQ);
    const b = await putState(kv, REQ);
    const header = `${stateCookieName(a.state)}=${a.csrf}; ${stateCookieName(b.state)}=${b.csrf}`;
    expect((await takeState(kv, a.state, header)).ok).toBe(true);
    expect((await takeState(kv, b.state, header)).ok).toBe(true);
  });

  it("will not accept another flow's cookie value", async () => {
    const { kv } = makeKv();
    const a = await putState(kv, REQ);
    const b = await putState(kv, REQ);
    // b's secret presented under a's cookie name.
    expect((await takeState(kv, a.state, `${stateCookieName(a.state)}=${b.csrf}`)).ok).toBe(false);
  });

  it("rejects a valid state presented without the cookie", async () => {
    const { kv } = makeKv();
    const { state } = await putState(kv, REQ);
    expect((await takeState(kv, state, null)).ok).toBe(false);
  });

  it("rejects a valid state presented with the WRONG cookie", async () => {
    const { kv } = makeKv();
    const { state } = await putState(kv, REQ);
    const out = await takeState(kv, state, `${stateCookieName(state)}=${crypto.randomUUID()}`);
    expect(out.ok).toBe(false);
  });

  it("leaves the record intact after a wrong-cookie attempt, so the real browser can still finish", async () => {
    const { kv } = makeKv();
    const { state, csrf } = await putState(kv, REQ);
    expect((await takeState(kv, state, `${stateCookieName(state)}=wrong`)).ok).toBe(false);
    // Ordering is load-bearing: deletion happens only after the CSRF check.
    expect((await takeState<typeof REQ>(kv, state, `${stateCookieName(state)}=${csrf}`)).ok).toBe(true);
  });

  it("is single-use once consumed, so a code cannot be replayed through it", async () => {
    const { kv } = makeKv();
    const { state, csrf } = await putState(kv, REQ);
    expect((await takeState(kv, state, `${stateCookieName(state)}=${csrf}`)).ok).toBe(true);
    expect((await takeState(kv, state, `${stateCookieName(state)}=${csrf}`)).ok).toBe(false);
  });

  it("rejects a malformed state before touching KV", async () => {
    const { kv, store } = makeKv();
    const out = await takeState(kv, "not-a-uuid", "irrelevant=x");
    expect(out).toEqual({ ok: false, reason: "malformed state" });
    expect(store.size).toBe(0);
  });
});

describe("pending consent records", () => {
  const base = {
    req: REQ,
    userId: "00u1",
    email: "a@example.com",
    oktaAccessToken: "t",
    scopes: ["REPLACE:read"],
    consentKey: "k",
  };

  it("stores and retrieves, and the csrf is not the nonce", async () => {
    const { kv } = makeKv();
    const { nonce, csrf } = await putPending(kv, base);
    expect(csrf).not.toBe(nonce);
    const rec = await getPending<typeof REQ>(kv, nonce);
    expect(rec?.csrf).toBe(csrf);
    expect(rec?.userId).toBe("00u1");
  });

  it("uses a per-flow cookie name so concurrent authorizations cannot clobber each other", async () => {
    const { kv } = makeKv();
    const a = await putPending(kv, base);
    const b = await putPending(kv, base);
    expect(a.nonce).not.toBe(b.nonce);
    const header = `${CONSENT_COOKIE_PREFIX}${a.nonce}=${a.csrf}; ${CONSENT_COOKIE_PREFIX}${b.nonce}=${b.csrf}`;
    expect(readCookie(header, CONSENT_COOKIE_PREFIX + a.nonce)).toBe(a.csrf);
    expect(readCookie(header, CONSENT_COOKIE_PREFIX + b.nonce)).toBe(b.csrf);
  });

  it("is gone after deletion, so a resubmit cannot mint twice", async () => {
    const { kv } = makeKv();
    const { nonce } = await putPending(kv, base);
    await deletePending(kv, nonce);
    expect(await getPending(kv, nonce)).toBeNull();
  });
});

describe("remembered consent", () => {
  const sub = "00u1";
  const client = "c1";
  const resource = "https://replace.{{domain_suffix}}/mcp";
  const redirect = "https://client.example/cb";

  it("is scoped per user, client AND resource, so approving one server is not approving another", () => {
    const a = consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: redirect });
    expect(consentKey({ sub: "other", clientId: client, resource: resource, redirectUri: redirect })).not.toBe(a);
    expect(consentKey({ sub: sub, clientId: "other", resource: resource, redirectUri: redirect })).not.toBe(a);
    expect(consentKey({ sub: sub, clientId: client, resource: "https://other.{{domain_suffix}}/mcp", redirectUri: redirect })).not.toBe(a);
  });

  it("encodes the client id and resource so a value containing ':' cannot forge a different key", () => {
    const sneaky = consentKey({ sub: sub, clientId: "c1:extra", resource: resource, redirectUri: redirect });
    const plain = consentKey({ sub: sub, clientId: "c1", resource: "extra:" + resource, redirectUri: redirect });
    expect(sneaky).not.toBe(plain);
  });

  it("ignores the PORT for loopback destinations, matching the provider's own rule", () => {
    // `isValidRedirectUri` in workers-oauth-provider compares only protocol,
    // hostname, pathname and search for loopback URIs (RFC 8252 — a native app
    // cannot reserve a port). A client registered at 127.0.0.1:5000 is therefore
    // validly authorized at ANY port, so a key that included the port would be
    // stricter than the rule that let the request in, and an ephemeral-port
    // local client would be re-prompted on every launch.
    const a = consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: "http://127.0.0.1:51111/cb" });
    const b = consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: "http://127.0.0.1:52222/cb" });
    expect(a).toBe(b);
    // ...and this is loopback-ONLY. Anywhere else, an exact match is what the
    // provider requires, so it is what the key requires.
    expect(consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: "https://client.example:8443/cb" })).not.toBe(
      consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: "https://client.example:9443/cb" }),
    );
  });

  it("still distinguishes loopback destinations that differ in more than the port", () => {
    // Dropping the port must not widen anything else: path, query, scheme and
    // host are all still part of the key.
    const base = consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: "http://127.0.0.1:5000/cb" });
    for (const other of [
      "http://127.0.0.1:5000/evil",
      "http://127.0.0.1:5000/cb?next=evil",
      "https://127.0.0.1:5000/cb",
      "http://127.0.0.2:5000/cb",
      "http://localhost:5000/cb",
    ]) {
      expect(consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: other })).not.toBe(base);
    }
  });

  it("treats every loopback spelling the provider accepts as loopback", () => {
    // isLoopbackUri accepts 127.x.x.x, ::1 and localhost (case-insensitively).
    // Missing one here would mean that spelling still keys on the port, so the
    // re-prompt-every-launch bug would survive for whoever uses it.
    for (const host of ["127.0.0.1", "127.1.2.3", "[::1]", "localhost", "LOCALHOST"]) {
      expect(consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: `http://${host}:1111/cb` })).toBe(
        consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: `http://${host}:2222/cb` }),
      );
    }
  });

  it("changes when the redirect destination changes, so a moved destination re-prompts", () => {
    // The reason the redirect URI is in the key. A CIMD client's redirect_uris
    // come from a document it controls and that is re-fetched on every
    // authorization, so without this a client could be approved at one
    // destination and then quietly collect codes at another for the rest of the
    // 90-day window — with the user never seeing the screen again.
    const approved = consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: redirect });
    expect(consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: "https://evil.example/cb" })).not.toBe(approved);
    // Same host, different path still counts: the whole URI is what was shown.
    expect(consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: redirect + "2" })).not.toBe(approved);
  });

  it("does NOT re-prompt a loopback client that came back on a different port", () => {
    // The provider admits any port for a registered loopback URI, so the
    // remembered decision has to admit it too — otherwise every launch of a
    // native client shows a consent page naming a port number the user cannot
    // possibly judge, which is the consent-fatigue failure mode.
    const approved = consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: "http://127.0.0.1:5000/cb" });
    expect(consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: "http://127.0.0.1:61234/cb" })).toBe(approved);
  });

  it("does not cover anything before a decision is recorded", async () => {
    const { kv } = makeKv();
    expect(await consentCovers(kv, consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: redirect }), ["REPLACE:read"])).toBe(false);
  });

  it("covers exactly what was approved", async () => {
    const { kv } = makeKv();
    const key = consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: redirect });
    await rememberConsent(kv, key, ["REPLACE:read"]);
    expect(await consentCovers(kv, key, ["REPLACE:read"])).toBe(true);
  });

  it("RE-PROMPTS when more is requested than was approved", async () => {
    const { kv } = makeKv();
    const key = consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: redirect });
    await rememberConsent(kv, key, ["REPLACE:read"]);
    // The whole point of being scope-bound: escalating to write must be seen.
    expect(await consentCovers(kv, key, ["REPLACE:read", "REPLACE:write"])).toBe(false);
  });

  it("covers a subset of a broader approval", async () => {
    const { kv } = makeKv();
    const key = consentKey({ sub: sub, clientId: client, resource: resource, redirectUri: redirect });
    await rememberConsent(kv, key, ["REPLACE:read", "REPLACE:write"]);
    expect(await consentCovers(kv, key, ["REPLACE:write"])).toBe(true);
  });
});

describe("/consent ordering (source-text guard)", () => {
  // Read as text on purpose: src/auth.ts transitively imports `cloudflare:`
  // modules and cannot be loaded under the plain-Node pool at all. A text
  // assertion that fails loudly beats no coverage of an ordering that has no
  // other observable signature.
  const auth = readFileSync(
    join(dirname(fileURLToPath(import.meta.url)), "..", "src/auth.ts"),
    "utf8",
  );

  it("deletes the pending record, mints, and only THEN remembers", () => {
    // Remembering before minting leaves a 90-day approval on record for a grant
    // that was never issued: completeAuthorization re-validates the redirect URI
    // against a freshly-fetched CIMD document and throws if it no longer
    // matches. The user is then never asked about that client again.
    //
    // Anchor past the deny branch, which has a deletePending of its own — a
    // slice including it would satisfy "delete before mint" trivially.
    const approve = auth.slice(auth.indexOf("// Delete before minting."));
    expect(approve).not.toBe(auth);
    const del = approve.indexOf("await deletePending(c.env.OAUTH_KV, nonce);");
    const mint = approve.indexOf("const res = await completeAndRedirect");
    const remember = approve.indexOf("await rememberConsent(");
    expect(del).toBeGreaterThan(-1);
    expect(mint).toBeGreaterThan(del);
    expect(remember).toBeGreaterThan(mint);
  });

  it("builds the consent key from all four named identity fields", () => {
    // This guard used to match a POSITIONAL call, because four `string`
    // parameters meant dropping or swapping one still typechecked and left every
    // unit test green. `ConsentIdentity` made that a compile error, so the guard
    // no longer carries the weight it did — what remains is that the call site
    // names the redirect URI at all, since including it is what makes a moved
    // destination re-prompt.
    expect(auth).toMatch(/consentKey\(\{[\s\S]{0,400}?redirectUri:\s*oauthReqInfo\.redirectUri/);
  });
});

describe("consentRedirectKey", () => {
  // Tested directly, not only through consentKey, because the property that
  // matters is a comparison against ANOTHER implementation: the provider's
  // isValidRedirectUri. Going through consentKey hides which component differed.

  it("drops the port for loopback and keeps everything else", () => {
    expect(consentRedirectKey("http://127.0.0.1:51111/cb")).toBe("http://127.0.0.1/cb");
    expect(consentRedirectKey("http://localhost:1234/cb?a=1")).toBe(
      "http://localhost/cb?a=1",
    );
    expect(consentRedirectKey("http://[::1]:9/cb")).toBe("http://[::1]/cb");
  });

  it("accepts every loopback spelling URL normalizes to 127.0.0.1", () => {
    // The provider's regex runs on URL.hostname, which normalizes 127.1,
    // 0177.0.0.1 and 2130706433 all to "127.0.0.1" before either of us sees it.
    for (const host of ["127.0.0.1", "127.1", "0177.0.0.1", "2130706433", "127.2.3.4"]) {
      expect(consentRedirectKey(`http://${host}:1/cb`)).toBe(
        consentRedirectKey(`http://${host}:2/cb`),
      );
    }
  });

  it("leaves non-loopback URIs byte-identical", () => {
    // The provider requires requestUri === registered off the loopback path, so
    // the key must be exactly as strict: fragment, userinfo, case and default
    // ports all preserved rather than normalized away.
    for (const uri of [
      "https://client.example:8443/cb",
      "https://client.example/cb#frag",
      "https://client.example/CB",
      "https://client.example:443/cb",
      "https://foo.localhost/cb",
      "https://127.example.com/cb",
    ]) {
      expect(consentRedirectKey(uri)).toBe(uri);
    }
  });

  it("does NOT case-fold the hostname it puts in the key", () => {
    // isLoopbackUri lowercases only for its own `localhost` comparison;
    // isValidRedirectUri then compares hostname VERBATIM. WHATWG URL
    // case-normalizes the host only for SPECIAL schemes, so under a custom
    // native-app scheme the two spellings below are distinct to the provider —
    // and must stay distinct here, or the key would be looser than the matcher.
    expect(consentRedirectKey("myapp://LOCALHOST/cb")).not.toBe(
      consentRedirectKey("myapp://localhost/cb"),
    );
    // ...while http:// is folded by URL itself, before either of us sees it.
    expect(consentRedirectKey("http://LOCALHOST:1/cb")).toBe(
      consentRedirectKey("http://localhost:2/cb"),
    );
  });

  it("returns an unparseable value untouched rather than widening it", () => {
    expect(consentRedirectKey("not a uri")).toBe("not a uri");
  });
});
