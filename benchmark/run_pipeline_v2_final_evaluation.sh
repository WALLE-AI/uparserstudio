#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
OMNI_ROOT="$ROOT/benchmark/OmniDocBench"
RUN_NAME=${PIPELINE_V2_EVAL_NAME:-pipeline-v2-bare-rust-final-full-20260901}
CONFIG=${PIPELINE_V2_OMNI_CONFIG:-$OMNI_ROOT/configs/pipeline_v2_bare_rust_final_full_20260901.yaml}
PREDICTIONS=${PIPELINE_V2_OMNI_PREDICTIONS:-$ROOT/benchmark/results/$RUN_NAME}
SUMMARY="$PREDICTIONS/summary.json"
METRICS="$OMNI_ROOT/result/${RUN_NAME}_quick_match_metric_result.json"
STAGE_EXECUTION="$OMNI_ROOT/result/${RUN_NAME}_quick_match_stage_execution.json"
ODL_CANDIDATE=${PIPELINE_V2_ODL_CANDIDATE:-$ROOT/opensource/opendataloader-bench/prediction/uparser-pipeline-v2-bare-rust-calibrated-20260901/evaluation.json}
ODL_BASELINE=${PIPELINE_V2_ODL_BASELINE:-$ROOT/opensource/opendataloader-bench/prediction/mineru-3.4.5-pipeline-20260825/evaluation.json}
OMNI_BASELINE=${PIPELINE_V2_OMNI_BASELINE:-$OMNI_ROOT/result/mineru-3.4.5-ppformula-20260825_quick_match_metric_result.json}
ACCURACY_GATE=${PIPELINE_V2_ACCURACY_GATE:-$ROOT/benchmark/results/${RUN_NAME}_accuracy_gate.json}
FINAL_REPORT=${PIPELINE_V2_FINAL_REPORT:-$ROOT/benchmark/results/${RUN_NAME}_final_gate.json}
LOG=${PIPELINE_V2_EVAL_LOG:-$ROOT/benchmark/results/${RUN_NAME}_evaluation.log}
OMNI_BASELINE_SECONDS=${PIPELINE_V2_OMNI_BASELINE_SECONDS:-1.003}
ODL_BASELINE_SECONDS=${PIPELINE_V2_ODL_BASELINE_SECONDS:-0.715631}
REUSE_METRICS=0

usage() {
  printf '%s\n' \
    "Usage: benchmark/run_pipeline_v2_final_evaluation.sh [--reuse-metrics]" \
    "" \
    "Runs the official OmniDocBench matcher/metrics, validates evaluator integrity," \
    "compares accuracy against MinerU 3.4.5, and writes a combined accuracy/performance gate." \
    "Use --reuse-metrics to validate and gate an already completed evaluator run."
}

case "${1:-}" in
  "") ;;
  --reuse-metrics) REUSE_METRICS=1 ;;
  -h|--help) usage; exit 0 ;;
  *) usage >&2; exit 2 ;;
esac

require_file() {
  if [[ ! -f "$1" ]]; then
    printf 'missing required file: %s\n' "$1" >&2
    exit 2
  fi
}

require_file "$CONFIG"
require_file "$SUMMARY"
require_file "$ODL_CANDIDATE"
require_file "$ODL_BASELINE"
require_file "$OMNI_BASELINE"
require_file "$OMNI_ROOT/.venv/bin/python"
command -v jq >/dev/null
command -v flock >/dev/null

expected_pages=$(jq 'length' "$ROOT/benchmark/OmniDocBenchData/OmniDocBench.json")
prediction_pages=$(find "$PREDICTIONS" -maxdepth 1 -type f -name '*.md' -printf '.' | wc -c)
summary_success=$(jq -r '.success' "$SUMMARY")
summary_failures=$(jq -r '.failures | length' "$SUMMARY")
model_contract=$(jq -r '.model_service_contract' "$SUMMARY")
empty_predictions=$(find "$PREDICTIONS" -maxdepth 1 -type f -name '*.md' -empty -printf '.' | wc -c)

