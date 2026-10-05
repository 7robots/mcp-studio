#!/bin/sh
# A stand-in for the Okta admin helper: `okta-fake.sh GET <path> [--org <org>]`.
[ "$1" = "GET" ] || { echo "only GET" >&2; exit 2; }
case "$2" in
  /api/v1/authorizationServers/default/policies)
    echo '[{"id":"00pINTERACTIVE"},{"id":"00pM2M"}]' ;;
  /api/v1/authorizationServers/default/policies/00pINTERACTIVE/rules)
    echo '[{"id":"0prOTHER","name":"Other","status":"ACTIVE","conditions":{"scopes":{"include":["openid"]}}},
           {"id":"0prINTERACTIVE","name":"Interactive","status":"ACTIVE","conditions":{"scopes":{"include":["openid","weather:read","tides:read"]}}}]' ;;
  /api/v1/authorizationServers/default/policies/00pM2M/rules)
    echo '[{"id":"0prM2M","name":"M2M","status":"ACTIVE","conditions":{"scopes":{"include":["*"]}}}]' ;;
  /api/v1/apps/0oaINTERACTIVE)
    echo '{"id":"0oaINTERACTIVE","settings":{"oauthClient":{"redirect_uris":["https://weather.mcp.acme.example/callback"]}}}' ;;
  *) echo "okta-fake: HTTP 404 E0000007 Not found: $2" >&2; exit 1 ;;
esac
