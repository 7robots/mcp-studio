// Data layer placeholder.
//
// Replace the contents of this file with your data access helpers — D1 query
// functions, R2 object operations, Vectorize lookups, etc. The tool handlers in
// src/mcp.ts should call functions defined here rather than inlining their
// SQL/storage logic, so the data layer is testable and swappable — these
// helpers are pure enough to unit-test with a stub binding (see test/mcp.test.ts).
//
// Below is an example D1 helper pattern. Delete it and replace with your own.
// If your server doesn't use D1, delete this file and remove the import plus the
// `list_examples` tool from src/mcp.ts.

import type { D1Database } from "@cloudflare/workers-types";

export interface ExampleRow {
  id: number;
  name: string;
  created_at: string;
}

export async function getExampleById(db: D1Database, id: number): Promise<ExampleRow | null> {
  const stmt = db.prepare("SELECT id, name, created_at FROM examples WHERE id = ?").bind(id);
  return (await stmt.first<ExampleRow>()) ?? null;
}

export async function listExamples(
  db: D1Database,
  limit: number,
  offset: number,
): Promise<ExampleRow[]> {
  const stmt = db
    .prepare("SELECT id, name, created_at FROM examples ORDER BY id DESC LIMIT ? OFFSET ?")
    .bind(limit, offset);
  const { results } = await stmt.all<ExampleRow>();
  return results ?? [];
}

/**
 * Delete one row. Returns false when nothing matched.
 *
 * The destructive half of the example data layer, paired with the MRTR
 * confirmation in src/mcp.ts — see src/confirm.ts for why the confirmation state
 * is signed and bound rather than merely opaque.
 */
export async function deleteExample(db: D1Database, id: number): Promise<boolean> {
  const res = await db.prepare("DELETE FROM examples WHERE id = ?").bind(id).run();
  return (res.meta?.changes ?? 0) > 0;
}
