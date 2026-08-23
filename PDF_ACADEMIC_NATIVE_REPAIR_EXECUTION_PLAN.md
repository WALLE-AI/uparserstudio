# UParser Native Academic PDF Repair Execution Plan

> Scope: native-text academic PDFs produced by TeX/LaTeX or similar publishing systems
> Primary regressions: `D:\llm\1805.06610v1.pdf` and `D:\llm\2607.07663v1.pdf`
> Baseline binary SHA-256: `A1926862B15E0209BEAFEADCF47E650ED71383A88051318EB5B4E832A006523E`
> Date: 2026-08-22
> Parent report: `PDF_NATIVE_OPTIMIZATION_REPORT.md`

## 1. Objective

Repair four independently measurable failures without routing clean native-text PDFs through full-page OCR:

1. Font decoding and ligature fallback must preserve every source glyph as correct Unicode text.
2. Display mathematics must retain operators, scripts, fractions, matrices, and reading order.
3. Running headers, footers, and folios must be removed without deleting real titles or body text.
4. Vector figures must be detected, associated with captions, and exported as inspectable assets when assets are enabled.

The work is complete only when both target PDFs pass their golden assertions and the full 200-document PDF benchmark, Native test suite, coverage gate, and performance gate show no regression.

## 2. Frozen Baseline

Current behavior on the eight-page target PDF:

| Signal | Baseline |
|---|---:|
| Parse exit code | 0 |
| Native parse time | 59-87 ms |
| Markdown bytes | 17,350 |
| Repeat-run output drift | 0 |
| Detected format | PDF / native text / academic paper |
| Obvious corrupted ligature words | at least 37 |
| Leaked running-header lines | 5 |
| Exported figure assets | 0 |
| Figure captions retained | 3 |
| Formula quality | flattened; operators and two-dimensional structure lost |

Representative mandatory corrections include:

| Current | Expected |
|---|---|
| `Eciency` | `Efficiency` |
| `articial` | `artificial` |
| `denition` | `definition` |
| `ecient` | `efficient` |
| `nite` | `finite` |
| `rst` | `first` |
| `dierent` | `different` |
| `gure` | `figure` |
| `xed` | `fixed` |

Current behavior on the 42-page secondary PDF:

| Signal | Baseline |
|---|---:|
| Parse exit code | 0 |
| Native parse time | about 135 ms |
| Markdown bytes | 145,566 |
| Detected format | PDF / native text |
| Detected primary genre | `contract` (incorrect) |
| Expected primary genre | academic survey/paper |
| Markdown headings | 39 |
| Emitted table rows | 7 |
| Exported figure assets | 0 |
| Figure captions present | Figure 1 through Figure 6 |
| Structural failures | Table 1 consumes following prose; duplicate References heading |
| Link failures | multiple arXiv URLs split across lines |

The secondary regression prevents overfitting to old Computer Modern fonts. It adds a modern 42-page survey with long references, a taxonomy table, multi-page structure, six vector figures, URLs, and substantially different font/layout behavior.

Before changing algorithms, add the PDF to the Native regression fixture inventory under a stable, license/provenance-documented name. Store a hand-reviewed golden Markdown excerpt and machine-readable assertions rather than relying only on one full-file snapshot.

## 3. Non-Negotiable Design Rules

- Prefer PDF encoding evidence in this order: valid ToUnicode, `/ActualText`, encoding Differences, embedded-font cmap/post names, font-scoped standard encoding, then an explicit unknown marker.
- Never repair missing `fi`, `ff`, or `fl` by dictionary or language-model guessing. A glyph-level fix must be supported by font metadata or a frozen font-specific encoding table.
- Never convert an entire native-text page to OCR merely because one font or formula is defective. Fallback must be region- or run-scoped.
- Low-confidence formula reconstruction must preserve readable Unicode tokens and coordinates; it must not emit fabricated LaTeX.
- Header/footer removal must use document-wide evidence and retain an escape hatch through the existing `--headers-footers` contract.
- Figure extraction must not classify table borders, underlines, separators, or ordinary text glyph outlines as figures.
- Every workstream lands behind regression tests and is evaluated independently before combining changes.

## 4. Phase P0: Diagnostic Instrumentation and Goldens

### P0.1 Font trace

Add an opt-in diagnostic trace, disabled in release output, containing:

- page, font resource, BaseFont, subtype, encoding, font descriptor, and embedded font object;
- source bytes/CIDs for each suspicious text operand;
- primary, remapped, fallback, Differences, glyph-name, and `/ActualText` candidates;
- selected candidate and selection reason;
- unmapped/control/PUA/ligature counters per font.

The trace must make it possible to explain why the glyph between `arti` and `cial` disappeared without reading raw content streams manually.

