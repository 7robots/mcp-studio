# Agent Guide: Building MCP Servers on Cloudflare Workers

For Claude Code agents tasked with creating a new MCP server (or converting an existing FastMCP one) using the Cloudflare Workers + stateless-MCP + Okta OAuth pattern. The MCP layer is stateless per spec 2026-07-28: no sessions, no Durable Object, a fresh `McpServer` per request.

**Audience**: You (the agent) own the build/deploy loop. The human reviews decisions and handles out-of-band steps (GitHub App authorization, onboarding a new Okta org).

## Prerequisites checklist

Before starting, verify:

- [ ] Cloudflare account with the **Cloudflare MCP server** connected — provides `mcp__cloudflare__execute` and `mcp__cloudflare__search`. Check by calling `mcp__cloudflare__execute` with a trivial `GET /user` request.
- [ ] `gh` CLI authenticated for the target GitHub org (`{{github_org}}`). Check with `gh auth status`.
- [ ] `mcp-studio` on the PATH, pointed at this instance (`mcp-studio pattern show` prints the pack and its version).
- [ ] An Okta tenant exists. For the interactive flow, the shared Okta application (Web Application type, client id `{{okta_client_id}}`) on the authorization server `{{okta_issuer}}`. For M2M, an API Services app with `client_credentials` and **this server's own** scopes (`<slug>:read` / `<slug>:write`). The one coarse fleet-wide `mcp-access` scope was retired 2026-08-21 — enterprise policy granularity is capped by scope granularity, and on a shared authorization server scopes are the only thing separating one server from another.
- [ ] If converting an existing MCP server: read the source first to understand the tool surface, parameter schemas, and data layer.

If any prerequisite is missing, surface it to the user before proceeding.

## Workflow: agentic, remote-only

Every operation maps to a specific tool. **Do not run `wrangler dev` or `wrangler deploy` from the user's terminal.** All deploys happen via Workers Builds (remote, automatic on git push).

| Operation | Tool / Method |
| --- | --- |
| Scaffold a new server from the pattern | `mcp-studio pattern render --out ~/GitHub/<server-name>` |
| Create / upload Worker script | `mcp__cloudflare__execute` (PUT `/workers/scripts/{name}`) |
| Set Worker secrets | `printf '%s' "$(op read "op://{{op_vault}}/…")" \| npx wrangler secret put <NAME> --name <worker>` (1Password pipe, per the `cloudflare` skill; `printf` strips the newline) |
| Create KV namespace | `mcp__cloudflare__execute` (POST `/storage/kv/namespaces`) |
| Create / list D1 | `mcp__cloudflare__execute` (`/d1/database`) |
| Discover endpoints | `mcp__cloudflare__search` |
| Connect repo / create build trigger | `mcp__cloudflare__execute` (Builds APIs) |
| Trigger / monitor build | `mcp__cloudflare__execute` (Builds APIs) |
| Pull build logs | `mcp__cloudflare__execute` (`/builds/builds/{uuid}/logs`) |
| Create GitHub repo, push, PRs | `gh` CLI via Bash |
| Typecheck + run unit tests | Workers Builds `build_command` (`npm run ci`) — runs in the cloud, gates the deploy |
| Build + deploy | Workers Builds (remote, automatic on `git push origin main`) |
| Conformance and static rules | `mcp-studio pattern check` / `status` / `lint` / `bless` (see [conformance.md](./conformance.md)) |
| Live fleet state after a deploy | `mcp-studio fleet status --json` |
| **Cloudflare Workers Builds GitHub App install** (one-time per Cloudflare account) | **USER ACTION** — Cloudflare dashboard |
| **Grant the GitHub App access to each new repo** (per repo) | **USER ACTION** — GitHub App settings |
| **Okta admin** (redirect URIs, scopes, policy rules, apps) | **Agent**, via the `okta-admin` skill, if installed (each write prompts {{operator_name}}). USER ACTION only to onboard a new org's service app, or when no such skill is available |

## Step 0: Working in a server that already exists

The steps below build a *new* server. If the repo is already there — you are
adding a tool, fixing a bug, patching auth — start here instead, because the
guide's shape otherwise assumes a greenfield scaffold and nothing will prompt
you to look.

```sh
grep -l "McpAgent\|agents/mcp" src/*.ts          # local check
mcp-studio pattern lint <repo>                    # same legacy markers, plus pins and config
```

A hit means the server predates the stateless 2026-07-28 spec. Confirm against
the deployment if you want certainty — a sessionful server rejects a bare call:

```sh
curl -s -X POST https://<server>/mcp \
  -H "authorization: Bearer $TOKEN" -H "content-type: application/json" \
  -H "accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
# 400 "Mcp-Session-Id header is required"  -> sessionful, migrate it
# 200 with a tool list                     -> stateless, carry on
```

