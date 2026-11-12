#!/usr/bin/env bash
# Point hermes-gpui at the hermes-crew gateway.
#
#   source scripts/hermes-crew-env.sh
#   cargo run -p hermes-desktop
#
# Auth, in order of preference:
#   1. HERMES_CREW_TOKEN      explicit deployment session token
#   2. bws                    read key hermes-crew-session-token from BSM
#                             (needs BWS_ACCESS_TOKEN in the environment)
# A token is only used if the gateway accepts it; otherwise the helper falls
# back, because a gateway started without HERMES_DASHBOARD_SESSION_TOKEN mints
# a random one per start and rejects the stored value.
#   3. HERMES_CREW_USER/PASS  or the hermes-dashboard Secret; the client then
#                             POSTs /auth/password-login itself and keeps the
#                             session cookie
#
# The token is a deployment `HERMES_DASHBOARD_SESSION_TOKEN`: it works as an
# `Authorization: Bearer` on REST and as `?token=` on the WebSocket, and unlike
# a login cookie it does not expire with the gateway process.
#
# Endpoint choice:
#   - default: the Service VIP. Correct on a host that is itself a k3s node
#     (zephyr, forge, sentry, nexus), where kube-proxy serves the VIP locally.
#   - HERMES_CREW_URL: override it. The tailnet ingress is
#     https://hermes-crew.tigris-ule.ts.net, subject to the tailnet ACL, so it
#     only works once the ACL admits the caller to the ingress proxy.
#
# Nothing is written to disk and no secret value is echoed.

HERMES_CREW_URL="${HERMES_CREW_URL:-http://10.43.29.77:9119}"
HERMES_CREW_BSM_KEY="${HERMES_CREW_BSM_KEY:-hermes-crew-session-token}"

_token=""
if [ -n "${HERMES_CREW_TOKEN:-}" ]; then
  _token="$HERMES_CREW_TOKEN"
elif command -v bws >/dev/null 2>&1 && [ -n "${BWS_ACCESS_TOKEN:-}" ]; then
  _token="$(bws secret get "$(bws secret list -o json 2>/dev/null \
    | jq -r --arg k "$HERMES_CREW_BSM_KEY" '.[] | select(.key == $k) | .id' | head -1)" \
    -o json 2>/dev/null | jq -r '.value // empty')"
fi

# The token is only useful if the gateway was started with that same value.
# When it was not, the server mints a random one per start and a bearer gets
# rejected, so verify before exporting and fall back to the login path.
if [ -n "$_token" ]; then
  _code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 \
    -H "Authorization: Bearer $_token" "$HERMES_CREW_URL/api/profiles" 2>/dev/null)"
  if [ "$_code" = "200" ]; then
    export HERMES_URL="$HERMES_CREW_URL"
    export HERMES_TOKEN="$_token"
    unset HERMES_BASIC HERMES_COOKIE
    echo "hermes-crew-env: HERMES_URL=$HERMES_URL (session token exported, not shown)"
    unset _token _code
    return 0 2>/dev/null || exit 0
  fi
  echo "hermes-crew-env: stored token rejected (HTTP ${_code:-none}); using password login" >&2
  unset _token _code
fi

if [ -z "${HERMES_CREW_USER:-}" ] || [ -z "${HERMES_CREW_PASS:-}" ]; then
  if command -v kubectl >/dev/null 2>&1; then
    HERMES_CREW_USER="$(kubectl -n hermes-crew get secret hermes-dashboard \
      -o jsonpath='{.data.username}' 2>/dev/null | base64 -d 2>/dev/null)"
    HERMES_CREW_PASS="$(kubectl -n hermes-crew get secret hermes-dashboard \
      -o jsonpath='{.data.password}' 2>/dev/null | base64 -d 2>/dev/null)"
  fi
fi

if [ -z "${HERMES_CREW_USER:-}" ] || [ -z "${HERMES_CREW_PASS:-}" ]; then
  echo "hermes-crew-env: no credentials; set HERMES_CREW_TOKEN (or BWS_ACCESS_TOKEN," >&2
  echo "  or HERMES_CREW_USER/HERMES_CREW_PASS)" >&2
  return 1 2>/dev/null || exit 1
fi

export HERMES_URL="$HERMES_CREW_URL"
export HERMES_BASIC="${HERMES_CREW_USER}:${HERMES_CREW_PASS}"
unset HERMES_TOKEN HERMES_COOKIE

echo "hermes-crew-env: HERMES_URL=$HERMES_URL (basic auth exported, values not shown)"
