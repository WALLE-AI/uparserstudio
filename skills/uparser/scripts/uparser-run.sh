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

# --- locate the real binary: always via ensure_uparser.sh ---
#
# This deliberately does NOT short-circuit to `command -v uparser` first.
# It used to, and that made ensure_uparser.sh's whole version-checking and
# supersede ladder dead code on the most common call path: a machine with any
# old `uparser` on PATH would keep using it forever, silently, with the upgrade
# logic never running. ensure_uparser.sh consults PATH itself, at the right
# priority and with a version comparison. $UPARSER_BIN is likewise handled
# there (and still wins unconditionally).
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$("$here/ensure_uparser.sh" | tail -1 || true)"
[ -n "$BIN" ] && [ -x "$BIN" ] || { echo "uparser binary not found and could not be downloaded/built" >&2; exit 2; }

exec "$BIN" "$@"
