# uparser protocol reference

Read this when you need to choose a protocol beyond `native`/`mineru-vlm`, configure an endpoint, or understand the JSON output. Run `uparser protocols` for the machine-readable capability matrix.

## Table of contents
1. Modes and protocols at a glance
2. `native` — zero-model text layer
3. `mineru-vlm` — two-stage VLM (recommended for quality)
4. Other model protocols
5. `pipeline` — typed multi-model StageGraph
6. Structured/OCR services
7. `auto` — profiler + router
8. Endpoints & serving
9. JSON `ParseResult` shape
10. Multi-format input (Office / OpenDocument / EPUB / RTF / CSV)

## 1. Modes and protocols at a glance

Prefer `--mode auto|native|protocol|pipeline`. Use `--protocol <name>` with
`--mode protocol`, or alone as a backward-compatible concrete selection.

| Protocol | Model? | Needs endpoint | OCR/scans | Strengths | Cost |
|---|---|---|---|---|---|
| `native` | none | no | no | fastest; born-digital PDFs **and** all Office/OpenDocument/EPUB/RTF/CSV input | ~ms/page |
| `mineru-vlm` | VLM (MinerU2.5) | yes (OpenAI-compatible) | yes | best reading order + tables | ~1–2 s/page |
| `dots-ocr` | VLM | yes | yes | single-round OCR VLM | ~1–2 s/page |
| `generic-vlm` | VLM | yes | yes | whole-page Markdown from a generic chat VLM | varies |
| `monkeyocr-v2` | VLM | yes | yes | two-stage layout→recognize | ~1–2 s/page |
| `pipeline` | multi (layout/ocr/formula/table) | mostly yes | yes | classic modular flow | varies |
| `paddleocr` | OCR | yes | yes | detect+recognize + geometric order | varies |
| `paddlex-structure` | structured service | yes | yes | PP-StructureV3-style layout parsing | varies |
| `auto` | — | depends on routed choice | — | picks a protocol from a profile | — |
| `mock` | none | no | no | smoke test only (not real output) | trivial |

## 2. `native`
One protocol name, **two pure-Rust engines**, both whole-document (they bypass the network scheduler, the cache and `--pages`/`--stream`/`--max-concurrency`/`--window-size` — passing those prints a "no effect" warning on stderr):

- **PDF engine** (internalized from pdf-inspector; lopdf-based, no PDFium, no OCR). Extracts the PDF text layer and renders full Markdown (headings, paragraphs, three-strategy tables).
- **Structured-document engine** (`uparser-document-engine`) for every non-PDF document format — see §10. It reads each format's *own* structure, so nothing is rasterized and nothing is guessed.

- Build: `--features native` (no PDFium download).
- Best for: born-digital PDFs, all Office/OpenDocument/EPUB/RTF/CSV input, speed, offline/no-GPU environments.
- Limitation: scanned/image-only pages produce little/no text (no OCR) — route those to a VLM.

## 3. `mineru-vlm`
Two-stage: layout detection → per-block content recognition, against a MinerU2.5 vision model on an OpenAI-compatible endpoint. Emits OTSL→HTML tables, LaTeX formulas, and crops image regions.
```bash
uparser parse --protocol mineru-vlm \
  --endpoint http://HOST:PORT/v1/chat/completions \
  --model MinerU2.5-Pro-2605-1.2B \
  --format markdown --max-concurrency 16 doc.pdf
```
- Requires the `pdfium` feature (page rasterization): build `--features native,pdfium`.
- Tune `--max-concurrency` (default 16; 32–100 for a strong endpoint).

## 4. Other model protocols

All require their matching deployed contract; do not point them at an arbitrary endpoint.

- `dots-ocr`: one-shot page JSON through OpenAI chat completions.
- `generic-vlm`: one-shot whole-page Markdown through OpenAI chat completions.
- `monkeyocr-v2`: layout-then-recognize with Python-literal decoding.
- `paddleocr`: PaddleOCR OCR service; boxes are geometrically reordered.
- `paddlex-structure`: PP-StructureV3-compatible `/layout-parsing` structured service.

`generic-vlm` and `paddlex-structure` are explicit-only until their actual services pass the
project quality gates. A registry entry or successful `protocols` listing is not evidence that
the service exists or that its quality is acceptable.

## 5. `pipeline`
Typed layout→OCR→formula→table StageGraph. The resolver validates dependencies, cycles,
required stages, external OCR compatibility, failure policy, and deterministic order before
page materialization. Each stage is `Local` or `Remote`; only `table` has a Local implementation
by default and needs `pipeline-local-table`. Per-stage flags: `--layout-backend/-endpoint`,
`--ocr-backend/-endpoint`, `--formula-backend/-endpoint`, and
`--table-backend/--table-model-path`. Passing `local` for layout/OCR/formula is a usage error.
Do not select pipeline unless its real endpoints have been checked; the current V2 report has
no completed pipeline quality benchmark.

## 6. Structured/OCR services

`paddleocr` performs detect+recognize OCR with geometric reading-order fallback and needs its
PaddleOCR REST contract. `paddlex-structure` consumes a `/layout-parsing` structured response.
They are distinct transports and are not interchangeable.