### P0.2 Layout trace

Export a debug JSON for the target PDF containing text-item boxes, baselines, font sizes, path/rectangle geometry, image/Form XObject boxes, candidate formula regions, repeated-line clusters, and candidate vector-figure regions.

### P0.3 Golden package

Create four independent goldens:

- `text.json`: expected corrected words and forbidden corrupted forms;
- `formula.json`: normalized tokens/structure and page-space bounding boxes for representative equations and the matrix;
- `margins.json`: expected removed and preserved lines by page;
- `figures.json`: three expected figure regions, captions, page numbers, and minimum crop bounds.

Exit condition: every baseline failure in both PDFs is reproducible by a focused automated assertion before algorithm changes begin.

## 5. Phase P1: ToUnicode and Ligature Repair

### P1.1 Determine the exact failing encoding

Use the P0 trace to identify whether the missing ligatures are caused by:

- an absent or sparse ToUnicode map;
- TeX Type1 encoding bytes in the C1 range;
- a Differences entry mapped to a multi-character glyph name;
- an embedded font cmap/post table that loses ligature names;
- fallback selection preferring readable-but-incomplete text;
- a later normalization stage dropping a valid decoded glyph.

No mapping change is allowed until the failing font and byte/CID are recorded in a test.

### P1.2 Make mappings multi-character-safe

Audit `FontEncodingMap`, `EncodingResult`, `ToUnicodeCMap`, and operand decoding so a mapping can represent `ff`, `fi`, `fl`, `ffi`, `ffl`, and `st` without reducing them to a single scalar too early. Preserve the selected decode source with the run for diagnostics.

### P1.3 Add font-scoped TeX encoding support

Implement standard TeX/Computer Modern encoding-vector lookup only when the BaseFont/font descriptor and encoding evidence match. Cover at least CM, EC, TC, and common Nimbus/LaTeX subset combinations observed in the fixture corpus. Differences and embedded font evidence override static tables.

### P1.4 Improve candidate scoring

Penalize candidates containing word-internal C1 controls, unknown glyph markers, implausible glyph loss, or a lower decoded-glyph count than an evidence-backed fallback. Do not award a candidate merely because the remaining ASCII letters form valid-looking text.

### P1.5 Tests and acceptance

Required tests:

- unit tests for every TeX ligature code encountered in the target;
- Differences mappings for Unicode ligatures and multi-character glyph names;
- primary versus embedded-font fallback selection;
- negative tests for WinAnsi punctuation, math symbols, CJK, Arabic, and existing Aptos handling;
- full target-PDF assertion that all forbidden corrupted forms are absent.

Acceptance:

- zero known corrupted ligature tokens in the target;
- zero replacement/control characters in emitted Markdown;
- no regression in the existing ToUnicode/font test inventory;
- NID on the 200-document set does not decrease.

## 6. Phase P2: Formula Reconstruction

### P2.1 Introduce a formula-region intermediate representation

The current `ItemType` has no formula variant. Add a formula IR at the extraction/structured boundary rather than embedding guessed Markdown in ordinary text. It should retain:

- ordered glyph tokens and original Unicode;
- font family and math-font evidence;
- baseline, font size, rise, and bounding box;
- confidence and reconstruction warnings;
- optional normalized LaTeX only when confidence is sufficient.

Keep the serialized contract backward compatible by making new fields optional and by preserving a plain-text fallback.

### P2.2 Detect formula regions

Combine evidence instead of using one global threshold:

- known math/symbol font names;
- operator and delimiter density;
- superscript/subscript baseline offsets;
- vertically stacked glyphs and fraction bars;
- alignment of multiple equation rows;
- nearby equation numbers;
- isolation from prose baselines.

Reject normal italic prose, references, tables, and chart labels through explicit negative fixtures.

### P2.3 Reconstruct in increasing complexity

Implement and validate in this order:

1. linear symbols and relations;
2. superscripts and subscripts;
3. fractions and over/under constructs;
4. grouped delimiters;
5. aligned equations;
6. matrices and cases.

Each layer consumes geometry from the prior layer. When ambiguous, emit normalized Unicode text plus a warning, not invented structure.

### P2.4 Rendering and acceptance

Render high-confidence regions as `$...$` or `$$...$$`; escape Markdown safely and keep equation numbers outside the math delimiters.

Acceptance:

- target equations retain comparison operators, summation terms, subscripts, fractions, and matrix row/column structure;
- formula token recall at least 99% on the target golden;
- normalized structure exact match at least 95% for selected target formulas;
- no new formula regions in the negative prose/table/chart corpus;
- TEDS, MHS, NID, and reading order do not regress on the 200-document set.

## 7. Phase P3: Running Header, Footer, and Folio Filtering

