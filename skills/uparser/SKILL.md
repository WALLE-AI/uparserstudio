---
name: uparser
description: Parse, classify, plan, and route PDF, Office, OpenDocument, EPUB, RTF, CSV/TSV, and image inputs with the uparser Architecture V2 CLI. Use for Markdown or structured JSON extraction, OCR/VLM selection, tables, formulas, reading order, assets, endpoint diagnostics, document ingestion, and failed or garbled extraction. Trigger even when uparser is not named if the task needs reliable mixed-format document parsing; do not use for ordinary text-file reading.
---

# uparser

`uparser` is a subprocess built for coding agents. Contract:

- **stdout** = the requested result only (JSON / Markdown / NDJSON), never logs.
- **stderr** = routing, progress, warnings. Never merge it into stdout before `jq`.
- **exit code** = `0` ok · `1` bad request or unusable input · `2` environment/dependency · `3` partial (usable, check `page_errors`) · `4` internal defect.
- On failure, stdout still carries `{"error":{"code","message","protocol","stage"}}` for `--format json`.

## Start here

```bash
UP=$(scripts/find_uparser.sh)          # or: export UPARSER_BIN=/path/to/uparser
"$UP" --version                        # need >= 0.4.0-rc.1 (has `plan`, `--mode`, `--output`)
```

Then pick one line — do not over-plan a document you already understand:

| You know | Run |
|---|---|
| Born-digital PDF, want text/Markdown fast, offline | `"$UP" parse f.pdf --mode native --format markdown` |
| DOCX/PPTX/XLSX/ODF/EPUB/RTF/CSV | `"$UP" parse f.docx --mode native --format markdown` |
| …and you need lossless structure (lists, table grids, notes) | `--mode native --format document-json` |
| Scanned PDF / PNG / JPEG, local only | `"$UP" doctor tesseract && "$UP" parse f.pdf --protocol tesseract --format markdown` |
| Scanned or complex layout, VLM endpoint available | `"$UP" parse f.pdf --mode protocol --protocol mineru-vlm --endpoint <url> --model <name> --format json` |
| Nothing — unknown file, or cost matters | `"$UP" plan f.pdf --prefer quality` first, then execute what it chose |

`--mode auto` (the default when neither `--mode` nor `--protocol` is given) runs the whole chain and picks for you. It is a ranker over observed evidence, not an oracle — when the answer must be auditable, keep `route_decision` from the result.

## Recipes

```bash
# Inspect before spending money/time. Both are cheap and never call a model.
"$UP" classify f.pdf | jq '{quality:.source_quality, kind, dominant_content}'
"$UP" plan f.pdf --prefer quality | jq '{picked:.plan.route.protocol, why:.plan.route.reason,
  rejected:[.plan.route.candidates[]|select(.feasible|not)|{protocol,rejection}]}'

# Validate a remote protocol BEFORE parsing. "Feasible" in plan != endpoint running.
"$UP" doctor mineru-vlm --endpoint http://127.0.0.1:19122/v1/chat/completions

# Machine consumption: text + geometry + category per block.
"$UP" parse f.pdf --mode native --format json --no-assets \
  | jq -r '.pages[].blocks[] | select(.text) | .text'

# Tables only (HTML strings), formulas only (LaTeX).
jq -r '.pages[].blocks[]|select(.category=="table").html' result.json
jq -r '.pages[].blocks[]|select(.category=="equation").latex' result.json

# One page of a big document, to validate an endpoint cheaply.
"$UP" parse big.pdf --pages 3 --protocol mineru-vlm --endpoint "$UPARSER_ENDPOINT"

# Long document, incremental consumption (one JSON object per completed window).
"$UP" parse big.pdf --stream --window-size 16 | while read -r line; do ...; done

# Reproducible measurement / benchmarking.
"$UP" parse f.pdf --no-cache --no-assets --output out.json

# Batch: keep going past a single bad file, and record which failed.
for f in docs/*.pdf; do
  "$UP" parse "$f" --mode native --format markdown --output "out/$(basename "$f" .pdf).md" \
    || echo "$f exit=$?" >> out/failures.txt
done
```

