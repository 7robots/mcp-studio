// Verifying the gateway's assertion is a trust boundary, so these tests are
// written as attacks: a forged signature, a swapped key, an assertion minted
// for another server, an expired one, and the classic "alg: none".
//
// Added with the client binding and the replay marker: an assertion naming a
// client other than the one presenting the bearer token, one naming no client at
// all, and the same assertion presented twice. `jti` used to be filler here
// (`"abc"` for every mint) precisely because nothing read it — so a mint now
// defaults to a UNIQUE jti, and the replay tests are the ones that deliberately
// re-present the same string.

import { describe, expect, it } from "vitest";

import { actorRequired, checkClaims, extractActorHeader, gateOnActor, verifyActor } from "../src/actor";

const ISSUER = "https://{{gateway_host}}";
const AUDIENCE = "https://replace.{{domain_suffix}}/mcp";
const JWKS_URL = "https://{{gateway_host}}/.well-known/jwks.json";
/** The client id on the bearer token the assertion accompanies (its `cid`). */
const CALLER = "0oaPlatformWorkload";

/** In-memory KV, mirroring makeKv in test/oauth-state.test.ts. */
function makeKv() {
  const store = new Map<string, string>();
  /** Every put's options, so the marker's TTL is assertable. */
  const puts: { key: string; ttl?: number }[] = [];
  const kv = {
    get: async (key: string) => store.get(key) ?? null,
    put: async (key: string, value: string, opts?: { expirationTtl?: number }) => {
      puts.push({ key, ttl: opts?.expirationTtl });
      store.set(key, value);
    },
    delete: async (key: string) => void store.delete(key),
  } as unknown as KVNamespace;
  return { kv, store, puts };
}

