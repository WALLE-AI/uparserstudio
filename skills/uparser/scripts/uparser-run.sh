#!/usr/bin/env bash
# uparser-run.sh — locate (or download/build) the uparser binary and exec it.
#
# This script used to also read ~/.config/uparser/config.toml and inject
# --endpoint/--model. That is gone: the binary now reads the same file itself,
# and does it correctly in three ways this wrapper could not.
#
#   - It keys the lookup on the protocol chosen AFTER routing. This wrapper
#     used the protocol as literally written on the command line, so a plain
#     `uparser-run.sh parse doc.pdf` looked up a section named `[auto]` and
#     silently injected nothing.
#   - It layers `[defaults]` under `[<protocol>]`, per key.
#   - It resolves api_key/headers/timeout_secs/max_retries and pipeline's
#     per-stage endpoints, none of which this wrapper ever handled.
#
# So this is now purely about finding the binary. Configuration lives in
# ~/.config/uparser/config.toml (override with $UPARSER_CONFIG) —
# see ../references/config.example.toml.
#
# Usage: uparser-run.sh parse --protocol mineru-vlm doc.pdf
set -euo pipefail

# --- locate the real binary: PATH first, else ensure_uparser.sh downloads a
#     version-pinned prebuilt from GitHub Releases (or builds from source) ---
BIN="${UPARSER_BIN:-$(command -v uparser || true)}"
if [ -z "$BIN" ]; then
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  BIN="$("$here/ensure_uparser.sh" | tail -1 || true)"
fi
[ -n "$BIN" ] && [ -x "$BIN" ] || { echo "uparser binary not found and could not be downloaded/built" >&2; exit 2; }

exec "$BIN" "$@"