## Reading the result

`--format json` top level: `source_path`, `source_sha256`, `protocol`, `routed_by`, `document_profile`,
`route_decision`, `preprocess_plan`, `model_endpoint`, `model_name`, `pages`, `page_errors`,
`capability_notes`, `warnings`, `timing`.

Each block: `geom`, `geom_frame`, `bbox_px`, `category_raw`, `category`, `reading_order`, `text`,
`html` (tables), `latex` (formulas), `spans`, `merge_hint`, `confidence`, `source`, `error`, `asset_path`.

After any exit `0` or `3`, read `warnings`, `capability_notes`, and `page_errors` — surface fidelity
losses to the user instead of silently presenting degraded text as complete.
`bbox_px` is `null` for protocols that answer with a finished document rather than geometry
(`generic-vlm`, `paddlex-structure`); don't build geometry logic on those.

`--format document-json` (structured native sources only, not PDF, not model routes) is the lossless
canonical view: typed table grids with row/column spans, list structure, notes, assets. It is **not** a
superset of `--format json` — that one has the geometry, provenance, and routing metadata.
Merged table cells are only recoverable from `document-json`.

## Flags that change behaviour, not just output

- Assets are written by default to `<source_stem>_images/`. `--assets-dir <dir>` relocates them;
  `--no-assets` avoids the filesystem side effect entirely (use it whenever you only want text —
  including for `--format json`, which otherwise still writes files).
- `--markdown-source canonical` is the default; `engine-legacy` is a one-release fallback for the
  native PDF engine's own writer.
- `--redact-pii` masks email / mainland-China phone / resident-ID in emitted output only. Cache and
  parsing stay source-faithful — it is not an anonymizer.
- `--pages`, `--stream`, `--window-size` (default 64), `--max-concurrency` (default 16) apply to model
  and pipeline execution. On native whole-document execution they are ignored and warn.
- `--raster-dpi` (default 200) only affects visual-page protocols.
- Model protocols cache on `(bytes, protocol, endpoint, model, options)`; native bypasses the cache.
- Structured-native only: `--no-notes`, `--headers-footers`, `--max-input-mib`.

Full flag list: `references/cli.md`. Protocol catalog and service contracts: `references/protocols.md`.
Routing/artifact internals: `references/v2-architecture.md`.

## When it fails

| Exit | Do |
|---|---|
| 1 | Fix the arguments, mode/protocol conflict, or output format. If the input itself is corrupt/unsupported, stop — retrying is pointless. |
| 2 | Repair the environment named in the message (endpoint down, LibreOffice, PDFium, Tesseract language data, encrypted PDF, missing file, unwritable output), then retry once. |
| 3 | Keep the result. Inspect `page_errors`; re-run only those pages with `--pages`. |
| 4 | Report the structured error as a defect. One retry with `--no-cache` distinguishes a stale cache entry from a real bug. |

Garbled or empty text from `--mode native` means the PDF has no reliable text layer — switch to
`--protocol tesseract` (local) or a VLM protocol. Do not retry native with different flags.

Never select `--protocol mock`; it is explicit-only placeholder output.

## Binary, endpoints, config

`scripts/find_uparser.sh [--build]` locates or builds it; `scripts/uparser-check.sh` reports readiness;
`scripts/uparser-parse.sh <file>` is a one-shot wrapper that picks native vs. VLM from the resolvable
endpoint. `.ps1` equivalents exist for Windows.

Build from `uparser/`: `cargo build --release --features native,pdfium`. PDFium is needed for page
rasterization, every vision protocol, native PDF asset crops, and local OCR — pure native text
extraction is not.

Endpoint/model resolution order: explicit flag → `UPARSER_ENDPOINT` / `UPARSER_MODEL` →
`~/.config/uparser/config.toml` (or `UPARSER_CONFIG`) under the effective protocol's section. See
`references/config.example.toml`. `UPARSER_OCR_LANG` overrides OCR language selection.