## 7. `auto` (the default mode)
Auto runs authoritative detection, L1/L2 analysis, optional conditional L3 semantic enrichment,
preprocessing planning, feasibility filtering, and quality/speed/cost ranking. Structured formats
normally select source-semantic native; scans/complex pages may select a model. Inspect the full
decision with `uparser plan --mode auto --prefer quality|speed|cost <file>`. `classify` prints only
the profile. A bare `uparser parse <file>` uses auto.

Two caveats: (1) the L2 structural signal that recognizes born-digital docs requires the `native`-enabled build (the shipped prebuilt is; without it, unclassifiable docs fall to the VLM fallback row). (2) if `auto` routes to a VLM, that VLM still needs an endpoint — resolved from `--endpoint` / `$UPARSER_ENDPOINT` / config; the binary prints a clear stderr hint if none is configured.

## 7b. Endpoint/model resolution
For `parse` and `doctor`, `--endpoint`/`--model` fall back (when omitted) to `$UPARSER_ENDPOINT`/`$UPARSER_MODEL`, then `~/.config/uparser/config.toml` (`$UPARSER_CONFIG` overrides the path) under the `[<effective-protocol>]` section. Explicit flags always win.

## 8. Endpoints & serving
VLM protocols talk to an **OpenAI-compatible `/v1/chat/completions`** endpoint (e.g. vLLM or LMDeploy). Example: serve `MinerU2.5-Pro-2605-1.2B` and pass `--endpoint http://127.0.0.1:PORT/v1/chat/completions --model MinerU2.5-Pro-2605-1.2B`. Verify reachability with `uparser doctor <protocol> --endpoint <url>` before a large run. `pipeline`, `paddleocr`, and `paddlex-structure` use non-chat REST contracts.

## 9. JSON `ParseResult` shape (`--format json`)
```jsonc
{
  "source_path": "...", "source_sha256": "...", "protocol": "native",
  "document_profile": { ... },
  "route_decision": { ... },
  "preprocess_plan": { ... },
  "pages": [
    { "page_num": 1, "width_px": 1275, "height_px": 1651,
      "blocks": [
        { "category": "title", "category_raw": "title",
          "bbox_px": [x0,y0,x1,y1], "reading_order": 0,
          "text": "…", "html": null, "latex": null,
          "asset_path": "doc_images/<hash>.png",  // for image/figure blocks
          "spans": [ ... ] }
      ] }
  ],
  "page_errors": [ { "page_num": 3, "message": "...", "stage": "content" } ],
  "warnings": [ "..." ]
}
```
Categories are normalized (`title`, `text`, `list`, `table`, `figure`/`image`, `formula`, `header`, `footer`, `page_number`, …). In Markdown output, `title`→`# `, `list`→`- `, tables→HTML, formulas→`$$…$$`, images→`![]()`.

## 10. Multi-format input

Two entirely different paths, chosen by protocol — this is the single most common thing to get wrong:

**`native` (and therefore `auto`, the default): source-semantic, fully offline.** No LibreOffice, no ImageMagick, no rasterization, no model, no network. Covers:

| Input | Extensions | Units produced |
|---|---|---|
| Word | `.docx`, `.doc` | one `flow` unit |
| PowerPoint | `.pptx`, `.ppt` | one `slide` unit per slide |
| Excel | `.xlsx`, `.xls`, `.xlsm`, `.xlsb`, `.xla`, `.xlam` | one `sheet` unit per sheet |
| OpenDocument | `.odt`, `.ods`, `.odp` | as their OOXML counterparts |
| EPUB | `.epub` | one `chapter` unit per spine item |
| RTF | `.rtf` | one `flow` unit |
| Delimited text | `.csv`, `.tsv`, `.tab` | one `sheet` unit |

The 16-variant contract is PDF, DOC, DOCX, PPT, PPTX, Excel, ODT, ODS, ODP, RTF,
EPUB, CSV, TSV, PNG, JPEG, and Unknown. Detection is signature/container-first, so a misnamed
DOCX still parses correctly. CSV/TSV require both the matching extension and consistent delimited
syntax; arbitrary text with a `.csv` suffix is rejected as Unknown.

Output shapes for these: `--format markdown` (flattened), `--format json` (the same page/block IR as the PDF protocols — one page per unit, `category` of `title`/`text`/`table`, tables as HTML with `rowspan`/`colspan`), `--format document-json` (lossless: units with `kind`/`label`, nested lists, table grids with covered-cell slots, `notes[]`, `assets[]`, structured `warnings[]`). `document-json` on a PDF is a usage error (exit 1).

Structured-only flags: `--no-notes`, `--headers-footers`, `--max-input-mib`.

Exit codes on this path: corrupt/unhandled format → 1; encrypted or over a resource budget → 2; parsed with losses → 0 with `warnings` populated.

**Forcing a VLM/OCR protocol on a non-PDF input: conversion-based.** DOCX/PPTX/images are converted to PDF first via **LibreOffice** (`soffice`) / **ImageMagick** (`magick`), which must be installed (missing tool → exit code 2). Only worth it when the file is really a wrapper around scanned images; for ordinary Office files it is slower, needs a GPU endpoint, and discards structure the source already states.
