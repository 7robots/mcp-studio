// Tests for the local JWKS validation path (src/jwt.ts).
//
// Signature/issuer/audience/expiry are jose's job and are verified before
// decideFromClaims runs, so these cover the parts we own: scope policy, claim
// extraction across the two shapes Okta issuers use, and the enable/disable
// switch that keeps production on the old path.

import { describe, expect, it } from "vitest";

import { decideFromClaims, extractScopes, tryOktaJwt } from "../src/jwt";

describe("extractScopes", () => {
  it("reads Okta's scp array", () => {
    expect(extractScopes({ scp: ["REPLACE:read", "mcp:write"] })).toEqual([
      "REPLACE:read",
      "mcp:write",
    ]);
  });

  it("reads a space-delimited scope string", () => {
    expect(extractScopes({ scope: "REPLACE:read  mcp:write" })).toEqual([
      "REPLACE:read",
      "mcp:write",
    ]);
  });

  it("returns empty when neither claim is present", () => {
    expect(extractScopes({})).toEqual([]);
  });

  it("ignores non-string entries in scp", () => {
    expect(extractScopes({ scp: ["REPLACE:read", 42, null] as never })).toEqual([
      "REPLACE:read",
    ]);
  });
});

describe("decideFromClaims", () => {
  // A real Cross App Access token: sub is the human, cid is the agent.
  const xaaClaims = {
    sub: "{{test_email}}",
    cid: "wlp15gji2sd4CBF3I698",
    scp: ["REPLACE:read"],
    exp: 4102444800,
  };

  it("accepts a token carrying the required scope", () => {
    const v = decideFromClaims(xaaClaims, "REPLACE:read");
    expect(v.active).toBe(true);
    expect(v.claims).toEqual({
      client_id: "wlp15gji2sd4CBF3I698",
      scope: "REPLACE:read",
      sub: "{{test_email}}",
      exp: 4102444800,
    });
  });

  // The whole point of Cross App Access: attribution to the user/agent pair,
  // not to either alone.
  it("carries both the human (sub) and the agent (cid) through", () => {
    const v = decideFromClaims(xaaClaims, "REPLACE:read");
    expect(v.claims?.sub).toBe("{{test_email}}");
    expect(v.claims?.client_id).toBe("wlp15gji2sd4CBF3I698");
  });

  // The negative test the demo relies on: the managed connection grants
  // REPLACE:read only, so a token can never carry mcp:write.
  it("rejects when the required scope is absent", () => {
    expect(decideFromClaims(xaaClaims, "mcp:write").active).toBe(false);
  });

  it("does not substring-match scopes", () => {
    expect(decideFromClaims({ scp: ["REPLACE:readonly"] }, "REPLACE:read").active).toBe(
      false,
    );
  });

  // A token without cid is unattributable: claims.client_id becomes
  // AuthInfo.clientId downstream, so this now mirrors the introspection
  // path's client_id requirement and rejects.
  it("rejects a scoped token without cid", () => {
    const v = decideFromClaims({ sub: "u", scp: ["REPLACE:read"] }, "REPLACE:read");
    expect(v.active).toBe(false);
    expect(v.claims).toBeUndefined();
  });
});

describe("tryOktaJwt — enable switch", () => {
  it("is disabled when OKTA_M2M_AUDIENCE is unset, so production is untouched", async () => {
    const env = {
      OKTA_ISSUER: "https://org.okta.com/oauth2/default",
    } as Env;
    await expect(tryOktaJwt("any.token.here", env)).resolves.toBeNull();
  });

  it("returns null rather than throwing on a non-JWT", async () => {
    const env = {
      OKTA_ISSUER: "https://org.okta.com/oauth2/aus1",
      OKTA_M2M_AUDIENCE: "https://example.test",
    } as Env;
    await expect(tryOktaJwt("not-a-jwt", env)).resolves.toBeNull();
  });

  // src/m2m.ts short-circuits `${userId}:${grantId}:${secret}` tokens before
  // introspection. This path runs first in src/index.ts, so pin that it also
  // declines them — a Worker-issued token must reach OAuthProvider, and the
  // JWKS path must not be the thing that claims it.
  it("declines a Worker-issued token shape even when enabled", async () => {
    const env = {
      OKTA_ISSUER: "https://org.okta.com/oauth2/aus1",
      OKTA_M2M_AUDIENCE: "https://example.test",
    } as Env;
    await expect(tryOktaJwt("user123:grant456:secret789", env)).resolves.toBeNull();
  });
});
