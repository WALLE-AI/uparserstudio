---
name: uparser
description: Parse documents (PDF, Word/PPT/Excel, and images) into structured Markdown or JSON with the `uparser` CLI — a Rust tool built specifically for coding agents to invoke as a subprocess. Use this skill WHENEVER the user wants to extract text, tables, formulas, reading order, or images from a PDF or Office/image document; convert a document to Markdown; OCR or VLM-parse a scanned document; classify a document's type/layout before parsing; or feed document content into a RAG/LLM pipeline — even if they don't say "uparser" by name. Also use it when choosing between a fast local text-layer parse and a high-accuracy vision-model parse, or when a previous document-parsing attempt gave garbled/empty output.
---

# uparser

`uparser` is a unified document-parsing CLI (Rust). It turns a document into a clean **Markdown** or a structured **JSON** intermediate representation (blocks with bounding boxes, categories, tables, formulas, reading order). It is designed to be driven by coding agents as a subprocess: **stdout = the result, stderr = logs, and the exit code is semantic** (see Output contract).

Pick the parsing engine with `--protocol`. The two you'll use most:

- **`native`** — pure-Rust, **zero model, no GPU, no network**. Milliseconds per page. Best for **born-digital (electronic) PDFs** where you just need the text/structure fast. Cannot read scanned/image-only pages (no OCR).
- **`mineru-vlm`** — highest accuracy (best reading order + tables). Requires an **OpenAI-compatible vLLM endpoint** serving a MinerU2.5 vision model. Use for **scanned documents, complex layouts, or when table/figure fidelity matters**.

## Getting the binary

If `uparser` is not already on PATH, build it from the `uparser/` workspace in this repo:

```bash
# native only (fast, no GPU, no PDFium download):
cargo build --release --features native
# add pdfium (needed to rasterize pages for the VLM/OCR protocols):
cargo build --release --features native,pdfium
```

The binary lands at `uparser/target/release/uparser`. `scripts/find_uparser.sh` locates or builds it and prints the path.

## Quick start

```bash
# Fast, local, born-digital PDF → Markdown:
uparser parse --protocol native --format markdown report.pdf > report.md

# Highest quality via a vision model (needs a vLLM endpoint):
uparser parse --protocol mineru-vlm \
  --endpoint http://127.0.0.1:19122/v1/chat/completions \
  --model MinerU2.5-2604-1.2B \
  --format markdown scan.pdf > scan.md

# Let uparser choose the engine automatically:
uparser parse --protocol auto --format markdown mystery.pdf > out.md
```

## Choosing a protocol

Decide with this table. When unsure, run `uparser classify <file>` first (cheap, no model) or use `--protocol auto`.

| Situation | Use | Why |
|---|---|---|
| Born-digital PDF, need speed, no GPU | `native` | ms/page, zero deps; text-layer extraction |
| Scanned / image-only PDF | `mineru-vlm` (or another VLM) | `native` has no OCR → empty output on scans |
| Complex tables / figures matter | `mineru-vlm` | best table (OTSL→HTML) + reading order |
| Don't know the document type | `auto` or `classify` first | Profiler routes born-digital→native, else→VLM |
| Office/image input (docx/pptx/xlsx/png) | any (ingest converts) | needs LibreOffice/ImageMagick for docx/pptx/img; xlsx/csv are read directly |

Other protocols: `dots-ocr`, `monkeyocr-v2` (single/two-stage VLMs, need their own endpoint), `pipeline` (traditional layout→OCR→formula→table), `paddleocr`. Run `uparser protocols` to introspect every adapter's capabilities as JSON. See `references/protocols.md` for details on all of them.

## Output contract (important for agents)

- **stdout** carries the result only: the Markdown (`--format markdown`) or the JSON `ParseResult` (`--format json`, default). Redirect it to a file or capture it.
- **stderr** carries logs, progress, and warnings. Never parse stdout+stderr together.
- **Exit codes** are semantic — branch on them:

| Code | Meaning | Agent action |
|---|---|---|
| 0 | success | use the result |
| 1 | usage error (bad flags/args) | fix the command |
| 2 | dependency/environment error (e.g. LibreOffice missing, endpoint unreachable) | install/fix env, retry |
| 3 | partial success (some pages failed) | result is usable; inspect `page_errors` in JSON |
| 4 | internal error | report; retry with `--no-cache` |

- On `--format json`, errors are a structured object: `{"error":{"code":...,"message":...,"protocol":...,"stage":...}}`.
- `--format json` results include `pages[].blocks[]` (text/html/latex/asset_path, `category`, `bbox_px`, `reading_order`), `page_errors`, and `warnings`.

## Common recipes

```bash
# JSON IR for a RAG pipeline (blocks with bboxes/categories):
uparser parse --protocol native --format json paper.pdf > paper.json

# Only certain pages of a large document:
uparser parse --protocol mineru-vlm --endpoint <url> --model <m> --pages 1-3,7 big.pdf

# Classify first (no model call) to decide routing / cost:
uparser classify paper.pdf        # → DocumentProfile JSON (kind, dominant_content, per-page)

# Check an endpoint is reachable before a big run:
uparser doctor mineru-vlm --endpoint http://127.0.0.1:19122/v1/chat/completions

# Force a fresh parse (skip the content-hash cache):
uparser parse --protocol native --no-cache doc.pdf

# Stream incremental NDJSON for a large doc (one line per window):
uparser parse --protocol mineru-vlm --endpoint <url> --model <m> --stream huge.pdf
```

## Images in Markdown

By default, image/figure regions are cropped and written to `<source_stem>_images/` next to the source, and referenced in the Markdown as `![](images/<hash>.png)` (MinerU-style). Override the folder with `--assets-dir <dir>`, or pass `--no-assets` to skip the filesystem side effect entirely (no `![]()` links).

## Key flags (see `parse --help` for all)

- `--protocol <native|mineru-vlm|dots-ocr|monkeyocr-v2|pipeline|paddleocr|auto|mock>`
- `--format <markdown|json>` (default `json`)
- `--endpoint <url>` / `--model <name>` — for the VLM/OCR protocols
- `--pages <1-5,7>` — 1-indexed page selection
- `--max-concurrency <N>` — concurrent model requests (default 16; raise to 32–100 for a beefy endpoint)
- `--no-cache`, `--stream`, `--assets-dir`, `--no-assets`, `--no-postprocess`

## Performance notes (measured on opendataloader-bench)

- `native` ≈ 0.05 s/doc, no GPU; strong quality on born-digital docs. Use it as the default fast path.
- `mineru-vlm` ≈ best overall quality (reading order + tables) but ~1–2 s/page and needs a GPU-backed endpoint. Repeated identical parses hit the content-hash cache (near-instant) unless `--no-cache`.

For the full protocol reference, capability matrix, and endpoint setup, read `references/protocols.md`.
