// The canonical resource identity comes from ONE var (PUBLIC_MCP_URL in
// wrangler.toml), so the advertised `resourceMetadata.resource` and the
// `/.well-known/oauth-protected-resource` URL a client is pointed at cannot drift
// apart. This is the only place that arithmetic is testable: src/index.ts
// transitively imports `cloudflare:` modules and will not load under the
// plain-Node pool.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { FALLBACK_PUBLIC_MCP_URL, resourceUrls } from "../src/resource";

// Reads wrangler.toml as text: it is data no other test would look at, and
// advertising a workers.dev URL as the canonical resource is a silent, CI-green
// production mistake.
const read = (rel: string) =>
  readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", rel), "utf8");

describe("resourceUrls — RFC 8707 / RFC 9728 identifiers", () => {
  const CANONICAL = "https://replace.{{domain_suffix}}/mcp";

  it("puts the well-known segment before the resource path, per RFC 9728 §3.1", () => {
    expect(resourceUrls(CANONICAL)).toEqual({
      resource: "https://replace.{{domain_suffix}}/mcp",
      resourceMetadataUrl:
        "https://replace.{{domain_suffix}}/.well-known/oauth-protected-resource/mcp",
      authorizationServer: "https://replace.{{domain_suffix}}",
    });
  });

  it("normalizes a trailing slash, query, and fragment away", () => {
    // A resource identifier is a bare URI; `/mcp` and `/mcp/` must not advertise
    // two different resources for the same endpoint.
    for (const variant of [`${CANONICAL}/`, `${CANONICAL}?x=1`, `${CANONICAL}#frag`]) {
      expect(resourceUrls(variant).resource).toBe(CANONICAL);
    }
  });

  it("falls back instead of throwing on a missing or malformed var", () => {
    // This runs on the request path. A bad var must not turn every request into
    // a 500 — it degrades to the compiled-in canonical URL.
    for (const bad of [undefined, "", "   ", "not a url", "://nope"]) {
      expect(resourceUrls(bad).resource).toBe(FALLBACK_PUBLIC_MCP_URL);
    }
  });

  it("falls back on any non-HTTP(S) scheme, not just on unparseable input", () => {
    // These all PARSE. Without an explicit scheme check they produce an origin
    // of "null" and a pathname with no leading slash, yielding
    // `null/.well-known/oauth-protected-resourcefoo` — and OAuthProvider throws
    // a TypeError on a non-HTTP(S) `resource`, which (since the provider is
    // built lazily on the request path) would be a cold-start 500 on EVERY
    // request rather than the graceful fallback this function documents.
    for (const opaque of [
      "mailto:someone@example.com",
      "data:text/plain,hello",
      "urn:ietf:rfc:7523",
      "javascript:alert(1)",
      "file:///etc/passwd",
      "ftp://example.com/mcp",
    ]) {
      const out = resourceUrls(opaque);
      expect(out.resource, opaque).toBe(FALLBACK_PUBLIC_MCP_URL);
      expect(out.authorizationServer, opaque).not.toBe("null");
      expect(out.resourceMetadataUrl, opaque).toContain("/.well-known/oauth-protected-resource/");
    }
  });

  it("produces a header-safe metadata URL for every accepted input", () => {
    // resourceMetadataUrl is interpolated into a WWW-Authenticate auth-param.
    // A quote or CR/LF there would inject a param or make new Response() throw.
    for (const input of [
      undefined,
      "",
      'https://h.example/mcp"injected="x',
      "https://h.example/mcp\r\nX-Injected: 1",
      "http://plain.example/deep/path/",
    ]) {
      const { resourceMetadataUrl } = resourceUrls(input as string | undefined);
      expect(resourceMetadataUrl).not.toMatch(/[\r\n]/);
      expect(resourceMetadataUrl.startsWith("https://") || resourceMetadataUrl.startsWith("http://")).toBe(
        true,
      );
    }
  });

  it("never advertises a workers.dev URL", () => {
    // wrangler.toml declares `routes` WITHOUT `workers_dev = true`, so this
    // Worker's *.workers.dev hostname does not serve. Advertising it as the
    // resource identifier would point clients at an endpoint that 404s.
    expect(FALLBACK_PUBLIC_MCP_URL).not.toContain("workers.dev");
    // Comments are allowed to *mention* it (they explain why it is wrong); no
    // active line may name it or turn the subdomain on.
    const active = read("wrangler.toml")
      .split("\n")
      .filter((line) => line.trim() !== "" && !line.trim().startsWith("#"));
    expect(active.filter((l) => l.includes("workers.dev"))).toEqual([]);
    expect(active.filter((l) => /workers_dev/.test(l))).toEqual([]);
  });
});