if [[ "$prediction_pages" -ne "$expected_pages" || "$summary_success" -ne "$expected_pages" ]]; then
  printf 'prediction completeness failure: files=%s success=%s expected=%s\n' \
    "$prediction_pages" "$summary_success" "$expected_pages" >&2
  exit 1
fi
if [[ "$summary_failures" -ne 0 || "$empty_predictions" -ne 0 ]]; then
  printf 'prediction integrity failure: failures=%s empty=%s\n' \
    "$summary_failures" "$empty_predictions" >&2
  exit 1
fi
if [[ "$model_contract" != bare_tensor ]]; then
  printf 'invalid model service contract: %s (expected bare_tensor)\n' "$model_contract" >&2
  exit 1
fi

mkdir -p "$(dirname "$FINAL_REPORT")"
exec 9>"$ROOT/benchmark/results/${RUN_NAME}.evaluation.lock"
if ! flock -n 9; then
  printf 'another evaluation wrapper already owns the lock for %s\n' "$RUN_NAME" >&2
  exit 2
fi

if [[ "$REUSE_METRICS" -eq 0 ]]; then
  export MPLCONFIGDIR=${MPLCONFIGDIR:-/tmp/omnidoc-mpl}
  export PERL5LIB=${PERL5LIB:-/tmp/omnidoc-texlive-root/usr/share/texlive/tlpkg}
  export LD_LIBRARY_PATH="/tmp/omnidoc-texlive-root/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  export PATH="/tmp/omnidoc-texlive-root/usr/bin:$PATH"
  export TEXMFCNF=${TEXMFCNF:-/tmp/omnidoc-texlive-root/usr/share/texlive/texmf-dist/web2c}
  export TEXMFROOT=${TEXMFROOT:-/tmp/omnidoc-texlive-root/usr/share/texlive}
  export TEXMFDIST=${TEXMFDIST:-/tmp/omnidoc-texlive-root/usr/share/texlive/texmf-dist}
  export TEXMFLOCAL=${TEXMFLOCAL:-/tmp/omnidoc-texlive-root/usr/share/texmf}
  export TEXMFSYSVAR=${TEXMFSYSVAR:-/tmp/omnidoc-texlive-root/var/lib/texmf}
  export TEXMFSYSCONFIG=${TEXMFSYSCONFIG:-/tmp/omnidoc-texlive-root/etc/texmf}
  export TEXMFVAR=${TEXMFVAR:-/tmp/omnidoc-texmf-var}
  export TEXMFCONFIG=${TEXMFCONFIG:-/tmp/omnidoc-texmf-config}
  export TEXMFCACHE=${TEXMFCACHE:-/tmp/omnidoc-texmf-cache}
  export VARTEXFONTS=${VARTEXFONTS:-/tmp/omnidoc-texfonts}
  export CDM_TEXLIVE_ROOT=${CDM_TEXLIVE_ROOT:-/tmp/omnidoc-texlive-root}
  export CDM_TEXLIVE_BIN=${CDM_TEXLIVE_BIN:-/tmp/omnidoc-texlive-root/usr/bin}
  export CDM_PDFLATEX=${CDM_PDFLATEX:-/tmp/omnidoc-texlive-root/usr/bin/pdflatex}
  export CDM_KPSEWHICH=${CDM_KPSEWHICH:-/tmp/omnidoc-texlive-root/usr/bin/kpsewhich}
  export CDM_SAVE_VIS=0
  export OMNIDOCBENCH_LATEX_TO_TEXT_TIMEOUT_SEC=0
  export PYTHONPATH=.
  "$CDM_KPSEWHICH" --version >/dev/null
  "$CDM_PDFLATEX" --version >/dev/null
  (
    cd "$OMNI_ROOT"
    "$OMNI_ROOT/.venv/bin/python" run_eval.py --config "$CONFIG"
  ) 2>&1 | tee "$LOG"
