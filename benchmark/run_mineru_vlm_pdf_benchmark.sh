#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
UPARSER_BIN=${UPARSER_BIN:-$ROOT/uparser/target/release/uparser}
PDF=${1:-}
OUTPUT_PREFIX=${2:-$ROOT/output/mineru-vlm-c1024}
ENDPOINT=${MINERU_VLM_ENDPOINT:-http://127.0.0.1:19122/v1/chat/completions}
MODEL=${MINERU_VLM_MODEL:-MinerU2.5-Pro-2605-1.2B}
CLIENT_CONCURRENCY=${MINERU_VLM_CONCURRENCY:-1024}
SERVER_MAX_NUM_SEQS=${MINERU_VLM_SERVER_MAX_NUM_SEQS:-}
RASTER_DPI=${MINERU_VLM_RASTER_DPI:-200}

usage() {
  printf '%s\n' \
    "Usage: benchmark/run_mineru_vlm_pdf_benchmark.sh PDF [OUTPUT_PREFIX]" \
    "" \
    "Required environment:" \
    "  MINERU_VLM_SERVER_MAX_NUM_SEQS  max-num-seqs used to start vLLM" \
    "" \
    "Optional environment:" \
    "  MINERU_VLM_ENDPOINT             default: $ENDPOINT" \
    "  MINERU_VLM_MODEL                default: $MODEL" \
    "  MINERU_VLM_CONCURRENCY          default: 1024" \
    "  MINERU_VLM_RASTER_DPI           default: 200"
}

if [[ -z "$PDF" || "$PDF" == "-h" || "$PDF" == "--help" ]]; then
  usage
  [[ -n "$PDF" ]] && exit 0
  exit 2
fi

if [[ ! -f "$PDF" ]]; then
  printf 'PDF not found: %s\n' "$PDF" >&2
  exit 2
fi
if [[ ! -x "$UPARSER_BIN" ]]; then
  printf 'uparser release binary not found: %s\n' "$UPARSER_BIN" >&2
  exit 2
fi
if [[ ! "$CLIENT_CONCURRENCY" =~ ^[0-9]+$ ]] || (( CLIENT_CONCURRENCY < 1000 )); then
  printf 'MINERU_VLM_CONCURRENCY must be an integer >= 1000 (got %s)\n' \
    "$CLIENT_CONCURRENCY" >&2
  exit 2
fi
if [[ ! "$SERVER_MAX_NUM_SEQS" =~ ^[0-9]+$ ]]; then
  printf '%s\n' \
    'MINERU_VLM_SERVER_MAX_NUM_SEQS must declare the vLLM --max-num-seqs value.' \
    'Refusing to equate HTTP queue depth with model execution concurrency.' >&2
  exit 2
fi
if (( SERVER_MAX_NUM_SEQS < 1000 )); then
  printf 'server max-num-seqs must be >= 1000 (got %s)\n' \
    "$SERVER_MAX_NUM_SEQS" >&2
  exit 2
fi

mkdir -p "$(dirname "$OUTPUT_PREFIX")"
MARKDOWN="${OUTPUT_PREFIX}.md"
TIMING="${OUTPUT_PREFIX}.timing.txt"
REPORT="${OUTPUT_PREFIX}.benchmark.md"

env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY \
  -u http_proxy -u https_proxy -u all_proxy \
  NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \
  /usr/bin/time \
    -f 'real_seconds=%e\nuser_seconds=%U\nsystem_seconds=%S\nmax_rss_kib=%M\nexit_code=%x' \
    -o "$TIMING" \
    "$UPARSER_BIN" parse "$PDF" \
      --protocol mineru-vlm \
      --endpoint "$ENDPOINT" \
      --model "$MODEL" \
      --format markdown \
      --output "$MARKDOWN" \
      --raster-dpi "$RASTER_DPI" \
      --window-size "$CLIENT_CONCURRENCY" \
      --max-concurrency "$CLIENT_CONCURRENCY" \
      --no-cache \
      --no-assets

bytes=$(stat -c '%s' "$MARKDOWN")
lines=$(wc -l < "$MARKDOWN")
sha256=$(sha256sum "$MARKDOWN" | awk '{print $1}')
headings=$(rg -c '^#{1,6} ' "$MARKDOWN" || true)
tables=$(rg -c '<table' "$MARKDOWN" || true)
real_seconds=$(awk -F= '$1 == "real_seconds" {print $2}' "$TIMING")
max_rss_kib=$(awk -F= '$1 == "max_rss_kib" {print $2}' "$TIMING")

{
  printf '# MinerU-VLM PDF benchmark\n\n'
  printf -- '- PDF: `%s`\n' "$PDF"
  printf -- '- Endpoint: `%s`\n' "$ENDPOINT"
  printf -- '- Model: `%s`\n' "$MODEL"
  printf -- '- Client concurrency: `%s`\n' "$CLIENT_CONCURRENCY"
  printf -- '- Declared server max-num-seqs: `%s`\n' "$SERVER_MAX_NUM_SEQS"
  printf -- '- Raster DPI: `%s`\n' "$RASTER_DPI"
  printf -- '- Real seconds: `%s`\n' "$real_seconds"
  printf -- '- Peak client RSS KiB: `%s`\n' "$max_rss_kib"
  printf -- '- Markdown bytes: `%s`\n' "$bytes"
  printf -- '- Markdown lines: `%s`\n' "$lines"
  printf -- '- Headings: `%s`\n' "${headings:-0}"
  printf -- '- Tables: `%s`\n' "${tables:-0}"
  printf -- '- SHA-256: `%s`\n' "$sha256"
} > "$REPORT"

printf 'benchmark report: %s\n' "$REPORT"
