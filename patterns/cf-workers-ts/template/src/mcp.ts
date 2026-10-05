import {
  acceptedContent,
  createMcpHandler,
  inputRequired,
  McpServer,
} from "@modelcontextprotocol/server";
import type { AuthInfo } from "@modelcontextprotocol/server";
import * as z from "zod/v4";

import { deletionCodec, type PendingDeletion } from "./confirm";
import { deleteExample, getExampleById, listExamples } from "./data";
import { requireScope, WRITE_SCOPE } from "./scopes";
import { SKILL_DESCRIPTION, SKILL_MD } from "./skill";

// Stored by src/auth.ts at completeAuthorization time and mapped onto AuthInfo
// by src/index.ts. Under the stateless protocol there is no per-session agent to
// hold these, so they travel per request.
//
// `scopes` and `client_id` are both load-bearing and both easy to omit:
// OAuthProvider exposes only ctx.props to the apiHandler, not the grant's
// scopes, so the scopes must be copied here at grant time or every scope check
// sees undefined. And client_id is the OAUTH CLIENT, which is not the user —
// conflating the two (e.g. setting clientId from okta_sub) makes per-client
// consent and revocation meaningless.
export interface ServerProps extends Record<string, unknown> {
  okta_sub: string;
  email: string;
  name?: string;
  okta_access_token: string;
  /** Scopes granted to this grant. */
  scopes?: string[];
  /** The OAuth client id holding this grant (distinct from the user). */
  client_id?: string;
  /** Set by src/ema.ts when this grant came from an ID-JAG assertion rather than
   *  an interactive approval. Read by src/index.ts to stamp
   *  `extra.auth_path = "enterprise"`, because an enterprise grant skipped the
   *  consent screen — the IdP consented instead — and a tool that cares about
   *  that must be able to tell. */
  enterprise?: boolean;
  /** The IdP that issued the assertion, when `enterprise` is set. */
  enterprise_issuer?: string;
}

interface ToolCtx {
  http?: { authInfo?: AuthInfo };
}

/** Scopes the caller actually holds, from either auth path. */
const granted = (ctx: ToolCtx): string[] | undefined => ctx.http?.authInfo?.scopes;

// Replace with your server's description — the LLM uses this to decide when
// to invoke any tool on this server.
const INSTRUCTIONS = "REPLACE — short description of what this MCP server does and when to use its tools.";

function text(value: string) {
  return { content: [{ type: "text" as const, text: value }] };
}

// Builds a fresh McpServer. Under the 2026-07-28 stateless protocol the
// createMcpHandler factory calls this once per HTTP request, so nothing here
// may hold cross-request state — every store belongs behind a binding on `env`.
// Exported so test/mcp.test.ts can build a server against stub bindings.

// Correlates the embedded elicitation with its response on re-entry.
const CONFIRM_KEY = "confirm_delete";

