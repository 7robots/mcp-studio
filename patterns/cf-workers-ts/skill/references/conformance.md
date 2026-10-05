# Fleet conformance for the security scaffold

Every server built on this pattern carries its own copy of the template's
security files. Copies drift, and drifted security code fails silently: one
independent review found three real defects living in exactly that gap — a fix
present in one repo while the documents said "fleet-wide". This mechanism makes
drift loud. It does not prevent divergence; it forces every divergence through
one reviewed decision.

The pattern pack (`pattern.toml`) declares what is under conformance; MCP Studio
runs the comparison; the instance repo stores and versions the blessed result.

## The two layers

**The manifest of allowed differences** lives in the instance's conformance
store: `<instance>/conformance/<repo>/<file-with-/→__>.diff` (for example
`conformance/<repo>/src__index.ts.diff`) — the unified diff between the
template's copy, rendered with this instance's values, and the repo's copy of
each security file. An empty file means byte-identical. These diffs are the
reviewable, versioned record of what each repo is permitted to differ in, and
they are reviewed **in the instance repo's git history**: a per-repo divergence
shows up there as a divergence.

**The per-repo CI gate** is `scripts/check-conformance.mjs`, run as
`npm run conformance` — the first step of every fleet repo's `npm run ci`.
Each repo's `conformance.json` names the sha256 of each security file as last
blessed, plus the pattern version it was blessed at; any edit without a re-bless
fails CI. Workers Builds runs repos in isolation, so this layer proves only
"unchanged since a human blessed it" — the cross-repo comparison is Studio's
job.

## The files under conformance

`src/actor.ts`, `src/auth.ts`, `src/consent.ts`, `src/ema.ts`, `src/index.ts`,
`src/jwt.ts`, `src/m2m.ts`, `src/oauth-state.ts`, `src/resource.ts`,
`src/scopes.ts`, and `test/matrix.workerd.test.ts` — the authoritative list is
`[security] files` in `pattern.toml` (`mcp-studio pattern show`).

Business logic (`src/mcp.ts`, data modules) is out by design: it is supposed to
differ. `src/index.ts` is in because wiring lives there, and an
exported-but-never-called control is the precise failure mode this exists to
catch — the actor verifier once shipped in exactly that state, exported and
imported by nothing, so `REQUIRE_GATEWAY_ACTOR` was a no-op.

The behavioral matrix spec is one file, byte-identical fleet-wide, driving the
same scenarios (metadata, M2M, both scope layers, the actor chain, EMA
provenance) through every server's real routes. Its per-repo variation
lives in `test/matrix.params.ts`, which is deliberately **not** tracked. Hashes
prove file identity; the matrix proves behaviour.

## Operating it

```sh
mcp-studio pattern check [repo…]           # exit 1 on any drift
mcp-studio pattern status [--json]         # per repo × file: drift line counts vs the template, OK/DRIFT vs the stored diff
mcp-studio pattern bless [repo…]           # regenerate the stored diffs and each repo's conformance.json
mcp-studio pattern bless [repo…] --store-only   # regenerate only the stored diffs
mcp-studio pattern lint [repo…] [--json]   # static rules: pins, lockfile, wrangler, placeholders, legacy markers
mcp-studio pattern show                    # the pack: version, pins, security files, rules
```

Fleet members come from the instance's `studio.toml`. `bless` rewrites a repo's
`conformance.json` only when the hashes or the pattern version changed, so
re-running it on a clean fleet is a no-op in the repos.

`check` fails a repo when any of these is true: its `conformance.json` names an
older pattern version than `pattern.toml`; a security file is missing; a file's
hash differs from its blessed hash; or the current diff against the template no
longer matches the stored one (which is how a template change that was never
ported shows up).

A security-file change flows one of two ways:

