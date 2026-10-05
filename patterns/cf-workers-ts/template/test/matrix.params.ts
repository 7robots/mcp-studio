// Per-repo parameters for the fleet behavioral matrix (matrix.workerd.test.ts).
//
// This file is the ALLOWED variation — the spec itself is conformance-tracked
// and byte-identical fleet-wide. Keep this to data: scope strings, what the
// metadata advertises, and one tool that needs a scope beyond readScope
// (null on a single-scope server, which then records the skip loudly).
export const MATRIX = {
  /** A scope that admits whoami and tools/list. */
  readScope: "REPLACE:read",
  /** Exactly what /.well-known/oauth-protected-resource must advertise. */
  advertisedScopes: ["REPLACE:read", "REPLACE:write"],
  /** A schema-valid call that needs a scope readScope does not grant. */
  stepUp: {
    tool: "delete_example",
    args: { id: 1 },
    scope: "REPLACE:write",
  } as { tool: string; args: Record<string, unknown>; scope: string } | null,
} as const;
