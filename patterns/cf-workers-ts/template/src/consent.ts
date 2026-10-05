// The consent screen.
//
// This is the control that breaks the open-DCR attack described in
// src/oauth-state.ts. `/register` accepts any client, and CIMD clients are
// self-published HTTPS documents, so neither mechanism proves who is asking.
// The redirect host is always shown, and the client's own self-reported name is
// treated as decoration. A DCR client cannot change where the code is delivered
// (registration is POST-only, with no update endpoint), but a CIMD client CAN,
// by editing the metadata document its redirect_uris are read from. That is why
// the redirect URI is part of the consent key: moving the destination re-prompts
// rather than silently reusing an approval given for somewhere else.
//
// Consent is REQUIRED, not optional. A flag that can switch it off is worse than
// no flag, because it will be off. What keeps it from being tedious is that a
// decision is remembered per (user, client, resource, redirect URI) and is
// scope-bound: asking for more, or moving the destination, re-prompts.

const CSP = [
  "default-src 'none'",
  "style-src 'unsafe-inline'",
  "img-src 'none'",
  "frame-ancestors 'none'",
  "base-uri 'none'",
  // NOTE: form-action is deliberately absent. Approval 302s cross-origin to the
  // client's redirect URI, and some browser generations enforced form-action
  // against post-submit redirects, which broke the flow.
].join("; ");

const PAGE_HEADERS = {
  "content-type": "text/html; charset=utf-8",
  "content-security-policy": CSP,
  "cache-control": "no-store",
  "referrer-policy": "no-referrer",
  "x-content-type-options": "nosniff",
};

// Characters that are invisible or reorder what follows them. An MCP client's
// self-reported name and its requested scope strings both reach this page
// attacker-controlled, and HTML escaping alone does not stop
// `scope=\u202Eetirw:ECALPER` from rendering as something else entirely, or a C0
// control from truncating what a reader sees. Stripped, not escaped, because
// there is no legitimate use for them in a client name or a scope token.
// \u00AD is a soft hyphen, \u061C an Arabic letter mark, \u180E a Mongolian
// vowel separator, and \u2028/\u2029 line and paragraph separators that break
// a value across lines. Listing only \u202A-\u202E left all five in.
const INVISIBLE =
  /[\u0000-\u001F\u007F-\u009F\u00AD\u061C\u180E\u200B-\u200F\u2028\u2029\u202A-\u202E\u2060-\u2064\u2066-\u2069\uFEFF]/g;

// NOTE U+061C (Arabic letter mark) and U+180E (Mongolian vowel separator)
// are legitimate in Arabic and Mongolian text and are stripped here. That
// continues existing policy — U+200E/U+200F were already stripped — and the
// cost is altered rendering of a client NAME, never of user data.
const stripInvisible = (s: string): string => s.replace(INVISIBLE, "");

