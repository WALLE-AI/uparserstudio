#!/usr/bin/env bash
# uparser-parse.sh — one-shot "do the right thing" parse for coding agents.
# Give it a file; it returns Markdown on stdout and a semantic exit code.
#
# What it decides for you (so an agent doesn't have to):
#   * ensures the `uparser` binary exists (downloads/builds via ensure_uparser.sh);
#   * NEVER selects the explicit-only `mock` protocol;
#   * picks the protocol automatically when you pass neither --mode nor --protocol:
#       - a VLM endpoint is resolvable (‑‑endpoint / $UPARSER_ENDPOINT / config)
#         → `--protocol auto` (Profiler routes born‑digital→native, scans→VLM);
#         the binary itself resolves the endpoint/model/credentials;
#       - otherwise → `--protocol native` (pure‑Rust, offline, no GPU;
#         flagged pages can use bounded OCR when PDFium+Tesseract are present).
#   * defaults --format to markdown (override with --format json).
#
# Anything you pass through (‑‑pages, ‑‑max-concurrency, ‑‑no-cache, an explicit
# ‑‑protocol/‑‑endpoint/‑‑model, …) is forwarded unchanged and always wins.
#
# Usage:
#   uparser-parse.sh <file> [any uparser parse flags...]
#   UPARSER_ENDPOINT=http://host:port/v1/chat/completions uparser-parse.sh scan.pdf
#
# Exit codes are the binary's own: 0 ok · 1 usage · 2 env/endpoint · 3 partial
# (usable, check page_errors) · 4 internal.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG="${UPARSER_CONFIG:-$HOME/.config/uparser/config.toml}"

[ "$#" -ge 1 ] || { echo "usage: uparser-parse.sh <file> [uparser parse flags...]" >&2; exit 1; }

# Does a VLM endpoint exist anywhere? This is the ONLY thing this wrapper
# still reads the config for, and only to choose auto-vs-native — it no
# longer injects --endpoint/--model, because the binary resolves those
# itself (per key, keyed on the post-routing protocol, with [defaults]
# layering and api_key/header support this awk reader cannot express).
read_ini() { # $1=section $2=key
  [ -f "$CONFIG" ] || return 0
  awk -v s="[$1]" -v k="$2" '
    /^[[:space:]]*\[/ { cur=$0; gsub(/^[[:space:]]+|[[:space:]]+$/,"",cur) }
    cur==s && $0 ~ "^[[:space:]]*"k"[[:space:]]*=" {
      sub(/^[^=]*=[[:space:]]*/,""); gsub(/^["'"'"']|["'"'"'][[:space:]]*$/,""); print; exit
    }' "$CONFIG"
}

# Any VLM section, plus [defaults] — previously this only ever looked at
# [mineru-vlm], so a machine configured for e.g. dots-ocr alone silently
# fell through to native.
endpoint_configured() {
  [ -n "${UPARSER_ENDPOINT:-}" ] && return 0
  for sec in defaults mineru-vlm monkeyocr-v2 navidc-ocr dots-ocr generic-vlm; do
    [ -n "$(read_ini "$sec" endpoint)" ] && return 0
  done
  return 1
}

# scan what the caller already provided
has_mode=0 has_protocol=0 has_ep=0 has_format=0
for a in "$@"; do
  case "$a" in
    --mode|--mode=*)         has_mode=1 ;;
    --protocol|--protocol=*) has_protocol=1 ;;
    --endpoint|--endpoint=*) has_ep=1 ;;
    --format|--format=*)     has_format=1 ;;
  esac
done

inject=()
[ "$has_format" -eq 0 ] && inject+=(--format markdown)

if [ "$has_mode" -eq 0 ] && [ "$has_protocol" -eq 0 ]; then
  if [ "$has_ep" -eq 1 ] || endpoint_configured; then
    inject+=(--protocol auto)
    echo "uparser-parse: no --protocol given; using 'auto' (endpoint resolved by the binary)" >&2
  else
    inject+=(--protocol native)
    echo "uparser-parse: no --protocol and no endpoint configured; using 'native' (offline; bounded page OCR may apply)" >&2
  fi
fi

# delegate to uparser-run.sh (binary resolution), parse first
exec "$HERE/uparser-run.sh" parse "$@" ${inject[@]+"${inject[@]}"}
