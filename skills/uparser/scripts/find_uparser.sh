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

# 1) already on PATH?
if command -v uparser >/dev/null 2>&1; then
  command -v uparser
  exit 0
fi

# 2) find the uparser workspace by walking up from this script.
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dir="$here"
ws=""
while [ "$dir" != "/" ]; do
  if [ -f "$dir/uparser/Cargo.toml" ]; then ws="$dir/uparser"; break; fi
  if [ -f "$dir/Cargo.toml" ] && [ -d "$dir/crates/uparser-core" ]; then ws="$dir"; break; fi
  dir="$(dirname "$dir")"
done

if [ -z "$ws" ]; then
  echo "could not locate the uparser workspace (no uparser/Cargo.toml found above $here)" >&2
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