fi

require_file "$METRICS"
require_file "$STAGE_EXECUTION"

teds_errors=$(jq -r '(.table.metric_debug.TEDS.error_case_count // 0) + (.table.metric_debug.TEDS.timeout_case_count // 0)' "$METRICS")
cdm_errors=$(jq -r '(.display_formula.metric_debug.CDM.exception_case_count // 0) + (.display_formula.metric_debug.CDM.timeout_case_count // 0)' "$METRICS")
fallbacks=$(jq '[.page_match.fallbacks[]?.count // 0] | add // 0' "$STAGE_EXECUTION")
if [[ "$teds_errors" -ne 0 || "$cdm_errors" -ne 0 || "$fallbacks" -ne 0 ]]; then
  printf 'evaluator integrity failure: TEDS=%s CDM=%s fallbacks=%s\n' \
    "$teds_errors" "$cdm_errors" "$fallbacks" >&2
  exit 1
fi

set +e
"$OMNI_ROOT/.venv/bin/python" "$ROOT/benchmark/check_pipeline_v2_gate.py" \
  --odl-candidate "$ODL_CANDIDATE" \
  --odl-baseline "$ODL_BASELINE" \
  --omni-candidate "$METRICS" \
  --omni-baseline "$OMNI_BASELINE" \
  --output "$ACCURACY_GATE"
accuracy_status=$?
set -e

odl_seconds=$(jq -r '.summary.elapsed_per_doc' "$ODL_CANDIDATE")
omni_seconds=$(jq -r '.elapsed_per_doc' "$SUMMARY")
jq -n \
  --slurpfile accuracy "$ACCURACY_GATE" \
  --argjson expected_pages "$expected_pages" \
  --argjson prediction_pages "$prediction_pages" \
  --argjson teds_errors "$teds_errors" \
  --argjson cdm_errors "$cdm_errors" \
  --argjson fallbacks "$fallbacks" \
  --argjson odl_seconds "$odl_seconds" \
  --argjson odl_baseline "$ODL_BASELINE_SECONDS" \
  --argjson omni_seconds "$omni_seconds" \
  --argjson omni_baseline "$OMNI_BASELINE_SECONDS" \
  '{
    schema_version: 1,
    status: (if $accuracy[0].status == "PASS" and
                $odl_seconds <= ($odl_baseline * 0.95) and
                $omni_seconds <= ($omni_baseline * 0.95) and
                $prediction_pages == $expected_pages and
                $teds_errors == 0 and $cdm_errors == 0 and $fallbacks == 0
             then "PASS" else "FAIL" end),
    accuracy: $accuracy[0],
    performance: {
      opendataloader: {
        candidate_seconds_per_document: $odl_seconds,
        mineru_seconds_per_document: $odl_baseline,
        required_maximum: ($odl_baseline * 0.95),
        passed: ($odl_seconds <= ($odl_baseline * 0.95))
      },
      omnidocbench: {
        candidate_seconds_per_page: $omni_seconds,
        mineru_seconds_per_page: $omni_baseline,
        required_maximum: ($omni_baseline * 0.95),
        passed: ($omni_seconds <= ($omni_baseline * 0.95))
      }
    },
    integrity: {
      expected_pages: $expected_pages,
      prediction_pages: $prediction_pages,
      model_service_contract: "bare_tensor",
      teds_errors: $teds_errors,
      cdm_errors: $cdm_errors,
      evaluator_fallbacks: $fallbacks,
      passed: ($prediction_pages == $expected_pages and
               $teds_errors == 0 and $cdm_errors == 0 and $fallbacks == 0)
    }
  }' >"$FINAL_REPORT"

jq '{status, performance, integrity, accuracy_status: .accuracy.status}' "$FINAL_REPORT"
final_status=$(jq -r '.status' "$FINAL_REPORT")
if [[ "$final_status" != PASS || "$accuracy_status" -ne 0 ]]; then
  exit 1
fi