**Migrate before doing the work you came to do.** Mixing a pattern migration
with a feature change makes both harder to review, so migrate in its own commit
first, then bless the repo (`mcp-studio pattern bless <repo>`) so its
conformance record reflects the new state. See
[§ Migrating an existing sessionful worker](#migrating-an-existing-sessionful-worker).

Real cost of skipping this: an agent once shipped substantial security work in
one server's `src/mcp.ts` and `src/m2m.ts` — the two files that carried the
pattern — and left `McpAgent` in place without remark. It surfaced the next day
only because `mcp-gateway` could not call it.

## Steps

Building a *new* server (or converting a FastMCP one)? Start at Step 1. Moving an *already deployed* Worker off the old `McpAgent`/Durable Object pattern? Skip to [Migrating an existing sessionful worker](#migrating-an-existing-sessionful-worker) — the Cloudflare resources already exist and the DO deletion has its own rules.

### Step 1: Scaffold from the pattern

```bash
SERVER=my-new-server
mcp-studio pattern render --out ~/GitHub/$SERVER
cd ~/GitHub/$SERVER
```

The render substitutes this instance's values (Okta org and app, domain suffix,
gateway host) into the template; the per-server placeholders (`REPLACE…`,
`replace-with-…`) are left for you. The rendered `README.md` carries the full
find-and-replace checklist.

Customize:
- `wrangler.toml` (ships as a real `wrangler.toml`, NOT `.example` — `@cloudflare/vitest-pool-workers` reads it to build the workerd test environment, so a `.example` name breaks every route test): worker name, D1 binding (`database_id` **and** `database_name`), KV id, plain-text Okta vars. `compatibility_flags` must include `global_fetch_strictly_public` or CIMD is silently unavailable. If `PUBLIC_MCP_URL` names a custom domain (`https://<name>.{{domain_suffix}}/mcp`), uncomment `routes` with the same host — they must agree, or RFC 9728 discovery points at a host the Worker does not serve. **No DO binding and no `[[migrations]]`** — the MCP layer is stateless. If you add `routes` for a custom domain, also set `workers_dev = true` or wrangler ≥4.116 will disable the `*.workers.dev` URL.
- `package.json`: `name`, `description`. Keep the pinned versions — `mcp-studio pattern lint` checks them against the pack.
- `src/mcp.ts`: replace the stub tools inside `buildServer(env)` with the server's actual tools. Each tool is `server.registerTool(name, { description, inputSchema: z.object({…}) }, handler)` with zod v4 (`import * as z from "zod/v4"`). Keep the `registerResource` skill block and the `createHandler` export — just fix the `skill://<server-name>` URI and resource name. Nothing in the factory may hold cross-request state; it runs once per HTTP request.
- `src/data.ts`: replace the example D1 helpers with your data layer (prepared statements bound directly to `env.DB`; export the pure helpers so tests can reach them).
- `src/skill.ts`: the LLM-facing usage guide, exposed as the MCP resource `skill://<server-name>` — the Workers equivalent of FastMCP's `SkillProvider` (which served a `SKILL.md` from disk; Workers have no runtime FS, so it's bundled as a string). When converting a FastMCP server, port its `skills/<name>/SKILL.md` into `SKILL_MD` (drop the frontmatter; escape backticks). Verify the doc against the *real* data/tools, not the source server's docs — they can be stale.
- `worker-configuration.d.ts`: add any new Env types (bindings, vars).
- `test/mcp.test.ts`: retarget `EXPECTED_TOOLS`, the `skill://` URI, the server name, and the schema-defaults assertion to your tools. The stateless handler is directly callable in-process — no Workers runtime, no session setup.
- `test/matrix.params.ts`: the per-repo parameters of the behavioral matrix suite (the read scope, the exact scopes the metadata advertises, and one schema-valid call that needs a scope beyond read — `null` on a single-scope server, which then records the skip loudly). The suite itself, `test/matrix.workerd.test.ts`, is identical fleet-wide.
- Keep `test/jwt.test.ts`, `test/m2m.test.ts` and `test/m2m.workerd.test.ts` as-is (they test the drop-in M2M logic). **Also add a `test/<datalayer>.test.ts`** for the pure functions in your data layer — parsing, field/column mapping, range/index math. Export those helpers from your modules so tests can import them. This is where conversion bugs concentrate; the tests run in the cloud and gate the deploy (see Step 4 + Step 5).