- **Template-first (the default).** Land it in the pack's `template/`, port it
  to every fleet repo, bump `[pack] version` in `pattern.toml` (date-based,
  `YYYY-MM-DD.N`) with a `CHANGELOG.md` entry, run `mcp-studio pattern bless`,
  then commit every fleet repo and the regenerated conformance store in the
  instance repo in one pass.
- **Deliberate per-repo difference.** Make the change, `mcp-studio pattern bless
  <repo>`, and commit both — the instance repo's diff then shows the divergence
  as a divergence, which is where it gets reviewed.

A repo's CI failure saying `changed without a bless` means someone edited a
security file directly; decide which flow above it belongs to rather than
regenerating the hash to make the error go away.

**That CI failure is invisible in production.** The build fails, so nothing
deploys, and the previous commit keeps serving. One such un-blessed edit (an
audience change in `src/jwt.ts` and the matrix test, in four repos) left
production a week behind with nothing alerting. After pushing to a fleet repo,
confirm the build's `build_outcome`, and check that `mcp-studio fleet status`
shows the new commit deployed.

## What this does not cover

- The gateway. It is a different design, not another copy of this one.
- Malicious commits. A committer can hand-edit `conformance.json`; this is
  drift detection for honest workflows, not tamper-proofing.
- Anything outside the security seam. Pins, lockfile shape, wrangler config and
  surviving placeholders are `mcp-studio pattern lint`'s job, not the hash gate's.

## Dependency policy

Since 2026-08-24, every deployable target and the template commit
`package-lock.json`; Workers Builds installs from it. The three
security-load-bearing direct dependencies — `@modelcontextprotocol/server`,
`@cloudflare/workers-oauth-provider`, `hono` — are pinned exactly in
`package.json`, so a bump of any of them is a visible diff, never a side effect
of an install. Everything else stays on ranges with major-version floors; the
lockfile holds the whole tree either way. The pins and floors live in
`pattern.toml` and `mcp-studio pattern lint` checks every repo against them.

Updates are a deliberate act: bump on top of the existing lockfile, run the
repo's full CI, and run `npm audit --omit=dev` in every fleet repo and on a
freshly rendered template (production deps only — devDependencies never ship in
a Worker bundle, and advisories in test tooling are noise that trains people to
ignore the answer; run the full-tree `npm audit` too after a bump of the test
toolchain). One commit per repo. A pin change in the template is a pattern
change: bump the pack version and record it in `CHANGELOG.md`.

The old "never commit a lockfile" rule was real, but the fix is a correct
lockfile, not the absence of one. **The lockfile must carry every `@img/sharp-*`
platform entry, with a version** — sharp arrives via miniflare, and Workers
Builds (Linux x64, npm 10.9.2) refuses a lockfile that lacks one:
`npm error Invalid Version:` when an entry has no `version`, or
`Missing: @img/sharp-linux-arm64@… from lock file` when it is absent. Both hit
three repos at once on 2026-10-05. The cause there was **root-owned entries in
`~/.npm/_cacache`** (left by an old `sudo npm`): npm cannot read those
packuments and silently leaves the packages out — no error, just a short
lockfile. Fix the cache (`sudo chown -R $(id -u):$(id -g) ~/.npm`) or set
`npm_config_cache` to a clean directory, write the lockfile with
`npx -y npm@10.9.2 install`, then check before pushing: all 27 entries present
(`python3 -c 'import json;p=json.load(open("package-lock.json"))["packages"];print(sum(k.startswith("node_modules/@img/") for k in p))'`
— `mcp-studio pattern lint` runs the same count) and
`rm -rf node_modules && npx -y npm@10.9.2 ci --os=linux --cpu=x64` passes.

Bump on top of the existing lockfile rather than regenerating from nothing: as of
2026-10-05 a fresh resolution crashes in arborist under both npm 10.9.2 and 11
(`Cannot read properties of null (reading 'edgesOut')`) on vitest 4's optional
`@vitest/browser-*` peers, which now resolve to vitest 5.
