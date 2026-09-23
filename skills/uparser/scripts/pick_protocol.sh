#!/usr/bin/env bash
# pick_protocol.sh — choose the protocol that gives the best OUTPUT QUALITY
# among the model endpoints this machine can actually reach right now.
#
# Shared by uparser-parse.sh and uparser-check.sh so the two cannot disagree
# about what would be used (they used to: check hardcoded mineru-vlm, parse
# enumerated six sections and then handed the decision to `auto`).
#
# ---------------------------------------------------------------------------
# WHY THIS EXISTS AT ALL — `--protocol auto` structurally cannot do this.
# Verified in uparser/crates/uparser-core/src/router.rs:
#   * model_candidate() hardcodes the protocol name "mineru-vlm", so the router
#     can NEVER select navidc-ocr / monkeyocr-v2 / dots-ocr, whatever you have
#     configured. (references/protocols.md:58 states this for navidc-ocr.)
#   * RoutingEnvironment::default() hardcodes model_protocol: true — the router
#     never probes an endpoint, so it believes a dead one is live.
#     (references/protocols.md:79: "does not probe remote endpoints during
#     planning. `doctor` is the explicit network preflight.")
#   * On a born-digital PDF it scores native 100 vs mineru-vlm 55, so `auto`
#     picks native. That is the 0.8920-vs-0.9252 gap in UPARSER_LEADERBOARD.md.
# And `uparser parse` has no --prefer flag (only `uparser plan` does). So the
# policy has to live here, in the wrapper.
# ---------------------------------------------------------------------------
#
# Usage: pick_protocol.sh --bin <uparser> [--file <path>] [--refresh] [--json]
# stdout: the protocol name, or the JSON object with --json
# exit:   0 always — it always names something, worst case "native".
set -euo pipefail

BIN=""; FILE=""; REFRESH=0; AS_JSON=0
while [ $# -gt 0 ]; do
  case "$1" in
    --bin)     BIN="$2"; shift 2 ;;
    --file)    FILE="$2"; shift 2 ;;
    --refresh) REFRESH=1; shift ;;
    --json)    AS_JSON=1; shift ;;
    *) shift ;;
  esac
done
[ -n "$BIN" ] || { echo "pick_protocol.sh: --bin is required" >&2; exit 1; }

CONFIG="${UPARSER_CONFIG:-$HOME/.config/uparser/config.toml}"
SKILL_STATE="${UPARSER_SKILL_HOME:-$HOME/.cache/uparser-skill}"
TTL_OK="${UPARSER_PROBE_TTL:-300}"            # a reachable answer is stable enough
TTL_NONE="${UPARSER_PROBE_NEGATIVE_TTL:-60}"  # but "nothing reachable" must expire
                                              # fast, or an endpoint that comes up
                                              # mid-batch is ignored for the rest of it

# Quality order. Evidence, both benchmarks in UPARSER_LEADERBOARD.md:
# opendataloader-bench overall — mineru-vlm .9252 > pipeline .9086 >
# navidc-ocr .9053 > monkeyocr-v2 .8824 > native .8766; OmniDocBench —
# navidc-ocr best tables/formulas, monkeyocr-v2 best text/reading-order.
DEFAULT_ORDER="mineru-vlm navidc-ocr monkeyocr-v2 pipeline dots-ocr paddlex-structure generic-vlm"
ORDER="${UPARSER_QUALITY_ORDER:-$DEFAULT_ORDER}"
# `mock` is deliberately absent and must stay absent: it emits placeholder
# output and is explicit-only (SKILL.md). `native`/`tesseract` are the
# fallbacks below, not candidates to probe.

emit() { # <protocol> <reason> <source> <configured-csv> <probed-json>
  if [ "$AS_JSON" -eq 1 ]; then
    printf '{"protocol":"%s","reason":"%s","source":"%s","configured":[%s],"probed":[%s]}\n' \
      "$1" "$2" "$3" "$4" "$5"
  else
    printf '%s\n' "$1"
  fi
  exit 0
}

