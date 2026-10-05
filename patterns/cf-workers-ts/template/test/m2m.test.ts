// The two bearer helpers that survived the move to local JWT verification.
//
// This file used to test the introspection machinery — decide(), resolveCacheTtl()
// and tryOktaM2M() with its KV cache — all of which is gone (see the header of
// src/m2m.ts for why). The properties those tests covered (scope policy, expiry,
// issuer pinning) now live in test/jwt.test.ts, asserted against the verification
// path that actually runs.

import { describe, expect, it } from "vitest";

import { extractBearer, looksWorkerIssued } from "../src/m2m";

const req = (headers: Record<string, string> = {}) =>
  new Request("https://replace.{{domain_suffix}}/mcp", { method: "POST", headers });

describe("extractBearer", () => {
  it("reads a bearer token", () => {
    expect(extractBearer(req({ authorization: "Bearer abc.def.ghi" }))).toBe("abc.def.ghi");
  });

  it("is case-insensitive on the scheme, as RFC 6750 requires", () => {
    expect(extractBearer(req({ authorization: "bearer abc" }))).toBe("abc");
    expect(extractBearer(req({ authorization: "BEARER abc" }))).toBe("abc");
  });

  it("trims surrounding whitespace rather than passing it to the verifier", () => {
    expect(extractBearer(req({ authorization: "Bearer   abc   " }))).toBe("abc");
  });

  it("returns null for a missing or non-Bearer header", () => {
    expect(extractBearer(req())).toBeNull();
    expect(extractBearer(req({ authorization: "Basic dXNlcjpwYXNz" }))).toBeNull();
    expect(extractBearer(req({ authorization: "Bearer" }))).toBeNull();
  });
});

describe("looksWorkerIssued", () => {
  it("recognises this Worker's own token format", () => {
    // `${userId}:${grantId}:${secret}` — three colon-separated parts.
    expect(looksWorkerIssued("user123:grant456:secret789")).toBe(true);
  });

  it("does not claim a JWT", () => {
    // A JWT has two dots and no colons. Misclassifying one would send every M2M
    // caller down the interactive path and 401 them.
    expect(looksWorkerIssued("eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJhIn0.sig")).toBe(false);
  });

  it("does not claim an opaque token with the wrong number of parts", () => {
    expect(looksWorkerIssued("two:parts")).toBe(false);
    expect(looksWorkerIssued("four:parts:here:now")).toBe(false);
    expect(looksWorkerIssued("nocolons")).toBe(false);
  });

  it("is why an interactive token never reaches jwtVerify", () => {
    // Not an optimization: an interactive token IS NOT a JWT, so verification
    // would fail on every authenticated interactive request. This short-circuit
    // is the difference between a cheap reject and a wasted verification per call.
    const interactive = "00uAbc:grantXyz:s3cr3t";
    expect(looksWorkerIssued(interactive)).toBe(true);
  });
});
