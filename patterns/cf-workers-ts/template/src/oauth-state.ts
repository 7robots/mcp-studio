// Server-side OAuth state, and the pending/remembered records the consent gate
// needs. Nothing here is ever handed to the browser except an opaque UUID and a
// cookie value.
//
// WHY THIS EXISTS. The previous implementation put the parsed authorization
// request straight into the `state` parameter as `btoa(JSON.stringify(req))` —
// unauthenticated, and trusted verbatim when it came back. Combined with an open
// `/register`, that was an account-takeover chain:
//
//   1. attacker registers a client via DCR with redirect_uri https://evil/cb
//   2. attacker forges a state naming that client and its redirect
//   3. victim (already signed in to Okta) loads the crafted authorize URL
//   4. /callback resolves the VICTIM's identity, then completes authorization
//      for the ATTACKER's client — delivering the code to https://evil/cb
//   5. attacker redeems it (they chose the PKCE verifier) and holds a token
//      acting as the victim
//
// `completeAuthorization` does re-validate the redirect URI against the client
// named in the state, which is exactly why step 1 matters: the attacker names
// their own registered client, for which their own redirect IS valid.
//
// The fix has two halves, and both are needed:
//   - the authorization request never leaves the server (this file), so there is
//     nothing to forge; and
//   - a consent screen (src/consent.ts) shows the redirect host, so even a
//     correctly-formed request for an unfamiliar client has to be approved.
//
// Ported from the equivalent in inno-platform, which closed the same gap.

const STATE_PREFIX = "oauth:state:";
const PENDING_PREFIX = "oauth:pending:";
const CONSENT_PREFIX = "oauth:consent:";

/** Long enough for an Okta login, short enough to bound a leaked nonce. */
export const STATE_TTL_SECONDS = 600;
export const PENDING_TTL_SECONDS = 600;
/** Remembering is UX only — the scope check still runs on every authorization. */
export const CONSENT_TTL_SECONDS = 90 * 24 * 60 * 60;

/** `__Host-` forbids a Domain attribute and requires Secure + Path=/, so a
 *  sibling host cannot set or overwrite it. */
// Per-flow, like the consent cookie. A single fixed name meant two concurrent
// authorizations in one browser clobbered each other, and a lure to /authorize
// was a cross-site DoS of a victim's legitimate pending flow.
export const STATE_COOKIE_PREFIX = "__Host-REPLACE_oauth_csrf-";
export const stateCookieName = (state: string): string => STATE_COOKIE_PREFIX + state;
export const CONSENT_COOKIE_PREFIX = "__Host-REPLACE_consent_csrf-";

const COOKIE_ATTRS = "HttpOnly; Secure; SameSite=Lax; Path=/";

// A nonce becomes part of a KV key AND part of a cookie name, so it is shape
// checked before either. An oversized value would otherwise trip KV's key limit
// as a 500 rather than a 400.
const NONCE_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
export const isNonce = (v: string | undefined | null): v is string => !!v && NONCE_RE.test(v);

export function setCookie(name: string, value: string, maxAge: number): string {
  return `${name}=${value}; ${COOKIE_ATTRS}; Max-Age=${maxAge}`;
}

export function clearCookie(name: string): string {
  return `${name}=; ${COOKIE_ATTRS}; Max-Age=0`;
}

export function readCookie(header: string | null, name: string): string | null {
  if (!header) return null;
  for (const part of header.split(";")) {
    const eq = part.indexOf("=");
    if (eq === -1) continue;
    if (part.slice(0, eq).trim() === name) return part.slice(eq + 1).trim();
  }
  return null;
}

/**
 * Constant-time string compare, in the sense that matters here: for two values
 * of equal length the work done does not depend on WHERE they first differ, so
 * a caller cannot narrow a secret byte by byte from response timing.
 *
 * Honest scoping: this is defence in depth against a threat that does not really
 * apply. The compare sits behind an awaited KV read whose latency varies by
 * orders of magnitude more than a 36-byte loop, `readCookie` already compares
 * cookie NAMES with plain `===`, and the secret is a 122-bit UUID inside a
 * 600-second window with no distinguishing error. Hand-rolling costs nothing and
 * keeps the wrapper testable — `crypto.subtle.timingSafeEqual` is a Workers
 * extension absent from the Node pool, and it throws on unequal lengths, so a
 * wrapper is needed either way and the wrapper is where a bug would live.
 *
 * Note the operands are NOT always server-generated: on /consent one is the
 * submitted form field and the other the submitted cookie. The length check
 * therefore reveals only the server value's length, which is a public constant.
 */
