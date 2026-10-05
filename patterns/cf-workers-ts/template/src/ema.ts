// Enterprise-Managed Authorization (MCP extension
// `io.modelcontextprotocol/enterprise-managed-authorization`).
//
// WHAT THIS CHANGES. In the ordinary flow each user authorizes each MCP server
// themselves, and this server's consent screen is where that happens. Under EMA
// the enterprise IdP decides instead: the MCP client authenticates the human,
// exchanges that identity for an Identity Assertion JWT Authorization Grant
// (ID-JAG) at the IdP, and presents the ID-JAG here. Access is granted because
// the organisation's policy says so — no per-server browser consent, and
// revoking at the IdP takes effect everywhere at once.
//
// WHAT WE IMPLEMENT. `workers-oauth-provider` (0.10.3+) ships the grant itself:
// `typ=oauth-id-jag+jwt` enforcement (RFC 8725 §3.11), the grant profile
// `urn:ietf:params:oauth:grant-profile:id-jag`, signature verification against
// the issuer's JWKS via WebCrypto, and bounded clock skew and assertion
// lifetime. Presence of the `enterpriseManagedAuthorization` option enables it —
// there is deliberately no `enabled` flag, so a fully-configured-but-forgotten
// one cannot silently disable it. What is left to us is the policy: WHICH
// issuers we trust, and HOW their claims map onto a local grant.
//
// STATUS. The library labels this "Experimental support" because the MCP
// extension and the underlying OAuth drafts are still moving. Nothing here is
// exercised until an IdP actually mints an ID-JAG, which for Okta requires Cross
// App Access — so this is implemented and unit-tested against a local keypair,
// not verified end to end. See docs/plans/mcp-2026-07-28-adoption.md.

import type {
  EmaClaimsMapperInput,
  EmaClaimsMapperResult,
  EmaTrustedIssuer,
  EmaTrustedIssuerResolverInput,
} from "@cloudflare/workers-oauth-provider";

import { SUPPORTED_SCOPES } from "./scopes";

/**
 * Parses `EMA_TRUSTED_ISSUERS`: a space- or comma-separated list of
 * `issuer=jwksUri` pairs. Config rather than code so adding a tenant is a var
 * change, and a hardcoded closure rather than tenant-writable storage because
 * `jwksUri` is fetched outbound by the AS. The library follows redirects with no
 * host allowlist and no IP-literal rejection, so the real controls are that this
 * value is operator config (never tenant input) and that it must share the
 * issuer's origin.
 */
export function parseTrustedIssuers(raw: string | undefined): EmaTrustedIssuer[] {
  if (!raw) return [];
  const pairs = raw.split(/[\s,]+/).filter(Boolean);
  const out = pairs
    .map((pair): EmaTrustedIssuer | null => {
      const eq = pair.indexOf("=");
      if (eq <= 0) return null;
      const issuer = pair.slice(0, eq);
      const jwksUri = pair.slice(eq + 1);
      // Reject anything that is not plainly an HTTPS URL, both because the
      // library requires it and to keep a typo from becoming an outbound fetch
      // to somewhere unintended.
      try {
        const j = new URL(jwksUri);
        const i = new URL(issuer);
        if (j.protocol !== "https:" || i.protocol !== "https:") return null;
        // Same-origin is the actual control. An `https:` check alone stops
        // nothing: the library's JWKS fetch follows redirects with no host
        // allowlist and no IP-literal rejection, so a bare scheme check would
        // still permit an outbound fetch anywhere. Requiring the JWKS to live
        // on the issuer's own origin means a trusted issuer can only point us
        // at itself.
        if (j.origin !== i.origin) return null;
      } catch {
        return null;
      }
      return { issuer, jwksUri, algorithms: ["RS256"] };
    })
    .filter((x) => x !== null);
  // A typo must not silently shrink the trust set. Dropping one bad pair out of
  // three would leave a PARTIAL set with no signal, and dropping the only pair
  // would disable the grant entirely — the exact silent-disable the library
  // avoids by having no `enabled` flag.
  if (out.length !== pairs.length) {
    throw new TypeError(
      `EMA_TRUSTED_ISSUERS: ${pairs.length - out.length} of ${pairs.length} entries are not ` +
        `valid \`https-issuer=https-jwks-uri\` pairs whose origins match`,
    );
  }
  return out;
}

