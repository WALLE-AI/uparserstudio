#!/usr/bin/env bash
# Locate (or build) the `uparser` binary and print its absolute path on stdout.
# Usage: find_uparser.sh [--build] [--features "native,pdfium"]
# Exit 0 with the path on stdout, or non-zero with an error on stderr.
set -euo pipefail

FEATURES="native,pdfium"
DO_BUILD=0
while [ $# -gt 0 ]; do
  case "$1" in
    --build) DO_BUILD=1; shift ;;
    --features) FEATURES="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 1 ;;
  esac
done

# 0) an explicit override always wins. SKILL.md offers `export UPARSER_BIN=...`
#    as the alternative to this script, but `UP=$(find_uparser.sh)` used to
#    ignore it entirely — so the documented escape hatch didn't work through
#    the documented entry point.
if [ -n "${UPARSER_BIN:-}" ]; then
  if [ -x "$UPARSER_BIN" ]; then
    echo "$UPARSER_BIN"; exit 0
  fi
  echo "UPARSER_BIN is set to '$UPARSER_BIN' but that is not an executable file" >&2
  exit 2
fi

# 1) already on PATH?
#    Kept ahead of the workspace search: an installed `uparser` is the normal
#    deployment, and overriding it is what UPARSER_BIN above is for.
if command -v uparser >/dev/null 2>&1; then
  command -v uparser
  exit 0
fi

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

# 2) find the uparser workspace: from this script, then from the caller's cwd.
#
#    The cwd root matters because the script is normally *installed* to
#    ~/.claude/skills/uparser/scripts, where nothing above it is a checkout —
#    so searching only from the script's own location fails with exit 2 for
#    every user running the skill from inside the repo, which is precisely
#    the case where a locally built binary does exist. Worse, callers that
#    fall back to ensure_uparser.sh would then download an older pinned
#    release and use it without complaint.
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cwd="$PWD"
ws="$(find_workspace "$here")"
[ -n "$ws" ] || ws="$(find_workspace "$cwd")"

if [ -z "$ws" ]; then
  echo "could not locate the uparser workspace (no uparser/Cargo.toml found above $here or $cwd)" >&2
  echo "set UPARSER_BIN=/path/to/uparser, or run this from inside the checkout" >&2
  exit 2
fi

bin="$ws/target/release/uparser"
if [ -x "$bin" ] && [ "$DO_BUILD" -eq 0 ]; then
  echo "$bin"; exit 0
fi

# 3) build it (release). Needs cargo + network for first pdfium fetch if that feature is on.
echo "building uparser (features: $FEATURES) — first build may take a few minutes..." >&2
( cd "$ws" && cargo build --release --features "$FEATURES" >&2 )
if [ -x "$bin" ]; then echo "$bin"; exit 0; fi
echo "build finished but binary not found at $bin" >&2
exit 3