### P3.1 Fix the evidence model

The current repeated-line filter intentionally keeps the first occurrence of every repeated candidate. That behavior is appropriate for some document titles but leaks running headers. Replace the unconditional rule with classification based on:

- normalized text and page-edge position;
- odd/even page parity clusters;
- stable Y and left/center/right alignment;
- attached or detached folio numbers;
- occurrence coverage after excluding cover/first page;
- whether the same text appears as a real body/title line away from the margin.

### P3.2 Support alternating academic headers

Treat author-name headers on even pages and short-title headers on odd pages as one running-header family. Normalize leading/trailing folios separately from semantic text. Remove every classified running-header occurrence, including its first margin occurrence, while preserving the actual title and author block in the body region.

### P3.3 Connect profiler evidence

Populate `repeated_header_footer_ratio` and per-page profile evidence from the same decisions used by extraction. The profiler must not report zero when lines were classified or removed.

### P3.4 Tests and acceptance

Add target tests plus synthetic fixtures for alternating headers, title repeated as a header, short author names, section titles near the top edge, tables touching margins, and the `--headers-footers` opt-out.

Acceptance:

- zero leaked running headers/folios in the default target output;
- title, author, affiliation, section headings, and references remain intact;
- `--headers-footers` restores the original margin content;
- no regression in existing folio and repeated-line tests.

## 8. Phase P4: Vector Figure Preservation

### P4.1 Separate vector figures from vector tables and text outlines

Add page-level vector-region detection using connected path/rectangle components, local geometry density, fills/strokes, nearby labels, and caption anchors. Exclude:

- vector grids accepted by the table detectors;
- isolated rules and underlines;
- page borders and decorative separators;
- glyph-outline pages classified as vector text;
- tiny repeated logos unless explicitly requested.

### P4.2 Add a figure-region contract

Represent a detected vector figure as a structured region with page-space bbox, caption linkage, confidence, source=`vector`, and stable figure id. Do not force the Native engine to write files directly.

### P4.3 Rasterize through the existing frontend

When assets are enabled and PDF rasterization is available, reuse the existing PDFium page source and core crop/asset writer:

1. render only pages containing accepted figure regions;
2. convert PDF-point boxes to pixel boxes using the recorded DPI;
3. crop with a small bounded padding;
4. encode PNG and place bytes in the existing block asset contract;
5. let the content-addressed asset writer assign paths and deduplicate bytes.

When PDFium is unavailable, retain the figure block, caption, bbox, and an explicit `asset_unavailable` warning. Do not silently emit zero figures.

### P4.4 Reading order and caption linkage

Insert each figure block near its associated caption while excluding internal chart labels from body reading order. Preserve labels as optional figure metadata so they remain available to downstream chart understanding.

### P4.5 Tests and acceptance

Required fixtures include the three target figures, a ruled table, a bar chart, an axis-only plot, a flow diagram, text rendered as outlines, and a page with both a figure and two prose columns.

Acceptance:

- target figure-region recall 3/3 and precision 3/3;
- three nonblank PNG assets when built with PDFium and assets enabled;
- each crop contains its expected figure and excludes unrelated body text;
- captions appear once and in reading order;
- no table-to-figure or text-outline-to-figure regression;
- `--no-assets` performs no rasterization and writes no files.

## 9. Phase P5: Academic Structure, Links, and Genre Guards

The secondary PDF introduces failures adjacent to the original four workstreams. They must be repaired separately so table or figure logic is not burdened with compensating for broken document structure.

### P5.1 Table termination

Add a table-boundary guard that ends a detected table when following content changes simultaneously in row geometry, prose density, font/heading role, and column alignment. A full-width paragraph must not be appended to the last table row merely because its baseline overlaps inferred column anchors.

Target acceptance:

- Table 1 contains only its intended header and five category rows;
- `Foundations, limits & safety` remains the final data row;
- `2. Preliminaries, Taxonomy, and Method` starts outside the table as a heading;
- no existing 200-document TEDS score decreases.

### P5.2 URL continuation

Join a wrapped URL only when adjacent fragments are contiguous in reading order and the combined value parses as a supported URI. Keep link text and destination synchronized. Do not join prose that merely follows a link at the next baseline.

Target acceptance:

- every arXiv `/abs/YYMM.NNNNN` link is complete;
- no destination ends in partial fragments such as `/abs/26`, `/arxi`, `/ab`, or `.or`;
- URL formatting tests cover punctuation, DOI links, line wraps, and two-column references.

### P5.3 Duplicate section headings

Deduplicate adjacent or page-boundary-equivalent headings after structure ordering, using normalized text plus heading role and distance. Preserve intentionally repeated running chapter titles when `--headers-footers` is requested.