export function trustedIssuerResolver(issuers: EmaTrustedIssuer[]) {
  return async (input: EmaTrustedIssuerResolverInput): Promise<EmaTrustedIssuer | null> =>
    issuers.find((i) => i.issuer === input.iss) ?? null;
}

/**
 * Maps validated ID-JAG claims onto a local grant.
 *
 * SCOPES COME FROM THE ASSERTION, NEVER FROM THE REQUEST. This is the whole
 * authorization decision, so it is worth being exact about why
 * `input.requestedScope` cannot be trusted:
 *
 *   parseEmaScopeParam (oauth-provider.js:1144) downscopes the client's
 *   requested `scope` to the assertion's scopes ONLY under
 *   `if (assertionScopes.length > 0)`. An ID-JAG with no `scope` claim therefore
 *   gives assertionScopes = [], the filter is skipped, and the CLIENT's
 *   requested scope reaches us verbatim.
 *
 * So a client presenting a scope-less assertion and asking for `REPLACE:write`
 * would have received write access the IdP never authorized — and with
 * `allowPublicClients: true` the assertion is the only credential. We therefore
 * read `claims.scope` directly and deny when it is absent: an enterprise grant
 * with no stated scope is not a grant.
 *
 * `requestedScope` is still honoured as a CEILING the client may lower itself
 * to (least privilege), never as a source of authority.
 *
 * Account linking: `sub` is recorded as the primary stable identifier and
 * `email` alongside it, per the extension's guidance. NOTE that this records
 * them rather than reconciling them — the same human arriving interactively and
 * via EMA is currently two local identities. That is invisible today because
 * reads are unfiltered and `owner_sub` is audit-only, and it becomes real the
 * moment ownership is enforced.
 */
export async function mapEmaClaims(
  input: EmaClaimsMapperInput<Env>,
): Promise<EmaClaimsMapperResult | null> {
  const { claims, requestedScope, clientInfo } = input;
  if (!claims.sub) return null;
  // The library rejects a userId containing ':' (it would collide with the
  // opaque token format `userId:grantId:secret`). Deny cleanly rather than
  // letting it surface as an error.
  if (claims.sub.includes(":")) return null;

  // The asserted scopes are the authority. Absent claim => no grant.
  const asserted =
    typeof claims.scope === "string" ? claims.scope.split(" ").filter(Boolean) : null;
  if (!asserted || asserted.length === 0) return null;

  const supported = new Set<string>(SUPPORTED_SCOPES);
  const granted = asserted
    // Only what this server implements — an IdP naming `admin:everything`
    // cannot produce a grant that looks broader than the server is.
    .filter((s) => supported.has(s))
    // ...and only what the client asked for, so a client may voluntarily take
    // less than the IdP allowed. `requestedScope` narrows; it never widens.
    .filter((s) => requestedScope.includes(s));

  // Deny rather than silently substituting a default. A default here would mean
  // an assertion authorizing nothing we implement still yields a usable token.
  if (granted.length === 0) return null;

  return {
    userId: claims.sub,
    metadata: { label: claims.email ?? claims.sub },
    scope: granted,
    props: {
      okta_sub: claims.sub,
      email: claims.email ?? claims.sub,
      // No upstream Okta access token exists on this path — the client never
      // gave us one, and the ID-JAG is not a credential for calling Okta.
      okta_access_token: "",
      scopes: granted,
      client_id: clientInfo.clientId,
      // How this grant was obtained. "the IdP's policy says this person may use
      // this server" is a different statement from "this person clicked
      // Approve", and downstream code should be able to tell them apart.
      enterprise: true,
      enterprise_issuer: claims.iss,
    },
  };
}
