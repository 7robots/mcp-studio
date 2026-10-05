// In-process tests for the stateless MCP handler (2026-07-28 protocol).
//
// The handler is a plain `fetch(request, options?)` function — no Workers
// runtime, no Durable Object, no session bookkeeping. POST JSON-RPC requests at
// it with stub bindings and assert on the response. Adapt the tool names below
// when you replace the template's stub tools.

import { describe, expect, it } from "vitest";

import { TOOL_SCOPES } from "../src/scopes";
import type { D1Database } from "@cloudflare/workers-types";

import { createHandler } from "../src/mcp";

const EXPECTED_TOOLS = ["delete_example", "echo", "list_examples", "whoami"];

// Minimal recording D1 stub: captures every prepare(sql).bind(params) call and
// returns empty result sets, so tools/call tests can assert what reached the
// SQL layer without a Workers runtime. Swap for your own data layer's stub.
function makeDbStub() {
  const calls: { sql: string; params: unknown[] }[] = [];
  const db = {
    prepare(sql: string) {
      return {
        bind(...params: unknown[]) {
          calls.push({ sql, params });
          return {
            first: async () => null,
            all: async () => ({ results: [] }),
          };
        },
      };
    },
  } as unknown as D1Database;
  return { db, calls };
}

function makeHandler(db?: D1Database) {
  // REQUEST_STATE_KEY must be present and >=32 bytes or deletionCodec() returns
  // null, the MRTR path is never built, and delete_example refuses instead of
  // asking — which would make the tests below pass for the wrong reason.
  const env = {
    DB: db ?? ({} as D1Database),
    REQUEST_STATE_KEY: "test-request-state-key-at-least-32-bytes-long",
  } as Env;
  return createHandler(env);
}

// POST one JSON-RPC request and parse the result whether it comes back as a
// plain JSON body or a single-message SSE stream.
// The scopes a caller holds in most of these tests. Tools guard themselves with
// requireScope (src/scopes.ts layer 2), so a call with no AuthInfo is REFUSED —
// pass authInfo: null explicitly to exercise that path.
const AUTHORIZED = {
  token: "test-token",
  clientId: "test-client",
  scopes: ["REPLACE:read", "REPLACE:write"],
  extra: { auth_path: "interactive", sub: "00uTestUser" },
};

async function rpc(
  handler: ReturnType<typeof makeHandler>,
  body: Record<string, unknown>,
  authInfo: unknown = AUTHORIZED,
): Promise<{ status: number; message: { result?: any; error?: any } }> {
  const res = await handler.fetch(
    new Request("http://localhost/mcp", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        accept: "application/json, text/event-stream",
      },
      body: JSON.stringify(body),
    }),
    authInfo ? { authInfo: authInfo as never } : undefined,
  );
  const raw = await res.text();
  const contentType = res.headers.get("content-type") ?? "";
  if (contentType.includes("text/event-stream")) {
    const data = raw
      .split("\n")
      .filter((line) => line.startsWith("data:"))
      .map((line) => line.slice(5).trim());
    return { status: res.status, message: JSON.parse(data[data.length - 1]) };
  }
  return { status: res.status, message: JSON.parse(raw) };
}

