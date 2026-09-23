#!/usr/bin/env bash
# find_uparser.sh — locate (or build) the `uparser` binary from a cargo
# workspace and print its absolute path on stdout.
#
# SCOPE NOTE: this script does NOT check versions and does NOT download.
# `ensure_uparser.sh` is the entry point now; it calls this one for two things:
#   --locate-only   report a local workspace build so ensure can version-compare it
#   --build         the last rung, when nothing is downloadable
# Call ensure_uparser.sh instead unless you specifically want from-source behavior.
#
# Usage: find_uparser.sh [--build] [--locate-only] [--features "native,pdfium"]
# Exit 0 with the path on stdout, or non-zero with an error on stderr.
set -euo pipefail

FEATURES="native,pdfium"
DO_BUILD=0
LOCATE_ONLY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --build)       DO_BUILD=1; shift ;;
    --locate-only) LOCATE_ONLY=1; shift ;;
    --features)    FEATURES="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 1 ;;
  esac
done

# walk up from $1 looking for the cargo workspace; echoes it, or nothing.
find_workspace() {
  dir="$1"
  while :; do
    if [ -f "$dir/uparser/Cargo.toml" ]; then echo "$dir/uparser"; return; fi
    if [ -f "$dir/Cargo.toml" ] && [ -d "$dir/crates/uparser-core" ]; then echo "$dir"; return; fi
    [ "$dir" = "/" ] && return
    dir="$(dirname "$dir")"
  done
}

# Search from this script's own location, then from the caller's cwd.
#
# The cwd root matters because the script is normally *installed* to
# ~/.claude/skills/uparser/scripts, where nothing above it is a checkout — so
# searching only from the script's location fails for every user running the
# skill from inside the repo, which is precisely the case where a locally built
# binary does exist.
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cwd="$PWD"
ws="$(find_workspace "$here")"
[ -n "$ws" ] || ws="$(find_workspace "$cwd")"

# --locate-only: report ONLY a local workspace build, so ensure_uparser.sh can
# version-compare it. Deliberately ignores $UPARSER_BIN and PATH — ensure
# handles those itself, at different priorities, with different rules. Folding
# them in here would make ensure label a PATH binary "the workspace build".
if [ "$LOCATE_ONLY" -eq 1 ]; then
  [ -n "$ws" ] || exit 3
  for cand in "$ws/target/release/uparser" "$ws/target/release/uparser.exe"; do
    [ -x "$cand" ] && { echo "$cand"; exit 0; }
  done
  exit 3
fi

# Direct invocation keeps its historical behavior, but callers should migrate:
# this path can hand back an arbitrarily old binary with no notice at all.
if [ "$DO_BUILD" -eq 0 ] && [ "${UPARSER_ENSURE_INTERNAL:-0}" != "1" ]; then
  echo "find_uparser.sh: note — this resolver does not check versions or download." >&2
  echo "                 Prefer ensure_uparser.sh, which keeps you on a current build." >&2
fi

# 0) an explicit override always wins.
if [ -n "${UPARSER_BIN:-}" ]; then
  if [ -x "$UPARSER_BIN" ]; then echo "$UPARSER_BIN"; exit 0; fi
  echo "UPARSER_BIN is set to '$UPARSER_BIN' but that is not an executable file" >&2
  exit 2
fi

# 1) already on PATH?
if [ "$DO_BUILD" -eq 0 ] && command -v uparser >/dev/null 2>&1; then
  command -v uparser
  exit 0
fi

if [ -z "$ws" ]; then
  echo "could not locate the uparser workspace (no uparser/Cargo.toml found above $here or $cwd)" >&2
  echo "set UPARSER_BIN=/path/to/uparser, or run this from inside the checkout" >&2
  exit 2
fi

bin="$ws/target/release/uparser"
[ -x "$bin" ] || [ ! -x "$bin.exe" ] || bin="$bin.exe"
if [ -x "$bin" ] && [ "$DO_BUILD" -eq 0 ]; then
  echo "$bin"; exit 0
fi

# 2) build it (release). Needs cargo, plus network for the first pdfium fetch
#    if that feature is on.
echo "building uparser (features: $FEATURES) — first build may take a few minutes..." >&2
( cd "$ws" && cargo build --release --features "$FEATURES" >&2 )
for cand in "$ws/target/release/uparser" "$ws/target/release/uparser.exe"; do
  [ -x "$cand" ] && { echo "$cand"; exit 0; }
done
echo "build finished but binary not found under $ws/target/release" >&2
exit 3