export function timingSafeEqualStr(a: string, b: string): boolean {
  const ab = new TextEncoder().encode(a);
  const bb = new TextEncoder().encode(b);
  if (ab.byteLength !== bb.byteLength) return false;
  let diff = 0;
  for (let i = 0; i < ab.byteLength; i++) diff |= ab[i] ^ bb[i];
  return diff === 0;
}

export const newNonce = (): string => crypto.randomUUID();

// --- the authorization-request record -------------------------------------

export interface StateRecord<Req> {
  req: Req;
  /** Second secret, held only in the `__Host-` cookie. Binds the state to the
   *  browser that started the flow, which is what a forgeable state lacked. */
  csrf: string;
  /**
   * Flow facts decided at /authorize that /callback needs and cannot re-derive.
   *
   * `forceConsent` comes from OIDC `prompt=consent` on the ORIGINAL request. By
   * the time /callback runs, the query string is Okta's redirect — `code` and
   * `state` — so reading `prompt` there finds nothing. It travels here rather
   * than as an extra property on `req`, because `req` is handed to
   * `completeAuthorization` and should stay exactly what the provider parsed.
   */
  forceConsent?: boolean;
}

export async function putState<Req>(
  kv: KVNamespace,
  req: Req,
  // Spread FIRST below, so a future key in here can never clobber `req` or the
  // `csrf` secret. Nothing collides today; the ordering is what keeps that true.
  flow: { forceConsent?: boolean } = {},
): Promise<{ state: string; csrf: string }> {
  const state = newNonce();
  const csrf = newNonce();
  await kv.put(
    STATE_PREFIX + state,
    JSON.stringify({ ...flow, req, csrf } satisfies StateRecord<Req>),
    {
      expirationTtl: STATE_TTL_SECONDS,
    },
  );
  return { state, csrf };
}

/**
 * Read the state record and verify the browser presented the matching cookie.
 *
 * ORDERING IS LOAD-BEARING: the record is deleted only AFTER the CSRF check
 * passes. A valid-state/wrong-cookie attempt therefore leaves the record intact
 * (so the legitimate browser can still finish), while a successful read consumes
 * it (so a code cannot be replayed through the same state).
 */
export async function takeState<Req>(
  kv: KVNamespace,
  state: string | undefined,
  cookieHeader: string | null,
): Promise<{ ok: true; req: Req; forceConsent?: boolean } | { ok: false; reason: string }> {
  if (!isNonce(state)) return { ok: false, reason: "malformed state" };
  const cookie = readCookie(cookieHeader, stateCookieName(state));
  const raw = await kv.get<StateRecord<Req>>(STATE_PREFIX + state, "json");
  if (!raw || !cookie || !timingSafeEqualStr(raw.csrf, cookie)) {
    return { ok: false, reason: "invalid or expired state" };
  }
  await kv.delete(STATE_PREFIX + state);
  return { ok: true, req: raw.req, forceConsent: raw.forceConsent };
}

// --- the pending-consent record -------------------------------------------

export interface PendingRecord<Req> {
  req: Req;
  userId: string;
  email: string;
  name?: string;
  oktaAccessToken: string;
  scopes: string[];
  consentKey: string;
  /**
   * True when this page was reached via OIDC `prompt=consent` — the user
   * deliberately came back to reconsider, rather than being interrupted by a
   * scope escalation. Only then does DENYING erase the remembered decision.
   */
  forceConsent?: boolean;
  csrf: string;
}

export async function putPending<Req>(
  kv: KVNamespace,
  record: Omit<PendingRecord<Req>, "csrf">,
): Promise<{ nonce: string; csrf: string }> {
  const nonce = newNonce();
  const csrf = newNonce();
  await kv.put(PENDING_PREFIX + nonce, JSON.stringify({ ...record, csrf }), {
    expirationTtl: PENDING_TTL_SECONDS,
  });
  return { nonce, csrf };
}

export const getPending = <Req>(kv: KVNamespace, nonce: string) =>
  kv.get<PendingRecord<Req>>(PENDING_PREFIX + nonce, "json");

export const deletePending = (kv: KVNamespace, nonce: string) =>
  kv.delete(PENDING_PREFIX + nonce);

// --- the remembered decision ---------------------------------------------

