---
name: uparser
description: Parse, classify, plan, and route PDF, Office, OpenDocument, EPUB, RTF, CSV/TSV, and image inputs with the uparser Architecture V2 CLI. Use for Markdown or structured JSON extraction, OCR/VLM selection, tables, formulas, reading order, assets, endpoint diagnostics, document ingestion, and failed or garbled extraction. Trigger even when uparser is not named if the task needs reliable mixed-format document parsing; do not use for ordinary text-file reading.
---

# uparser

Use uparser as an Agent-first subprocess. Keep its streams separate:

- stdout is the requested Markdown/JSON result only;
- stderr contains routing, progress, and warnings;
- exit codes are semantic: `0` success, `1` invalid/unsupported request, `2` dependency or environment failure, `3` partial result, `4` internal failure.

## V2 mental model

V2 has one preparation chain and three execution families:

```text
detect -> analyze (L1/L2, optional L3) -> route -> PreprocessPlan
       -> materialize only the selected channel -> execute -> postprocess/assets/result
```

- `--mode auto`: run the complete chain and select a feasible protocol. This is the default when neither mode nor protocol is supplied.
- `--mode native`: source-semantic, in-process parsing. PDF analysis is reused by execution; structured documents reuse their canonical source artifact.
- `--mode protocol --protocol <name>`: use one service/model protocol whose endpoint returns a complete page result.
- `--mode pipeline`: let core compose typed layout/OCR/formula/table stages.

`--protocol native|tesseract|mineru-vlm|...|auto` remains a compatibility shortcut. Do not combine a mode with a conflicting protocol. `--mode protocol` requires a concrete model protocol; select local OCR as `--protocol tesseract`.

Read [references/v2-architecture.md](references/v2-architecture.md) when debugging routing, artifact reuse, or execution-family boundaries. Read [references/protocols.md](references/protocols.md) before choosing a less common protocol or configuring its service contract.

## Default workflow

For an unfamiliar document or a batch where model cost matters:

1. Run `uparser plan --mode auto --prefer quality <file>`.
2. Inspect `profile.source_format`, `profile.source_quality`, genre confidence/evidence, candidate feasibility/rejections, selected protocol, and `preprocess`.
3. If the selected protocol is remote, run `uparser doctor <protocol> --endpoint <url>`. A candidate being listed as feasible is not proof that its endpoint is running.
4. Parse. Prefer JSON for automation and Markdown for direct reading.
5. On exit `0` or `3`, inspect `warnings`, `capability_notes`, `page_errors`, `route_decision`, and `preprocess_plan`. Surface meaningful recovery or fidelity warnings to the user.

`--prefer quality|speed|cost` belongs to `plan`; `parse --mode auto` currently executes the quality policy. To execute a speed/cost plan exactly, pass the selected concrete mode/protocol to `parse`.

```bash
# Inspect without executing a parser.
uparser classify mystery.pdf
uparser plan mystery.pdf --mode auto --prefer quality

# Fast, source-faithful local parse.
uparser parse report.pdf --mode native --format markdown --output report.md

# Lossless source structure for a non-PDF document.
uparser parse contract.docx --mode native --format document-json --output contract.json

# Explicit MinerU model protocol.
uparser parse scan.pdf --mode protocol --protocol mineru-vlm \
  --endpoint http://127.0.0.1:19122/v1/chat/completions \
  --model MinerU2.5-Pro-2605-1.2B --format json

# Local full-page OCR. Requires PDFium and a working Tesseract installation.
uparser doctor tesseract
uparser parse scan.pdf --protocol tesseract --format markdown
```

## Choose the execution path

| Input or requirement | Preferred path | Important boundary |
|---|---|---|
| Born-digital PDF | `--mode native` | Fast, local text/structure path; preserves engine Markdown by default |
| PDF with a few scanned or garbled pages | `--mode native` | With PDFium + Tesseract, only flagged pages may be replaced by bounded local OCR |
| Fully scanned PDF or PNG/JPEG | `--protocol tesseract` for local OCR, or `mineru-vlm` for layout quality | Explicit native rejects inputs without reliable text/source semantics |
| DOC(X), PPT(X), Excel, ODF, EPUB, RTF, CSV/TSV | `--mode native` for source fidelity/offline work | Reads source structure directly; no conversion or model |
| Presentation where visual grouping matters | `plan` then a model protocol | Visual mode converts structured input through LibreOffice first |
| Complex layout, reading order, tables/formulas | `mineru-vlm` or another verified model protocol | Match the adapter to the deployed response contract |
| Separately deployable layout/OCR/formula/table stages | `--mode pipeline` | Select only after all real stage endpoints/backends are validated |

