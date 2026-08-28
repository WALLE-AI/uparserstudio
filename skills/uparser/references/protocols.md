# uparser protocol reference

Read this when choosing a protocol beyond the common native/MinerU paths, configuring a service, or interpreting the common JSON IR. Treat `uparser protocols` as the runtime capability catalog.

## Execution catalog

| Selection | V2 family / shape | Transport | Best use | Required runtime |
|---|---|---|---|---|
| `native` | native document | in process | source-faithful PDF and structured documents | `native` feature for PDF |
| `tesseract` | native / one-shot page | in process | local OCR for scanned PDF/images | PDFium, Tesseract, language data |
| `mineru-vlm` | model / layout-then-recognize | OpenAI chat completions | strong reading order, tables, formulas | PDFium + matching MinerU endpoint |
| `dots-ocr` | model / one-shot page | OpenAI chat completions | dots.ocr JSON contract | PDFium + matching endpoint |
| `generic-vlm` | model / one-shot page | OpenAI chat completions | whole-page Markdown | PDFium + prompt-compatible endpoint |
| `monkeyocr-v2` | model / layout-then-recognize | OpenAI chat completions | MonkeyOCR v2 contract | PDFium + matching endpoint |
| `paddleocr` | model / structured service | PaddleOCR REST | OCR boxes + geometric ordering | PDFium + PaddleOCR service |
| `paddlex-structure` | model / structured service | `/layout-parsing` REST | server-composed PP-StructureV3 result | PDFium + PaddleX service |
| `pipeline` | pipeline / StageGraph | per-stage services/in-process table | client-composed typed stages | configured stage backends |
| `mock` | test | none | tests only | explicit selection only |

`auto` is orchestration, not another parser. It analyzes, ranks, and selects one of the concrete paths.

## Native and local OCR

`native` reuses analysis artifacts instead of parsing the source twice:

- a PDF reuses `PdfProcessResult` for execution and engine Markdown;
- a structured input reuses `CanonicalDocument` unless non-default document options require a controlled reparse.

Native is whole-document execution and bypasses the model result cache and scheduler. `--pages`, `--stream`, `--window-size`, and `--max-concurrency` do not limit it.

Native PDF has two optional PDFium-assisted behaviors:

1. Materialize discovered Image XObjects and vector/chart regions as bounded crops.
2. For an otherwise native-usable PDF, replace only pages flagged as scanned or suspiciously garbled with local Tesseract output.

The second behavior is a bounded hybrid fallback, not permission to force explicit native on a fully scanned document. Explicit native rejects `scanned`, `image_only`, or unknown-quality sources before execution. Use `--protocol tesseract` or a VLM for those.

`tesseract` is a full one-shot page protocol. Run `uparser doctor tesseract`; explicit Tesseract defaults to `eng`, so set `UPARSER_OCR_LANG` for another language. Automatic `eng` / simplified-Chinese / traditional-Chinese selection applies to Native's bounded hybrid fallback. Availability requires both PDF rasterization and the Tesseract executable/language data.

## Model protocols

Model protocols are not interchangeable merely because several use OpenAI-compatible HTTP. Their preprocessing and decoding contracts differ:

- `mineru-vlm`: hard-resized page, layout then per-region recognition, MinerU custom tokens, OTSL tables.
- `dots-ocr`: smart-resized page, strict JSON with protocol-specific recovery.
- `generic-vlm`: full-page Markdown. Use only with a prompt/model that actually follows this contract.
- `monkeyocr-v2`: pixel-bounded layout then recognition with Python-literal decoding.
- `paddleocr`: PaddleOCR service boxes, not chat completions.
- `paddlex-structure`: service-side fused layout parsing from `/layout-parsing`.

Example:

```bash
uparser doctor mineru-vlm --endpoint http://HOST:PORT/v1/chat/completions
uparser parse doc.pdf --mode protocol --protocol mineru-vlm \
  --endpoint http://HOST:PORT/v1/chat/completions \
  --model MinerU2.5-Pro-2605-1.2B --format json --max-concurrency 16
```

