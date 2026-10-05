#!/usr/bin/env bash
# Fail if any template placeholder survives into a generated server.
#
# WHY THIS EXISTS. A checklist-exact instantiation of this template once passed
# 171/171 tests with placeholder consent prose still in SCOPE_HELP,
# database_name = "REPLACE", and src/skill.ts untouched. The suite covers what the
# tests reference — scope names, cookie prefixes, hostnames — and says nothing
# about prose, or about config no test reads. The checklist is the completeness
# check; the suite is not. This makes the checklist mechanical.
#
# Deliberately NOT part of the TEMPLATE's `npm run ci`: its own CI must pass while
# the placeholders are still there (ruling 2026-08-23). A GENERATED repo is the
# opposite: generation rewrites its `ci` to run this first, so a placeholder or a
# route/var disagreement can never ride a green build to production (review P-1).
set -euo pipefail

# Files whose placeholders are load-bearing documentation rather than config.
EXCLUDE='^(README\.md|scripts/check-placeholders\.sh|docs/)'

# Placeholder PATTERNS, not the bare word. A case-insensitive search for
# "replace" matches `.replace(` calls and ordinary prose; an uppercase-only search
# sailed past `replace-with-your-server-name` in package.json, `name =` in
# wrangler.toml and the Okta tenant hostname — lowercase markers sitting in
# exactly the config a deploy depends on. Both failure modes were real: the first
# use of this script hit the second one.
PATTERNS=(
  'REPLACE-WITH-'          # uppercase opaque-id and name markers
  'replace-with-'          # lowercase, case-constrained markers
  'REPLACE:'               # the two scope names
  'REPLACE_'               # cookie prefixes
  'replace\.mcp\.'         # placeholder hostname
  '\bREPLACE\b'            # a bare marker, uppercase only
)

args=()
for pat in "${PATTERNS[@]}"; do args+=(-e "$pat"); done

hits=$(grep -rnE --binary-files=without-match \
        "${args[@]}" \
        --include='*.ts' --include='*.toml' --include='*.json' --include='*.md' \
        . 2>/dev/null \
      | grep -v node_modules \
      | grep -vE "$EXCLUDE" || true)

if [ -n "$hits" ]; then
  echo "Unreplaced template placeholders:"
  echo
  echo "$hits"
  echo
  echo "Work the find-and-replace checklist in README.md. Note that src/skill.ts"
  echo "is prose the model reads and no test asserts — it will not fail CI, and it"
  echo "is the file most often shipped untouched."
  exit 1
fi


# Structural agreement: an active custom-domain route must carry the SAME host
# as PUBLIC_MCP_URL. The advertised RFC 9728 resource is derived from the var,
# so a route/var disagreement advertises an endpoint that is not the one
# serving — the exact miswire src/resource.ts warns about (review finding C-1).
route_host=$(grep -oE '^routes = \[\{ pattern = "[^"]+"' wrangler.toml | sed 's/.*pattern = "//; s/"$//' || true)
public_host=$(grep -oE '^PUBLIC_MCP_URL = "https://[^/"]+' wrangler.toml | sed 's|.*https://||' || true)
if [ -n "$route_host" ] && [ "$route_host" != "$public_host" ]; then
  echo "wrangler.toml route host ($route_host) != PUBLIC_MCP_URL host ($public_host)."
  exit 1
fi

# The one placeholder that is a real secret rather than a string to swap.
if ! grep -q 'REQUEST_STATE_KEY' worker-configuration.d.ts; then
  echo "worker-configuration.d.ts no longer declares REQUEST_STATE_KEY."
  exit 1
fi

echo "No template placeholders remain."
echo
echo "Still yours to check, because nothing here can:"
echo "  * src/skill.ts   — does it describe YOUR server to a model?"
echo "  * SCOPE_HELP     — does each line say what that scope actually permits?"
echo "  * REQUEST_STATE_KEY set as a Worker secret (>=32 bytes), or the"
echo "    destructive tool refuses rather than confirming."