export function buildServer(env: Env): McpServer {
  const codec = deletionCodec(env);
  const server = new McpServer(
    { name: "REPLACE-WITH-SERVER-NAME", version: "0.1.0" },
    {
      // MRTR state verification runs in the SEAM, before any handler body. A
      // failure is answered with a frozen -32602 whose reason never reaches the
      // wire. Without this option the handler receives the raw echoed string.
      ...(codec ? { requestState: { verify: codec.verify } } : {}),
      instructions: INSTRUCTIONS },
  );

  // Expose the LLM-facing usage guide as an MCP resource (skill://...).
  // This is the MCP-SDK equivalent of FastMCP's SkillProvider — clients read
  // it to learn the server's data model and tools. Keep it; edit src/skill.ts.
  server.registerResource(
    "replace-with-your-server-name-skill",
    "skill://replace-with-your-server-name",
    { title: "REPLACE-WITH-SERVER-NAME skill", description: SKILL_DESCRIPTION, mimeType: "text/markdown" },
    async (uri: URL) => ({
      contents: [{ uri: uri.href, mimeType: "text/markdown", text: SKILL_MD }],
    }),
  );

  // Replace the stub tools below with your own.
  //
  // Tool registration shape (@modelcontextprotocol/server v2):
  //   server.registerTool(name, { description, inputSchema }, handler)
  //
  // - name: stable identifier the LLM calls. Snake_case is conventional.
  // - description: what the tool does + when to invoke it. The LLM reads this.
  // - inputSchema: a zod v4 SCHEMA — `z.object({ ... })`, not a bare shape
  //   object. Import zod as `import * as z from "zod/v4"`.
  // - handler: async (args, ctx) => ({ content: [{ type: "text", text: "..." }] })
  //
  // Bindings come from the `env` this factory closes over; the caller's identity
  // rides `ctx.http.authInfo` (see the whoami tool below and src/index.ts).

  server.registerTool(
    "echo",
    {
      description: "Echo a message back. Replace this with your real tool.",
      inputSchema: z.object({
        message: z.string().min(1).max(1000),
        response_format: z.enum(["json", "markdown"]).default("json"),
      }),
    },
    async ({ message, response_format }, ctx) => {
      // Layer 2 of the scope policy (src/scopes.ts): the boundary a caller
      // cannot avoid by omitting the Mcp-Name header that layer 1 reads.
      // Every tool handler opens with this, and the tool name must match its
      // key in TOOL_SCOPES.
      const denied = requireScope(granted(ctx), "echo");
      if (denied) return denied;

      if (response_format === "markdown") {
        return text(`# Echo\n\n> ${message}`);
      }
      return text(JSON.stringify({ message }, null, 2));
    },
  );

  // Example of a tool backed by the data layer (src/data.ts). Delete both if
  // your server doesn't use D1.
  server.registerTool(
    "list_examples",
    {
      description: "List example rows. Replace with your real data-layer tool.",
      inputSchema: z.object({
        limit: z.number().int().min(1).max(100).default(20),
        offset: z.number().int().min(0).default(0),
      }),
    },
    async ({ limit, offset }, ctx) => {
      // Layer 2 of the scope policy (src/scopes.ts): the boundary a caller
      // cannot avoid by omitting the Mcp-Name header that layer 1 reads.
      // Every tool handler opens with this, and the tool name must match its
      // key in TOOL_SCOPES.
      const denied = requireScope(granted(ctx), "list_examples");
      if (denied) return denied;

      const rows = await listExamples(env.DB, limit, offset);
      return text(JSON.stringify({ count: rows.length, offset, rows }, null, 2));
    },
  );

  // The WRITE-scoped example, and the one destructive tool. Two things it exists
  // to demonstrate, both of which a starter that only reads cannot show:
  //
  //   1. WRITE_SCOPE actually gating something. A template with two scopes where
  //      no tool requires the second makes consentCovers, the step-up 403 and the
  //      broader-scope re-prompt all vacuous — the exact condition the two-scope
  //      decision exists to avoid.
  //   2. MRTR (multi-round-trip). An irreversible tool asks first: it returns an
  //      InputRequiredResult carrying an elicitation, and acts only when the retry
  //      echoes back an explicit confirmation.
  //
  // Delete it if your server has nothing destructive — but if it has anything
  // irreversible, this is the shape to copy.
  server.registerTool(
    "delete_example",
    {
      description:
        "Delete one example row by id. Asks for confirmation first and cannot be undone. " +
        "Replace with your real destructive tool, or delete both.",
      inputSchema: z.object({
        id: z.number().int().min(1).describe("Row id to delete."),
      }),
    },
    async ({ id }, ctx) => {
      const denied = requireScope(granted(ctx), "delete_example");
      if (denied) return denied;

      // ROUND 2. requestState() returns the payload the SEAM already verified —
      // signature, expiry, and the caller binding — so reaching here means THIS
      // caller minted THIS state. The id comes from the SEALED payload and never
      // from the arguments, or a confirmation obtained for one row could be
      // retried against another.
      const pending = codec ? ctx.mcpReq.requestState<PendingDeletion>() : undefined;
      if (pending) {
        const answer = acceptedContent(
          ctx.mcpReq.inputResponses,
          CONFIRM_KEY,
          z.object({ confirm: z.boolean() }),
        );
        // Missing, declined, cancelled and schema-invalid all arrive as
        // undefined. Anything short of an explicit true is a refusal.
        if (!answer?.confirm) {
          return text(
            JSON.stringify({ deleted: false, reason: "Not confirmed.", id: pending.exampleId }, null, 2),
          );
        }
        const gone = await deleteExample(env.DB, pending.exampleId);
        return text(
          JSON.stringify(
            gone ? { deleted: true, id: pending.exampleId } : { deleted: false, reason: "No longer exists." },
            null,
            2,
          ),
        );
      }

      // ROUND 1. Ask, naming what will be destroyed — a confirmation reading only
      // "are you sure?" is one a model can answer on the human's behalf.
      if (codec) {
        const row = await getExampleById(env.DB, id);
        if (!row) return text(`Error: no example with id ${id}.`);
        const requestState = await codec.mint({ exampleId: id, label: row.name }, ctx);
        return inputRequired({
          requestState,
          inputRequests: {
            [CONFIRM_KEY]: inputRequired.elicit({
              message: `Permanently delete "${row.name}" (id ${id})? This cannot be undone.`,
              requestedSchema: z.object({
                confirm: z.boolean().describe("Yes, delete this permanently."),
              }),
            }),
          },
        });
      }

      // No key configured: REFUSE. Deleting unconfirmed would make a missing
      // secret more permissive than a present one, which is the wrong direction
      // for the only irreversible tool here.
      return text(
        "Error: deletion requires confirmation, and REQUEST_STATE_KEY is not configured " +
          "to protect the confirmation state. Nothing was deleted.",
      );
    },
  );

  server.registerTool(
    "whoami",
    {
      description: "Return the calling client's identity as resolved by the Worker's auth dispatcher.",
      inputSchema: z.object({}),
    },
    async (_args, ctx) => {
      // Layer 2 of the scope policy (src/scopes.ts): the boundary a caller
      // cannot avoid by omitting the Mcp-Name header that layer 1 reads.
      // Every tool handler opens with this, and the tool name must match its
      // key in TOOL_SCOPES.
      const denied = requireScope(granted(ctx), "whoami");
      if (denied) return denied;

      // Both auth paths hand the SDK an AuthInfo (see src/index.ts): the
      // interactive path maps OAuthProvider's stored Okta props, the M2M path
      // maps the claims from the locally-verified Okta JWT. `extra.auth_path`
      // says which.
      // Non-null by construction: requireScope above refuses a caller with no
      // AuthInfo, so this is a type narrowing rather than a runtime branch. If
      // you remove the guard, restore a real check here.
      const auth = ctx.http!.authInfo!;
      const extra = (auth.extra ?? {}) as {
        auth_path?: string;
        sub?: string;
        email?: string;
        name?: string;
        on_behalf_of?: string;
        gateway_purpose?: string;
        gateway_run_id?: string;
        enterprise_issuer?: string;
      };
      return text(
        JSON.stringify(
          {
            authenticated: true,
            auth_path: extra.auth_path ?? "unknown",
            client_id: auth.clientId,
            sub: extra.sub,
            email: extra.email,
            name: extra.name,
            scopes: auth.scopes,
            // Present only on an EMA grant (auth_path "enterprise"): WHICH IdP
            // consented on the user's behalf. Provenance without the issuer is
            // half a provenance — found by the fleet matrix suite, which
            // asserted it and discovered no whoami reported it.
            ...(extra.enterprise_issuer ? { enterprise_issuer: extra.enterprise_issuer } : {}),
            // Present only when a gateway X-MCP-Actor assertion VERIFIED —
            // auth_path reads m2m+actor. This is the fleet's one-call check
            // that the verification chain is live rather than degrading.
            ...(extra.on_behalf_of
              ? {
                  on_behalf_of: extra.on_behalf_of,
                  gateway_purpose: extra.gateway_purpose,
                  gateway_run_id: extra.gateway_run_id,
                }
              : {}),
          },
          null,
          2,
        ),
      );
    },
  );

  return server;
}

// One handler per isolate; all per-request work happens inside the factory.
// `legacy` is left at its default ('stateless'), so 2025-era clients (initialize
// handshake, no _meta envelope) are served per-request from the same factory —
// no sessions on either path.
//
// The factory also receives `{ era, authInfo, requestInfo }` if you need to vary
// the server per request (e.g. hide a tool from legacy clients). Ignore it
// otherwise — tools can read the same AuthInfo off `ctx.http.authInfo`.
export function createHandler(env: Env) {
  return createMcpHandler(() => buildServer(env));
}

export type Handler = ReturnType<typeof createHandler>;
