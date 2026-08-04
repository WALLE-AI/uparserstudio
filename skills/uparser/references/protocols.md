# uparser protocol reference

Read this when you need to choose a protocol beyond `native`/`mineru-vlm`, configure an endpoint, or understand the JSON output. Run `uparser protocols` for the machine-readable capability matrix.

## Table of contents
1. Protocols at a glance
2. `native` — zero-model text layer
3. `mineru-vlm` — two-stage VLM (recommended for quality)
4. `dots-ocr` / `monkeyocr-v2` — other VLMs
5. `pipeline` — traditional multi-model
6. `paddleocr` — OCR + geometric reading order
7. `auto` — profiler + router
8. Endpoints & serving
9. JSON `ParseResult` shape
10. Multi-format ingestion

## 1. Protocols at a glance

| Protocol | Model? | Needs endpoint | OCR/scans | Strengths | Cost |
|---|---|---|---|---|---|
| `native` | none | no | no | fastest, born-digital text/headings/tables | ~ms/page |
| `mineru-vlm` | VLM (MinerU2.5) | yes (OpenAI-compatible) | yes | best reading order + tables | ~1–2 s/page |
| `dots-ocr` | VLM | yes | yes | single-round OCR VLM | ~1–2 s/page |
| `monkeyocr-v2` | VLM | yes | yes | two-stage layout→recognize | ~1–2 s/page |
| `pipeline` | multi (layout/ocr/formula/table) | mostly yes | yes | classic modular flow | varies |
| `paddleocr` | OCR | yes | yes | detect+recognize + geometric order | varies |
| `auto` | — | depends on routed choice | — | picks a protocol from a profile | — |
| `mock` | none | no | no | smoke test only (not real output) | trivial |

## 2. `native`
Pure-Rust engine (internalized from pdf-inspector; lopdf-based, no PDFium, no OCR). Extracts the PDF text layer and renders full Markdown (headings, paragraphs, three-strategy tables). Whole-document, bypasses the network scheduler.
- Build: `--features native` (no PDFium download). 
- Best for: born-digital PDFs, speed, offline/no-GPU environments.
- Limitation: scanned/image-only pages produce little/no text (no OCR) — route those to a VLM.

## 3. `mineru-vlm`
Two-stage: layout detection → per-block content recognition, against a MinerU2.5 vision model on an OpenAI-compatible endpoint. Emits OTSL→HTML tables, LaTeX formulas, and crops image regions.
```bash
uparser parse --protocol mineru-vlm \
  --endpoint http://HOST:PORT/v1/chat/completions \
  --model MinerU2.5-2604-1.2B \
  --format markdown --max-concurrency 16 doc.pdf
```
- Requires the `pdfium` feature (page rasterization): build `--features native,pdfium`.
- Tune `--max-concurrency` (default 16; 32–100 for a strong endpoint).

## 4. `dots-ocr` / `monkeyocr-v2`
Same invocation shape as `mineru-vlm` (`--endpoint`/`--model`), different model contracts. Use when you have one of these models deployed. `dots-ocr` is single-round; `monkeyocr-v2` is two-stage.

## 5. `pipeline`
Traditional layout→OCR→formula→table, each stage independently `Local` (in-process ONNX, only the `table` stage by default, needs the `pipeline-local-table` feature) or `Remote` (a lightweight "Pipeline Model Serving" REST endpoint). Per-stage flags: `--layout-backend/-endpoint`, `--ocr-backend/-endpoint`, `--formula-backend/-endpoint`, `--table-backend/--table-model-path`. `layout`/`ocr`/`formula` have no Local implementation — passing `local` for those is a usage error.

## 6. `paddleocr`
Detect+recognize OCR with a from-scratch XY-cut geometric reading-order fallback. Single fixed `text` category. Needs a PaddleOCR-style REST endpoint (`--endpoint`).

## 7. `auto`
Runs the Profiler (L1 format + L2 structural, no model) then the Router to pick a protocol, logs the choice to stderr, then parses. Use when you don't know the document. Inspect the decision first with `uparser classify <file>` (prints a `DocumentProfile`: `kind`, `dominant_content`, per-page `has_table_region`/`needs_ocr`, etc.).

## 8. Endpoints & serving
VLM protocols talk to an **OpenAI-compatible `/v1/chat/completions`** endpoint (e.g. a vLLM or LMDeploy server). Example: serve `MinerU2.5-Pro-2604-1.2B` with vLLM and point `--endpoint http://127.0.0.1:PORT/v1/chat/completions --model MinerU2.5-2604-1.2B`. Verify reachability with `uparser doctor <protocol> --endpoint <url>` before a large run. `pipeline`/`paddleocr` use their own lightweight (non-chat-completions) REST contracts.

## 9. JSON `ParseResult` shape (`--format json`)
```jsonc
{
  "source_path": "...", "source_sha256": "...", "protocol": "native",
  "document_profile": { ... },            // present for --protocol auto/classify
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

## 10. Multi-format ingestion
`uparser` accepts PDF, DOCX, PPTX, XLSX, CSV, and images. DOCX/PPTX/images are converted to PDF first via **LibreOffice** (`soffice`) / **ImageMagick** (`magick`) — those must be installed for those inputs (a missing tool → exit code 2). XLSX/CSV are read as structured data directly (no rasterization, no model). PDFs go straight through.