function esc(s: string): string {
  return stripInvisible(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

export interface ConsentView {
  nonce: string;
  csrf: string;
  /** Self-reported by the client. Attacker-controlled; length-capped. */
  clientName: string | null;
  /** Where the authorization code will be delivered. Always shown, because it is
   *  the thing the user is really being asked to judge — and it is part of the
   *  consent key, so a change to it re-prompts. */
  redirectHost: string;
  /** Present when the client id is a CIMD URL — a domain someone controls. */
  clientIdUrl: string | null;
  email: string;
  scopes: string[];
  resourceName: string;
}

// REPLACE: what each scope actually permits, in the user's terms, not the
// scope's own words. This is the only text on the page that tells a human what
// they are about to hand over, so write it as consequences ("permanently delete
// your X") rather than as a restatement of the scope name. A scope with no entry
// here falls back to a generic line — see scopeHelp() below.
// Exported so tests can assert the page renders THIS text, whatever you set it
// to, rather than hard-coding the placeholder strings — a test pinned to the
// placeholders fails the moment you do what the README tells you to.
export const SCOPE_HELP: Record<string, string> = {
  "REPLACE:read": "REPLACE — what a read token can see, in plain words",
  "REPLACE:write": "REPLACE — what a write token can change or destroy, in plain words",
};

/**
 * SCOPE_HELP is a plain object, so a bare index of `toString`, `constructor`,
 * `__proto__`, `valueOf` or `hasOwnProperty` returns an INHERITED non-string.
 * `??` does not fire on that and `esc()` then threw, turning the consent page
 * into a 500. `/authorize` rejects unknown scopes before they reach here, so
 * this is the belt to that braces — which is exactly why it must not itself
 * break. The same trap `scopeForTool()` already guards in src/scopes.ts.
 */
function scopeHelp(scope: string): string {
  const fallback = "Access granted by this scope";
  if (!Object.hasOwn(SCOPE_HELP, scope)) return fallback;
  const help = SCOPE_HELP[scope];
  return typeof help === "string" ? help : fallback;
}

export function renderConsent(v: ConsentView): Response {
  // Strip BEFORE slicing, then escape.
  //
  // Not for the reason first claimed here: an all-invisible name did NOT render a
  // blank row, because the template guard below is `${name ? ... : ""}` and an
  // empty string is falsy. What slicing-first actually did was let invisible
  // padding SUPPRESS a real name — "\u200B" x 80 followed by "Evil Corp" was cut
  // at 80 characters, stripped to nothing, and the row vanished, hiding a name
  // the user should have been shown. Stripping first surfaces it.
  //
  // (A visibly-blank row is still reachable via characters no class catches —
  // U+3164, U+2800, U+00A0. Closing that needs a "contains a visible character"
  // test, not a longer deny-list.)
  const cleaned = v.clientName ? stripInvisible(v.clientName).slice(0, 80) : "";
  const name = cleaned ? esc(cleaned) : null;
  // Cap the list. /authorize now rejects unknown scopes, so this should never
  // trigger — it is the belt to that braces, because the alternative is a page
  // where the meaningful scope sits at position 301 with the Approve button
  // below it.
  const shown = v.scopes.slice(0, 12);
  const overflow = v.scopes.length - shown.length;
  const rows = shown
    .map(
      (s) =>
        `<li><code>${esc(s)}</code><span>${esc(scopeHelp(s))}</span></li>`,
    )
    .join("") +
    (overflow > 0 ? `<li><strong>+${overflow} more scope(s) not shown</strong></li>` : "");

  const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Authorize access — ${esc(v.resourceName)}</title>
<style>
:root{color-scheme:light dark}
body{font:16px/1.5 system-ui,-apple-system,sans-serif;max-width:34rem;margin:3rem auto;padding:0 1.25rem}
h1{font-size:1.35rem;margin:0 0 1.25rem}
dl{margin:0 0 1.5rem;padding:1rem 1.25rem;border:1px solid #8884;border-radius:.5rem}
dt{font-size:.78rem;text-transform:uppercase;letter-spacing:.04em;opacity:.7;margin-top:.75rem}
dt:first-child{margin-top:0}
dd{margin:.15rem 0 0;font-weight:600;word-break:break-all}
ul{list-style:none;padding:0;margin:0 0 1.75rem}
li{padding:.6rem 0;border-top:1px solid #8884}
li code{font-weight:600}
li span{display:block;opacity:.75;font-size:.9rem}
form{display:inline}
button{font:inherit;padding:.6rem 1.1rem;border-radius:.4rem;border:1px solid #8886;cursor:pointer}
button.primary{background:#1a56db;color:#fff;border-color:#1a56db}
.note{margin-top:1.75rem;font-size:.85rem;opacity:.7}
</style></head><body>
<h1>Authorize access to ${esc(v.resourceName)}</h1>
<dl>
  ${name ? `<dt>Application (self-reported)</dt><dd>${name}</dd>` : ""}
  <dt>Will receive the authorization at</dt><dd>${esc(v.redirectHost)}</dd>
  ${v.clientIdUrl ? `<dt>Client identity document</dt><dd>${esc(v.clientIdUrl)}</dd>` : ""}
  <dt>Signed in as</dt><dd>${esc(v.email)}</dd>
</dl>
<ul>${rows}</ul>
<form method="POST" action="/consent">
  <input type="hidden" name="nonce" value="${esc(v.nonce)}">
  <input type="hidden" name="csrf" value="${esc(v.csrf)}">
  <button type="submit" name="decision" value="approve" class="primary">Approve</button>
  <button type="submit" name="decision" value="deny">Deny</button>
</form>
<p class="note">Only approve if you recognise the destination above. An application can
call itself anything; what it cannot do is change where the authorization is delivered
without asking you again.</p>
<p class="note">Approving is remembered for this application, at this destination, for
these permissions, for up to 90 days. Asking for more, or moving the destination, brings
you back here.</p>
</body></html>`;

  return new Response(html, { status: 200, headers: PAGE_HEADERS });
}

export function renderDenied(): Response {
  return new Response(
    `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Access denied</title></head>
<body style="font:16px/1.5 system-ui,sans-serif;max-width:34rem;margin:3rem auto;padding:0 1.25rem">
<h1 style="font-size:1.35rem">Access denied</h1>
<p>No authorization was issued. You can close this page.</p></body></html>`,
    { status: 200, headers: PAGE_HEADERS },
  );
}

/** A CIMD client id is an HTTPS URL; anything else is an opaque DCR id. */
export function clientIdAsUrl(clientId: string): string | null {
  try {
    const u = new URL(clientId);
    return u.protocol === "https:" ? u.href : null;
  } catch {
    return null;
  }
}

/**
 * The one interpolated value on the consent page with no length cap of its own —
 * `clientName` caps at 80 and `scopes` at 12. Now that full paths and queries are
 * shown under `word-break:break-all`, a long redirect URI could push the Approve
 * button below the fold, which is a consent page that cannot be read before it is
 * used. Escaping is handled separately by esc().
 */
function capped(value: string): string {
  return value.length <= 120 ? value : `${value.slice(0, 119)}\u2026`;
}

/**
 * The destination as the user should SEE it.
 *
 * `redirectHostOf` showed the host alone, while the consent key is computed from
 * the full URI — so `https://app.example/cb` and `https://app.example/evil-cb`
 * were two distinct records rendering as one identical string. The page's own
 * closing line promises "at this destination... moving the destination brings you
 * back here", and the re-prompt was visually indistinguishable from the first
 * prompt: the user was asked to judge a change they could not see.
 *
 * Loopback still says "(any port)" and drops the port, because the KEY drops it
 * too (RFC 8252, and the provider matches that way). The page must describe
 * exactly what approving approves — no narrower, no wider.
 */
export function redirectDisplay(redirectUri: string): string {
  try {
    const u = new URL(redirectUri);
    const loopback =
      /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(u.hostname) ||
      u.hostname === "[::1]" ||
      u.hostname.toLowerCase() === "localhost";
    // `u.hash` included: two fragment routes on one path are two distinct
    // consent records, and without it they render identically.
    const path = `${u.pathname}${u.search}${u.hash}`;
    return capped(
      loopback
        ? `${u.protocol}//${u.hostname} (any port)${path}`
        : `${u.protocol}//${u.host}${path}`,
    );
  } catch {
    return capped(redirectUri);
  }
}

export function redirectHostOf(redirectUri: string): string {
  try {
    const u = new URL(redirectUri);
    // For loopback, say "any port" rather than showing one. The remembered
    // decision is keyed WITHOUT the port — it has to be, because the provider
    // matches loopback URIs that way (RFC 8252) — so displaying a single
    // ephemeral port would state a narrower grant than the one being given. The
    // page must not describe less than what approving actually approves.
    const loopback =
      /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(u.hostname) ||
      u.hostname === "[::1]" ||
      u.hostname.toLowerCase() === "localhost";
    if (loopback) return `${u.hostname} (any port)`;
    return u.host || redirectUri;
  } catch {
    return redirectUri;
  }
}
