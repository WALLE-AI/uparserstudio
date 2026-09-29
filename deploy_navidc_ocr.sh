#!/usr/bin/env bash
# NaviDC-OCR vLLM deployment script.
#
# Source: UPARSER_GUIDE.md §5.3 "NaviDC-OCR 部署:必须用上游 vLLM 插件(实测)"
#         and BENCHMARK_REPORT.md (same command, confirmed working end-to-end).
#
# Why the plugin is required: this checkpoint's config.json declares
# architectures: ["Qwen2_5_VLForConditionalGeneration"], but the text tower is
# actually Qwen3-structured (decoupled head_dim=128, no qkv bias, per-layer
# q/k RMSNorm). Vanilla vLLM's built-in Qwen2_5_VL implementation assumes
# Qwen2 conventions (head_dim=64, bias=True, no qk_norm) and will either
# refuse to start (mrope assertion) or worse, silently drop all 28 layers of
# QK normalization and produce wrong output. opensource/NaviDC-OCR/NaviOCR-vllm
# is the upstream out-of-tree vLLM plugin that overrides the model
# registration to use the Qwen3 text-tower implementation instead.
#
# This environment is already built at /tmp/navidc_vllm_env (a relocated
# conda-style prefix, python 3.12.8, vllm==0.11.0, transformers==4.57.0,
# NaviOCR-vllm installed editable from opensource/NaviDC-OCR/NaviOCR-vllm) —
# confirmed via `pip show` in that prefix. It has no bin/activate (it's a bare
# prefix, not a standard venv/conda env), so this script invokes its `vllm`
# binary directly rather than trying to `conda activate`/`source activate` it.
# Override NAVIDC_VLLM_ENV to point at a different prefix if you rebuild one.
set -euo pipefail

MODEL_PATH="${1:-/path/to/NaviDC-OCR}"
GPU="${CUDA_VISIBLE_DEVICES:-1}"
PORT="${PORT:-8010}"
SERVED_MODEL_NAME="${SERVED_MODEL_NAME:-StarDoc-AI/NaviDC-OCR}"
GPU_MEM_UTIL="${GPU_MEM_UTIL:-0.30}"
MAX_MODEL_LEN="${MAX_MODEL_LEN:-8192}"   # must stay >=8192: adapter's stage-2
                                          # budget is max_tokens=4096, and vLLM
                                          # rejects requests if max-model-len
                                          # is <= that.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAVIOCR_VLLM_PLUGIN="${REPO_ROOT}/opensource/NaviDC-OCR/NaviOCR-vllm"
NAVIDC_VLLM_ENV="${NAVIDC_VLLM_ENV:-/tmp/navidc_vllm_env}"
VLLM_BIN="${NAVIDC_VLLM_ENV}/bin/vllm"
PIP_BIN="${NAVIDC_VLLM_ENV}/bin/pip"

if [ -x "${VLLM_BIN}" ]; then
  echo "== using pre-built env: ${NAVIDC_VLLM_ENV} =="
  "${PIP_BIN}" show vllm NaviOCR-vllm 2>/dev/null | grep -E "^(Name|Version|Editable project location):"
else
  echo "== ${NAVIDC_VLLM_ENV} not found or has no vllm binary — building it from scratch =="
  # Pinned versions the plugin was validated against.
  python3 -m venv "${NAVIDC_VLLM_ENV}"
  "${PIP_BIN}" install "vllm==0.11.0"
  "${PIP_BIN}" install "transformers==4.57.1"
  "${PIP_BIN}" install -e "${NAVIOCR_VLLM_PLUGIN}"
fi

echo "== launch vLLM OpenAI-compatible server =="
echo "env             : ${NAVIDC_VLLM_ENV}"
echo "model path      : ${MODEL_PATH}"
echo "served-model    : ${SERVED_MODEL_NAME}"
echo "port            : ${PORT}"
echo "CUDA_VISIBLE_DEVICES=${GPU}"
echo
echo "Expect this in the startup log (confirms the plugin actually took effect):"
echo "  Model architecture Qwen2_5_VLForConditionalGeneration ... will be overwritten"
echo "  by the new model class NaviOCR_vllm.qwen2_5_vl:..."
echo

CUDA_VISIBLE_DEVICES="${GPU}" "${VLLM_BIN}" serve "${MODEL_PATH}" \
  --served-model-name "${SERVED_MODEL_NAME}" \
  --port "${PORT}" \
  --max-model-len "${MAX_MODEL_LEN}" \
  --trust-remote-code \
  --gpu-memory-utilization "${GPU_MEM_UTIL}" \
  --limit-mm-per-prompt '{"image":4}'

# --- after the server is up, verify + use it via uparser ---
#
#   uparser doctor navidc-ocr --endpoint http://127.0.0.1:${PORT}/v1/chat/completions
#
#   uparser parse doc.pdf --mode protocol --protocol navidc-ocr \
#     --endpoint http://127.0.0.1:${PORT}/v1/chat/completions \
#     --model "${SERVED_MODEL_NAME}" --layout-mode segmentation --format markdown
#
# NOTE: use a release build of uparser. A debug build's client-side image
# crop/resize/PNG-encode is ~10x slower and will time out on dense pages
# (52-block newspaper page: 31.7s release vs >300s debug — bottleneck is the
# client CPU, not the model server).
