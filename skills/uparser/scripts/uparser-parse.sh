#!/usr/bin/env bash
# uparser-parse.sh — one-shot "do the right thing" parse for coding agents.
# Give it a file; it returns Markdown on stdout and a semantic exit code.
#
# What it decides for you (so an agent doesn't have to):
#   * ensures the `uparser` binary exists (downloads/builds via ensure_uparser.sh);
#   * NEVER uses the `mock` protocol (the raw binary's silent default) — so you
#     never get placeholder text back by accident;
#   * picks the protocol automatically when you don't pass --protocol:
#       - a VLM endpoint is resolvable (‑‑endpoint / $UPARSER_ENDPOINT / config)
#         → `--protocol auto` (Profiler routes born‑digital→native, scans→VLM),
#         with the endpoint/model injected for the VLM branch;
#       - otherwise → `--protocol native` (pure‑Rust, offline, no GPU).
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

# read one key from an INI section of the config (quiet if absent)
read_ini() { # $1=section $2=key
  [ -f "$CONFIG" ] || return 0
  awk -v s="[$1]" -v k="$2" '
    /^[[:space:]]*\[/ { cur=$0; gsub(/^[[:space:]]+|[[:space:]]+$/,"",cur) }
    cur==s && $0 ~ "^[[:space:]]*"k"[[:space:]]*=" {
      sub(/^[^=]*=[[:space:]]*/,""); gsub(/^["'"'"']|["'"'"'][[:space:]]*$/,""); print; exit
    }' "$CONFIG"
}

# scan what the caller already provided
has_protocol=0 has_ep=0 has_model=0 has_format=0
for a in "$@"; do
  case "$a" in
    --protocol|--protocol=*) has_protocol=1 ;;
    --endpoint|--endpoint=*) has_ep=1 ;;
    --model|--model=*)       has_model=1 ;;
    --format|--format=*)     has_format=1 ;;
  esac
done

inject=()
[ "$has_format" -eq 0 ] && inject+=(--format markdown)

if [ "$has_protocol" -eq 0 ]; then
  # resolve a VLM endpoint from flags → env → config[mineru-vlm]
  ep="${UPARSER_ENDPOINT:-}"; [ -n "$ep" ] || ep="$(read_ini mineru-vlm endpoint)"
  md="${UPARSER_MODEL:-}";    [ -n "$md" ] || md="$(read_ini mineru-vlm model)"
  if [ "$has_ep" -eq 1 ] || [ -n "$ep" ]; then
    inject+=(--protocol auto)
    [ "$has_ep" -eq 0 ]    && [ -n "$ep" ] && inject+=(--endpoint "$ep")
    [ "$has_model" -eq 0 ] && [ -n "$md" ] && inject+=(--model "$md")
    echo "uparser-parse: no --protocol given; using 'auto' with endpoint ${ep:-<from cli>}" >&2
  else
    inject+=(--protocol native)
    echo "uparser-parse: no --protocol and no endpoint; using 'native' (offline, no OCR)" >&2
  fi
fi

# delegate to uparser-run.sh (binary resolution + config injection), parse first
exec "$HERE/uparser-run.sh" parse "$@" ${inject[@]+"${inject[@]}"}
