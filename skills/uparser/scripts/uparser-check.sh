#!/usr/bin/env bash
# uparser-check.sh — one-call preflight an agent can run before parsing.
# Ensures the binary exists and prints a compact JSON status to stdout so the
# caller can branch programmatically. Exit 0 if the binary is usable, 2 if not.
#
# Reports:
#   { "binary": "<path>|null", "ok": true|false,
#     "version": "<installed>|null",              # what we resolved to
#     "latest":  "<newest published>|null",       # for this platform
#     "latest_source": "api|cache|last_known_good|fallback_pin|user_pin|...",
#     "protocols": [ ... ],                       # from `uparser protocols`
#     "endpoint": "<url>|null",                   # resolved (flag/env/config)
#     "endpoint_reachable": true|false|null,      # only probed if an endpoint is known
#     "quality_protocol": "<name>",               # what uparser-parse.sh would use
#     "quality_reason": "<why>" }
#
# The quality_* fields come from pick_protocol.sh — the SAME code uparser-parse.sh
# uses, so the two can no longer disagree about what would run. (They used to:
# this script hardcoded mineru-vlm while parse enumerated six sections.)
#
# Usage:
#   uparser-check.sh [--protocol mineru-vlm] [--endpoint <url>] [--file <path>]
#   (the endpoint is resolved by the binary itself: flag > $UPARSER_ENDPOINT >
#    config[<protocol>] > config[defaults] > built-in default)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

proto="mineru-vlm"; ep_cli=""; file=""
while [ $# -gt 0 ]; do
  case "$1" in
    --protocol) proto="${2:-mineru-vlm}"; shift 2 ;;
    --protocol=*) proto="${1#--protocol=}"; shift ;;
    --endpoint) ep_cli="${2:-}"; shift 2 ;;
    --endpoint=*) ep_cli="${1#--endpoint=}"; shift ;;
    --file) file="${2:-}"; shift 2 ;;
    --file=*) file="${1#--file=}"; shift ;;
    *) shift ;;
  esac
done

jstr() { [ "$1" = "null" ] && printf 'null' || printf '"%s"' "$1"; }

# 1) ensure the binary (UPARSER_BIN → version-checked workspace/PATH → cache →
#    download → build)
bin="$("$HERE/ensure_uparser.sh" 2>/dev/null | tail -1 || true)"
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
  printf '{"binary":null,"ok":false,"version":null,"latest":null,"latest_source":null,"protocols":[],"endpoint":null,"endpoint_reachable":null,"quality_protocol":null,"quality_reason":null}\n'
  echo "uparser-check: binary not found and could not be downloaded/built" >&2
  exit 2
fi

installed="$("$bin" --version 2>/dev/null | awk 'NR==1{print $2}' || true)"

# 2) what is the newest published build for this platform, and how sure are we
lv="$("$HERE/latest_version.sh" --json 2>/dev/null || true)"
latest="$(printf '%s' "$lv" | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')"
lsrc="$(printf '%s' "$lv"   | sed -n 's/.*"source":"\([^"]*\)".*/\1/p')"

# 3) protocols (machine-readable capability list → just the names here)
protos="$("$bin" protocols 2>/dev/null || echo '[]')"
names="$(printf '%s' "$protos" | grep -o '"name"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"\([^"]*\)"$/\1/' | paste -sd, - 2>/dev/null || true)"
[ -n "$names" ] && names="$(printf '%s' "$names" | sed 's/[^,]*/"&"/g')"

# 4) probe the named protocol. `doctor` resolves the endpoint itself (flag >
#    $UPARSER_ENDPOINT > config[<protocol>] > config[defaults] > built-in
#    default) and echoes the resolved value back, so we read it from doctor's
#    own output instead of re-implementing the lookup here — this wrapper's
#    copy could only ever see one flat section and knew nothing about
#    [defaults] or api_key.
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

# 5) what would uparser-parse.sh actually pick right now?
pick="$("$HERE/pick_protocol.sh" --bin "$bin" ${file:+--file "$file"} --json 2>/dev/null || true)"
qproto="$(printf '%s' "$pick" | sed -n 's/.*"protocol":"\([^"]*\)".*/\1/p')"
qreason="$(printf '%s' "$pick" | sed -n 's/.*"reason":"\([^"]*\)".*/\1/p')"

printf '{"binary":%s,"ok":true,"version":%s,"latest":%s,"latest_source":%s,"protocols":[%s],"endpoint":%s,"endpoint_reachable":%s,"quality_protocol":%s,"quality_reason":%s}\n' \
  "$(jstr "$bin")" "$(jstr "${installed:-null}")" "$(jstr "${latest:-null}")" "$(jstr "${lsrc:-null}")" \
  "${names:-}" "$(jstr "${ep:-null}")" "$reachable" \
  "$(jstr "${qproto:-null}")" "$(jstr "${qreason:-null}")"