# --- format gate -------------------------------------------------------------
# Graded by what the format actually gives up, NOT a blanket "structured =
# native". Three tiers:
#
#  (1) pdf/png/jpeg — a visual channel is the only channel. Probe.
#
#  (2) pptx/ppt/odp — slides ARE visual layout: absolutely-positioned text
#      boxes with no reading-order semantics to preserve. uparser's own router
#      agrees and scores a presentation +35 toward the model and -35 against
#      native even when the source is structured (router.rs, and the test
#      `presentation_routes_model_even_when_structured`). So probe these too —
#      but only if LibreOffice is actually installed, because the model route
#      needs it to materialize slides and its absence is a hard environment
#      failure (references/protocols.md:126).
#
#  (3) everything else structured (docx/xlsx/csv/odt/rtf/epub) — these carry
#      exact structure a model could only ever re-infer from pixels: real table
#      cells with spans, real list nesting, real headings. xlsx/csv do not even
#      rasterize (they read cells directly), and `--format document-json`, the
#      only lossless view with row/column spans, exists for these sources ONLY.
#      protocols.md:126: "visual conversion discards source semantics."
#      Going through a model here is slower, needs LibreOffice, and is strictly
#      worse — while still emitting plausible-looking Markdown, which is what
#      makes it dangerous as a default.
if [ -n "$FILE" ]; then
  ext="$(printf '%s' "${FILE##*.}" | tr '[:upper:]' '[:lower:]')"
  case "$ext" in
    pdf|png|jpg|jpeg) ;;
    pptx|ppt|odp)
      if command -v soffice >/dev/null 2>&1 || command -v libreoffice >/dev/null 2>&1; then
        echo "uparser-parse: $ext is slide layout; probing model protocols (router scores" >&2
        echo "               presentations toward a model even when structured)" >&2
      else
        echo "uparser-parse: $ext would benefit from a model route, but LibreOffice is not" >&2
        echo "               installed to materialize the slides; keeping 'native'" >&2
        emit native no-libreoffice format-gate '' ''
      fi ;;
    *)
      echo "uparser-parse: $ext carries exact structure a model can only re-infer; keeping" >&2
      echo "               'native' (a model route loses source semantics and needs LibreOffice)" >&2
      emit native structured-source format-gate '' '' ;;
  esac
fi

case "${UPARSER_PREFER:-quality}" in
  speed|cost)
    echo "uparser-parse: UPARSER_PREFER=${UPARSER_PREFER}; skipping endpoint probes" >&2
    emit auto preference-speed forced '' '' ;;
esac
if [ "${UPARSER_NO_PROBE:-0}" = "1" ]; then
  emit auto probe-disabled forced '' ''
fi

# --- which protocols are actually configured ---------------------------------
# Same awk reader uparser-parse.sh has always used. It is line-oriented and does
# not understand TOML inline tables, which is fine: it only decides WHETHER to
# probe. The binary itself remains the authority on endpoint resolution.
# ONE awk pass listing every section that carries an `endpoint`, plus a marker
# for [pipeline.stages]. The obvious shape — read_ini per protocol — costs ~21
# process spawns per invocation (7 protocols x endpoint + defaults + section
# checks), and a per-file batch pays that per file; on Windows that measured
# 2.2s per call against a 0.12s process floor.
sections_with_endpoint() {
  [ -f "$CONFIG" ] || return 0
  awk '
    /^[[:space:]]*\[/ { cur=$0; gsub(/^[[:space:]]+|[[:space:]]+$/,"",cur)
                        gsub(/^\[|\]$/,"",cur)
                        if (cur=="pipeline.stages") print "pipeline"
                        next }
    cur!="" && $0 ~ /^[[:space:]]*endpoint[[:space:]]*=/ { print cur; cur="" }
  ' "$CONFIG"
}
# Hoisted out of the loop on purpose: this used to be recomputed per protocol.
have=" $(sections_with_endpoint | tr '\n' ' ') "

# A [defaults] endpoint, or $UPARSER_ENDPOINT, makes every protocol resolvable.
# That is semantically right (the binary would resolve it), and the
# endpoint-dedupe in the probe loop stops it turning into 7 probes of one URL.
blanket=0
case "$have" in *" defaults "*) blanket=1 ;; esac
[ -n "${UPARSER_ENDPOINT:-}" ] && blanket=1

configured=""; conf_csv=""
for p in $ORDER; do
  keep=0
  if [ "$blanket" -eq 1 ]; then keep=1
  else case "$have" in *" $p "*) keep=1 ;; esac
  fi
  if [ "$keep" -eq 1 ]; then
    configured="${configured:+$configured }$p"
    conf_csv="${conf_csv:+$conf_csv,}\"$p\""
  fi
