// This server's public identity as an OAuth protected resource.
//
// Every identifier below is derived from ONE configured URL, the PUBLIC_MCP_URL
// var in wrangler.toml, so they cannot drift apart — and from config rather than
// the request, because the Host header is chosen by the caller and a resource
// identifier a caller can choose is not an identifier.
//
// Kept in its own module, apart from src/index.ts, for one concrete reason:
// index.ts transitively imports `cloudflare:` modules through
// @cloudflare/workers-oauth-provider and therefore cannot be loaded under the
// plain-Node vitest pool at all. Here the derivation is a pure function with
// real unit tests (test/resource.test.ts).

/**
 * Fallback for a deploy that forgot the var. wrangler.toml is the source of
 * truth; this only keeps a missing var from turning every request into a 500.
 *
 * ⚠️ Whatever this is, it must be a host this Worker actually serves. If
 * wrangler.toml declares `routes` without `workers_dev = true`, the
 * *.workers.dev hostname does NOT serve and a resource identifier pointing there
 * advertises an unreachable endpoint; if `routes` is commented out, the custom
 * domain is the unreachable one. See the note above `routes` in wrangler.toml.
 */
export const FALLBACK_PUBLIC_MCP_URL = "https://replace.{{domain_suffix}}/mcp";

export interface ResourceUrls {
  /** RFC 8707 resource identifier — the canonical MCP endpoint URI. */
  resource: string;
  /** RFC 9728 metadata location: origin + /.well-known/… + the resource path. */
  resourceMetadataUrl: string;
  /** The authorization server issuing tokens for this resource. */
  authorizationServer: string;
}

/**
 * Derive the resource identifier, its RFC 9728 metadata URL, and the
 * authorization server from the configured public MCP endpoint.
 *
 * RFC 9728 §3.1 inserts the well-known segment BEFORE the resource's own path
 * (`https://host/.well-known/oauth-protected-resource/mcp`), not after it, which
 * is why this is path arithmetic rather than plain concatenation.
 *
 * A missing or malformed value falls back rather than throwing: this runs on the
 * request path, and a bad var must not turn every request into a 500.
 *
 * "Malformed" includes any non-HTTP(S) scheme, not just what `new URL()` rejects.
 * That check is load-bearing twice over:
 *
 *   - An opaque-scheme URL (`mailto:`, `data:`, `urn:`) has an origin of "null"
 *     and a pathname with NO leading slash, so the arithmetic below would emit
 *     `null/.well-known/oauth-protected-resourcefoo` — and, because such a
 *     pathname can still contain a quote or a space, would inject an extra
 *     auth-param into the WWW-Authenticate challenge built from it.
 *   - OAuthProvider itself throws a TypeError when `resourceMetadata.resource`
 *     is not an absolute HTTP(S) URI. Since the provider is constructed lazily
 *     on the request path, that would be a cold-start 500 on EVERY request —
 *     strictly worse than the fallback this function promises.
 */
export function resourceUrls(raw: string | undefined): ResourceUrls {
  let url: URL;
  try {
    url = new URL(raw?.trim() || FALLBACK_PUBLIC_MCP_URL);
    if (url.protocol !== "https:" && url.protocol !== "http:") {
      url = new URL(FALLBACK_PUBLIC_MCP_URL);
    }
  } catch {
    url = new URL(FALLBACK_PUBLIC_MCP_URL);
  }
  // Query and fragment are dropped — a resource identifier is a bare URI — and a
  // trailing slash is trimmed so `/mcp` and `/mcp/` advertise the same thing.
  const path = url.pathname === "/" ? "" : url.pathname.replace(/\/+$/, "");
  return {
    resource: `${url.origin}${path}`,
    resourceMetadataUrl: `${url.origin}/.well-known/oauth-protected-resource${path}`,
    authorizationServer: url.origin,
  };
}
