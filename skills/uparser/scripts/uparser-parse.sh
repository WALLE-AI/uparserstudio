#!/usr/bin/env bash
# uparser-parse.sh — one-shot "do the right thing" parse for coding agents.
# Give it a file; it returns Markdown on stdout and a semantic exit code.
#
# What it decides for you (so an agent doesn't have to):
#   * ensures a current `uparser` binary exists (ensure_uparser.sh: version
#     check against GitHub Releases, TTL-cached, degrades offline);
#   * NEVER selects the explicit-only `mock` protocol;
#   * picks the protocol when you pass neither --mode nor --protocol, and by
#     default picks for QUALITY: it probes the model endpoints you actually
#     configured and runs the best reachable one (pick_protocol.sh);
#   * defaults --format to markdown (override with --format json).
#
# Why quality-first is not just `--protocol auto`: `auto` never probes an
# endpoint, its model candidate is hardwired to mineru-vlm, and on a
# born-digital PDF it scores native above every model — see the long comment in
# pick_protocol.sh for the verified specifics.
#
# The cost is real and deliberate: on a born-digital PDF a VLM route trades
# roughly 15x wall-clock for about +0.05 overall accuracy (UPARSER_LEADERBOARD.md).
# Set UPARSER_PREFER=speed to get the old endpoint-agnostic routing back.
# Structured sources (DOCX/XLSX/CSV/...) always stay native regardless.
#
# Anything you pass through (--pages, --max-concurrency, --no-cache, an explicit
# --protocol/--endpoint/--model, ...) is forwarded unchanged and always wins.
#
# Usage:
#   uparser-parse.sh <file> [any uparser parse flags...]
#   UPARSER_PREFER=speed uparser-parse.sh scan.pdf
#
# Exit codes are the binary's own: 0 ok · 1 usage · 2 env/endpoint · 3 partial
# (usable, check page_errors) · 4 internal.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

[ "$#" -ge 1 ] || { echo "usage: uparser-parse.sh <file> [uparser parse flags...]" >&2; exit 1; }

# scan what the caller already provided, and find the input file (the first
# argument that is not a flag and not a flag's value)
has_mode=0 has_protocol=0 has_format=0
FILE=""; skip_next=0
for a in "$@"; do
  if [ "$skip_next" -eq 1 ]; then skip_next=0; continue; fi
  case "$a" in
    --mode|--mode=*)         has_mode=1 ;;
    --protocol|--protocol=*) has_protocol=1 ;;
    --format|--format=*)     has_format=1 ;;
  esac
  case "$a" in
    --*=*) ;;
    --*)   skip_next=1 ;;      # this flag takes a separate value
    *)     [ -z "$FILE" ] && FILE="$a" ;;
  esac
done

inject=()
[ "$has_format" -eq 0 ] && inject+=(--format markdown)

if [ "$has_mode" -eq 0 ] && [ "$has_protocol" -eq 0 ]; then
  # Resolve the binary once here and hand the same one to both the protocol
  # probe and the run, instead of resolving it twice.
  BIN="$("$HERE/ensure_uparser.sh" | tail -1 || true)"
  [ -n "$BIN" ] && [ -x "$BIN" ] || { echo "uparser binary not found and could not be downloaded/built" >&2; exit 2; }
  export UPARSER_BIN="$BIN"

  proto="$("$HERE/pick_protocol.sh" --bin "$BIN" ${FILE:+--file "$FILE"} || echo native)"
  inject+=(--protocol "$proto")
fi

# delegate to uparser-run.sh (binary resolution), parse first
exec "$HERE/uparser-run.sh" parse "$@" ${inject[@]+"${inject[@]}"}
