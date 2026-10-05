import { describe, expect, it } from "vitest";

import {
  SCOPE_HELP,
  clientIdAsUrl,
  redirectHostOf,
  renderConsent,
  renderDenied,
} from "../src/consent";
import { SUPPORTED_SCOPES } from "../src/scopes";
import { mapEmaClaims, parseTrustedIssuers, trustedIssuerResolver } from "../src/ema";

const view = {
  nonce: "11111111-1111-1111-1111-111111111111",
  csrf: "22222222-2222-2222-2222-222222222222",
  clientName: "Claude",
  redirectHost: "claude.ai",
  clientIdUrl: null as string | null,
  email: "a@example.com",
  scopes: ["REPLACE:read", "REPLACE:write"],
  resourceName: "REPLACE-WITH-RESOURCE-NAME",
};

describe("consent page", () => {
  it("always shows the redirect host, which is part of what the decision is keyed on", async () => {
    const html = await renderConsent(view).text();
    expect(html).toContain("claude.ai");
    expect(html).toContain("Will receive the authorization at");
  });

  it("shows the self-reported client name but caps its length so it cannot crowd out the host", async () => {
    const html = await renderConsent({ ...view, clientName: "X".repeat(500) }).text();
    const shown = html.match(/X+/)?.[0] ?? "";
    expect(shown.length).toBeLessThanOrEqual(80);
    expect(html).toContain("claude.ai");
  });

  it("escapes an attacker-supplied client name rather than rendering markup", async () => {
    const html = await renderConsent({
      ...view,
      clientName: '<img src=x onerror=alert(1)>"',
    }).text();
    expect(html).not.toContain("<img");
    expect(html).toContain("&lt;img");
  });

  it("escapes a hostile redirect host too", async () => {
    const html = await renderConsent({ ...view, redirectHost: '</dd><script>x</script>' }).text();
    expect(html).not.toContain("<script>");
  });

  it("carries a CSP that forbids framing and inline script", async () => {
    const res = renderConsent(view);
    const csp = res.headers.get("content-security-policy") ?? "";
    expect(csp).toContain("frame-ancestors 'none'");
    expect(csp).toContain("default-src 'none'");
    // form-action is deliberately absent — approval 302s cross-origin.
    expect(csp).not.toContain("form-action");
    expect(res.headers.get("cache-control")).toBe("no-store");
  });

  it("names each scope with what it actually permits", async () => {
    // Asserts against SCOPE_HELP itself, so customizing the prose cannot break
    // CI. The property is that the page renders the HELP TEXT and not merely the
    // scope identifier — a consent page listing bare scope names tells a user
    // nothing about what they are agreeing to.
    const html = await renderConsent({ ...view, scopes: SUPPORTED_SCOPES }).text();
    for (const scope of SUPPORTED_SCOPES) {
      expect(html).toContain(scope);
      const help = SCOPE_HELP[scope];
      expect(help, `SCOPE_HELP has no entry for ${scope}`).toBeTypeOf("string");
      // esc() escapes and strips, so compare on a plain-text prefix rather than
      // the whole string, which may contain characters the page rewrites.
      expect(html).toContain(help.split(/[<>&"']/)[0].slice(0, 40));
    }
  });

  it("does not let invisible padding SUPPRESS a real client name", async () => {
    // The reason stripping happens BEFORE the 80-char slice, and the direction
    // that matters: slicing first cut this name at 80 zero-width characters,
    // stripped that to nothing, and rendered NO row — hiding a name the user
    // should have been shown. (An all-invisible name rendered no row under
    // either order, because the template guard is `${name ? ... : ""}` and an
    // empty string is falsy, so that case proves nothing about the order.)
    const html = await renderConsent({
      ...view,
      clientName: "\u200B".repeat(80) + "Evil Corp",
    }).text();
    expect(html).toContain("Evil Corp");
    expect(html).toContain("Application (self-reported)");
    expect(html).not.toContain("\u200B");
  });

  it("strips the invisible characters added after the first pass", async () => {
    // U+00AD (soft hyphen), U+2028/U+2029 (line/paragraph separator), U+061C
    // (Arabic letter mark) and U+180E (Mongolian vowel separator) all render as
    // nothing, survive HTML escaping, and were NOT in the original class — so a
    // name could be split or padded with them and still read as something else.
    // U+2028 and U+2029 also terminate a line for some readers and log viewers.
    const html = await renderConsent({
      ...view,
      clientName: "Ev\u00ADil\u2028 Co\u061Crp\u180E\u2029",
    }).text();
    for (const ch of ["\u00AD", "\u2028", "\u2029", "\u061C", "\u180E"]) {
      expect(html).not.toContain(ch);
    }
    // ...and the name itself survives, reassembled.
    expect(html).toContain("Evil Corp");
  });

  it("survives a scope name that is an Object.prototype member", async () => {
    // `SCOPE_HELP[scope] ?? fallback` returned the INHERITED function for these,
    // so `??` never fired and esc() threw "s.replace is not a function" — a 500
    // instead of a consent page. /authorize rejects unknown scopes first, but
    // this is the belt to that braces and must not itself break.
    for (const evil of ["toString", "constructor", "__proto__", "valueOf", "hasOwnProperty"]) {
      const html = await renderConsent({ ...view, scopes: [evil] }).text();
      expect(html).toContain("Access granted by this scope");
      expect(html).not.toContain("native code");
    }
  });

  it("discloses that the decision is remembered, for how long, and what re-prompts", async () => {
    // Consent is the only control on this server, so the page has to say what
    // approving actually commits to: it is not a one-time grant, it lasts 90
    // days, and it is scoped to this destination and these permissions.
    const html = await renderConsent(view).text();
    expect(html).toContain("remembered for this application");
    expect(html).toContain("up to 90 days");
    expect(html).toContain("moving the destination");
  });

  it("says \"any port\" for a loopback destination, because the key ignores the port", async () => {
    // The remembered decision is keyed WITHOUT the port for loopback — it has to
    // be, since the provider matches loopback URIs that way — so showing one
    // ephemeral port would describe a narrower grant than the one being given.
    expect(redirectHostOf("http://127.0.0.1:51111/cb")).toBe("127.0.0.1 (any port)");
    expect(redirectHostOf("http://localhost:3000/cb")).toBe("localhost (any port)");
    const html = await renderConsent({
      ...view,
      redirectHost: redirectHostOf("http://127.0.0.1:51111/cb"),
    }).text();
    expect(html).toContain("127.0.0.1 (any port)");
    expect(html).not.toContain("51111");
  });

  it("still shows host:port for a non-loopback destination, where the key is exact", () => {
    expect(redirectHostOf("https://client.example:8443/cb")).toBe("client.example:8443");
  });

  it("posts the nonce and csrf back to /consent", async () => {
    const html = await renderConsent(view).text();
    expect(html).toContain('action="/consent"');
    expect(html).toContain(view.nonce);
    expect(html).toContain(view.csrf);
    expect(html).toContain('value="approve"');
    expect(html).toContain('value="deny"');
  });

  it("shows the CIMD document URL when the client id is one", async () => {
    const html = await renderConsent({
      ...view,
      clientIdUrl: "https://app.example.com/client.json",
    }).text();
    expect(html).toContain("https://app.example.com/client.json");
    expect(html).toContain("Client identity document");
  });

  it("renders a denial page that issues nothing", async () => {
    const res = renderDenied();
    expect(res.status).toBe(200);
    expect(await res.text()).toContain("Access denied");
  });
});

describe("clientIdAsUrl", () => {
  it("recognises an HTTPS CIMD client id", () => {
    expect(clientIdAsUrl("https://app.example.com/c.json")).toBe("https://app.example.com/c.json");
  });

  it("returns null for an opaque DCR id or a non-HTTPS URL", () => {
    expect(clientIdAsUrl("bqoslBV1jWPa3kDu")).toBeNull();
    expect(clientIdAsUrl("http://app.example.com/c.json")).toBeNull();
    expect(clientIdAsUrl("javascript:alert(1)")).toBeNull();
  });
});

describe("redirectHostOf", () => {
  it("extracts the host, and falls back to the raw value when unparseable", () => {
    expect(redirectHostOf("https://claude.ai/api/mcp/auth_callback")).toBe("claude.ai");
    // Loopback is the exception, and deliberately so — see the "any port" test
    // above: the key ignores the port there, so the page must not imply it does
    // not.
    expect(redirectHostOf("http://127.0.0.1:3000/cb")).toBe("127.0.0.1 (any port)");
    expect(redirectHostOf("not a url")).toBe("not a url");
  });
});

describe("EMA trusted issuers", () => {
  it("parses issuer=jwksUri pairs", () => {
    const out = parseTrustedIssuers(
      "https://idp.example.com=https://idp.example.com/keys https://b.example=https://b.example/keys",
    );
    expect(out).toHaveLength(2);
    expect(out[0]).toEqual({
      issuer: "https://idp.example.com",
      jwksUri: "https://idp.example.com/keys",
      algorithms: ["RS256"],
    });
  });

  it("is empty when unset, which leaves the ID-JAG grant disabled", () => {
    expect(parseTrustedIssuers(undefined)).toEqual([]);
    expect(parseTrustedIssuers("")).toEqual([]);
  });

  it("THROWS rather than silently trusting fewer issuers than configured", () => {
    // A typo must not shrink the trust set quietly: dropping one pair of three
    // leaves a partial set, and dropping the only pair disables the grant.
    expect(() => parseTrustedIssuers("https://a.example=http://169.254.169.254/latest")).toThrow();
    expect(() => parseTrustedIssuers("https://a.example=file:///etc/passwd")).toThrow();
    expect(() => parseTrustedIssuers("https://a.example=notaurl")).toThrow();
    expect(() => parseTrustedIssuers("noequals")).toThrow();
    expect(() => parseTrustedIssuers("=https://a.example/keys")).toThrow();
    // one good, one bad
    expect(() =>
      parseTrustedIssuers("https://a.example=https://a.example/keys https://b.example=nope"),
    ).toThrow();
  });

  it("requires the JWKS to live on the issuer's own origin", () => {
    // A bare https: check stops nothing — the library's JWKS fetch follows
    // redirects with no host allowlist. Same-origin is the real constraint.
    expect(() => parseTrustedIssuers("https://a.example=https://evil.example/keys")).toThrow();
    expect(parseTrustedIssuers("https://a.example=https://a.example/oauth2/v1/keys")).toHaveLength(1);
  });

  it("resolves only an exact issuer match", async () => {
    const resolve = trustedIssuerResolver(
      parseTrustedIssuers("https://idp.example.com=https://idp.example.com/keys"),
    );
    expect(await resolve({ iss: "https://idp.example.com" } as never)).not.toBeNull();
    expect(await resolve({ iss: "https://idp.example.com.evil" } as never)).toBeNull();
    expect(await resolve({ iss: "https://other.example" } as never)).toBeNull();
  });
});

describe("EMA claim mapping", () => {
  const input = (over: Record<string, unknown> = {}) =>
    ({
      claims: {
        sub: "00uEnterprise",
        email: "e@corp.example",
        iss: "https://idp.corp.example",
        scope: "REPLACE:read",
      },
      clientInfo: { clientId: "https://app.example/c.json" },
      requestedScope: ["REPLACE:read"],
      resource: "https://replace.{{domain_suffix}}/mcp",
      ...over,
    }) as never;

  it("uses sub as the primary identifier and email as the label", async () => {
    const out = await mapEmaClaims(input());
    expect(out?.userId).toBe("00uEnterprise");
    expect(out?.metadata).toEqual({ label: "e@corp.example" });
  });

  it("falls back to sub when the assertion carries no email", async () => {
    const out = await mapEmaClaims(
      input({
        claims: { sub: "00uNoEmail", iss: "https://idp.corp.example", scope: "REPLACE:read" },
      }),
    );
    expect(out?.metadata).toEqual({ label: "00uNoEmail" });
    expect((out?.props as Record<string, unknown>).email).toBe("00uNoEmail");
  });

  it("denies an assertion with no subject", async () => {
    expect(
      await mapEmaClaims(input({ claims: { iss: "https://idp.corp.example", scope: "REPLACE:read" } })),
    ).toBeNull();
  });

  it("denies a subject containing ':', which the library would reject anyway", async () => {
    const out = await mapEmaClaims(
      input({ claims: { sub: "a:b", iss: "https://i.example", scope: "REPLACE:read" } }),
    );
    expect(out).toBeNull();
  });

  it("marks the grant as enterprise, so it is distinguishable from a consented one", async () => {
    const props = (await mapEmaClaims(input()))?.props as Record<string, unknown>;
    expect(props.enterprise).toBe(true);
    expect(props.enterprise_issuer).toBe("https://idp.corp.example");
  });

  it("takes scopes from the ASSERTION, not from what the client requested", async () => {
    // The escalation this prevents: the library only downscopes `requestedScope`
    // to the assertion when the assertion HAS a scope claim
    // (oauth-provider.js:1159, `if (assertionScopes.length > 0)`), so a
    // scope-less ID-JAG would otherwise let the client name REPLACE:write itself.
    const out = await mapEmaClaims(
      input({
        claims: { sub: "s", iss: "https://i.example", scope: "REPLACE:read" },
        requestedScope: ["REPLACE:read", "REPLACE:write"],
      }),
    );
    expect(out?.scope).toEqual(["REPLACE:read"]);
  });

  it("DENIES an assertion carrying no scope claim at all", async () => {
    const out = await mapEmaClaims(
      input({ claims: { sub: "s", iss: "https://i.example" }, requestedScope: ["REPLACE:write"] }),
    );
    expect(out).toBeNull();
  });

  it("denies an empty scope claim", async () => {
    const out = await mapEmaClaims(
      input({ claims: { sub: "s", iss: "https://i.example", scope: "   " } }),
    );
    expect(out).toBeNull();
  });

  it("drops asserted scopes this server does not implement", async () => {
    const out = await mapEmaClaims(
      input({
        claims: { sub: "s", iss: "https://i.example", scope: "REPLACE:read other:read admin:everything" },
        requestedScope: ["REPLACE:read", "other:read", "admin:everything"],
      }),
    );
    expect(out?.scope).toEqual(["REPLACE:read"]);
  });

  it("DENIES when the assertion names nothing this server implements", async () => {
    // Previously this granted REPLACE:read as a "usable default" — access the IdP
    // never authorized.
    const out = await mapEmaClaims(
      input({
        claims: { sub: "s", iss: "https://i.example", scope: "other:thing" },
        requestedScope: ["other:thing"],
      }),
    );
    expect(out).toBeNull();
  });

  it("lets a client take LESS than the assertion allows, but never more", async () => {
    const out = await mapEmaClaims(
      input({
        claims: { sub: "s", iss: "https://i.example", scope: "REPLACE:read REPLACE:write" },
        requestedScope: ["REPLACE:read"],
      }),
    );
    expect(out?.scope).toEqual(["REPLACE:read"]);
  });

  it("keeps grant scope and props scopes identical", async () => {
    const out = await mapEmaClaims(
      input({
        claims: { sub: "s", iss: "https://i.example", scope: "REPLACE:read REPLACE:write" },
        requestedScope: ["REPLACE:read", "REPLACE:write"],
      }),
    );
    expect(out?.scope).toEqual(["REPLACE:read", "REPLACE:write"]);
    expect((out?.props as Record<string, unknown>).scopes).toEqual(out?.scope);
  });

  it("carries no upstream Okta token, because this path never receives one", async () => {
    const props = (await mapEmaClaims(input()))?.props as Record<string, unknown>;
    expect(props.okta_access_token).toBe("");
  });
});
