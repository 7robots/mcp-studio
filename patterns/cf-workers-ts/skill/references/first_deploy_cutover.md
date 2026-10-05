# First deploy of a generated server — ordered checklist

Write nothing new while running this. Every step has a rollback, and the order is
load-bearing: several of these are one-way once a client has a token.

A template that is instantiated without a checklist like this one tends never to
be deployed, or to be deployed in the wrong order. Do not skip to `wrangler
deploy`.

## Before you start

- [ ] Rendered with `mcp-studio pattern render --out ~/GitHub/<name>`, and
  `npm run ci` green in the generated server.
- [ ] `npm run check:placeholders` clean — and **rewritten into the generated
  repo's `ci` script** (`"ci": "npm run conformance && npm run check:placeholders && …"`),
  so it runs on every build, not once. The template keeps it out of its own `ci`
  because the template's CI has to pass with its placeholders in place; a
  generated repo must not. The checker also fails a wrangler route whose host
  disagrees with PUBLIC_MCP_URL.
- [ ] `mcp-studio pattern lint <repo>` clean: pins, lockfile (all `@img/sharp-*`
  entries), wrangler flags and vars, no surviving placeholders, no legacy markers.
- [ ] `src/skill.ts` reads as a description of *this* server. No test asserts it.
- [ ] Every `SCOPE_HELP` line says what that scope actually permits, in words a
      non-engineer can judge. The consent page is the authorization control; a
      scope row that names itself and nothing else is not consent.

## 1. Okta objects, before any deploy

**No new Okta application.** Every server on this pattern shares one interactive
client (`{{okta_client_id}}`) on one custom authorization server
(`{{okta_issuer}}`) — the rendered `wrangler.toml` already names both. A new
server reuses both. What it needs is two new scopes and one more redirect URI on
objects that already exist. (The instance's `studio.toml` `[identity.okta]`
records the authorization server, both apps and the policy-rule ids.)

This matters beyond convenience: a second app would mean a second client secret,
a second consent surface, and per-app policy rules to keep in step — the same
drift the shared-AS decision accepted the opposite trade to avoid.

A server whose scopes do not exist on the authorization server has no working M2M
path at all, and a hard cutover leaves it with none.

