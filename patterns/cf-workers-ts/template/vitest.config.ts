import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

// Two projects, because they test different things and cannot share a runtime.
//
//   unit     — pure helpers, plain Node. Fast, and most of the suite.
//   workerd  — the actual routes, inside workerd via miniflare, so `SELF.fetch()`
//              exercises src/index.ts and src/auth.ts with real bindings.
//
// The workerd project exists because those two files are importable in NO Node
// test: Hono and @cloudflare/workers-oauth-provider pull in `cloudflare:`
// modules. That left the helpers well covered and the FLOW untested — a
// regression that stopped calling takeState entirely would not have failed
// anything. Route-level tests are the only thing that catches that.
export default defineConfig({
  test: {
    projects: [
      {
        test: {
          name: "unit",
          include: ["test/**/*.test.ts"],
          exclude: ["test/**/*.workerd.test.ts"],
        },
      },
      {
        plugins: [
          cloudflareTest({
            // Miniflare reads the real wrangler.toml, so the bindings, vars and
            // compatibility flags under test are the deployed ones.
            wrangler: { configPath: "./wrangler.toml" },
            miniflare: {
              // Secrets are correctly absent from wrangler.toml, so supply
              // obvious placeholders. Nothing reaches real Okta — see
              // outboundService below.
              bindings: {
                OKTA_CLIENT_SECRET: "test-client-secret",
                COOKIE_ENCRYPTION_KEY: "test-cookie-key",
                // 32+ bytes, or deletionCodec() returns null and the MRTR path is
                // never exercised — which is how inverting a destructive tool's
                // behaviour can pass a whole suite.
                REQUEST_STATE_KEY: "test-request-state-key-at-least-32-bytes-long",
                // Commented in the template's wrangler.toml (opt-in), bound here
                // so the matrix suite exercises the full actor chain.
                GATEWAY_ISSUER: "https://{{gateway_host}}",
                GATEWAY_JWKS_URL: "https://{{gateway_host}}/.well-known/jwks.json",
              },
              // Every fetch the Worker makes is answered here, so the flow
              // tests never touch the network. Stubbing `globalThis.fetch` from
              // the test would NOT work: SELF.fetch runs the Worker in its own
              // isolate, so the stub would apply to the wrong one.
              //
              // The stub keys off the request, which lets one static handler
              // serve every case: a code of `bad-code` makes Okta reject the
              // exchange, and `no-sub` returns a userinfo document with no
              // subject, so the fail-closed branches are reachable.
              outboundService: (request: Request) => {
                const url = new URL(request.url);
                if (url.pathname.endsWith("/v1/token")) {
                  return request.text().then((body) => {
                    if (body.includes("code=bad-code")) {
                      return Response.json(
                        { error: "invalid_grant", error_description: "code is invalid" },
                        { status: 400 },
                      );
                    }
                    const sub = body.includes("code=no-sub") ? "" : "00uTestUser";
                    // RFC 6749 §3.3 lets an AS report a NARROWER granted scope
                    // instead of erroring. This code makes that reachable.
                    const scope = body.includes("code=downgrade-all")
                      ? "openid profile email offline_access"
                      : undefined;
                    return Response.json({
                      access_token: `okta-access-token-for-${sub || "nobody"}`,
                      token_type: "Bearer",
                      expires_in: 3600,
                      ...(scope ? { scope } : {}),
                    });
                  });
                }
                if (url.pathname.endsWith("/v1/userinfo")) {
                  const auth = request.headers.get("authorization") ?? "";
                  if (auth.includes("for-nobody")) return Response.json({});
                  return Response.json({
                    sub: "00uTestUser",
                    email: "tester@example.com",
                    name: "Test User",
                  });
                }
                // Same fixed key as the fake enterprise IdP below — deliberately,
                // because it means one key could sign either an access token or
                // an ID-JAG, which is exactly the confusion jwt.ts's typ guard
                // exists to refuse.
                if (url.pathname.endsWith("/v1/keys")) {
                  return Response.json({ keys: [{"kty": "RSA", "n": "rMZAa31krtgsQWkLjzOcLN9q_cRzdvNidE_yIN3m4dCYxWrGLk84XMTTx6OalWAQnNnXgqSVCMf4xJvUZs4ERBIDXiILX_G5xlFUODYMOOkuDzhSFS_0AxdTk4ada9hwO2MKDlY0RqQcz7Pw1wdZLmjeh-4WpP3EFaAEdBr8c7ZBXKbJ0lhCmtmHd6uC_Nl4v8EakutnbO-ANr4GPV9H8rxiBtQkax2s6htSwOGSj93AGGCKuZ9aUHapZB8p7u45xJuUvr6Q-1SUVI5e7z8m1fZZefkULKEnn8SRxUoMJRQZ0T-YiQU9Ijg55qT8ae8G-03pbhq8k_sKoaopp7xWiQ", "e": "AQAB", "kid": "ema-test-key", "alg": "RS256", "use": "sig"}] });
                }

                // The fake GATEWAY's JWKS, for the matrix suite's X-MCP-Actor
                // scenarios. Ed25519 — deliberately a different algorithm AND a
                // different key from the RSA one above, so an actor assertion
                // can never verify against the IdP key or vice versa.
                if (url.hostname === "{{gateway_host}}" && url.pathname === "/.well-known/jwks.json") {
                  return Response.json({ keys: [{ kty: "OKP", crv: "Ed25519", x: "jeYDokDz3cqVljYqaBtW3gDZx5C2Fjef8qeYAI7kVfs", kid: "matrix-gw-key", alg: "EdDSA", use: "sig" }] });
                }

                return new Response(`unexpected outbound fetch: ${request.url}`, { status: 599 });
              },
            },
          }),
        ],
        test: {
          name: "workerd",
          include: ["test/**/*.workerd.test.ts"],
        },
      },
    ],
  },
});
