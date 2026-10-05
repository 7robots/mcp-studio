// Multi-round-trip (MRTR) confirmation for destructive tools.
//
// MCP 2026-07-28 lets a tool answer "I need something from the human first" by
// returning an `InputRequiredResult` carrying an embedded elicitation and an
// opaque `requestState`; the client collects the answer and retries with both.
//
// THE STATE IS ATTACKER-CONTROLLED. It round-trips through the client, and the
// SDK applies no integrity protection by default — `ctx.mcpReq.requestState()`
// hands back whatever string came in. For a confirmation on a DELETE that is the
// whole ballgame: unprotected, a caller could mint their own state naming any
// character id and skip the confirmation they were supposed to be answering.
//
// So the state is HMAC-SHA256 sealed with `createRequestStateCodec` and BOUND to
// the caller and the method. Binding matters as much as the signature: without
// it, one user's validly-signed confirmation could be replayed by another caller
// against the same id.
//
// The payload is SIGNED, NOT ENCRYPTED — the client can read it. A character id
// is not a secret; nothing else goes in here.

import {
  createRequestStateCodec,
  type RequestStateCodec,
  type ServerContext,
} from "@modelcontextprotocol/server";

/** What a pending destructive confirmation remembers between rounds. */
export interface PendingDeletion {
  exampleId: number;
  /** The label shown in the prompt, so a retry cannot silently target another. */
  label: string;
}

/** How long a human has to answer before the state expires. */
const CONFIRM_TTL_SECONDS = 300;

/**
 * The caller identity a state is bound to.
 *
 * `clientId` is the OAuth client; `sub` is the human or workload. Both are in the
 * binding because either alone is too coarse: two humans share a client, and one
 * human uses several.
 */
function bindingFor(ctx: ServerContext): string {
  const auth = ctx.http?.authInfo;
  const sub = ((auth?.extra ?? {}) as { sub?: string }).sub ?? "";
  return [ctx.mcpReq?.method ?? "", auth?.clientId ?? "", sub].join(" ");
}

/**
 * Build the codec. Per request, because it closes over `env`.
 *
 * Returns null when no key is configured, and every caller treats that as "MRTR
 * confirmation is unavailable" rather than falling back to an unprotected state.
 * A missing secret must not silently downgrade a confirmation into a no-op.
 */
export function deletionCodec(env: Env): RequestStateCodec<PendingDeletion> | null {
  const key = env.REQUEST_STATE_KEY;
  // The codec throws RangeError below 32 bytes; checking here turns a 500 on
  // every destructive call into one clear refusal at the place that knows why.
  if (!key || new TextEncoder().encode(key).length < 32) return null;
  return createRequestStateCodec<PendingDeletion>({
    key,
    ttlSeconds: CONFIRM_TTL_SECONDS,
    bind: bindingFor,
  });
}