Do not infer service health from registry membership, a default endpoint, or an auto candidate marked feasible. The current router knows compiled/local capabilities but does not probe remote endpoints during planning. `doctor` is the explicit network preflight.

`generic-vlm` and `paddlex-structure` should remain explicit choices until the actual deployment has passed task-specific quality checks.

## Pipeline

Pipeline is for client-side composition: core owns typed layout, OCR, formula, table, assemble, and ordering dependencies. This is distinct from a structured service that returns a fused result in one request.

The StageGraph resolver validates required stages, cycles, typed compatibility, external OCR requirements, failure policy, and deterministic order before page materialization. Current backend controls include:

- `--layout-backend` / `--layout-endpoint`
- `--ocr-backend` / `--ocr-endpoint`
- `--formula-backend` / `--formula-endpoint`
- `--table-backend` / `--table-model-path`

Layout, OCR, and formula do not have confirmed local implementations; passing `local` is a usage error. Local table execution requires `pipeline-local-table`. Do not select pipeline from a catalog entry alone: validate every configured stage and its real quality/latency first.

## Auto routing

Auto performs authoritative format detection, L1/L2 analysis, optional conditional L3 enrichment, route scoring, feasibility filtering, and preprocessing planning. The default parse policy is quality.

Important routing boundaries:

- source-semantic spreadsheets and ordinary structured text strongly favor native;
- reliable native-text books, standards, legal/regulatory documents, and resumes can remain native;
- scans/images make local OCR or a model necessary;
- presentation/visual grouping can favor a model even though native source parsing is possible;
- pipeline is not auto-feasible until its deployment and quality gate are known.

Use `plan` to audit the choice:

```bash
uparser plan input --mode auto --prefer quality
uparser plan input --mode auto --prefer speed
uparser plan input --mode auto --prefer cost
```

`plan --prefer` does not persist into a later `parse --mode auto`. Execute the plan's selected concrete protocol explicitly when preference fidelity matters.

## Input materialization

- Native PDF: PDF text/source artifact, no conversion.
- Native structured document: canonical source semantics, no conversion.
- Model/pipeline PDF: lazily rasterized pages through PDFium.
- Model/pipeline PNG/JPEG: decoded directly as a one-page visual source; no ImageMagick conversion.
- Model/pipeline structured document: LibreOffice converts to PDF, then PDFium rasterizes selected pages.

Missing LibreOffice for a visual Office route is an environment failure. Prefer native for ordinary Office documents because visual conversion discards source semantics.

## Output contract

The common `--format json` result includes:

```jsonc
{
  "source_path": "...",
  "source_sha256": "...",
  "protocol": "native",
  "routed_by": { "kind": "auto" },
  "document_profile": { "source_format": "pdf", "source_quality": "native_text" },
  "route_decision": { "protocol": "native", "candidates": [] },
  "preprocess_plan": { "input_channel": "pdf_text", "conversion": "none" },
  "pages": [
    { "page_num": 1, "width_px": 1275, "height_px": 1651,
      "blocks": [
        { "category": "title", "bbox_px": [0, 0, 100, 30],
          "reading_order": 0, "text": "...", "html": null,
          "latex": null, "asset_path": null, "spans": [] }
      ] }
  ],
  "page_errors": [],
  "capability_notes": [],
  "warnings": [],
  "timing": {}
}
```

Native may emit richer categories such as `formula`, `normative_clause`, `mandatory_clause`, and `annex_heading`. Treat inferred formula text conservatively: `latex` remains empty unless source/model evidence provides real LaTeX.

`--format document-json` is a separate lossless structured-document contract and requires a non-PDF native route. `--markdown-source engine` preserves native/document engine behavior; `canonical` uses the shared IR renderer for comparison.

## Endpoints and configuration

Endpoint/model resolution order is:

1. `--endpoint` / `--model`
2. `UPARSER_ENDPOINT` / `UPARSER_MODEL`
3. `~/.config/uparser/config.toml` or `UPARSER_CONFIG`, under `[<effective-protocol>]`
4. adapter default where one exists

Auto resolves configuration after routing, using the effective protocol section. Keep stdout/stderr separate and use the structured result/error rather than scraping diagnostic prose.
