#!/usr/bin/env bash
# Fail if any org-specific value from an instance appears in the product tree.
#
#   scripts/check-neutral.sh <instance-dir> [<instance-dir>…]
#
# The deny list is every quoted string value (6+ chars) in each instance's
# studio.toml and pattern-values.toml, so it never has to be written down in
# this public repo. examples/ is exempt (it is the fictional acme instance),
# and so is the repo's own GitHub URL.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
[ $# -ge 1 ] || { echo "usage: $0 <instance-dir>…" >&2; exit 64; }
patterns="$(mktemp)"; trap 'rm -f "$patterns"' EXIT
for inst in "$@"; do
  for f in "$inst/studio.toml" "$inst/pattern-values.toml"; do
    [ -f "$f" ] || continue
    grep -oE '"[^"]{6,}"' "$f" | tr -d '"' | sed -E 's#^https?://##; s#/.*$##' | grep -vE '^(op:|env:|~|\.\.?/)'
    # bare words that identify an org: GitHub orgs/accounts and the instance name
    grep -E '^\s*(name|account|github_org)\s*=' "$f" | grep -oE '"[^"]+"' | tr -d '"'
    grep -E '^\s*orgs\s*=' "$f" | grep -oE '"[^"]+"' | tr -d '"'
  done
done | grep -vE '^(mcp-gateway|cf-workers-ts|pattern-values\.toml|conformance(\.json)?|mcp-server|okta-api|resolve|default|main|auto|claude|codex|Employee)$' \
     | awk 'length >= 4' | sort -u > "$patterns"
hits="$(cd "$root" && git ls-files -co --exclude-standard \
  | grep -vE '^(examples/|scripts/check-neutral\.sh$|Cargo\.lock$)' \
  | xargs grep -n -i -F -f "$patterns" 2>/dev/null \
  | grep -vE 'github\.com/[^/]+/mcp-studio' || true)"
if [ -n "$hits" ]; then
  echo "org-specific values found in the product tree:" >&2
  echo "$hits" >&2
  exit 1
fi
echo "neutral: $(wc -l < "$patterns" | tr -d ' ') instance values, no hits"