Do not promise that auto routing is a quality oracle. It ranks candidates from observed evidence and compiled/runtime signals. Preserve its reason codes, rejected candidates, and confidence when auditability matters.

## Native and structured documents

The `native` family contains two in-process engines:

- PDF: text layer, reading order, headings, lists, tables, formula/semantic hints, and image/vector region discovery.
- Structured documents: DOC/DOCX, PPT/PPTX, Excel variants, ODT/ODS/ODP, EPUB, RTF, and CSV/TSV from their own source semantics.

For structured inputs, choose output deliberately:

- `--format markdown`: readable flattened output.
- `--format json`: common page/block IR shared with PDF and model protocols.
- `--format document-json`: lossless canonical units, lists, table grids, notes, assets, and structured warnings. Valid only for structured native documents, not PDF or model routes.

Structured-only flags are `--no-notes`, `--headers-footers`, and `--max-input-mib <N>`. Scheduler flags (`--pages`, `--stream`, `--window-size`, `--max-concurrency`) have no effect on native whole-document execution and produce a warning.

Native PDF asset geometry is retained even without PDFium. Actual PDF image/vector crops require PDFium; `--no-assets` prevents rasterization and writes. Native page-level OCR additionally requires Tesseract and the requested language data. `UPARSER_OCR_LANG` overrides automatic `eng` / `chi_sim+eng` / `chi_tra+eng` selection.

Known structured-format losses should remain visible through warnings. Notable current gaps include limited style/heading recovery for legacy DOC, flattened legacy PPT tables and unsupported EMF/WMF pictures, and ordered-only RTF list typing.

## Results and side effects

Use `--format json` when downstream code needs blocks, bounding boxes, normalized categories, formulas, tables, provenance, or routing metadata. `--markdown-source canonical` is the default: every source, native PDF included, is rendered by the one shared renderer. `engine-legacy` keeps the native engine's own Markdown writer for one release as a fallback.

Assets are written by default to `<source_stem>_images/`. Use `--assets-dir <dir>` to control the location or `--no-assets` to avoid filesystem writes. `--output <path>` writes the aggregate successful result to a file while preserving errors on the normal channels.

`--redact-pii` redacts common email, mainland-China phone, and resident-ID values only in emitted CLI output. Parsing and cache content remain source-faithful; do not treat it as a general anonymizer.

Model/scheduler execution supports `--pages`, `--stream` (NDJSON windows), `--window-size`, and `--max-concurrency`. Native bypasses the model result cache; model protocols use the content/plan/options fingerprint cache unless `--no-cache` is set. Always use `--no-cache` for benchmarks or reproducibility checks.

## Failure handling

- Exit `1`: fix arguments/mode/output selection, or stop retrying an unchanged corrupt/unsupported input.
- Exit `2`: repair the dependency or environment (endpoint, LibreOffice, PDFium, Tesseract/language data, encryption, resource budget, output/assets path), then retry.
- Exit `3`: keep the usable result and inspect `page_errors`; retry only failed pages if appropriate.
- Exit `4`: preserve the structured error and report the defect; one retry with `--no-cache` can distinguish a stale cache issue.

For JSON requests, failures use `{"error":{"code","message","protocol","stage"}}`. Never merge stderr into stdout before parsing JSON.

## Binary and helpers

The skill scripts can locate, download, or build the binary:

```bash
scripts/uparser-check.sh
scripts/uparser-parse.sh report.pdf
scripts/find_uparser.sh --build
```

The pinned downloader assets may lag Architecture V2. If `uparser --version` or help lacks `--mode`, `plan`, `--output`, `--redact-pii`, or `tesseract`, build the current workspace and set `UPARSER_BIN` rather than silently using an older CLI. Build from `uparser/` with:

```bash
cargo build --release --features native,pdfium
```

PDFium is needed for page rasterization, vision protocols, native PDF asset crops, and local OCR. Pure native text/source parsing does not need it.

Endpoint/model resolution is: explicit flag, `UPARSER_ENDPOINT` / `UPARSER_MODEL`, then `~/.config/uparser/config.toml` (or `UPARSER_CONFIG`) under the effective protocol section. See [references/config.example.toml](references/config.example.toml).