Leave **drop-in unchanged** (all are under conformance):
- `src/index.ts` (dispatcher — builds the `AuthInfo` for every auth path, runs the actor gate and the pre-handler scope gate, and memoizes one handler per isolate)
- `src/jwt.ts` (local M2M verification against the issuer's JWKS — `tryOktaJwt`, `decideFromClaims`, `extractScopes`)
- `src/m2m.ts` (`extractBearer`, `looksWorkerIssued`, and the `M2MVerdict` type)
- `src/actor.ts` (the `X-MCP-Actor` verifier, wired in with enforcement off)
- `src/auth.ts` (Okta upstream OAuth dance — only the redirect URIs / scopes may differ)
- `tsconfig.json`, `.gitignore` (`package-lock.json` IS committed — see gotchas)

Then make the generated repo's `ci` script run the gates the template's own CI
cannot (the template has to pass with its placeholders in place):
`"ci": "npm run conformance && npm run check:placeholders && npm run typecheck && npm run test"`,
with `"conformance": "node scripts/check-conformance.mjs"`. A fleet repo also
carries `conformance.json` and `scripts/check-conformance.mjs`;
`mcp-studio pattern lint <repo>` names whichever is missing, and
`mcp-studio pattern bless <repo>` writes `conformance.json` once the repo is in
the fleet (see [conformance.md](./conformance.md)).

### Step 2: Create Cloudflare resources

Via `mcp__cloudflare__execute`:

1. **Create the KV namespace** for OAuth state:
   ```js
   const kv = await cloudflare.request({
     method: "POST",
     path: `/accounts/${accountId}/storage/kv/namespaces`,
     body: { title: `${serverName}-oauth` },
   });
   // kv.result.id → put into wrangler.toml under [[kv_namespaces]] id
   ```

2. **Create the data layer** if needed:
   - D1: `POST /accounts/{id}/d1/database` with `{name}`. Put returned `uuid` into wrangler.toml under `[[d1_databases]] database_id`.
   - R2 / Vectorize / Queues: separate APIs — discover via `mcp__cloudflare__search`.
   - If the server reuses an existing data resource (e.g. converting a FastMCP server), find it via the list endpoint.

3. **Upload a placeholder Worker** (needed before Workers Builds can attach a trigger, because trigger creation requires the script's tag):
   ```js
   const code = `export default { fetch() { return new Response("bootstrap"); } };`;
   const metadata = { main_module: "worker.js", compatibility_date: "2026-05-01" };
   const b = `F${Date.now()}`;
   const body = [
     `--${b}`,
     'Content-Disposition: form-data; name="metadata"',
     'Content-Type: application/json',
     '',
     JSON.stringify(metadata),
     `--${b}`,
     'Content-Disposition: form-data; name="worker.js"; filename="worker.js"',
     'Content-Type: application/javascript+module',
     '',
     code,
     `--${b}--`,
   ].join("\r\n");
   await cloudflare.request({
     method: "PUT",
     path: `/accounts/${accountId}/workers/scripts/${serverName}`,
     body, contentType: `multipart/form-data; boundary=${b}`, rawBody: true,
   });
   ```

4. **Capture the script tag**:
   ```js
   const detail = await cloudflare.request({
     method: "GET",
     path: `/accounts/${accountId}/workers/services/${serverName}`,
   });
   const scriptTag = detail.result.default_environment.script_tag; // 32-char hex
   ```
   Save this — you'll need it for the build trigger lookup.

5. **Inject Worker secrets** from 1Password (the `{{op_vault}}` vault) — preferred path when the 1Password CLI (`op`) and a wrangler login (`npx wrangler whoami`) are available; the value never enters the transcript or a file (see the `cloudflare` skill). Keep one item per worker (item name = worker name) with one concealed field per secret env var. The `printf '%s'` is load-bearing — `op read` appends a trailing newline that wrangler would otherwise store as part of the secret (Okta then rejects Basic auth with `invalid_client`):
   ```bash
   printf '%s' "$(op read "op://{{op_vault}}/${SERVER}/OKTA_CLIENT_SECRET")" | npx wrangler secret put OKTA_CLIENT_SECRET
   op item edit "$SERVER" --vault {{op_vault}} --generate-password='letters,digits,48' "REQUEST_STATE_KEY[concealed]="
   printf '%s' "$(op read "op://{{op_vault}}/${SERVER}/REQUEST_STATE_KEY")" | npx wrangler secret put REQUEST_STATE_KEY
   ```
   `OKTA_CLIENT_SECRET` is the shared interactive app's secret (the same value every server on the pattern uses). `REQUEST_STATE_KEY` (≥32 bytes) seals the MRTR confirmation on the template's destructive tool; it is generated inside 1Password so it never touches the shell. Without it the destructive tool **refuses** rather than deleting unconfirmed. `COOKIE_ENCRYPTION_KEY` is **not** required: provider 0.10.3+ derives props-encryption keys from the tokens themselves and nothing reads that var.
   Fallback (no `op`/wrangler auth) — the values-in-transcript API route, only for values the user pastes anyway, or the dashboard as USER ACTION:
   ```js
   await cloudflare.request({
     method: "PUT",
     path: `/accounts/${accountId}/workers/scripts/${serverName}/secrets`,
     body: { name: "OKTA_CLIENT_SECRET", text: "<value>", type: "secret_text" },
   });
   ```
   After upload, confirm every secret binding shows `secret_text` via the bindings API.

### Step 3: Initialize the GitHub repo and push

```bash
cd ~/GitHub/$SERVER
git init -b main
git add . && git commit -m "feat: initial scaffold"
gh repo create {{github_org}}/$SERVER --private --source . --remote origin --push
```

### Step 4: Connect Workers Builds

**Step 4a — USER ACTION**: Make sure the Cloudflare Workers Builds GitHub App can see this repo. Two cases:

- **First Worker on a new Cloudflare account**: install the App via Cloudflare dashboard → Workers & Pages → Builds → Connect GitHub. Pick **All repositories** or just the target repo.
- **Subsequent Workers**: the App is already installed, but you still have to grant it access to each new repo. GitHub → org/user settings → Integrations → **Cloudflare Workers Builds** → Configure → add the new repo to **Only select repositories** (or switch to **All repositories**). Without this, `PUT /builds/repos/connections` fails with `8000012: The project is linked to a repository that no longer exists`.

Confirm completion before proceeding.

**Step 4b — agent**: Create the repo connection (idempotent for the same repo):

```js
// Get GitHub numeric IDs first
// $ gh api repos/<org>/<server> --jq '{repo_id: .id, owner_id: .owner.id, owner_login: .owner.login}'
const conn = await cloudflare.request({
  method: "PUT",
  path: `/accounts/${accountId}/builds/repos/connections`,
  body: {
    provider_type: "github",
    provider_account_id: "<owner_id as string>",
    provider_account_name: "<owner_login>",
    repo_id: "<repo_id as string>",
    repo_name: "<server>",
  },
});
// conn.result.repo_connection_uuid → save for trigger creation
```

**Step 4c**: List existing triggers. Cloudflare only auto-creates two triggers for the **very first** Worker bootstrapped on the account (immediately after the GitHub App install). For every subsequent Worker, the trigger list is empty and you must create one yourself.

```js
const list = await cloudflare.request({
  method: "GET",
  path: `/accounts/${accountId}/builds/workers/${scriptTag}/triggers`, // script tag, not name
});
```

If `list.result.length === 2`, the auto-created triggers exist (`Deploy default branch` → `npx wrangler deploy` on `main`, `Deploy non-production branches` → `npx wrangler versions upload`). They ship with an **empty `build_command`** — PATCH the `main` production trigger to run the test gate:

```js
await cloudflare.request({
  method: "PATCH",
  path: `/accounts/${accountId}/builds/triggers/<PRODUCTION_TRIGGER_UUID>`,
  body: { build_command: "npm run ci" },
});
```

If `list.result.length === 0` (no triggers auto-created), create one — with `build_command` set to the test gate up front:

```js
// Get a build token (Cloudflare auto-creates one on GitHub App authorization)
const tokens = await cloudflare.request({
  method: "GET", path: `/accounts/${accountId}/builds/tokens`,
});
const buildTokenUuid = tokens.result[0].build_token_uuid;

await cloudflare.request({
  method: "POST",
  path: `/accounts/${accountId}/builds/triggers`,
  body: {
    external_script_id: scriptTag,
    build_token_uuid: buildTokenUuid,
    repo_connection_uuid: conn.result.repo_connection_uuid,
    trigger_name: "Production Deploy",
    // Runs the conformance gate, typecheck and tests before deploy — any
    // failure blocks the deploy. Tests run in the cloud; no local install.
    build_command: "npm run ci",
    deploy_command: "npx wrangler deploy",
    root_directory: "/",
    branch_includes: ["main"],
    branch_excludes: [],
    path_includes: ["*"],
    path_excludes: [],
  },
});
```

The Workers Builds pipeline is `npm install` → `build_command` → `deploy_command`. With `build_command: "npm run ci"`, a conformance failure, typecheck error or failing unit test fails the build and the deploy never runs — **silently**, from production's point of view: the previous commit keeps serving. Always confirm `build_outcome` after a push.

### Step 5: First build + smoke test

Trigger the build:

```js
const build = await cloudflare.request({
  method: "POST",
  path: `/accounts/${accountId}/builds/triggers/<TRIGGER_UUID>/builds`,
  body: { branch: "main" },
});
```

Poll for completion (typical: 20–90s):

```js
const status = await cloudflare.request({
  method: "GET",
  path: `/accounts/${accountId}/builds/builds/${build.result.build_uuid}`,
});
// status.result.status === "stopped" && status.result.build_outcome === "success"
```

On failure, pull logs:

```js
const logs = await cloudflare.request({
  method: "GET",
  path: `/accounts/${accountId}/builds/builds/${build.result.build_uuid}/logs`,
});
```

Smoke test via curl in Bash. Get the public URL:

```js
const sub = await cloudflare.request({
  method: "GET", path: `/accounts/${accountId}/workers/subdomain`,
});
// URL = `https://${serverName}.${sub.result.subdomain}.workers.dev`
```

```bash
URL=https://${SERVER}.${SUBDOMAIN}.workers.dev   # or the custom domain once attached
curl -sS -w 'HTTP %{http_code}\n' $URL/                                                    # 200
curl -sS -w 'HTTP %{http_code}\n' $URL/.well-known/oauth-authorization-server              # 200
curl -sS -w 'HTTP %{http_code}\n' $URL/.well-known/oauth-protected-resource/mcp            # 200 on the canonical host only (RFC 9728; provider 1.x 404s it elsewhere)
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' -X POST $URL/mcp -d '{}'                    # 401
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' -X POST $URL/mcp -H 'authorization: Bearer garbage' -d '{}'  # 401
```

Once you have a valid bearer (Step 7), two more checks confirm the stateless posture:

```bash
# No standalone SSE stream under the 2026-07-28 protocol.
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' $URL/mcp -H "Authorization: Bearer $TOKEN"   # 405

# Schema defaults apply on a bare tools/call — proves the zod schema is wired
# through the validator, not just the type system.
curl -sS -X POST $URL/mcp -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"<list-tool>","arguments":{}},"id":2}'
# Expect the tool's default limit (e.g. 20 rows), not an argument-validation error.
```

For the full `/authorize` → Okta redirect, register a DCR client first (the library is strict about unregistered client_ids):

```bash
REG=$(curl -sS -X POST $URL/register -H 'content-type: application/json' \
  -d '{"client_name":"smoke","redirect_uris":["http://localhost:9999/cb"],"token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"],"response_types":["code"]}')
