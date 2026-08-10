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
#   (endpoint also read from $UPARSER_ENDPOINT or config[<protocol>|mineru-vlm])
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG="${UPARSER_CONFIG:-$HOME/.config/uparser/config.toml}"

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

read_ini() { # $1=section $2=key
  [ -f "$CONFIG" ] || return 0
  awk -v s="[$1]" -v k="$2" '
    /^[[:space:]]*\[/ { cur=$0; gsub(/^[[:space:]]+|[[:space:]]+$/,"",cur) }
    cur==s && $0 ~ "^[[:space:]]*"k"[[:space:]]*=" {
      sub(/^[^=]*=[[:space:]]*/,""); gsub(/^["'"'"']|["'"'"'][[:space:]]*$/,""); print; exit
    }' "$CONFIG"
}
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

# 3) resolve + probe an endpoint if one is known
ep="$ep_cli"; [ -n "$ep" ] || ep="${UPARSER_ENDPOINT:-}"; [ -n "$ep" ] || ep="$(read_ini "$proto" endpoint)"
reachable="null"
if [ -n "$ep" ]; then
  # doctor is diagnostic-only: it always exits 0 and reports status in its JSON
  # `reachable` field, so read that field rather than the exit code.
  dout="$("$bin" doctor "$proto" --endpoint "$ep" 2>/dev/null || true)"
  case "$dout" in
    *'"reachable"'*true*)  reachable="true" ;;
    *'"reachable"'*false*) reachable="false" ;;
  esac
fi

printf '{"binary":%s,"ok":true,"protocols":[%s],"endpoint":%s,"endpoint_reachable":%s}\n' \
  "$(jstr "$bin")" "${names:-}" "$(jstr "${ep:-null}")" "$reachable"