**Do this step over the API, not the Admin Console:** the `okta-admin` skill's
new-server recipe, if installed, creates both scopes, adds them to both policy
rules, and registers the redirect URI (step 4's Okta line) against the fleet's
object IDs. Each write prompts {{operator_name}}. The checklist below says what
must be true; the recipe is how.

- [ ] Create both scopes on the EXISTING custom AS (`<name>:read`,
      `<name>:write`).
      **Neither may be a default scope** — a default scope is added to
      `client_credentials` tokens whether or not it was requested, so it
      distinguishes nothing.
- [ ] Add both to **both** access-policy rules that grant this fleet's scopes
      (interactive "Allow Authorization Code" and M2M "Bearer Token Access
      Rule"),
      **alongside the OIDC scopes** (`openid profile email offline_access`). Okta
      matches a rule only when EVERY requested scope is in that rule's list, and
      this server forwards its MCP scopes upstream — so a rule carrying the MCP
      scopes but not the OIDC ones will not match, and interactive login fails
      with `access_denied`.
- [ ] Verify before deploying, with the M2M app's own credential:
      ```sh
      curl -s -X POST "{{okta_issuer}}/v1/token" -u "$CID:$SECRET" \
        -d grant_type=client_credentials --data-urlencode "scope=<name>:read"
      ```
      A token back means the scope exists and is grantable. `access_denied` means
      the policy rule does not match; `invalid_scope` means the scope is not
      there. For any other failure, the Okta System Log names the cause (the
      `okta-admin` skill's failed-login recipe, if installed).

**Rollback:** remove the scopes from the rule. Nothing is deployed yet, so there
is nothing to break.

## 2. Bindings and secrets

- [ ] Create the D1 database and KV namespace; put their real ids in
      `wrangler.toml`. Deploying with placeholder ids fails fast, which is the
      intended behaviour.
- [ ] Apply the schema. **Commit it as `migrations/`** rather than applying it by
      hand — a schema that exists only in the deployed database once hid every
      database-touching test in a repo from itself for months (the empty test
      database answered `no such table`, and the tools turned that into
      passing error results).
- [ ] Set the secrets, value-unseen, from a 1Password item in the
      `{{op_vault}}` vault named for the deploy target: `OKTA_CLIENT_SECRET`
      and `REQUEST_STATE_KEY`. (`COOKIE_ENCRYPTION_KEY` is read by nothing on
      provider 0.10.3+.)
- [ ] `npx wrangler deploy --dry-run` and read the binding table it prints.

**Rollback:** nothing is serving traffic yet. Delete the bindings.

## 3. Deploy, then verify before wiring DNS

Deploy to the `workers.dev` hostname first. It is not a URL anyone has, so a
broken build harms nothing.

- [ ] `npm run ci && npx wrangler deploy`
- [ ] `GET /.well-known/oauth-protected-resource/mcp` returns your resource
      identifier and your advertised scopes.
- [ ] `GET /.well-known/oauth-authorization-server` reports
      `client_id_metadata_document_supported: true`. If it says false you are
      missing `global_fetch_strictly_public`, and CIMD — and therefore EMA — will
      not work, with no error saying so.
- [ ] An unauthenticated `POST /mcp` returns 401 with a `WWW-Authenticate`
      challenge naming the scope.
- [ ] An M2M token for `<name>:read` reaches `tools/list`, and `whoami`
      reports `auth_path: "m2m"`.
- [ ] A read-only token is REFUSED by the write tool, with the refusal naming the
      scope needed.

**Rollback:** `npx wrangler rollback`, or delete the Worker. No DNS points at it.

## 4. Custom domain and the Okta redirect URI

This is the step with a real ordering constraint, because a redirect URI is
matched exactly.

- [ ] Add the custom domain to the Worker. Confirm the health endpoint answers on
      it before touching Okta.
- [ ] **Add the new callback URL to the shared Okta app's redirect URIs**
      (`okta-admin` recipe, step 3 — skip if step 1 already did it),
      keeping every existing one. The app already holds one per fleet server;
      adding another affects no other server, and there is no window in which
      only one works.
- [ ] Set `PUBLIC_MCP_URL` to the custom-domain `/mcp` URL and redeploy. Note
      that setting the resource identifier switches on exact RFC 8707 audience
      pinning, so **the first request after this deploy 401s for any token minted
      before it**. A retrying client self-heals on refresh; a non-retrying one
      looks broken until re-authorization. A brand-new server has no grants, so
      this is free — do it now rather than later.
- [ ] Complete one real interactive login through a real client. Read the consent
      page as a user would: does it name the destination you expect, and does each
      scope row say what it permits?
- [ ] If you registered a `workers.dev` callback in step 3 to test the flow
      early, remove it now. A first deploy usually has none — this line is here
      because a DOMAIN MIGRATION does, and that is the case where removing the old
      URI before a successful login on the new one locks everyone out.

**Rollback:** point `PUBLIC_MCP_URL` back and redeploy, leaving every redirect URI
registered. Remove a URI only after a real login has succeeded without it.

## 5. Register with the gateway

- [ ] `register_server` with the custom-domain `/mcp` URL — **never** the
      `workers.dev` one. A Worker cannot fetch another Worker on the same account
      without `global_fetch_strictly_public`, and the failure is Cloudflare error
      1042, which reads like an auth problem. Servers have sat quarantined in a
      gateway for exactly this.
- [ ] `call_tool` through the gateway returns real data.
- [ ] `list_servers` shows it healthy.

**Rollback:** `unregister_server`. The server stays independently callable —
the gateway is a convenience layer, not a dependency.

## 6. Wire CI last

- [ ] Create the Workers Builds trigger with `build_command: npm run ci`. Until
      this exists, nothing runs the tests before a deploy. One Worker once went
      three months without it while its README claimed otherwise.
- [ ] Push a trivial commit and confirm the build ran and gated.
- [ ] Add the repo to the fleet (the instance's `studio.toml`) and
      `mcp-studio pattern bless <repo>`; from then on the conformance gate in its
      `ci` is live. `mcp-studio fleet status` should show it deployed, live,
      registered and conforming.

## After

- [ ] Record anything that did not work here: in the server repo it happened
      in, and — if it is a lesson about the pattern — in the pack's skill or
      template, with a `CHANGELOG.md` entry.
