# UParser V2 Current Coverage Report

## Scope

- Date: 2026-08-22
- Tool: `cargo-llvm-cov 0.9.0`
- Packages: `uparser-core`, `uparser-document-engine`, `uparser-native-engine`
- Features: `native`
- Excluded bindings: `uparser-napi`, `uparser-python`
- Result: 1420 unit/integration tests in the current inventory; final package batches passed with 0 assertion failures
- Machine artifacts: `benchmark/results/coverage_core_final.json`, `benchmark/results/coverage_document_final.json`, and `benchmark/results/coverage_native_final.json`

Reproduction command:

```powershell
cargo llvm-cov --workspace --exclude uparser-napi --exclude uparser-python --features native --json --output-path ..\benchmark\results\coverage_v2_current.json
cargo llvm-cov -p uparser-core --features native --lib --bins --tests --json --output-path ..\benchmark\results\coverage_core_final.json
cargo llvm-cov -p uparser-document-engine --json --output-path ..\benchmark\results\coverage_document_final.json
cargo llvm-cov -p uparser-native-engine --lib --tests --json --output-path ..\benchmark\results\coverage_native_final.json
```

## Summary

| Package | Files | Lines | Line coverage | Functions | Function coverage | Regions | Region coverage |
|---|---:|---:|---:|---:|---:|---:|---:|
| `uparser-core` | 45 | 10540/11455 | 92.01% | 1181/1323 | 89.27% | 15443/16768 | 92.10% |
| `uparser-document-engine` | 20 | 8275/9400 | 88.03% | 639/716 | 89.25% | 12397/14346 | 86.41% |
| `uparser-native-engine` | 33 | 42139/46390 | 90.84% | 3243/3517 | 92.21% | 75602/82770 | 91.34% |
| **Total** | **98** | **60954/67245** | **90.64%** | **5063/5556** | **91.13%** | **103442/113884** | **90.83%** |

## P0 Files

| File | Lines | Covered | Coverage | Status |
|---|---:|---:|---:|---|
| `uparser-document-engine/src/formats/sheet.rs` | 305 | 278 | 91.15% | File gate passed; more XLS behavior cases remain |
| `uparser-document-engine/src/formats/doc.rs` | 609 | 590 | 96.88% | File gate passed; style recovery remains a product gap |
| `uparser-native-engine/src/extractor/xobjects.rs` | 709 | 633 | 89.28% | File gate passed; filtered and real complex fixtures remain |
| `uparser-native-engine/src/tounicode.rs` | 3046 | 2624 | 86.15% | File gate passed; embedded-font fixture coverage remains |
| `uparser-native-engine/src/lib.rs` | 5006 | 4406 | 88.01% | File gate passed |

## Structured Office Increment

DOCX chart, diagram, image-alt, and embedded-object tests plus PPTX hyperlink tests raised the document engine package above its 88% gate. Current structured format files are `docx.rs` 1199/1408 (85.16%), `pptx.rs` 567/669 (84.75%), `odf.rs` 833/964 (86.41%), and `render/mod.rs` 666/730 (91.23%). ODF tests cover trimming repeated trailing empty rows/cells without removing internal sparse gaps.

## Core Increment

The CLI entry point now returns `ExitCode` instead of calling `std::process::exit`, allowing LLVM profiles and normal process cleanup to complete on Windows without changing the published exit-code contract. The 46 CLI tests now cover 621/754 lines (82.36%), including aggregate `--output`, output failures, image-only PDFs with and without metadata, and structured `doctor` errors. Additional Native adapter, profiler, scheduler, semantic classifier, structured lowering, local Tesseract, lightweight CLI, conversion failure, and in-memory XLSX tests keep `uparser-core` above its 92% package gate.

## Native Increment

The earlier thirteen-test increment covers direct and indirect link annotations, URI actions, AcroForm field inheritance and filtering, tagged-table extraction, synthetic structure-tree parsing, and consolidated financial-value splitting. `extractor/links.rs`, `structure_tree.rs`, and `tables/financial.rs` cover 410/427 (96.02%), 874/908 (96.26%), and 172/173 (99.42%) lines respectively.

The final Native increment adds Markdown analysis/preprocessing boundaries, public detector strategies, vector-grid and coordinate helpers, TSR entrypoints, CMap decision caching, simple/CID font-width error handling, and real PDF regressions for a split borderless fish table and SIFT prose falsely classified as a table. Three tests cover misplaced and correctly ordered Part/Chapter headings in contents pages. Four tests cover a wrapped year in a date cell and extraction of a sparse numbered table caption, including negative guards. Two tests repair a wide joined header before complete numeric rows while preserving a true spanning title before a textual header. Six further tests cover unambiguous bounding-box header placement, rejection of multi-column spans and nonnumeric bodies, preservation of data-row placement, and sparse percentage rows. The repaired postprocessor covers 631/668 lines (94.46%); `tables/format.rs`, `tables/grid.rs`, and `tables/detect_heuristic.rs` cover 1055/1094 (96.44%), 693/711 (97.47%), and 1486/1690 (87.93%) lines respectively. Native now covers 42139/46390 lines (90.84%); the three-package aggregate covers 60954/67245 lines (90.64%). `lib.rs`, `extractor/fonts.rs`, `markdown/analysis.rs`, `markdown/preprocess.rs`, and `detector.rs` retain their package-gate coverage; exact values remain available in the machine artifact.

## Sheet Increment

Five new tests cover an in-memory XLSX end-to-end parse plus cell types, formulas, merged-cell spans, invalid merge warnings, date/duration rendering, number formatting, and header inference. `sheet.rs` increased from 0% in the previous report to 91.15% in the current report.

Seven new XObject tests cover direct and indirect resource dictionaries, Form matrices, font style propagation, nested Forms, images inside Forms, recursive depth limits, white-text suppression, and `TJ` column gaps. `xobjects.rs` increased from 6.80% to 89.28%.

Twelve new legacy DOC tests cover FIB variable sections and flags, compressed and UTF-16 piece tables, main-body clipping, truncated structures and warnings, language codepages, block/table state transitions, text and stream budgets, and real OLE parsing through the selected table stream. `doc.rs` increased from 58.19% to 96.88%.

Thirty new ToUnicode tests cover malformed text CMaps, `bfchar`/`bfrange`, `usecmap`, Identity maps, CID-to-GID maps, Type0 and simple-font fallbacks, page filters, Form XObjects, and the bundled Adobe Japan1/GB1/CNS1/Korea1 binary CMaps. The real-resource tests exposed and fixed binary CMap decoding: first records use fixed-width values while subsequent records use signed delta encoding. `tounicode.rs` increased from 48.91% to 86.15%.

## Limits

- Windows Application Control intermittently rejected newly built test executables with OS error 4551. Retrying succeeded for the final Core and Native package coverage runs; the report therefore uses three independently successful package artifacts rather than claiming a single successful workspace instrumentation run.
- Native coverage was rerun after `cargo llvm-cov clean --workspace`; this removed stale profiles that had incorrectly merged source revisions and inflated the denominator. The final Native artifact contains one clean 863-test run.
- LLVM emitted no Rust branch coverage data; branch coverage is not inferred from the zero-valued field.
- Coverage does not replace semantic quality, performance, holdout, robustness, or external-service gates.
