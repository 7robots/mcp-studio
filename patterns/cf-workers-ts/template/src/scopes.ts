// Per-tool scope policy for this server.
//
// This server's scopes are `REPLACE:read` and `REPLACE:write`, defined on the Okta
// custom authorization server `oauth2/default`. They replaced the single
// fleet-wide `mcp-access` scope on 2026-08-21: `mcp-access` is a DEFAULT scope
// on that AS (a client_credentials request with no `scope` parameter receives
// it), so it distinguished nothing — every caller of every server had it.
//
// WHAT THESE SCOPES DO AND DO NOT BUY:
//
// On the M2M path they are a real boundary — Okta decides which scopes the
// calling app may obtain. On the INTERACTIVE path they are SELF-ASSERTED: any
// client may put `scope=REPLACE:read REPLACE:write` on its authorization request and
// receive it, because the AS grants what was asked for. So per-tool scopes are
// least-privilege plumbing and an audit signal for human callers, not an
// authorization boundary against them. The boundary for a human is the Okta
// login itself; the boundary for a workload is Okta's scope grant.
//
// Enforcement happens in two places, deliberately:
//
//   1. `src/index.ts`, from the `Mcp-Name` request header, BEFORE the handler
//      runs. This is the spec-conformant path: it can return a real
//      `403 insufficient_scope` with a `WWW-Authenticate` challenge that names
//      the scopes needed, which is what lets a client step up (MCP 2026-07-28
//      § Scope Challenge Handling). It only works for clients that send the
//      header, which 2026-07-28 requires but older clients do not.
//
//   2. Inside each tool handler, via `requireScope()`. This is the actual
//      security boundary, because it cannot be avoided by omitting a header.
//      It can only return a tool error, not an HTTP status.
//
// Layer 1 is the user experience; layer 2 is the guarantee. A tool absent from
// TOOL_SCOPES fails closed in both.

export const READ_SCOPE = "REPLACE:read";
export const WRITE_SCOPE = "REPLACE:write";

/** Every tool this server exposes, and the single scope it requires. */
export const TOOL_SCOPES: Record<string, string> = {
  // REPLACE: one entry per tool in src/mcp.ts. Every tool needs an entry — a
  // tool missing from this map has no scope policy, and `scopeForTool` returns
  // null for it, which means the pre-handler gate cannot decide and the
  // in-handler guard must refuse it. Keep this map and src/mcp.ts in step.
  echo: READ_SCOPE,
  list_examples: READ_SCOPE,
  whoami: READ_SCOPE,
  // The one write-scoped tool. Keeping at least one is what stops the step-up
  // 403, consentCovers and the broader-scope re-prompt from being vacuous.
  delete_example: WRITE_SCOPE,
};

/** Scopes this resource advertises in its protected-resource metadata. */
export const SUPPORTED_SCOPES = [READ_SCOPE, WRITE_SCOPE];

/**
 * The scope a tool call needs, or null when the tool is unknown to this policy.
 * Callers must treat null as "deny", not "allow" — an unmapped tool is either a
 * typo or a tool someone added without deciding its scope.
 */
export function scopeForTool(toolName: string | null | undefined): string | null {
  if (!toolName) return null;
  // Object.hasOwn, not a bare index: the tool name arrives from the Mcp-Name
  // header, and a plain-object lookup of "constructor" / "__proto__" /
  // "toString" returns an inherited NON-STRING. That failed closed, but it put
  // `scope="function Object() { [native code] }"` on the wire and was one
  // refactor away from being an injection point.
  if (!Object.hasOwn(TOOL_SCOPES, toolName)) return null;
  const scope = TOOL_SCOPES[toolName];
  return typeof scope === "string" ? scope : null;
}

// A header value containing CR, LF or a bare quote makes `new Response()` throw,
// which would turn an intended 403 into a 500. Restrict to printable ASCII and
// drop the quote/backslash characters that would break the auth-param grammar.
function headerSafe(value: string): string {
  return value.replace(/[\r\n]+/g, " ").replace(/["\\]/g, "'").replace(/[^\x20-\x7E]/g, "?").slice(0, 200);
}

export function hasScope(granted: readonly string[] | undefined, needed: string): boolean {
  return !!granted && granted.includes(needed);
}

/**
 * `403` + RFC 6750 `insufficient_scope` challenge. Per MCP 2026-07-28, all
 * scopes required for the operation go in ONE challenge — challenging
 * incrementally forces a separate authorization round trip per missing scope.
 */
export function insufficientScopeResponse(
  required: readonly string[],
  resourceMetadataUrl: string,
  description: string,
): Response {
  const challenge = [
    'Bearer error="insufficient_scope"',
    `scope="${required.map(headerSafe).join(" ")}"`,
    `resource_metadata="${resourceMetadataUrl}"`,
    `error_description="${headerSafe(description)}"`,
  ].join(", ");
  return new Response(
    JSON.stringify({ error: "insufficient_scope", error_description: description }),
    {
      status: 403,
      headers: { "content-type": "application/json", "www-authenticate": challenge },
    },
  );
}

/**
 * Handler-side guard — layer 2. Returns null when the caller may proceed, or
 * the tool-error result to return when it may not.
 *
 * `granted` being undefined means the request arrived with no AuthInfo at all.
 * That fails closed: this server has no anonymous surface.
 */
export function requireScope(
  granted: readonly string[] | undefined,
  toolName: string,
): { content: { type: "text"; text: string }[]; isError: true } | null {
  const needed = scopeForTool(toolName);
  if (needed && hasScope(granted, needed)) return null;
  const reason = needed
    ? `This tool requires the '${needed}' scope. The token presented carries: ${
        granted?.length ? granted.join(", ") : "(none)"
      }. Re-authorize requesting '${needed}'.`
    : `Tool '${toolName}' has no scope policy on this server and is refused.`;
  return { content: [{ type: "text" as const, text: `Error: ${reason}` }], isError: true };
}

/**
 * The tool name a Streamable HTTP POST is calling, from its headers alone.
 *
 * MCP 2026-07-28 requires `Mcp-Method` and `Mcp-Name` on every POST precisely so
 * an intermediary can route and enforce policy without parsing the body. A value
 * arrives Base64-wrapped (`=?base64?…?=`, base64url alphabet) when it is not
 * header-safe.
 *
 * This lives in scopes.ts, not index.ts, because index.ts transitively imports
 * `cloudflare:` modules and cannot be loaded under the plain-Node test pool at
 * all. This function parses attacker-controlled input, so it belongs somewhere
 * it can actually be unit-tested.
 *
 * Returns null when the request is not a tools/call, names nothing, or the
 * wrapper will not decode — never a partial or guessed name.
 */
export function mcpToolName(req: Request): string | null {
  if (req.headers.get("mcp-method") !== "tools/call") return null;
  const raw = req.headers.get("mcp-name");
  if (!raw) return null;
  const m = raw.match(/^=\?base64\?(.*)\?=$/);
  if (!m) return raw;
  try {
    return atob(m[1].replace(/-/g, "+").replace(/_/g, "/"));
  } catch {
    return null;
  }
}
