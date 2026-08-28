# uparser Pipeline Model Server

Pipeline V2 inference service aligned to local MinerU 3.4.5. The default page backend runs the
official PP-DocLayoutV2 pipeline, including formula/table branches, paragraph finalization, and
official Markdown generation. Existing `magic-pdf.json` weights remain available as the `legacy`
profile for stage-level diagnostics.

Development startup on CPU:

```bash
PYTHONPATH=services/pipeline-model-server/src \
UPARSER_PIPELINE_DEVICE=cpu \
UPARSER_PIPELINE_PROFILE=mineru-3.4.5 \
UPARSER_MINERU_ROOT=opensource/MinerU \
UPARSER_MINERU_CONFIG=/home/dataset1/gaojing/mineru.json \
python -m uparser_pipeline_server.app
```

The MinerU 3.4.5 profile defaults to `PP-FormulaNet-plus-M` because the local
OmniDocBench gate is higher than UniMERNet-small. Set
`MINERU_FORMULA_CH_SUPPORT=False` only for an explicit UniMERNet comparison.
Startup rejects Transformers versions outside MinerU's declared
`>=4.57.3,<5.0.0` range or runtimes that cannot resolve the `hgnet_v2`
backbone used by PP-DocLayoutV2.

Default address: `http://127.0.0.1:9001`. Inspect `/health`, `/v2/models`, and `/docs` before submitting
batch requests. Use `/v2/pipeline/documents:analyze` for PDFs so cross-page table and paragraph
finalization remain active; `/v2/pipeline/pages:analyze` is intended for independent images.
Production CUDA startup requires a passing `scripts/pipeline_model_preflight.py` result.

The standalone stage endpoints still use the legacy-compatible backends. Set
`UPARSER_PIPELINE_PROFILE=legacy` to make the old staged page analyzer authoritative as well.
Generated figure assets are returned by default; benchmark-only deployments may set
`UPARSER_PIPELINE_RETURN_ASSETS=false` to avoid transferring assets that the scorers do not consume.
Decoded images are limited to 50 million pixels by default. OmniDocBench contains trusted source
images up to 145,456,564 pixels, so its isolated evaluation service uses
`UPARSER_PIPELINE_MAX_IMAGE_PIXELS=200000000`; do not copy that override to an untrusted upload
service without an equivalent resource policy.
