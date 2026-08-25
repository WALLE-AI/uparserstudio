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

Default address: `http://127.0.0.1:9001`. Inspect `/health`, `/v2/models`, and `/docs` before submitting
batch requests. Production CUDA startup requires a passing `scripts/pipeline_model_preflight.py` result.

The standalone stage endpoints still use the legacy-compatible backends. Set
`UPARSER_PIPELINE_PROFILE=legacy` to make the old staged page analyzer authoritative as well.