describe("stateless MCP handler", () => {
  it("lists the registered tools", async () => {
    const { status, message } = await rpc(makeHandler(), {
      jsonrpc: "2.0",
      id: 1,
      method: "tools/list",
    });
    expect(status).toBe(200);
    expect(message.error).toBeUndefined();
    const names = message.result.tools.map((t: { name: string }) => t.name);
    expect(names.sort()).toEqual([...EXPECTED_TOOLS].sort());
  });

  it("serves consecutive requests with no session state", async () => {
    const handler = makeHandler();
    const first = await rpc(handler, { jsonrpc: "2.0", id: 1, method: "tools/list" });
    const second = await rpc(handler, { jsonrpc: "2.0", id: 2, method: "tools/list" });
    expect(first.message.result.tools).toEqual(second.message.result.tools);
  });

  it("exposes the skill resource", async () => {
    const handler = makeHandler();
    const list = await rpc(handler, { jsonrpc: "2.0", id: 1, method: "resources/list" });
    const uris = list.message.result.resources.map((r: { uri: string }) => r.uri);
    expect(uris).toContain("skill://replace-with-your-server-name");

    const read = await rpc(handler, {
      jsonrpc: "2.0",
      id: 2,
      method: "resources/read",
      params: { uri: "skill://replace-with-your-server-name" },
    });
    expect(read.message.result.contents[0].text).toContain("REPLACE-WITH-SERVER-NAME");
  });

  it("applies schema defaults on tools/call (limit=20, offset=0 reach SQL)", async () => {
    const { db, calls } = makeDbStub();
    const { status, message } = await rpc(makeHandler(db), {
      jsonrpc: "2.0",
      id: 1,
      method: "tools/call",
      params: { name: "list_examples", arguments: {} },
    });
    expect(status).toBe(200);
    expect(message.error).toBeUndefined();
    expect(message.result.isError).not.toBe(true);
    const listCall = calls.find((c) => c.sql.includes("LIMIT ? OFFSET ?"));
    expect(listCall).toBeDefined();
    expect(listCall!.params.slice(-2)).toEqual([20, 0]);
  });

  it("rejects out-of-range tool arguments", async () => {
    const { db, calls } = makeDbStub();
    const { message } = await rpc(makeHandler(db), {
      jsonrpc: "2.0",
      id: 1,
      method: "tools/call",
      params: { name: "list_examples", arguments: { limit: 9999 } },
    });
    // Validation failure surfaces as a tool error (or protocol error on some
    // eras) — either way, no SQL may run.
    const failed = message.error !== undefined || message.result?.isError === true;
    expect(failed).toBe(true);
    expect(calls).toHaveLength(0);
  });

  // Layer 2 of the scope policy, asserted for EVERY tool rather than one of
  // them. Driven off TOOL_SCOPES so a tool added later is covered as soon as it
  // gets a policy entry — a hand-written test naming one tool reads like
  // coverage of the pattern and is coverage of a single instance.
  //
  // Both cases need a `db` stub because a data-layer tool would otherwise fail
  // for the wrong reason; the guard runs first, so no SQL should be issued.
  // Valid arguments per tool. REQUIRED, not cosmetic: calling a tool with `{}`
  // when it has a required argument fails SCHEMA validation before the scope
  // guard runs, so the refusal assertion below would hold with the guard
  // deleted — a vacuous test. (Measured: with `{}`, deleting echo's guard failed
  // zero tests.) Add an entry when you add a tool.
  const VALID_ARGS: Record<string, Record<string, unknown>> = {
    echo: { message: "hi" },
    list_examples: {},
    whoami: {},
    // Valid, and deliberately not `{}` — a call with invalid arguments is refused
    // by SCHEMA validation before any scope guard runs, so a guard test using
    // empty args passes with the guard deleted.
    delete_example: { id: 1 },
  };

  describe.each(Object.keys(TOOL_SCOPES))("the %s tool's scope guard", (tool) => {
    it("has valid arguments in VALID_ARGS, so the refusals below are not vacuous", () => {
      expect(
        VALID_ARGS,
        `add a VALID_ARGS entry for ${tool} or its guard tests prove nothing`,
      ).toHaveProperty(tool);
    });

    it("REFUSES a call carrying no AuthInfo at all", async () => {
      const { db, calls } = makeDbStub();
      const { message } = await rpc(
        makeHandler(db),
        { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: tool, arguments: VALID_ARGS[tool] ?? {} } },
        null,
      );
      // Assert the refusal came from the GUARD, not from the handler throwing on
      // a missing AuthInfo — a test that accepts any error passes when the guard
      // is deleted and the tool crashes instead, which is vacuous.
      expect(message.result?.isError, `${tool} served a caller with no identity`).toBe(true);
      expect(message.result.content[0].text).toContain(TOOL_SCOPES[tool]);
      expect(calls).toHaveLength(0);
    });

    it("REFUSES a caller holding some other scope", async () => {
      // Holding SOME scope is not holding THIS tool's scope. With one scope in
      // TOOL_SCOPES this branch cannot fire, which is why the template ships two.
      const { db, calls } = makeDbStub();
      const { message } = await rpc(
        makeHandler(db),
        { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: tool, arguments: VALID_ARGS[tool] ?? {} } },
        { ...AUTHORIZED, scopes: ["some:other-scope"] },
      );
      expect(message.result?.isError, `${tool} served a caller without its scope`).toBe(true);
      // Names the scope the caller is missing, so a conforming client can ask for
      // it — and so this test cannot be satisfied by an unrelated crash.
      expect(message.result.content[0].text).toContain(TOOL_SCOPES[tool]);
      expect(calls).toHaveLength(0);
    });
  });

  it("every registered tool has a TOOL_SCOPES entry", async () => {
    // The guard tests above iterate TOOL_SCOPES, so a tool registered in
    // src/mcp.ts but missing from the policy would be silently unguarded AND
    // silently untested. This is what catches that.
    const { message } = await rpc(makeHandler(), {
      jsonrpc: "2.0",
      id: 1,
      method: "tools/list",
    });
    const registered = (message.result.tools as { name: string }[]).map((t) => t.name).sort();
    expect(registered).toEqual(Object.keys(TOOL_SCOPES).sort());
  });

  it("passes AuthInfo through to tools", async () => {
    const handler = makeHandler();
    const res = await handler.fetch(
      new Request("http://localhost/mcp", {
        method: "POST",
        headers: {
          "content-type": "application/json",
          accept: "application/json, text/event-stream",
        },
        body: JSON.stringify({
          jsonrpc: "2.0",
          id: 1,
          method: "tools/call",
          params: { name: "whoami", arguments: {} },
        }),
      }),
      {
        authInfo: {
          token: "test-token",
          clientId: "0oaTestServiceApp",
          // A scope this server actually supports. The retired fleet-wide
          // `mcp-access` scope would now be refused by the in-handler guard
          // before the identity was ever echoed back.
          scopes: ["REPLACE:read"],
          extra: { auth_path: "m2m", sub: "svc-account" },
        },
      },
    );
    const raw = await res.text();
    const payload = raw.includes("data:")
      ? JSON.parse(raw.split("\n").filter((l) => l.startsWith("data:")).pop()!.slice(5).trim())
      : JSON.parse(raw);
    const identity = JSON.parse(payload.result.content[0].text);
    expect(identity).toMatchObject({
      authenticated: true,
      auth_path: "m2m",
      client_id: "0oaTestServiceApp",
      scopes: ["REPLACE:read"],
    });
  });

  it("still completes a 2025-era initialize handshake (legacy stateless fallback)", async () => {
    const { status, message } = await rpc(makeHandler(), {
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "legacy-test", version: "0.0.1" },
      },
    });
    expect(status).toBe(200);
    expect(message.error).toBeUndefined();
    expect(message.result.protocolVersion).toBe("2025-06-18");
    expect(message.result.serverInfo.name).toBe("REPLACE-WITH-SERVER-NAME");
  });
});