done

if [ -z "$configured" ]; then
  echo "uparser-parse: no model endpoint configured in $CONFIG; using 'native' (offline)" >&2
  emit native no-endpoint-configured none "" ""
fi

# --- probe cache -------------------------------------------------------------
# Keyed on the config CONTENTS (not its mtime: stat's format flags differ across
# platforms and a copied config would produce a stale hit), plus the endpoint
# env overrides, plus an identity for the binary — protocol defaults and the
# `doctor` contract can change between releases, so a different binary must not
# reuse an old verdict.
#
# The binary identity is its path + size + mtime, NOT `$BIN --version`. Calling
# --version spawns a 14 MB executable on every single invocation, which a
# per-file batch pays per file; `ls -ln` is one stat. Both a downloaded upgrade
# (path contains the version) and a local rebuild (mtime moves) still change it.
bin_id="$(ls -ln "$BIN" 2>/dev/null | awk '{print $5, $6, $7, $8}' || true)"
key_material="$CONFIG|$(cat "$CONFIG" 2>/dev/null || true)|${UPARSER_ENDPOINT:-}|${UPARSER_MODEL:-}|$BIN|$bin_id|$ORDER"
KEY="$(printf '%s' "$key_material" | sha256sum 2>/dev/null | cut -c1-16)"
PROBE="$SKILL_STATE/probe-${KEY:-none}.txt"

if [ "$REFRESH" -eq 0 ] && [ -f "$PROBE" ]; then
  p_ts="$(awk '{print $1}' "$PROBE" 2>/dev/null || true)"
  p_proto="$(awk '{print $2}' "$PROBE" 2>/dev/null || true)"
  if [ -n "${p_ts:-}" ] && [ -n "${p_proto:-}" ]; then
    age=$(( $(date +%s) - p_ts ))
    ttl="$TTL_OK"; [ "$p_proto" = "native" ] && ttl="$TTL_NONE"
    if [ "$age" -ge 0 ] && [ "$age" -le "$ttl" ]; then
      emit "$p_proto" cached cache "$conf_csv" ""
    fi
  fi
fi

remember_probe() {
  mkdir -p "$SKILL_STATE" 2>/dev/null || return 0
  printf '%s %s\n' "$(date +%s)" "$1" > "$PROBE.$$" 2>/dev/null \
    && mv -f "$PROBE.$$" "$PROBE" 2>/dev/null || true
}

# --- probe -------------------------------------------------------------------
probed=""; seen_endpoints=""
for p in $configured; do
  out="$("$BIN" doctor "$p" 2>/dev/null || true)"
  ep="$(printf '%s' "$out" | grep -o '"endpoint"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"\([^"]*\)"$/\1/' | head -1)"
  # Skip a protocol whose endpoint we already probed — the [defaults] case would
  # otherwise probe one URL up to seven times, at ~2.5s each when it is down.
  case " $seen_endpoints " in *" $ep "*) continue ;; esac
  [ -n "$ep" ] && seen_endpoints="$seen_endpoints $ep"

  # `doctor` ALWAYS exits 0 and reports status in this field — never read $?.
  # And any HTTP answer counts as reachable: the real MinerU deployment replies
  # 405 to this probe, so requiring 200 would mark a working endpoint dead.
  reach=false
  case "$out" in *'"reachable"'*true*) reach=true ;; esac
  detail="$(printf '%s' "$out" | grep -o '"detail"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"\([^"]*\)"$/\1/' | head -1)"
  probed="$probed{\"protocol\":\"$p\",\"reachable\":$reach,\"endpoint\":\"$ep\"},"

  if [ "$reach" = "true" ]; then
    echo "uparser-parse: using '$p' for quality (reachable at $ep)" >&2
    remember_probe "$p"
    emit "$p" reachable probe "$conf_csv" "${probed%,}"
  fi
  echo "uparser-parse: $p unreachable at ${ep:-?}${detail:+ — $detail}" >&2
done

# Nothing reachable. Fall back to native, NOT to `auto`:
# RoutingEnvironment::default() hardcodes model_protocol: true, so `auto` still
# believes a model endpoint exists and can route to the dead one — failing late,
# or appearing to have used a VLM when it did not.
echo "uparser-parse: no configured model endpoint is reachable; using 'native'" >&2
remember_probe native
emit native no-endpoint-reachable probe "$conf_csv" "${probed%,}"
