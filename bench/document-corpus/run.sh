#!/bin/bash
# Head-to-head corpus harness: uparser native vs anydoc.
# Reproduces the tables in NATIVE_VS_ANYDOC_EVALUATION_AND_PLAN.md §3.1 / §3.3.
#
#   bash bench/document-corpus/run.sh [outdir]
#
# Prereqs:
#   cd uparser && cargo build --release --features native -p uparser-core --bin uparser
#   cd opensource/anydoc && cargo build --release --example convert
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
UP="$ROOT/uparser/target/release/uparser.exe"
AD="$ROOT/opensource/anydoc/target/release/examples/convert.exe"
FIX="$ROOT/opensource/anydoc/tests/fixtures"
OUT="${1:-$ROOT/bench/document-corpus/out}"

# uparser itself has no dependency on anydoc: the engine's own regression
# tests (crates/uparser-document-engine/src/lib.rs) build every fixture they
# need in-process. This script is a *development-only* side-by-side against an
# external converter, and it degrades to a uparser-only run when that
# converter is not built.
[ -x "$UP" ] || { echo "missing $UP" >&2; exit 1; }
COMPARE=1
[ -x "$AD" ] || { echo "note: comparison binary not built; reporting uparser only" >&2; COMPARE=0; }
[ -d "$FIX" ] || { echo "missing corpus dir $FIX" >&2; exit 1; }

mkdir -p "$OUT/anydoc" "$OUT/native"

echo "=== formats: per-file success / size / wall-ms ==="
printf "%-6s %-32s %6s %9s %7s %6s %9s %7s\n" \
  format file ad_rc ad_bytes ad_ms nv_rc nv_bytes nv_ms
ad_ok=0; ad_n=0; nv_ok=0; nv_n=0
for d in csv doc docx epub odp ods odt ppt pptx rtf xls xlsx; do
  for f in "$FIX/$d"/*; do
    [ -f "$f" ] || continue
    b=$(basename "$f")
    adrc=-; adms=-; adb=-
    if [ "$COMPARE" -eq 1 ]; then
      s=$(date +%s%N); "$AD" "$f" -o "$OUT/anydoc/$d-$b.md" >/dev/null 2>&1; adrc=$?
      e=$(date +%s%N); adms=$(( (e-s)/1000000 ))
      adb=0; [ -f "$OUT/anydoc/$d-$b.md" ] && adb=$(wc -c < "$OUT/anydoc/$d-$b.md")
    fi

    s=$(date +%s%N)
    "$UP" parse "$f" --protocol native --format markdown --no-assets \
      > "$OUT/native/$d-$b.md" 2>"$OUT/native/$d-$b.err"; nvrc=$?
    e=$(date +%s%N); nvms=$(( (e-s)/1000000 ))
    nvb=$(wc -c < "$OUT/native/$d-$b.md")

    printf "%-6s %-32s %6s %9s %7s %6s %9s %7s\n" \
      "$d" "$b" "$adrc" "$adb" "$adms" "$nvrc" "$nvb" "$nvms"
    ad_n=$((ad_n+1)); nv_n=$((nv_n+1))
    [ "$adrc" = "0" ] && ad_ok=$((ad_ok+1))
    [ "$nvrc" -eq 0 ] && nv_ok=$((nv_ok+1))
  done
done
echo
echo "success: anydoc $ad_ok/$ad_n   native $nv_ok/$nv_n"

echo
echo "=== abuse (expect NON-zero rc) / malformed (--recovers expect rc 0) ==="
printf "%-34s %6s %6s  %s\n" file ad_rc nv_rc nv_message
for f in "$FIX"/abuse/* "$FIX"/malformed/*; do
  [ -f "$f" ] || continue
  b=$(basename "$f")
  adrc=-
  [ "$COMPARE" -eq 1 ] && { timeout 30 "$AD" "$f" -o /dev/null >/dev/null 2>&1; adrc=$?; }
  timeout 30 "$UP" parse "$f" --protocol native --format markdown \
    >/dev/null 2>"$OUT/err.txt"; nvrc=$?
  printf "%-34s %6s %6s  %s\n" "$b" "$adrc" "$nvrc" \
    "$(head -c 100 "$OUT/err.txt" | tr '\n' ' ')"
done
rm -f "$OUT/err.txt"

echo
echo "outputs under $OUT"