CID=$(echo "$REG" | python3 -c "import sys,json; print(json.load(sys.stdin)['client_id'])")

curl -sS -o /dev/null -w 'HTTP %{http_code}\nLocation: %{redirect_url}\n' \
  "$URL/authorize?client_id=$CID&response_type=code&redirect_uri=http://localhost:9999/cb&state=x&code_challenge=YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWE&code_challenge_method=S256"
# Expect HTTP 302 with Location pointing to {{okta_issuer}}/v1/authorize
```

### Step 6: Okta objects

Over the API with the `okta-admin` skill's new-server recipe, if installed (otherwise USER ACTION in the Admin Console) — the full ordered version with rollbacks is [first_deploy_cutover.md § 1](./first_deploy_cutover.md#1-okta-objects-before-any-deploy):

1. Create this server's two scopes on the shared authorization server and add them to both access-policy rules.
2. Add `https://<host>/callback` to the shared Okta application's allowed sign-in redirect URIs.
3. If M2M is required (e.g., for Marketplace registration), confirm the M2M Okta service app issues tokens carrying this server's scopes, matching `OKTA_M2M_SCOPE` in `wrangler.toml` (a space-separated ANY-OF list).

### Step 7: End-to-end verification

- **Interactive**: connect from Claude Desktop / Code / Claude.ai with the `/mcp` URL (the custom domain — since provider 1.x a `*.workers.dev` URL does not support interactive OAuth discovery). Walk through Okta and the consent page. Verify `tools/list` and a tool call succeed.
- **M2M**: have the marketplace (or a curl with a fresh Okta `client_credentials` token) hit `/mcp` with `Authorization: Bearer …`. Verify `tools/list` succeeds.
- **Which path was taken**: call the `whoami` tool (keep a version of the template's stub) — `extra.auth_path` reads `"m2m"` for a direct M2M call, `"m2m+actor"` through the gateway with a verified assertion, `"interactive"` for a human. There is no M2M cache to inspect any more: local verification writes nothing to KV, and the Okta System Log should show **no** `token.introspect` events for this server.

### Step 8 (optional): Custom-domain cutover

When the new Worker replaces one already serving a custom hostname (e.g. a proxy fronting the old deployment), move the Workers Custom Domain in place — no DNS edits, no downtime:

```js
await cloudflare.request({
  method: "PUT",
  path: `/accounts/${accountId}/workers/domains`,
  body: {
    zone_id: "<zone id>",
    hostname: "<name>.{{domain_suffix}}",
    service: "<new-worker-name>",
    environment: "production",
    override_existing_origin: true, // required when the hostname is already attached elsewhere
  },
});
```

Write the rollback down first (same call with the old service name), then re-run the Step 5 smoke tests against the FQDN — confirm the OAuth metadata issuer and the `/authorize` → Okta `redirect_uri` both show the custom hostname. Okta redirect URIs must then include `https://<fqdn>/callback`. Clients previously connected to that hostname re-authenticate once, since token issuance moved to the new Worker.

## Migrating an existing sessionful worker

For a Worker already deployed on the old `McpAgent`/Durable Object pattern (`agents` SDK + `@modelcontextprotocol/sdk` v1).

1. **Swap the deps.** Remove `agents` and `@modelcontextprotocol/sdk`; add `@modelcontextprotocol/server` at the pack's pinned version (`mcp-studio pattern show`). Bump `zod` to v4 and import it as `zod/v4`. Commit the updated `package-lock.json` (see [conformance.md § Dependency policy](./conformance.md#dependency-policy) for how to write it safely).
2. **Rewrite `src/mcp.ts`.** The `McpAgent` subclass becomes a plain `buildServer(env)` factory returning a fresh `McpServer`, plus `createHandler(env) = createMcpHandler(() => buildServer(env))`. `this.server.tool(name, description, shape, handler)` becomes `server.registerTool(name, { description, inputSchema: z.object(shape) }, handler)`. `this.env` becomes the captured `env`; `this.props` becomes `ctx.http.authInfo`.
3. **Rewrite the dispatcher.** `MyMcp.serve("/mcp")` disappears on both paths; both now call `handler.fetch(request, { authInfo })`. Drop the `export { MyMcp }` line — nothing binds the class anymore. In practice: take the template's `src/index.ts` and the rest of the security files wholesale, then bless.
4. **Drop the DO binding and add a deletion migration.** Remove `[[durable_objects.bindings]]` from `wrangler.toml`. Keep every existing `[[migrations]]` tag and **append** a new one:

   ```toml
   [[migrations]]
   tag = "v1"
   new_sqlite_classes = ["MyMcp"]   # existing history — do not delete

   [[migrations]]
   tag = "v2"
   deleted_classes = ["MyMcp"]      # the new one
   ```

   Deploying without the `deleted_classes` tag fails: wrangler refuses to drop a class that migration history says exists. (`mcp-studio pattern lint` accepts retired classes in migration history and refuses a live DO binding.)
5. **Know that this is one-way.** `wrangler rollback` does **not** work across a Durable Object migration. If you need the old version back, you deploy a *new* migration tag that re-creates the class (`new_sqlite_classes` again) along with the old code. Say this out loud to the user before deploying, and confirm the stateless build passes `npm run ci` first.
6. **Remove `MCP_OBJECT` and `DurableObjectNamespace`** from `worker-configuration.d.ts`.
7. **Re-verify both auth paths** after deploy: interactive from a real client, M2M with a fresh Okta token, plus the `GET /mcp` → 405 and bare-`arguments` default checks from Step 5. Existing connectors keep working — their Worker-issued tokens are still valid; only the transport underneath changed.

## Common gotchas

- **GitHub App install is one-time per account, but each new repo needs an explicit grant.** Initial App install via Cloudflare dashboard fails the first connection with `8000008: This project is disconnected from your Git account`. After install, every subsequent repo also needs to be added to the GitHub App's repository access list, or `PUT /builds/repos/connections` fails with `8000012: The project is linked to a repository that no longer exists`.
- **Auto-creation of the build trigger config only happens for the very first Worker on the account.** Every Worker auto-deploys on push to `main` once the trigger config exists — the difference is just whether Cloudflare creates the config for you (first Worker only) or you create it via `POST /builds/triggers` (every subsequent Worker). Don't confuse "trigger config auto-creation" with "auto-deploy on push" — those are independent.
- **The 2-triggers-per-Worker limit.** Cloudflare auto-creates two triggers on first authorization. Don't create more or you'll hit error `12030: Number of triggers created exceeds limit`.
- **Trigger listing wants the script tag**, not the script name. `/builds/workers/{external_script_id}/triggers` — get `external_script_id` from `/workers/services/{name}.default_environment.script_tag`.
- **`workers-oauth-provider` strictly validates client_id.** Smoke tests with synthetic client_ids return 500. Use DCR first (or test through a real MCP client).
- **`/.well-known/oauth-protected-resource` 404** has two causes. On the canonical host it means a `0.0.x` provider (bump). Since provider 1.x it is *also* the correct answer under any non-canonical Host — including the Worker's own `*.workers.dev` name. Probe the custom domain.
- **`/authorize?client_id=unknown` returns 500** in `0.6.0`+ (was 302 in `0.0.x`). This is correct stricter behavior, not a bug to fix.
- **Docs-only commits also redeploy** unless you add `path_excludes: ["**/*.md"]` to the trigger.
- **`GET /mcp` returns 405 and that's correct.** The 2026-07-28 protocol has no standalone SSE stream; legacy `GET`/`DELETE` are answered 405 by the default `legacy: 'stateless'` posture, and stale `Mcp-Session-Id` headers are ignored. Don't "fix" it.
- **Caller identity comes from `AuthInfo`, not `props`.** `src/index.ts` builds one on every path and passes it as `handler.fetch(request, { authInfo })`; tools read `ctx.http.authInfo` and branch on `extra.auth_path`. See [bearer_token_auth.md § Caller identity](./bearer_token_auth.md#caller-identity-authinfo-on-every-path).
- **Don't wire a JSON-schema validator.** The SDK's `workerd` package-export condition picks an eval-free one (`CfWorkerJsonSchemaValidator`) automatically. Ajv requires `eval`, which Workers forbid.
- **Dependency pins drift.** `@cloudflare/workers-types` v4 now ERESOLVE-fails against wrangler ≥4.116's peer range — v5 is required. The pack's `pattern.toml` holds the exact pins and major floors (full list in [SKILL.md § Quick orientation](../SKILL.md#quick-orientation)); `mcp-studio pattern lint` reports any repo off them, and that `agents` / `@modelcontextprotocol/sdk` / Ajv are *absent* (v2 replaces both SDKs).
- **DO commit `package-lock.json`** (policy reversed 2026-08-24). The old prohibition dated to a lockfile that omitted `@img/sharp-*` platform packages — sharp arrives transitively via miniflare → wrangler — failing `npm clean-install`. The fleet builds from committed lockfiles; `@modelcontextprotocol/server`, `workers-oauth-provider` and `hono` are pinned exactly; and running `npm audit --omit=dev` in every fleet repo plus a freshly rendered template answers "any advisories anywhere?". The sharp-entry trap still exists in a different form — see [conformance.md § Dependency policy](./conformance.md#dependency-policy).
- **`wrangler` ≥4.116 disables the workers.dev subdomain when `routes` are present.** Adding a custom domain silently takes down the `*.workers.dev` URL that connectors and Okta redirect URIs point at. Set `workers_dev = true` in `wrangler.toml` to keep both live.
- **`op read | wrangler secret put` stores a trailing newline.** Symptom: Okta `invalid_client` on the `/callback` token exchange even though the secret is correct in 1Password. Always wrap: `printf '%s' "$(op read ...)"`. The old no-login verification trick (`/callback?code=bogus&state=<base64 …>`) no longer works — `/callback` now refuses an unknown state nonce before any token exchange, which is the point of the server-side state record. Verify with a real interactive login, or confirm the binding is `secret_text`.
- **OAuth succeeds server-side but the client "never authenticates".** If `OAUTH_KV` shows fresh `grant:`/`token:` keys and authed `POST /mcp` requests hang ~10s (the `Claude-User` client timeout) with tiny CPU, the server is fine — the claude.ai/Desktop connector entry is carrying stale state (old server, old Okta tenant, pinned session). Have the user delete and re-add the connector; don't chase server code.
- **Okta 404 on `/oauth2/default/v1/authorize` → `OKTA_DOMAIN` is the admin-console hostname.** `https://<org>-admin.okta.com` serves only the admin UI, not the OAuth endpoints. Use the org domain (`{{okta_domain}}`, no `-admin`). Easy to hit because the browser shows the `-admin` host while you're logged into the admin console. Verify with `curl -s {{okta_issuer}}/.well-known/openid-configuration` before retrying the flow.

## Checklist

- [ ] Scaffold with `mcp-studio pattern render --out ~/GitHub/<server>`
- [ ] Work the find-and-replace checklist in the rendered `README.md` — including the three things a new server MUST set and that are easy to miss: the two scope names in `src/scopes.ts` (plus one `TOOL_SCOPES` entry per tool), `SCOPE_HELP` in `src/consent.ts`, and the two `__Host-` cookie prefixes in `src/oauth-state.ts`
- [ ] Customize `wrangler.toml` (no DO binding, no migrations; `routes` and `PUBLIC_MCP_URL` must name the same host; `workers_dev = true` if you add `routes`), `package.json`, `src/mcp.ts`, `src/data.ts`, `src/skill.ts`, `worker-configuration.d.ts`, `test/matrix.params.ts`
- [ ] Open every tool handler with `requireScope(granted(ctx), "<tool>")` — layer 2 of the scope policy, and the only one a caller cannot bypass by omitting the `Mcp-Name` header
- [ ] Retarget `test/mcp.test.ts` to your tool names and add a `VALID_ARGS` entry per tool; write `test/<datalayer>.test.ts` for the data layer's pure functions; keep the shipped suites — 229 tests before you add anything
- [ ] Confirm BOTH vitest projects run: `npm run test:unit` and `npm run test:workerd`. The workerd one exercises `src/index.ts` and `src/auth.ts`, which no Node test can import at all
- [ ] Rewrite the generated repo's `ci` to run `conformance` and `check:placeholders` first; `npm run check:placeholders` and `mcp-studio pattern lint <repo>` both clean
- [ ] Confirm `package-lock.json` is present, committed, and carries all `@img/sharp-*` entries
- [ ] Create KV namespace via API; write id to `wrangler.toml`
- [ ] Create D1 / other data layer; write IDs to `wrangler.toml`; commit the schema as `migrations/`
- [ ] Upload placeholder Worker; capture script tag
- [ ] Inject `OKTA_CLIENT_SECRET` and `REQUEST_STATE_KEY` from 1Password via `printf '%s' "$(op read …)" | npx wrangler secret put`. (`COOKIE_ENCRYPTION_KEY` is **not** required: provider 0.10.3+ derives props-encryption keys from the tokens themselves and nothing reads that var. Older guides list it; it does nothing.)
- [ ] `git init`, push to GitHub
- [ ] USER ACTION: GitHub App access for the repo (App install is one-time per account; each new repo still needs an explicit grant)
- [ ] Create repo connection; create the production trigger (or PATCH the auto-created one) with `build_command: "npm run ci"`
- [ ] Trigger first build; poll for `outcome: success` — confirm the build log shows conformance + `tsc` + `vitest` ran before deploy
- [ ] Smoke test: well-known endpoints (both, on the canonical host), `/mcp` 401 gate, `/authorize` 302 to Okta with a registered client, and `client_id_metadata_document_supported: true` on `/.well-known/oauth-authorization-server` (`false` means the compat flag is missing and clients fall back to deprecated DCR)
- [ ] Expect the CONSENT PAGE on the first interactive connect — it is required and has no off switch. Confirm the redirect host shown is the one you expect.
- [ ] Okta: both scopes on the shared AS, in both policy rules; `/callback` on the shared app's redirect URIs (`okta-admin` new-server recipe, if installed)
- [ ] Verify interactive flow from Claude Desktop / Code
- [ ] If M2M: verify with Marketplace or curl + Okta token; `whoami` reports `auth_path: "m2m"`
- [ ] Verify the stateless posture: authed `GET /mcp` → 405; authed `tools/call` with `arguments: {}` applies schema defaults
- [ ] **Register with `mcp-gateway`** (a default step, not optional). One admin
      call against `https://{{gateway_host}}`:
      `register_server{url: "https://<name>.{{domain_suffix}}/mcp"}`. The gateway
      probes it, reads its scopes from its RFC 9728 metadata, indexes its tools,
      and it is callable immediately — no redeploy. Then confirm `list_servers`
      shows it with `health: "ok"`.
      Skipping this is how a registry drifts: one server sat at a stale
      workers.dev URL, another pointed at a Worker that was never deployed,
      and the reference implementation was missing outright, because
      nothing was responsible for keeping the registry and the fleet in step.
      A server that publishes no `scopes_supported` is REFUSED, so if
      registration fails here, check `/.well-known/oauth-protected-resource/mcp`
      before anything else.
- [ ] Add the repo to the fleet (the instance's `studio.toml`, or the discovery topic it uses) and `mcp-studio pattern bless <repo>`; `mcp-studio fleet status` should then show it deployed, live, registered and conforming
- [ ] Register in MCP Marketplace (separate registry-side step)

## Reference implementation

The pack's template is the reference: render it (`mcp-studio pattern render --out <dir>`) and read it. It is ahead of any individual fleet repo by construction — every security-file change lands there first and is ported out — and behind them in one respect only: its tools are stubs. It ships **two** scopes and a write-scoped destructive tool (`delete_example`, with an HMAC-sealed MRTR confirmation) precisely so that scope-bound consent, step-up, re-prompt and confirmation branches are exercised rather than present-but-unreachable. For how far a given fleet repo has drifted from it, `mcp-studio pattern status <repo>`.
