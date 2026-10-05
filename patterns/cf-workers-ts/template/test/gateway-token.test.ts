import { SignJWT, createLocalJWKSet, exportJWK, generateKeyPair } from "jose";
import { describe, expect, it } from "vitest";

import { bearerOf, verifyGatewayToken } from "../src/gateway-token";

const ISS = "https://{{gateway_host}}";
const AUD = "https://x.{{domain_suffix}}/mcp";

async function setup() {
  const { publicKey, privateKey } = await generateKeyPair("EdDSA", { crv: "Ed25519" });
  const jwk = { ...(await exportJWK(publicKey)), kid: "k1", alg: "EdDSA" };
  const jwks = createLocalJWKSet({ keys: [jwk] });
  const sign = (claims: Record<string, unknown> = {}, typ = "at+jwt") =>
    new SignJWT({ sub_type: "user", client_id: "mcp-client", scope: "x:use", jti: "j1", ...claims })
      .setProtectedHeader({ alg: "EdDSA", typ, kid: "k1" })
      .setIssuer(ISS)
      .setAudience(AUD)
      .setSubject("00uPerson")
      .setIssuedAt()
      .setExpirationTime("5m")
      .sign(privateKey);
  const opts = { issuer: ISS, audience: AUD, requiredScope: "x:use", jwks };
  return { sign, opts };
}

describe("verifyGatewayToken", () => {
  it("accepts a gateway token for this server and returns its caller", async () => {
    const { sign, opts } = await setup();
    expect(await verifyGatewayToken(await sign(), opts)).toEqual({
      ok: true,
      claims: { sub: "00uPerson", subType: "user", clientId: "mcp-client", scopes: ["x:use"], jti: "j1" },
    });
  });

  it("refuses the actor assertion's typ, another audience, another issuer, and a missing scope", async () => {
    const { sign, opts } = await setup();
    expect((await verifyGatewayToken(await sign({}, "JWT"), opts)).ok).toBe(false);
    expect((await verifyGatewayToken(await sign(), { ...opts, audience: "https://y.{{domain_suffix}}/mcp" })).ok).toBe(false);
    expect((await verifyGatewayToken(await sign(), { ...opts, issuer: "https://evil.example" })).ok).toBe(false);
    expect((await verifyGatewayToken(await sign({ scope: "other" }), opts)).ok).toBe(false);
    expect((await verifyGatewayToken(await sign({ sub_type: "admin" }), opts)).ok).toBe(false);
    expect((await verifyGatewayToken(null, opts)).ok).toBe(false);
  });

  it("refuses a token signed by a key not in the JWKS", async () => {
    const { opts } = await setup();
    const other = await setup();
    expect((await verifyGatewayToken(await other.sign(), opts)).ok).toBe(false);
  });

  it("reads the bearer from the request", () => {
    expect(bearerOf(new Request("https://x", { headers: { authorization: "Bearer abc" } }))).toBe("abc");
    expect(bearerOf(new Request("https://x"))).toBeNull();
  });
});