Target acceptance: exactly one `References` section heading in the default secondary output.

### P5.4 Genre and parse hints

Revise genre scoring so academic-paper evidence (arXiv identifier, Abstract, numbered sections, citations, References, affiliations) dominates incidental words associated with contracts, regulation, or governance. Derive `emphasize_formulas` and `emphasize_charts` from actual font/path/layout evidence as well as genre keywords.

Target acceptance:

- secondary primary genre is `academic_paper` or `academic_survey`, not `contract`;
- figures 1-6 produce chart/figure evidence in the profile;
- `repeated_header_footer_ratio` reflects actual margin decisions rather than a constant zero;
- routing remains Native for native text and does not trigger full-page OCR.

## 10. Integration Sequence

Execute in this order because later work depends on clean glyphs and stable regions:

| Order | Batch | Dependency | Merge gate |
|---:|---|---|---|
| 1 | P0 diagnostics/goldens | none | all four failures reproduced |
| 2 | P1 font/ligature repair | P0 font trace | text golden and NID pass |
| 3 | P3 header/footer repair | P0 layout trace | margin golden and RO pass |
| 4 | P2 formula IR/reconstruction | P1 clean symbols | formula golden and MHS pass |
| 5 | P4 vector figure regions/assets | P0 layout trace | figure precision/recall pass |
| 6 | P5 structure/link/genre guards | P0 and individual failure goldens | structure and profiler gates pass |
| 7 | combined regression and optimization | P1-P5 | all release gates pass |

P2, P4, and P5 may be developed independently after P0, but they must not be merged together before their individual output diffs are reviewed.

## 11. Verification Matrix

For every batch run:

1. focused unit tests for the modified module;
2. full `uparser-native-engine` tests;
3. both target-PDF golden assertion sets and full Markdown diffs;
4. 200-document OpenDataLoader evaluator with per-document deltas;
5. output hash comparison against the frozen prediction set;
6. five-round target microbenchmark and seven-round 200-document benchmark for the final batch;
7. clean LLVM coverage for affected packages;
8. malformed-font/content-stream tests and bounded-input checks.

Mandatory global guards:

| Metric | Requirement |
|---|---:|
| Both target parse success | 100% |
| Both target repeat-run hash drift | 0 |
| Primary corrupted ligature forms | 0 |
| Both target leaked running headers | 0 |
| Primary vector figure recall/precision | 3/3 and 3/3 |
| Secondary vector figure recall/precision | 6/6 and 6/6 |
| Primary formula token recall | >=99% |
| Secondary Table 1 structure | exact six-row match including header |
| Secondary broken arXiv URLs | 0 |
| Secondary primary genre | academic paper/survey |
| PDF-Public Overall/NID/TEDS/MHS | none may decrease |
| Native line coverage | >=90% |
| Full Native tests | 0 failures |
| Five-round median CV | <=3% |
| Native median performance | no more than 5% regression from 26.466 ms/doc |
| `--no-assets` filesystem writes | 0 |

## 12. Risk Controls

- Font fixes have the largest global blast radius. Gate every static mapping by font identity and encoding evidence.
- Formula reconstruction can generate plausible but false semantics. Preserve raw tokens and make confidence observable.
- Aggressive header removal can delete titles and table headers. Require margin, repetition/parity, and body-duplicate evidence together.
- Vector clustering can absorb tables or entire pages. Reuse table claims first and enforce region size/density bounds.
- PDFium is optional. Figure metadata must work without it; only asset materialization depends on it.
- New structured fields must remain optional so existing JSON and Markdown callers continue to deserialize old output.
- Table termination and URL joining must be geometry- and parser-backed; broad text regex rewrites are not acceptable.
- Genre changes affect routing. Require frozen route decisions for all 200 public documents before merge.

## 13. Definition of Done

This plan is complete only when:

- both target goldens have zero known text, formula, margin, figure, table-boundary, URL, and genre failures;
- at least 20 additional TeX/LaTeX academic PDFs from different producers pass the same category-level checks;
- the 200-document quality metrics do not regress and changed outputs are manually classified;
- Native tests, coverage, determinism, memory, and performance gates pass;
- profiler output reports the target's figure and repeated-margin evidence consistently with the parser;
- the release report records the binary SHA, fixture provenance, metrics, remaining limitations, and exact reproduction commands.

Until all conditions pass, report progress per workstream and do not claim that academic PDF extraction is fully repaired.

Chinese engineering-standard PDFs are tracked separately in `PDF_NATIVE_OPTIMIZATION_REPORT.md`. Their mixed-page OCR, normative hierarchy, mandatory-provision, and dense scoring-table gates must also pass before making a general Native PDF quality claim.