/**
 * The redirect URI as the consent key must see it.
 *
 * The provider's own `isValidRedirectUri` IGNORES the port for loopback URIs —
 * RFC 8252, because a native app cannot reserve one — and compares only
 * protocol, hostname, pathname and search. So a client registered at
 * `http://127.0.0.1:5000/cb` is validly authorized at ANY port.
 *
 * Keying on the full URI would make the memory stricter than the rule that
 * admitted the request: an ephemeral-port local client would be re-prompted on
 * every launch, and the destination it asks the user to judge would read
 * `127.0.0.1:52222` — a different unjudgeable number every time. Consent
 * fatigue is the failure mode consent actually has, so the key canonicalizes
 * exactly what the provider canonicalizes, and only for loopback. Every other
 * destination is keyed on the full URI, where an exact match is also what the
 * provider requires.
 */
export function consentRedirectKey(redirectUri: string): string {
  let u: URL;
  try {
    u = new URL(redirectUri);
  } catch {
    // The provider validated this before it reached us, so this is unreachable.
    // Key on the raw value rather than widening anything on an unparsed URI.
    return redirectUri;
  }
  // Lowercase for the PREDICATE only, because that is what isLoopbackUri does —
  // and then keep the hostname VERBATIM in the key, because isValidRedirectUri
  // compares `hostname` with no case folding. WHATWG URL case-normalizes the
  // host only for special schemes, so under a custom native-app scheme
  // (`myapp://LOCALHOST/cb`) lowercasing here would collapse two spellings the
  // provider treats as distinct. Harmless in effect — host case does not change
  // where the code is delivered — but this function exists to be exactly as
  // loose as the matcher and no looser.
  const loopback =
    /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(u.hostname) ||
    u.hostname === "[::1]" ||
    u.hostname.toLowerCase() === "localhost";
  if (!loopback) return redirectUri;
  return `${u.protocol}//${u.hostname}${u.pathname}${u.search}`;
}

/**
 * Per (user, client, resource, REDIRECT URI).
 *
 * The resource is in the key so approving this server does not silently approve
 * another. The redirect URI is in the key because a CIMD client's
 * `redirect_uris` come from a document re-fetched on every authorization —
 * so without it, whoever controls that document could move the delivery host
 * after approval and, for the remaining 90 days, have codes minted with no
 * re-prompt. The redirect host is the one thing the consent page asks the user
 * to judge, so a decision must not outlive a change to it. A DCR client cannot
 * move its own (registration is POST-only, no update endpoint), but keying on
 * it costs nothing and covers both.
 *
 * The value keyed on is `consentRedirectKey(redirectUri)`, not the raw URI —
 * see there for why loopback ports must be dropped.
 */
/**
 * Named rather than positional. Four `string` parameters typechecked whichever
 * order they were passed in, and a swap would have produced a different-but-valid
 * key — self-consistent, so no test could catch it, and the failure mode is either
 * collapsing two users' consent or splitting one user's across two records. The
 * object makes the call site say which is which.
 */
export interface ConsentIdentity {
  sub: string;
  clientId: string;
  resource: string;
  redirectUri: string;
}

export const consentKey = ({ sub, clientId, resource, redirectUri }: ConsentIdentity): string =>
  `${CONSENT_PREFIX}${encodeURIComponent(sub)}:${encodeURIComponent(clientId)}:${encodeURIComponent(resource)}:${encodeURIComponent(consentRedirectKey(redirectUri))}`;

export async function rememberConsent(
  kv: KVNamespace,
  key: string,
  scopes: string[],
): Promise<void> {
  await kv.put(key, JSON.stringify({ scopes }), { expirationTtl: CONSENT_TTL_SECONDS });
}

/**
 * Drop a remembered decision.
 *
 * There was no way to do this short of the 90-day TTL, which made "deny" a
 * strictly weaker action than "approve": approving was durable, denying was
 * momentary, and a user who changed their mind had no move. Revoking the OAuth
 * grant does not help either — the provider revokes grants, and this record is
 * not a grant, so the next authorization for the same (user, client, resource,
 * destination) would complete silently for the rest of the 90 days.
 */
export async function forgetConsent(kv: KVNamespace, key: string): Promise<void> {
  await kv.delete(key);
}

/**
 * True when a remembered decision already covers everything being asked for.
 * Scope-bound on purpose: a later request for MORE re-prompts, which is exactly
 * the moment a user would want to see the screen again.
 */
export async function consentCovers(
  kv: KVNamespace,
  key: string,
  wanted: string[],
): Promise<boolean> {
  const rec = await kv.get<{ scopes: string[] }>(key, "json");
  if (!rec) return false;
  const approved = new Set(rec.scopes ?? []);
  return wanted.every((s) => approved.has(s));
}
