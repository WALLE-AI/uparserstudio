# Native Capability Manifest

This manifest freezes the document-processing behavior that architecture v2
must preserve. A source move is not complete until its row has a target
consumer and the listed checks pass. Algorithm tuning is a separate change.

## Gates

- `preserved`: the implementation remains owned by its current engine.
- `adapted`: core may add a typed adapter or reusable artifact without
  reimplementing the algorithm.
- `shadowed`: old and new orchestration run against the same fixture and their
  observable outputs are compared.
- `retired`: only orchestration code may reach this state, after shadow tests.
  Engine algorithms do not become `retired` as part of architecture v2.

## uparser-native-engine

| Capability | Source | Observable contract | Existing coverage | V2 target |
|---|---|---|---|---|
| PDF type and OCR routing | `detector.rs`, `text_quality.rs`, `lib.rs::{detect_pdf_mem,process_pdf_mem,extract_pages_markdown_mem}` | PDF type, confidence, page count, OCR pages and reason codes, encoding warning | engine unit tests; core profiler L2 tests | preserved; exposed through a reusable PDF analysis artifact |
| Content extraction | `extractor/content_stream.rs`, `extractor/xobjects.rs`, `extractor/mod.rs` | text, positions, images/forms and selected-page behavior | engine extractor tests | preserved |
| Fonts and text decoding | `extractor/fonts.rs`, `tounicode.rs`, `glyph_names.rs`, `adobe_korea1.rs`, `text_utils.rs` | ToUnicode/CMap fallback, widths, styles, ligatures, CJK and RTL text | engine unit tests and bundled bcmaps | preserved |
| Geometry and reading order | `extractor/layout.rs`, `extractor/reading_order.rs`, `extractor/underline.rs` | line grouping, columns, page-number filtering, image-anchored flow and underlines | engine unit tests | preserved; no replacement in core |
| Structure and Markdown | `structure_tree.rs`, `markdown/{analysis,classify,heading,preprocess,convert,postprocess}.rs` | headings, paragraphs, lists, code/captions, repeated-line removal and Markdown | engine tests; opendataloader benchmark | preserved; engine Markdown remains authoritative until G-N passes |
| Tables | `tables/{detect_heuristic,detect_lines,detect_rects,detect_struct,financial,grid,structured,format}.rs` | heuristic, ruled/vector, rectangle, structure-tree and financial tables | engine table tests; Table/TEDS benchmark | preserved |
| Hybrid/region APIs | region and structured-table APIs in `lib.rs` | bounded page/region extraction and structured-table fallback behavior | engine unit tests | preserved for profiler and future mixed-page routing |

## uparser-document-engine

| Capability | Source | Observable contract | Existing coverage | V2 target |
|---|---|---|---|---|
| Authoritative format detection | `detect.rs` | 16 enum variants; container identity before extension hint | detection/integration tests | preserved; the only format enum used by core |
| Package and relationship resolution | `package.rs`, `ooxml.rs` | root relationships, parts, limits and malformed/encrypted errors | integration and mutation tests | preserved |
| DOC/DOCX | `formats/doc.rs`, `formats/docx.rs` | text encoding, styles/headings, lists, tables, links, images, notes, headers/footers | engine tests; core native document tests | preserved |
| PPT/PPTX | `formats/ppt/`, `formats/pptx.rs` | slide order, titles, styled text, lists, tables, pictures and notes | engine tests | preserved |
| ODF | `formats/odf.rs` | ODT/ODS/ODP metadata, styles, lists, repeated/spanned cells and assets | engine tests and mutation tests | preserved |
| EPUB/RTF | `formats/epub.rs`, `formats/rtf.rs` | spine/nav/anchors/notes/links and RTF encoding/styles/notes | engine tests | preserved |
| Sheets and delimited text | `formats/sheet.rs`, `formats/csv.rs` | workbook units, merged cells, value kinds, header inference, CSV/TSV delimiter handling | engine tests; core native document tests | preserved; replaces the legacy core bypass after shadow comparison |
| Source semantic IR/rendering | `model.rs`, `render/mod.rs` | blocks/inlines/tables/assets/notes/warnings and document JSON/Markdown | renderer and integration tests | preserved; mapped losslessly into the v2 result |

## uparser-core

| Capability | Source | Observable contract | V2 target |
|---|---|---|---|
| Native adaptation | `adapters/native.rs` | one parse per source, PDF/structured dispatch, assets, semantic errors and compatibility output | move orchestration only; shadow before retiring the old entry |
| Reading order and geometry | `reading_order.rs`, `geometry.rs` | stable ordering, coordinate transforms, sanitization and de-duplication | preserve implementation and tests |
| Content and postprocess | `content_normalize.rs`, `postprocess.rs` | punctuation/whitespace normalization and geometric paragraph merge | preserve implementation and tests |
| Formula and table repair | `formula_repair.rs`, `otsl.rs` | repair chain and OTSL-to-HTML warnings | preserve implementation and tests |
| Protocol output interpretation | `output_parse.rs`, `category_map.rs` | protocol grammar decoding and normalized categories | preserve implementation and tests |
| Imaging and robustness | `imaging.rs`, `robustness.rs` | resize/crop/rotation and bounded retry/degeneracy handling | preserve implementation and tests |

## Required Shadow Matrix

Before removing `structured_bypass` or the native CLI/API branches, compare:

- PDF: digital text, scanned, mixed, broken encoding, multi-column, headings,
  repeated headers/footers, links, each table strategy and malformed input.
- Structured: every supported format, with applicable metadata, headings,
  lists, merged tables, links, images, notes/navigation and warnings.
- Outputs: engine Markdown, compatibility JSON, document JSON, assets, error
  stage/exit code, document profile and timing.

The frozen quality baseline is `uparser-native` Overall 0.8756. The 2026-08-20
post-refactor run over 200 documents reproduced it at Overall 0.8754249671,
NID 0.9150189060, TEDS 0.8141173616 and MHS 0.7875113953. All 200 prediction
files are present; one scanned document is explicitly rejected by native and
has the same empty output as the frozen run. The 2026-08-21 cold V2 run took
0.0508103502 seconds per document. The runner reuses the native
analysis artifact and preserves engine Markdown byte-for-byte. G-N also
requires separate Reading Order, Table and Heading reports; an aggregate gain
cannot hide a regressed sub-capability.
