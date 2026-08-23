# UParser v0.4.0-rc.1

This is an Architecture V2 release candidate. It is intended for controlled
evaluation and is not a claim of general superiority over LiteParse, Anydoc,
or pdf-inspector.

## Highlights

- Adds Architecture V2 document classification, planning, routing, and typed
  pipeline execution while retaining the explicit native/protocol modes.
- Generalizes native PDF recovery for ToUnicode/ligature defects, wrapped URLs,
  academic headers, technical-standard clauses, lists, TOCs, and false tables.
- Preserves embedded, scanned, rectangle/bar, and general painted-path figures
  through the asset pipeline, with bounded caption relations.
- Emits conservative untagged display-formula blocks with token geometry and
  confidence without fabricating LaTeX.
- Improves structured Office/OpenDocument/EPUB/RTF/CSV parsing and document-kind
  analysis.
- Adds PII-redacted resume diagnostics and stronger resume/list arbitration.

## Windows assets

- `uparser-v0.4.0-rc.1-windows-x86_64.exe`
- `uparser-v0.4.0-rc.1-windows-x86_64-pdfium.dll`
- `uparser-v0.4.0-rc.1-windows-x86_64.zip` (contains `uparser.exe`,
  `pdfium.dll`, release notes, a source/build manifest, and third-party
  licenses/attribution)
- `SHA256SUMS`

Keep `pdfium.dll` next to `uparser.exe` when using PDF rasterization, OCR, or
vision-model protocols. Pure native text-layer PDF parsing does not depend on
PDFium.

## Verification status

- Native engine: 891 tests passed.
- Core with native features: 398 tests passed.
- CLI: 47 tests passed.
- Contract tests: 2 tests passed.
- Native structured-document tests: 10 tests passed.
- Eight paper/standard/book/resume samples were parsed and audited locally.

## Known release blockers

- The product crates remain `UNLICENSED`; public redistribution terms have not
  been selected.
- The Windows executable is unsigned. Local Windows Application Control policy
  may reject it until it is signed by an allowed publisher.
- Stratified 200-document quality, coverage, five-round performance, and all
  competitor G-C gates are not complete.
- Remaining rich IR work includes cross-line formulas, explicit table spans and
  cross-page table identity, clause hierarchy/style provenance, notes and
  references, typed resume chronology, and internal chart semantics.

See `PDF_NATIVE_OPTIMIZATION_REPORT.md` and
`ARCHITECTURE_V2.0_OPTIMIZATION_EXECUTION_PLAN.md` for the evidence and gate
definitions.
