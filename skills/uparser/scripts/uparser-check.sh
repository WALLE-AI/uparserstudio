#!/usr/bin/env bash
# uparser-check.sh — one-call preflight an agent can run before parsing.
# Ensures the binary exists and prints a compact JSON status to stdout so the
# caller can branch programmatically. Exit 0 if the binary is usable, 2 if not.
#
# Reports:
#   { "binary": "<path>|null", "ok": true|false,
#     "protocols": [ ... ],                      # from `uparser protocols`
#     "endpoint": "<url>|null",                  # resolved (flag/env/config)
#     "endpoint_reachable": true|false|null }    # only probed if an endpoint is known
#
# Usage:
#   uparser-check.sh [--protocol mineru-vlm] [--endpoint <url>]
#   (the endpoint is resolved by the binary itself: flag > $UPARSER_ENDPOINT >
#    config[<protocol>] > config[defaults] > built-in default)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

proto="mineru-vlm"; ep_cli=""
while [ $# -gt 0 ]; do
  case "$1" in
    --protocol) proto="${2:-mineru-vlm}"; shift 2 ;;
    --protocol=*) proto="${1#--protocol=}"; shift ;;
    --endpoint) ep_cli="${2:-}"; shift 2 ;;
    --endpoint=*) ep_cli="${1#--endpoint=}"; shift ;;
    *) shift ;;
  esac
done

jstr() { [ "$1" = "null" ] && printf 'null' || printf '"%s"' "$1"; }

# 1) ensure the binary (PATH → cache → download → build)
bin="$("$HERE/ensure_uparser.sh" 2>/dev/null | tail -1 || true)"
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
  printf '{"binary":null,"ok":false,"protocols":[],"endpoint":null,"endpoint_reachable":null}\n'
  echo "uparser-check: binary not found and could not be downloaded/built" >&2
  exit 2
fi

# 2) protocols (machine-readable capability list → just the names here)
protos="$("$bin" protocols 2>/dev/null || echo '[]')"
names="$(printf '%s' "$protos" | grep -o '"name"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"\([^"]*\)"$/\1/' | paste -sd, - 2>/dev/null || true)"
[ -n "$names" ] && names="$(printf '%s' "$names" | sed 's/[^,]*/"&"/g')"

# 3) probe the endpoint. `doctor` resolves it itself (flag > $UPARSER_ENDPOINT
#    > config[<protocol>] > config[defaults] > built-in default) and echoes the
#    resolved value back, so we read it from doctor's own output instead of
#    re-implementing the lookup here — this wrapper's copy could only ever see
#    one flat section and knew nothing about [defaults] or api_key.
reachable="null"; ep=""
if [ -n "$ep_cli" ]; then
  dout="$("$bin" doctor "$proto" --endpoint "$ep_cli" 2>/dev/null || true)"
else
  dout="$("$bin" doctor "$proto" 2>/dev/null || true)"
fi
ep="$(printf '%s' "$dout" | grep -o '"endpoint"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"\([^"]*\)"$/\1/' | head -1)"
# doctor is diagnostic-only: it always exits 0 and reports status in its JSON
# `reachable` field, so read that field rather than the exit code.
case "$dout" in
  *'"reachable"'*true*)  reachable="true" ;;
  *'"reachable"'*false*) reachable="false" ;;
esac

printf '{"binary":%s,"ok":true,"protocols":[%s],"endpoint":%s,"endpoint_reachable":%s}\n' \
  "$(jstr "$bin")" "${names:-}" "$(jstr "${ep:-null}")" "$reachable"