function b64url(bytes: Uint8Array | ArrayBuffer): string {
  const view = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let binary = "";
  for (const b of view) binary += String.fromCharCode(b);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

const encodeSegment = (value: unknown) => b64url(new TextEncoder().encode(JSON.stringify(value)));

async function keypair() {
  const pair = (await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"])) as CryptoKeyPair;
  const publicJwk = (await crypto.subtle.exportKey("jwk", pair.publicKey)) as unknown as Record<string, unknown>;
  return { pair, publicJwk: { ...publicJwk, alg: "EdDSA", use: "sig", kid: "test-kid" } };
}

async function mint(
  signer: CryptoKey,
  claims: Record<string, unknown> = {},
  header: Record<string, unknown> = {},
): Promise<string> {
  const now = Math.floor(Date.now() / 1000);
  const payload = {
    iss: ISSUER,
    sub: "00uHuman",
    aud: AUDIENCE,
    iat: now,
    exp: now + 60,
    // Unique per mint: a shared in-memory store would otherwise make the second
    // test to run look like a replay of the first.
    jti: crypto.randomUUID(),
    act: { client_id: CALLER },
    ...claims,
  };
  const signingInput = `${encodeSegment({ alg: "EdDSA", typ: "JWT", kid: "test-kid", ...header })}.${encodeSegment(payload)}`;
  const signature = await crypto.subtle.sign({ name: "Ed25519" }, signer, new TextEncoder().encode(signingInput));
  return `${signingInput}.${b64url(signature)}`;
}

function env(over: Record<string, unknown> = {}): Env {
  return {
    GATEWAY_ISSUER: ISSUER,
    GATEWAY_JWKS_URL: JWKS_URL,
    PUBLIC_MCP_URL: AUDIENCE,
    // A FRESH namespace per call, so a test that does not care about replay
    // cannot be broken by one that does. Replay tests reuse one env().
    OAUTH_KV: makeKv().kv,
    ...over,
  } as unknown as Env;
}

/** The caller identity verifyActor binds `act.client_id` against. */
const caller = (clientId = CALLER) => ({ clientId });

function request(actor?: string): Request {
  return new Request(AUDIENCE, { headers: actor ? { "x-mcp-actor": actor } : {} });
}

function jwksFetch(keys: unknown[], onCall?: () => void) {
  return (async () => {
    onCall?.();
    return new Response(JSON.stringify({ keys }), { headers: { "content-type": "application/json" } });
  }) as unknown as typeof fetch;
}

describe("extractActorHeader", () => {
  it("reads the header, treating blank as absent", () => {
    expect(extractActorHeader(request("abc"))).toBe("abc");
    expect(extractActorHeader(request("   "))).toBeNull();
    expect(extractActorHeader(request())).toBeNull();
  });
});

describe("checkClaims", () => {
  const now = 1_800_000_000;
  const good = {
    iss: ISSUER,
    aud: AUDIENCE,
    sub: "00uHuman",
    iat: now,
    exp: now + 60,
    jti: "jti-1",
    act: { client_id: CALLER },
  };

  it("accepts a well-formed assertion", () => {
    expect(checkClaims(good, { issuer: ISSUER, audience: AUDIENCE, callerClientId: CALLER }, now)).toBeNull();
  });

  it("rejects an assertion minted for a different server", () => {
    // Audience binding: without it, an assertion obtained for moon replays here.
    expect(checkClaims({ ...good, aud: "https://other.{{domain_suffix}}/mcp" }, { issuer: ISSUER, audience: AUDIENCE, callerClientId: CALLER }, now))
      .toContain("audience mismatch");
  });

  it("rejects an assertion from a different issuer", () => {
    expect(checkClaims({ ...good, iss: "https://evil.example" }, { issuer: ISSUER, audience: AUDIENCE, callerClientId: CALLER }, now))
      .toContain("issuer mismatch");
  });

  it("rejects an expired assertion, allowing a little clock skew", () => {
    expect(checkClaims({ ...good, exp: now - 10 }, { issuer: ISSUER, audience: AUDIENCE, callerClientId: CALLER }, now)).toBeNull();
    expect(checkClaims({ ...good, exp: now - 120 }, { issuer: ISSUER, audience: AUDIENCE, callerClientId: CALLER }, now)).toContain("expired");
  });

  it("rejects one issued well in the future", () => {
    expect(checkClaims({ ...good, iat: now + 600 }, { issuer: ISSUER, audience: AUDIENCE, callerClientId: CALLER }, now)).toContain("future");
  });

  it("rejects a missing subject", () => {
    expect(checkClaims({ ...good, sub: "" }, { issuer: ISSUER, audience: AUDIENCE, callerClientId: CALLER }, now)).toContain("missing subject");
  });

  it("REJECTS an assertion acting for a client other than the one calling", () => {
    // The gap this closes: `act.client_id` was declared and never compared, so
    // any platform workload holding a solar:read token could present an
    // assertion naming a DIFFERENT client and have the call recorded against it.
    const verdict = checkClaims(
      { ...good, act: { client_id: "0oaSomeoneElse" } },
      { issuer: ISSUER, audience: AUDIENCE, callerClientId: CALLER },
      now,
    );
    expect(verdict).toContain("acting-client mismatch");
    expect(verdict).toContain("0oaSomeoneElse");
  });

  it("REJECTS an assertion that names no acting client at all", () => {
    // Absent is refused, not waved through: an optional binding is not a
    // binding, and "unstated" is not something this server can record.
    for (const act of [undefined, {}, { client_id: "" }, { client_id: 7 }, "nonsense"]) {
      expect(
        checkClaims({ ...good, act }, { issuer: ISSUER, audience: AUDIENCE, callerClientId: CALLER }, now),
        `act=${JSON.stringify(act)} was accepted`,
      ).toContain("names no acting client");
    }
  });

  it("REJECTS an assertion with no jti, since there would be nothing to consume", () => {
    for (const jti of [undefined, "", 42]) {
      expect(
        checkClaims({ ...good, jti }, { issuer: ISSUER, audience: AUDIENCE, callerClientId: CALLER }, now),
        `jti=${JSON.stringify(jti)} was accepted`,
      ).toContain("no jti");
    }
  });

  it("accepts the matching client id", () => {
    // Positive control for the two above: without it, a checkClaims that
    // rejected EVERYTHING would still pass them.
    expect(checkClaims(good, { issuer: ISSUER, audience: AUDIENCE, callerClientId: CALLER }, now)).toBeNull();
  });
});

describe("verifyActor", () => {
  it("accepts an assertion signed by the published key", async () => {
    const { pair, publicJwk } = await keypair();
    const verdict = await verifyActor(request(await mint(pair.privateKey)), env(), caller(), {
      fetchImpl: jwksFetch([publicJwk]),
    });
    expect(verdict.ok).toBe(true);
    expect(verdict.claims?.sub).toBe("00uHuman");
  });

  it("refuses when the gateway vars are not configured, rather than throwing", async () => {
    // Opt-in module: unconfigured must not read as verified. With the flag off
    // the verdict is ignored; with it on this refuses every caller — the right
    // direction for a gate whose configuration is missing.
    const { pair, publicJwk } = await keypair();
    const verdict = await verifyActor(
      request(await mint(pair.privateKey)),
      env({ GATEWAY_ISSUER: undefined, GATEWAY_JWKS_URL: undefined }),
      caller(),
      { fetchImpl: jwksFetch([publicJwk]) },
    );
    expect(verdict.ok).toBe(false);
    expect(verdict.reason).toContain("not configured");
  });

  it("canonicalizes the audience from PUBLIC_MCP_URL, so a trailing slash still verifies", async () => {
    // resourceUrls() trims the trailing slash the RFC 9728 metadata also trims;
    // demanding the raw var here would refuse every assertion the gateway mints
    // against the advertised resource.
    const { pair, publicJwk } = await keypair();
    const verdict = await verifyActor(
      request(await mint(pair.privateKey)),
      env({ PUBLIC_MCP_URL: `${AUDIENCE}/` }),
      caller(),
      { fetchImpl: jwksFetch([publicJwk]) },
    );
    expect(verdict.ok, verdict.reason).toBe(true);
  });

  it("rejects an assertion signed by some other key", async () => {
    const mine = await keypair();
    const theirs = await keypair();
    const verdict = await verifyActor(request(await mint(theirs.pair.privateKey)), env(), caller(), {
      fetchImpl: jwksFetch([mine.publicJwk]),
    });
    expect(verdict.ok).toBe(false);
    expect(verdict.reason).toContain("did not verify");
  });

  it("rejects a tampered payload", async () => {
    const { pair, publicJwk } = await keypair();
    const jwt = await mint(pair.privateKey);
    const [h, p, s] = jwt.split(".");
    const claims = JSON.parse(atob(p.replace(/-/g, "+").replace(/_/g, "/")));
    claims.sub = "00uSomeoneElse";
    const forged = `${h}.${encodeSegment(claims)}.${s}`;
    const verdict = await verifyActor(request(forged), env(), caller(), { fetchImpl: jwksFetch([publicJwk]) });
    expect(verdict.ok).toBe(false);
  });

  it('refuses "alg": "none" outright', async () => {
    // The header is attacker-controlled; the algorithm must not be.
    const { publicJwk } = await keypair();
    const now = Math.floor(Date.now() / 1000);
    const unsigned = `${encodeSegment({ alg: "none", typ: "JWT" })}.${encodeSegment({
      iss: ISSUER,
      sub: "00uHuman",
      aud: AUDIENCE,
      iat: now,
      exp: now + 60,
    })}.`;
    const verdict = await verifyActor(request(unsigned), env(), caller(), { fetchImpl: jwksFetch([publicJwk]) });
    expect(verdict.ok).toBe(false);
    expect(verdict.reason).toContain("unsupported algorithm");
  });

  it("does not fetch the JWKS for an assertion that fails its claims", async () => {
    // Cheap checks first: a wrong audience should not cost a network call.
    const { pair, publicJwk } = await keypair();
    let fetched = 0;
    const jwt = await mint(pair.privateKey, { aud: "https://other.{{domain_suffix}}/mcp" });
    const verdict = await verifyActor(request(jwt), env(), caller(), {
      fetchImpl: jwksFetch([publicJwk], () => (fetched += 1)),
    });
    expect(verdict.ok).toBe(false);
    expect(fetched).toBe(0);
  });

  it("reports a missing header rather than throwing", async () => {
    const verdict = await verifyActor(request(), env(), caller(), { fetchImpl: jwksFetch([]) });
    expect(verdict).toMatchObject({ ok: false, reason: "no X-MCP-Actor header" });
  });

  it("rejects a malformed assertion", async () => {
    const verdict = await verifyActor(request("not.a.jwt"), env(), caller(), { fetchImpl: jwksFetch([]) });
    expect(verdict.ok).toBe(false);
  });

  it("fails closed when the JWKS cannot be retrieved", async () => {
    const { pair } = await keypair();
    const failing = (async () => new Response("nope", { status: 503 })) as unknown as typeof fetch;
    const verdict = await verifyActor(request(await mint(pair.privateKey)), env(), caller(), { fetchImpl: failing });
    expect(verdict).toMatchObject({ ok: false, reason: "could not retrieve the gateway's JWKS" });
  });

  it("still verifies across a rotation, when the assertion's kid is one of several published", async () => {
    const retiring = await keypair();
    const current = await keypair();
    const jwt = await mint(current.pair.privateKey, {}, { kid: "current" });
    const verdict = await verifyActor(request(jwt), env(), caller(), {
      fetchImpl: jwksFetch([{ ...retiring.publicJwk, kid: "retiring" }, { ...current.publicJwk, kid: "current" }]),
    });
    expect(verdict.ok).toBe(true);
  });
});

describe("the assertion is bound to the calling client", () => {
  it("accepts an assertion whose act.client_id is the calling client", async () => {
    const { pair, publicJwk } = await keypair();
    const verdict = await verifyActor(request(await mint(pair.privateKey)), env(), caller(), {
      fetchImpl: jwksFetch([publicJwk]),
    });
    expect(verdict.ok).toBe(true);
    expect(verdict.claims?.act?.client_id).toBe(CALLER);
  });

  it("REFUSES a validly-signed assertion minted for another client", async () => {
    // Signature, issuer, audience and lifetime all good — the only thing wrong
    // is who it says it is acting for. Before the binding this was accepted, and
    // `on_behalf_of` was recorded from it.
    const { pair, publicJwk } = await keypair();
    const jwt = await mint(pair.privateKey, { act: { client_id: "0oaAnotherWorkload" } });
    const verdict = await verifyActor(request(jwt), env(), caller(), {
      fetchImpl: jwksFetch([publicJwk]),
    });
    expect(verdict.ok).toBe(false);
    expect(verdict.reason).toContain("acting-client mismatch");
  });

  it("does not fetch the JWKS for a client-binding failure", async () => {
    // The binding is a claim check, so it must be decided before the network.
    const { pair, publicJwk } = await keypair();
    let fetched = 0;
    const jwt = await mint(pair.privateKey, { act: { client_id: "0oaAnotherWorkload" } });
    await verifyActor(request(jwt), env(), caller(), {
      fetchImpl: jwksFetch([publicJwk], () => (fetched += 1)),
    });
    expect(fetched).toBe(0);
  });
});

describe("an assertion is single-use", () => {
  it("REFUSES the same assertion presented twice", async () => {
    // The ~90s replay window: anyone who captured an assertion and also held a
    // valid bearer token could re-present it until it expired.
    const { pair, publicJwk } = await keypair();
    const shared = env(); // one KV, so the second call sees the first's marker
    const jwt = await mint(pair.privateKey);
    const deps = { fetchImpl: jwksFetch([publicJwk]) };

    const first = await verifyActor(request(jwt), shared, caller(), deps);
    expect(first.ok, first.reason).toBe(true);

    const second = await verifyActor(request(jwt), shared, caller(), deps);
    expect(second.ok).toBe(false);
    expect(second.reason).toContain("already been used");
  });

  it("still accepts a DIFFERENT assertion from the same client", async () => {
    // The marker is per-jti, not per-client: consuming one must not lock the
    // caller out for the rest of the TTL.
    const { pair, publicJwk } = await keypair();
    const shared = env();
    const deps = { fetchImpl: jwksFetch([publicJwk]) };
    expect((await verifyActor(request(await mint(pair.privateKey)), shared, caller(), deps)).ok).toBe(true);
    expect((await verifyActor(request(await mint(pair.privateKey)), shared, caller(), deps)).ok).toBe(true);
  });

  it("does NOT consume the jti when the signature fails to verify", async () => {
    // Ordering: marking before verifying would let anyone who can guess a jti
    // burn the legitimate holder's one use with an unsigned assertion.
    const mine = await keypair();
    const theirs = await keypair();
    const { kv, store } = makeKv();
    const shared = env({ OAUTH_KV: kv });

    const now = Math.floor(Date.now() / 1000);
    const jti = "shared-jti-under-attack";
    const forged = await mint(theirs.pair.privateKey, { jti, iat: now, exp: now + 60 });
    const bad = await verifyActor(request(forged), shared, caller(), {
      fetchImpl: jwksFetch([mine.publicJwk]),
    });
    expect(bad.ok).toBe(false);
    expect(store.size, "a forgery spent a replay marker").toBe(0);

    // ...and the real holder can still use that jti.
    const genuine = await mint(mine.pair.privateKey, { jti, iat: now, exp: now + 60 });
    const good = await verifyActor(request(genuine), shared, caller(), {
      fetchImpl: jwksFetch([mine.publicJwk]),
    });
    expect(good.ok, good.reason).toBe(true);
    expect(store.size).toBe(1);
  });

  it("keys the marker on a hash, so neither the issuer nor the jti is readable", async () => {
    const { pair, publicJwk } = await keypair();
    const { kv, store } = makeKv();
    const jwt = await mint(pair.privateKey, { jti: "distinctive-jti-value" });
    await verifyActor(request(jwt), env({ OAUTH_KV: kv }), caller(), {
      fetchImpl: jwksFetch([publicJwk]),
    });
    const key = [...store.keys()][0];
    expect(key).toMatch(/^gateway-actor-jti:[0-9a-f]{64}$/);
    expect(key).not.toContain("distinctive-jti-value");
  });

  it("never asks KV for a TTL below its 60-second floor", async () => {
    // KV refuses `expirationTtl` under 60 — verified against miniflare:
    // `400 Invalid expiration_ttl of 5. Expiration TTL must be at least 60.`
    // The library's EMA store uses the assertion's exact remaining life
    // (`Math.max(1, exp - now)`); the gateway mints 60-second assertions, so
    // copied verbatim that is under the floor for almost every one of them — a
    // 500 instead of a verdict. So the marker clamps, and this is the test that
    // says so.
    const { pair, publicJwk } = await keypair();
    const { kv, puts } = makeKv();
    const now = Math.floor(Date.now() / 1000);
    // Still valid (exp is in the future) but with well under 60s left.
    const jwt = await mint(pair.privateKey, { iat: now - 60, exp: now + 20 });
    const verdict = await verifyActor(request(jwt), env({ OAUTH_KV: kv }), caller(), {
      fetchImpl: jwksFetch([publicJwk]),
    });
    expect(verdict.ok, verdict.reason).toBe(true);
    expect(puts).toHaveLength(1);
    expect(puts[0].ttl).toBeGreaterThanOrEqual(60);
  });

  it("keeps the marker for the whole window in which the assertion is ACCEPTED", async () => {
    // The window that matters is the ACCEPTANCE window, not the validity window.
    // checkClaims admits an assertion while `exp + MAX_CLOCK_SKEW_SECONDS >= now`,
    // so a marker whose TTL was `exp - now` expired AT exp and left the skew
    // allowance uncovered: with the minter's 60s lifetime, a captured assertion
    // replayed exactly once, 60-90s in. The floor test above cannot see this —
    // it uses a nearly-spent assertion, where the 60s clamp happens to cover the
    // hole — so this one uses a FRESH 60s assertion, which is what the gateway
    // actually sends and the case that was broken.
    const { pair, publicJwk } = await keypair();
    const { kv, puts } = makeKv();
    const now = Math.floor(Date.now() / 1000);
    const exp = now + 60;
    const jwt = await mint(pair.privateKey, { iat: now, exp });
    const verdict = await verifyActor(request(jwt), env({ OAUTH_KV: kv }), caller(), {
      fetchImpl: jwksFetch([publicJwk]),
      now: () => now * 1000,
    });
    expect(verdict.ok, verdict.reason).toBe(true);
    // 30 is MAX_CLOCK_SKEW_SECONDS in src/actor.ts. Asserted as exp + skew
    // rather than a literal 90 so the two move together.
    expect(puts[0].ttl).toBeGreaterThanOrEqual(exp + 30 - now);
  });

  it("reports a KV failure as itself, not as a signature failure", async () => {
    // The consume used to sit inside the key-import try/catch, whose catch means
    // "that key would not import, try the next one". A KV throw fell into it and
    // came back as `signature did not verify` — a storage outage that reads as
    // key rotation. Still fails closed; it just says what happened.
    const { pair, publicJwk } = await keypair();
    const exploding = {
      get: async () => null,
      put: async () => {
        throw new Error("KV PUT failed: 429 Too Many Requests");
      },
    } as unknown as KVNamespace;
    const verdict = await verifyActor(
      request(await mint(pair.privateKey)),
      env({ OAUTH_KV: exploding }),
      caller(),
      { fetchImpl: jwksFetch([publicJwk]) },
    );
    expect(verdict.ok).toBe(false);
    expect(verdict.reason).toContain("could not record single use");
    expect(verdict.reason).not.toContain("signature");
  });

  it("keeps a long-lived assertion's marker for as long as the assertion lasts", async () => {
    // The other half: the clamp is a FLOOR, not a fixed value. A marker that
    // expired before its assertion would reopen the replay window it closes.
    const { pair, publicJwk } = await keypair();
    const { kv, puts } = makeKv();
    const now = Math.floor(Date.now() / 1000);
    const jwt = await mint(pair.privateKey, { iat: now, exp: now + 600 });
    await verifyActor(request(jwt), env({ OAUTH_KV: kv }), caller(), {
      fetchImpl: jwksFetch([publicJwk]),
    });
    expect(puts[0].ttl).toBeGreaterThanOrEqual(600);
  });

  it("fails closed when there is no KV namespace to record single use", async () => {
    const { pair, publicJwk } = await keypair();
    const verdict = await verifyActor(
      request(await mint(pair.privateKey)),
      env({ OAUTH_KV: undefined }),
      caller(),
      { fetchImpl: jwksFetch([publicJwk]) },
    );
    expect(verdict.ok).toBe(false);
    expect(verdict.reason).toContain("replay protection");
  });
});

describe("gateOnActor", () => {
  // The enforcement policy, testable in Node precisely because it lives in this
  // file rather than index.ts. F-1's sharpest lesson: the verifier used to be
  // exported and never imported, so nothing enforced anything.

  it("refuses 403 when the flag is on and no assertion is presented", async () => {
    const { refusal, actor } = await gateOnActor(
      request(),
      env({ REQUIRE_GATEWAY_ACTOR: "true" }),
      caller(),
      { fetchImpl: jwksFetch([]) },
    );
    expect(actor).toBeUndefined();
    expect(refusal?.status).toBe(403);
    expect(await refusal!.text()).toContain("X-MCP-Actor");
  });

  it("degrades a BAD assertion to plain M2M when the flag is off — never to an actor", async () => {
    // A bogus assertion must not become `+actor` attribution nothing validated.
    const mine = await keypair();
    const theirs = await keypair();
    const forged = await mint(theirs.pair.privateKey);
    const { refusal, actor } = await gateOnActor(request(forged), env(), caller(), {
      fetchImpl: jwksFetch([mine.publicJwk]),
    });
    expect(refusal).toBeUndefined();
    expect(actor).toBeUndefined();
  });

  it("returns the verified actor whether or not the flag is on", async () => {
    const { pair, publicJwk } = await keypair();
    const { actor } = await gateOnActor(request(await mint(pair.privateKey)), env(), caller(), {
      fetchImpl: jwksFetch([publicJwk]),
    });
    expect(actor?.sub).toBe("00uHuman");
  });
});

describe("actorRequired", () => {
  it("is off unless explicitly turned on", () => {
    expect(actorRequired(env())).toBe(false);
    expect(actorRequired(env({ REQUIRE_GATEWAY_ACTOR: "false" }))).toBe(false);
    expect(actorRequired(env({ REQUIRE_GATEWAY_ACTOR: "TRUE" }))).toBe(false);
    expect(actorRequired(env({ REQUIRE_GATEWAY_ACTOR: "true" }))).toBe(true);
  });

  it("accepts the unquoted TOML boolean, not only the string (F-2)", () => {
    // `REQUIRE_GATEWAY_ACTOR = true` in wrangler.toml arrives as a real
    // boolean. The old `=== "true"` compare made that configuration silently
    // fail OPEN during the exact cutover where the operator turned the gate on.
    expect(actorRequired(env({ REQUIRE_GATEWAY_ACTOR: true } as never))).toBe(true);
    expect(actorRequired(env({ REQUIRE_GATEWAY_ACTOR: false } as never))).toBe(false);
  });
});