// MRTR (multi-round-trip) on the destructive example tool. The pattern this
// demonstrates matters more than the tool: an irreversible operation asks first,
// and the state linking the two rounds is attacker-controlled on the way back.
describe("delete_example asks before it deletes", () => {
  const CAPABLE = {
    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
    // Without this the SEAM refuses the embedded elicitation with -32021 rather
    // than returning it — the SDK gates each request kind on what the client
    // declared it can handle.
    "io.modelcontextprotocol/clientCapabilities": { elicitation: { form: {} } },
  };

  /** One row, and a delete that reports whether it removed anything. */
  const makeDb = (rows: { id: number; name: string }[]) => {
    const live = new Map(rows.map((r) => [r.id, r]));
    return {
      db: {
        prepare(sql: string) {
          return {
            bind: (...args: unknown[]) => ({
              first: async () => live.get(Number(args[0])) ?? null,
              run: async () => {
                const had = live.delete(Number(args[0]));
                return { meta: { changes: had ? 1 : 0 } };
              },
              all: async () => ({ results: [...live.values()] }),
            }),
          };
        },
      } as unknown as D1Database,
      live,
    };
  };

  const call = async (
    handler: ReturnType<typeof makeHandler>,
    args: Record<string, unknown>,
    cont: Record<string, unknown> = {},
    meta: unknown = CAPABLE,
  ) => {
    const res = await handler.fetch(
      new Request("http://localhost/mcp", {
        method: "POST",
        headers: {
          "content-type": "application/json",
          accept: "application/json, text/event-stream",
          "MCP-Protocol-Version": "2026-07-28",
          // Required on every modern request, and cross-checked against the
          // body — omitting them is -32020, not a lenient default.
          "Mcp-Method": "tools/call",
          "Mcp-Name": "delete_example",
        },
        body: JSON.stringify({
          jsonrpc: "2.0",
          id: 1,
          method: "tools/call",
          // requestState and inputResponses ride PARAMS, not `arguments` — the
          // SDK lifts them off params before the handler sees them.
          params: { _meta: meta, name: "delete_example", arguments: args, ...cont },
        }),
      }),
      { authInfo: AUTHORIZED } as never,
    );
    const raw = await res.text();
    const line = raw.split("\n").filter((l) => l.startsWith("data:")).pop();
    const parsed = JSON.parse(line ? line.slice(5) : raw);
    if (!parsed.result && !parsed.error) throw new Error(`unexpected body: ${raw.slice(0, 400)}`);
    return parsed;
  };

  it("ASKS on the first call, and deletes nothing", async () => {
    const { db, live } = makeDb([{ id: 1, name: "Doomed row" }]);
    const msg = await call(makeHandler(db), { id: 1 });
    expect(msg.result.resultType).toBe("input_required");
    expect(msg.result.requestState).toBeTypeOf("string");
    // The prompt must name what dies — "are you sure?" is a question a model can
    // answer on the human's behalf.
    expect(msg.result.inputRequests.confirm_delete.params.message).toContain("Doomed row");
    expect(live.has(1)).toBe(true);
  });

  it("deletes on the second call when the human accepts", async () => {
    const { db, live } = makeDb([{ id: 1, name: "Goner" }]);
    const handler = makeHandler(db);
    const first = await call(handler, { id: 1 });
    const second = await call(handler, { id: 1 }, {
      requestState: first.result.requestState,
      inputResponses: { confirm_delete: { action: "accept", content: { confirm: true } } },
    });
    expect(second.result.content[0].text).toContain('"deleted": true');
    expect(live.has(1)).toBe(false);
  });

  it("deletes NOTHING when the human declines", async () => {
    const { db, live } = makeDb([{ id: 1, name: "Spared" }]);
    const handler = makeHandler(db);
    const first = await call(handler, { id: 1 });
    const second = await call(handler, { id: 1 }, {
      requestState: first.result.requestState,
      inputResponses: { confirm_delete: { action: "decline" } },
    });
    expect(second.result.content[0].text).toContain("Not confirmed");
    expect(live.has(1)).toBe(true);
  });

  it("REFUSES a tampered requestState and deletes nothing", async () => {
    const { db, live } = makeDb([{ id: 1, name: "Tampered" }]);
    const handler = makeHandler(db);
    const first = await call(handler, { id: 1 });
    const state: string = first.result.requestState;
    // A middle byte: flipping the last character of a base64url payload can
    // decode to identical bytes.
    const mid = Math.floor(state.length / 2);
    const tampered = state.slice(0, mid) + (state[mid] === "A" ? "B" : "A") + state.slice(mid + 1);
    expect(tampered).not.toBe(state);
    const second = await call(handler, { id: 1 }, {
      requestState: tampered,
      inputResponses: { confirm_delete: { action: "accept", content: { confirm: true } } },
    });
    expect(second.error.code).toBe(-32602);
    expect(live.has(1)).toBe(true);
  });

  it("takes the target from the SEALED state, not the arguments", async () => {
    // The attack: confirm a harmless row, then retry that confirmation against a
    // different id.
    const { db, live } = makeDb([
      { id: 1, name: "Victim" },
      { id: 2, name: "Decoy" },
    ]);
    const handler = makeHandler(db);
    const first = await call(handler, { id: 2 });
    await call(handler, { id: 1 }, {
      requestState: first.result.requestState,
      inputResponses: { confirm_delete: { action: "accept", content: { confirm: true } } },
    });
    expect(live.has(1)).toBe(true);
    expect(live.has(2)).toBe(false);
  });
});
