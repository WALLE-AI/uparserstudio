# UParser v0.4.0

This finalizes the Architecture V2 release candidate series (`v0.4.0-rc.1`,
`v0.4.0-rc.2`) as a general release. There is no source change from
`v0.4.0-rc.2` — this tag exists to publish stable Windows and Linux binaries
and to move the `uparser` skill's downloader pins onto a release that ships
both platforms' assets.

## Highlights (carried over from the rc series)

- Architecture V2 document classification, planning, routing, and typed
  pipeline execution, alongside the explicit native/protocol modes.
- Generalized native PDF recovery for ToUnicode/ligature defects, wrapped
  URLs, academic headers, technical-standard clauses, lists, TOCs, and false
  tables.
- Embedded, scanned, rectangle/bar, and general painted-path figures
  preserved through the asset pipeline, with bounded caption relations.
- Conservative untagged display-formula blocks with token geometry and
  confidence, without fabricating LaTeX.
- Improved structured Office/OpenDocument/EPUB/RTF/CSV parsing and
  document-kind analysis.
- A unified Markdown renderer shared across all model protocols and the
  native engine, plus a `monkeyocr-v2` adapter re-aligned against upstream
  `core_runner.py` (image preprocessing, OTSL tokenization, repeat-token
  detection, and document assembly).
- PII-redacted resume diagnostics and stronger resume/list arbitration.

## Windows assets

- `uparser-v0.4.0-windows-x86_64.exe`
- `uparser-v0.4.0-windows-x86_64-pdfium.dll`
- `uparser-v0.4.0-windows-x86_64.zip` (contains `uparser.exe`, `pdfium.dll`,
  release notes, a source/build manifest, and third-party licenses/attribution)

## Linux assets

- `uparser-v0.4.0-linux-x86_64`

## SHA256SUMS

Checksums for every asset above are published as `SHA256SUMS`.

Keep `pdfium.dll` next to `uparser.exe` when using PDF rasterization, OCR, or
vision-model protocols. Pure native text-layer PDF parsing does not depend on
PDFium.

## Known release blockers (unchanged from rc.2)

- The product crates remain `UNLICENSED`; public redistribution terms have
  not been selected.
- The Windows executable is unsigned. Local Windows Application Control
  policy may reject it until it is signed by an allowed publisher.
- Stratified 200-document quality, coverage, five-round performance, and all
  competitor G-C gates are not complete.
- Remaining rich IR work includes cross-line formulas, explicit table spans
  and cross-page table identity, clause hierarchy/style provenance, notes and
  references, typed resume chronology, and internal chart semantics.

See `BENCHMARK_REPORT.md`, `RENDERER_UNIFICATION_EXECUTION_PLAN.md`, and
`MONKEYOCR_V2_ALIGNMENT_PLAN.md` for the evidence behind the changes since
`v0.4.0-rc.1`.
