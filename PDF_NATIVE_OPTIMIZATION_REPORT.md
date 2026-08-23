# UParser Native PDF Optimization Report

> Date: 2026-08-23
> Release SHA-256: `B05100F5AB3FCB041758492557162D5C19911B208889B31E5113ECADF4BA13CB`
> Detailed academic-PDF plan: `PDF_ACADEMIC_NATIVE_REPAIR_EXECUTION_PLAN.md`

## 0. Execution Status (2026-08-23)

This optimization pass is implemented and verified for the bounded fixes that can be supported by the current Native evidence. It does **not** complete every architecture work package below, and it does not authorize a comprehensive competitor-leading claim.

Completed in this pass:

- added `technical_standard` as a first-class genre and synchronized legacy `kind` for standards, books, papers, and resumes;
- gave strong document-level standard/book/paper/resume structures precedence over incidental contract, bid, or regulation vocabulary;
- fixed TOC detection for `目录`, `目錄`, `目次`, `目 次`, Markdown headings, and printed page targets;
- propagated encoding/OCR warnings and added explicit warnings for U+FFFD, suspicious repeated leader glyphs, probable TeX ligature loss, collapsed table rows, long academic formula/reading-order lines, missing figure assets, and suspicious resume identity/contact fields;
- stopped long numbered resume skills from becoming headings, preserved all six professional skills as list items, and fixed fully-bold ordered-list markers;
- made reliable native-text resumes prefer Native over MinerU-VLM despite a column hint; Auto now selects Native `90` versus MinerU-VLM `80`;
- added opt-in `parse --redact-pii` output redaction for email, mainland-China phone, and resident-ID values without changing source-faithful parse/cache results;
- reduced long-book running headers using chapter-span-aware recurrence thresholds;
- demoted sustained low-level visual heading runs in Traditional Chinese quotations while preserving numbered chapter/section runs;
- rejected the Wenzhou book's only false Markdown table and restored the projected columns to prose reading order;
- removed academic running headers only when their text exactly matches front-matter title/author headings;
- repaired common arXiv URLs split across layout lines or by spacing around scheme/domain fragments.
- enabled the existing optional `liteparse-pdfium = 1.3.0` dependency in the Release binary; PDFium remains an external dynamically loaded runtime (`pdfium.dll`), not a UParser rewrite;
- wired local Tesseract 5.5.3 into Native as bounded page-level fallback, with automatic `chi_sim+eng`/`chi_tra+eng` selection, language-data validation, and provenance in `capability_notes`;
- replaced only JGJ page 6 after Native emitted page-level OCR evidence; all clean native pages remain untouched, and engine Markdown falls back to the merged canonical page IR only when a replacement occurs;
- added reversible GBK/UTF-8 mojibake detection as a second ToUnicode quality signal and registered `tesseract` as an explicit CLI protocol with a local `doctor` probe.
- normalized numeric academic section prefixes that were glued to English titles (`3.2Decode` -> `3.2 Decode`) without changing body text or model names;
- rejected sparse equation-shaped Markdown tables, preserved their extracted tokens as plain display text, and demoted numbered display equations that were falsely promoted to headings;
- deduplicated adjacent equivalent section headings and repaired whitespace between `arxiv.org/` and an `abs/YYMM.NNNNN` path before link formatting.
- added opt-in `UPARSER_FONT_TRACE` diagnostics for suspicious source bytes and implemented a BaseFont-scoped Computer Modern OT1 fallback for `0x0B..0x0F` (`ff`, `fi`, `fl`, `ffi`, `ffl`); all known corrupted ligature forms in `1805.06610v1` are now absent without dictionary guessing.
- preserved complete logical records beneath vertically merged table cells when a row still carries independent item, rule, and numeric-score fields; on JGJ 59 Appendix B this restores the scoring-table body from 128 to 260 Markdown data rows and reduces the longest pipe row from 1,038 to 346 characters.
- added technical-standard block IR: declared mandatory clause identifiers are recovered from the publication notice, matching body blocks become `mandatory_clause`, other three-level clause blocks become `normative_clause`, and appendix titles become `annex_heading`; declaration fragments remain evidence rather than being misclassified as body clauses.
- preserved tagged-PDF structure roles (`Formula`, `Note`, `Reference`/`BibEntry`, `Caption`, `Code`, and list roles) from Native MCIDs into normalized Block/Span IR. Formula spans become `is_inline_formula=true`, but `latex` remains empty unless the source provides real LaTeX; the three academic regression PDFs are untagged and therefore correctly produce zero source-backed semantic-role blocks.
- added conservative untagged display-formula inference to Native canonical/document JSON. A line becomes `Formula`/`equation` only when mathematical-font mass, operators, script baseline offsets, short symbolic tokens, or a trailing equation number provide combined evidence; token spans retain geometry and become `is_inline_formula=true`, confidence records the inference, and `latex` remains empty. Lines with substantial prose, URLs/email, table-like labels, or at least 80% overlap with an image/chart asset are rejected or demoted. Real audits retain 8 formula lines on `1805`, 5 on `2608`, 1 on GB50870, and 2 on JGJ59, while `2607`, both books, and the resume remain at zero inferred formula blocks.
- preserved Native Image XObjects as ordered `image` blocks using real MediaBox page dimensions, rasterized only pages containing materializable regions at 150 DPI, cropped the exact rectangles through PDFium, and wrote content-addressed PNG assets through the existing core asset writer. Explicitly scanned pages with no extracted item become full-page `ScannedPage` assets while true `no_text` blank pages remain empty; tiny masks/markers are filtered, `--no-assets` performs no rasterization or writes, successful materialization clears the obsolete missing-asset profile warning, and Native builds without PDFium retain geometry with explicit `asset_unavailable` evidence.
- propagated the existing table-safe rectangle/bar-chart detector into the public Native artifact and Core IR. Accepted regions become padded `VectorChart` blocks and reuse the same bounded PDFium asset path; uniform table grids and stable multi-row financial value columns are rejected by negative evidence.
- added document-agnostic painted-path extraction for top-level content streams and nested Form XObjects, covering `m/l/h/re`, cubic `c/v/y`, stroke, fill, and fill-stroke operations. A sweep-line connected-component detector retains curved, diagonal, and closed axis-aligned figure geometry while rejecting unpainted clip paths, page frames, prose panels, outlined-glyph swarms, uniform grids, stable numeric columns, and pure-axis multi-row text schedules. `2608.16157v1` now materializes five complete `VectorChart` assets (Figures 1-5); `1805.06610v1` retains its state-transition diagram; the modern-business financial statements are not emitted as charts.
- added backward-compatible `asset_caption` relations to image/chart blocks. Binding combines source `Caption` roles, numbered multilingual figure-caption grammar, same-page distance, horizontal overlap, and one-to-one assignment; table/source/note captions and distant candidates are rejected. Wrapped caption lines are joined only across compatible adjacent baselines until terminal punctuation, and one caption can be shared by horizontally aligned sibling Image XObjects. On `2608.16157v1`, all five vector assets bind to complete Figures 1-5 captions; on `1805.06610v1`, the state diagram binds to Figure 1, both side-by-side Figure 2 image assets share its complete caption, and Figure 3 binds to its image asset.

Verification completed:

- `uparser-native-engine`: **891 passed, 0 failed**;
- `uparser-core --features native,pdfium --lib`: **400 passed, 0 failed**;
- the Native-without-PDFium `asset_unavailable` regression test passes;
- all seven mandatory PDFs parsed successfully with the final Release binary;
- `git diff --check` reports no whitespace errors (only repository line-ending notices);
- final sample outputs are in `benchmark/results/*_optimized.md` and are ignored local artifacts.

Still open because the current build has no trustworthy evidence or required runtime:

- JGJ page 6 raster/OCR fallback is implemented, but Tesseract still confuses some TOC text and dotted leaders; the exact page-image golden is therefore not yet closed. Resume field-level visual comparison remains open;
- JGJ B.1-B.19 physical scoring rows and standard clause/mandatory block roles are restored, but explicit rowspan/colspan schema IR, cross-page logical-table identity, clause parent/child links, two-dimensional formula grouping/reconstruction, note/reference IR, and typed resume-field/chronology IR remain architectural work;
- figure region discovery, materialization, and explicit caption-to-asset relations are implemented for painted paths and Image XObjects; richer semantic reconstruction of internal panels and chart data remains open, while `--no-assets` remains faithful;
- the 200-document stratified quality rerun, coverage thresholds, five/seven-round performance CV, and G-C competitor dominance gate were not rerun in this pass;
- competitor smoke rerun was attempted, but the local Python runtime was absent and network policy blocked `uv` from downloading CPython. The existing competitor report remains `INSUFFICIENT`.

## 1. Scope

This report tracks empirical Native PDF failures and optimization work across four document families:

These are four semantic/layout classes inside the same PDF file format. Format detection (`pdf`) is necessary but insufficient; genre, page quality, semantic structure, and rendering fidelity require separate decisions.

- academic PDFs with TeX fonts, formulas, running headers, tables, links, and vector figures;
- Chinese engineering standards with covers, notices, forewords, contents, numbered clauses, mandatory provisions, annexes, and dense scoring tables;
- Traditional Chinese academic monographs with long-form chapters, running heads, notes, bibliographies, financial tables, and scanned covers;
- multi-column resumes with identity/contact fields, skills, chronology, projects, education, links, and sensitive personal data.

Current mandatory regression documents:

1. `D:\llm\1805.06610v1.pdf`
2. `D:\llm\2607.07663v1.pdf`
3. `D:\llm\2608.16157v1.pdf`
4. `D:\llm\GB50870-2013建筑施工安全技术统一规范.pdf`
5. `D:\llm\建筑施工安全检查标准.pdf` (JGJ 59-2011)
6. `D:\llm\近代企業的商道、商術與商法_OA_15July.pdf`
7. `D:\llm\溫州基督徒與中國的全球化_OA_15July.pdf`
8. `D:\llm\高静-算法工程师2026630.pdf`

## 2. Reproduction Configuration

Final measurements use the current Release binary in explicit Native mode with cache and asset output disabled:

```powershell
uparser.exe parse <input.pdf> --mode native --format markdown --no-assets --no-cache
uparser.exe classify <input.pdf>
uparser.exe plan <input.pdf>
```

An additional parse with `--assets-dir` and assets enabled was run for every document to check figure/image materialization.

## 3. Current Result Matrix

| Document | Pages | Parse ms | Markdown bytes | Classification | Main result |
|---|---:|---:|---:|---|---|
| `1805.06610v1` | 8 | 38.4 prior warm | 17,347 | academic paper | genre correct; running headers removed; known CM OT1 ligatures repaired; 3 embedded figures materialized; 8 high-confidence formula lines typed; two-dimensional reconstruction remains |
| `2607.07663v1` | 42 | 103.1 | 146,314 | academic paper | genre corrected; wrapped arXiv links repaired; 6 embedded figures materialized; table/figure semantics remain |
| `2608.16157v1` | 16 | 2,183 concurrent wall observation | 64,394 | academic paper | 11 glued headings removed; false equation table rejected; 2 real tables and 5 formula-line blocks retained; all five vector figures and captions linked; two-dimensional formula reconstruction remains |
| GB 50870-2013 | 26 | 62.9 | 39,649 | technical standard | genre and TOC corrected; one display formula typed; annex/clause hierarchy remains |
| JGJ 59-2011 | 56 profile / 55 emitted pages | 5.2 s concurrent native reparse | Markdown 145,210 | technical standard | all 19 Appendix B captions/totals retained; physical table rows restored; 2 display formulas typed; page-6 TOC OCR and explicit span/schema IR remain |
| `近代企業的商道、商術與商法` | 463 | 5,262.5 | 1,069,640 | book | genre/TOC corrected; repeated headers reduced 190 to 16; notes/tables/assets remain |
| `溫州基督徒與中國草根全球化` | 222 | 690.8 | 466,092 | book | genre corrected; repeated headers 88 to 13; H5 136 to 62; false table removed |
| `高静-算法工程师2026630` | 4 | 47.8 | 12,447 | resume | kind/route/list fixed; suspicious fields warned; opt-in PII redaction added |

All eight final parses returned exit code 0. The timings above are single observations and not a statistically controlled benchmark; `2608` was parsed concurrently with two other papers, while the first cold `1805` invocation was 4.4 seconds under Windows application scanning followed by the reported 38.4 ms warm rerun. These numbers demonstrate no sample-level performance regression but do not satisfy the five-round CV or competitor gates. The original asset-enabled baseline produced zero files. After the asset implementation, `1805` produces 3 Image-XObject PNGs plus its vector state-transition diagram, `2607` produces 6 image assets, the modern-business book produces 10 image assets, and the Wenzhou book produces 17; every retained image block has a real bbox/path and representative crops were visually verified. `2608` now produces five complete path-derived vector assets, visually verified against Figures 1-5. The modern-business financial statement negative regression produces no `VectorChart` block.

## 4. GB 50870-2013 Analysis

Sections 4-8 preserve the 2026-08-22 baseline diagnosis for traceability. Items resolved by the 2026-08-23 implementation are listed in Section 0 and the final matrix; unqualified baseline wording such as "wrong genre" is not the final state.

### 4.1 What works

- Correctly detected as a 26-page native-text PDF and routed to Native.
- Cover, publication notice, foreword, contents text, eight chapters, Appendix A, terminology notes, and referenced standards are present.
- Clause identifiers such as `1.0.1`, `5.2.1`, and `7.2.2` remain readable and ordered.
- The JSON IR now exposes 96 clause blocks: 94 `normative_clause` blocks plus the declared mandatory provisions `5.2.1` and `7.2.2`; two Appendix A headings are `annex_heading` blocks.
- The simple `3.0.2` hazard-level table is reconstructed as a valid four-row Markdown table.
- There are no U+FFFD replacement characters in the output.

### 4.2 Failures

1. **Wrong genre:** primary genre is `bid`; the document is a national engineering technical standard.
2. **Contents not recognized:** the text contains `目次` and section/page leaders, but `has_toc=false`.
3. **CJK semantic substitutions:** examples include `参边单位`, `危险等级稀疏的取值`, `分布分项工程`, `施工启动机械`, `队预防`, `编织`, `內容`, and `釆用`. Some may originate in a defective source text layer, but Native currently emits them without a quality warning or region fallback.
4. **Heading hierarchy is too style-driven:** cover and notice typography generates many low-level headings while chapter/section semantics are not represented consistently.
5. **Nested clauses are flattened:** numbered paragraphs and subitems are concatenated into long prose blocks instead of preserving `1`, `1)`, and deeper list levels.
6. **Mandatory provision semantics are not explicit:** `5.2.1` and `7.2.2` are present, but the output contract does not identify them as mandatory provisions independently of generic bold detection.
7. **Complex annex tables lose structure:** Appendix A is split into multiple Markdown tables; merged cells, repeated headers, continuation relationships, and row ownership are degraded. Stray `提供单位` and continuation labels appear outside table structure.
8. **No asset/region materialization:** asset-enabled parsing emits zero files and no inspectable regions.

Baseline output SHA-256: `7B6F57B79A34B32DDC991693E7FB282135A93B91823076EC47679B45AD61A25F`.

## 5. JGJ 59-2011 Analysis

### 5.1 What works

- Correctly detected as a 56-page PDF with predominantly native text and routed to Native.
- The main normative clauses from chapters 1-5 are largely readable.
- All 19 Appendix B scoring-table captions are present.
- The parser identifies table-bearing pages; the current Native output emits 260 non-separator Markdown table rows after row-span-aware record preservation.
- Numeric values, units, clause identifiers, and most Chinese body text survive outside the damaged contents page.
- The JSON IR now exposes 87 clause blocks: 85 `normative_clause` blocks plus the declared mandatory provisions `4.0.1` and `5.0.3`; the publication-notice declaration itself is retained as evidence, not counted as a body provision.

### 5.2 Failures

Items 1, 3, 4, 11, and 12 below are retained as baseline traceability; the final state resolves genre/TOC/leader corruption, adds bounded page-6 OCR, and propagates the fallback in `capability_notes`.

1. **Resolved genre baseline:** primary genre is now `technical_standard`, not `contract`.
2. **Mixed-page fallback is now active, but not golden-quality:** page 6 is rasterized at 300 DPI through PDFium and replaced by local `chi_sim+eng` Tesseract output. It has zero page errors and no long leader-noise runs after cleanup, but several TOC entries remain misrecognized and the page is a single OCR block rather than structured section/page columns.
3. **Leader glyph corruption:** the same contents region emits 742 `壳` characters in place of dot leaders or encoded glyphs.
4. **Contents not recognized:** despite a visible contents page and section/page patterns, `has_toc=false`.
5. **False headings:** bold notice and foreword continuation lines become H1-H3 headings. The document emits 75 headings, including ordinary sentences and list continuations.
6. **Clause hierarchy is flattened:** multi-level inspection requirements are rendered as single long paragraphs, weakening legal/normative structure and searchability.
7. **Mandatory block roles restored; source styling remains:** `4.0.1` and `5.0.3` are now `mandatory_clause` blocks in JSON IR, but exact bold/style provenance and clause parent/child links are not yet modeled.
8. **Physical scoring rows restored; span IR remains:** serial/item/rule/score records are no longer collapsed by continuation cleanup. All 19 Appendix B captions and all 19 numeric totals are present, but Markdown still represents vertically merged groups with blank cells rather than explicit rowspan metadata.
9. **Column count is unstable:** many tables emit an extra empty column and inconsistent cell counts between captions, headers, data rows, totals, inspector, and date rows.
10. **Cross-page logical identity remains absent:** page-local physical rows are stable, but continuation headers and grouped categories are not yet assigned one document-level table ID and explicit schema.
11. **Partial image handling:** page 6 now has bounded raster/OCR fallback, but general image/vector asset emission remains absent.
12. **Partial profiler/execution evidence:** page-level OCR evidence now reaches the hybrid execution plan and `capability_notes`; richer region confidence and repeated-header evidence remain incomplete.

Baseline output SHA-256: `F2F628D71E1AE34B4ACA1DFB46B2D66E4D197D9B1AC81E5731B3D851E4B838F1`.

Current row-span-repaired Markdown SHA-256: `79E669A2D756BCD265DD3BC3CD9A02F69F225BA2B5ECF4120315AD3B13E93DE4`. The GB 50870 regression output remained byte-identical (`7B6F57B79A34B32DDC991693E7FB282135A93B91823076EC47679B45AD61A25F`).

## 6. Traditional Chinese Monograph Analysis

### 6.1 `近代企業的商道、商術與商法`

What works:

- Native extracts the full 463-page text in reading order through the bibliography; Traditional Chinese, Japanese names, Latin text, links, chapter titles, notes, and many ordinary tables remain readable.
- The title/credits, visible contents, 11 chapters, appendices, and bibliography are present. Output is deterministic between asset-disabled and asset-enabled runs.
- The output contains 52 Markdown table groups and 800 table rows; basic rectangular tables are often usable.

Measured defects:

1. Primary genre is incorrectly `contract` at confidence 0.90 even though the academic tag is present; `kind=unknown`.
2. A visible `目錄` and its page targets are extracted, but `has_toc=false` and the plan sets `preserve_toc=false`.
3. At least 190 running-head/page-number lines survive, such as repeated chapter titles and `x 目錄`; the profiler reports a zero repeated-header/footer ratio.
4. Typography dominates hierarchy: 424 headings are emitted (`H1=28`, `H2=65`, `H3=23`, `H4=10`, `H5=298`). Endorsements and bibliography continuation entries become headings, while wrapped TOC titles split into separate H5 nodes.
5. Notes are page-local prose instead of linked note nodes. Markers drift into sentences, note bodies merge together, and long bibliography records receive false H2 headings.
6. Complex financial/statistical tables collapse multiple physical rows into one cell. The output has 67 lines over 1,000 characters and a 2,894-character maximum line; syntactically consistent pipe counts do not make these tables semantically correct.
7. One U+FFFD character occurs in ordinary Chinese text, with no page/run quality warning or fallback.
8. Pages 1 and 463 are image-only, yet asset-enabled parsing produces zero files and byte-identical Markdown; cover/back-cover evidence is silently lost.

Baseline output SHA-256: `E2698AC3D5E3D11FB2BF95C0E1AE570FDC84AF1DA3B7BCD363CAB4E6B3CF9353`.

### 6.2 `溫州基督徒與中國草根全球化`

What works:

- Native preserves the main text, seven-chapter progression, notes, acknowledgements, references, four source links, and most Traditional Chinese without U+FFFD.
- The book completes through the final reference pages, and the asset-enabled and asset-disabled outputs are deterministic and byte-identical.

Measured defects:

1. Genre and kind are both `unknown`; genre confidence is only 0.20 despite strong book metadata, chapter, note, and bibliography evidence.
2. At least 88 running-head/page-number lines survive while the profiler again reports a zero repeated-header/footer ratio.
3. Heading semantics are unstable: 207 headings are emitted (`H1=6`, `H2=50`, `H3=1`, `H4=14`, `H5=136`). Numbered subsections become H1, and long quotations become dozens of H5 lines.
4. TeX/font mappings lose the `Th` sequence in English citations (`The -> e`, `Their -> eir`, `Theory -> eory`) and corrupt other Latin glyphs. Numeric range punctuation also disappears (`293-316 -> 293316`, for example).
5. Notes and references are flattened into long lines; markers are embedded in body prose, URLs can absorb adjacent punctuation, and 29 lines exceed 1,000 characters.
6. A two-column prose fragment is falsely emitted as the book's only four-row Markdown table. This is column interleaving, not a real table.
7. Pages 1 and 222 are image-only, but asset-enabled parsing produces zero assets and no explicit unavailable-region block.

Baseline output SHA-256: `F679CE2E1DBFE238039F0FB45DAEA22ED9536B8C49E3CBC36B8D943302BAC057`.

## 7. Resume Analysis

### 7.1 `高静-算法工程师2026630`

What works:

- Correctly identified as a four-page native-text PDF and as `resume` with confidence 0.79; there are no OCR warnings or U+FFFD characters.
- Name, target role, summary, skills, two employers, project chronology, achievements, education, and all four hyperlinks are retained. Section and employer/project ordering is broadly correct.
- Native completes in 177.5 ms and emits 12,459 bytes.

Measured defects:

1. `kind=report` conflicts with the correct resume genre. Auto routes to MinerU-VLM at only 0.20 confidence (`80` versus Native `70`) solely from `ResumeLayout`, even though the PDF has a clean native text layer.
2. High-risk identity/compensation fields contain suspicious alphanumeric substitutions. pdf-inspector reproduces the same strings, confirming defective source text mapping rather than a UParser-only rewrite; Native nevertheless emits them without a field-level quality warning or visual comparison.
3. The six numbered professional-skill items are not represented as a list. Items 1, 2, and 4 become H1, item 3/5/6 remain prose, and wrapped lines are separated from their originating item.
4. The result contains zero Markdown bullets. Work duties, project steps, and achievements are flattened into large paragraphs, reducing ATS/search precision and making individual claims impossible to address structurally.
5. Layout word breaks survive (`v eRL`, split Chinese words and framework names), while `内容` and `业绩` are not modeled as typed fields.
6. Dates are readable but not normalized or validated for overlap/future ranges. A resume parser must preserve source text while exposing structured chronology and validation warnings separately.
7. Raw Markdown contains direct PII. CLI output needs an opt-in redaction policy for logs/evaluation artifacts without altering the default faithful parse result.

Baseline output SHA-256: `DE740247A784FD2BB1E9E4CF23BD33BE874A72C82EA92C08D8B389A58EBA3E47`.

## 8. Cross-Format Root-Cause Map

| Class | Body-text status | Genre status | Dominant correctness risk | Required semantic contract |
|---|---|---|---|---|
| Academic paper | ordinary prose mostly readable | 1/2 primary genres correct | formula/font errors can change scientific meaning | sections, formulas, tables, figures, citations, links |
| Engineering standard | ordinary clauses mostly readable | 0/2 primary genres correct | clause/table corruption can change mandatory safety interpretation | clauses, mandatory flags, annex/scoring tables, TOC |
| Academic monograph | long-form prose mostly readable | 0/2 primary genres correct | note/reference/header/table loss damages scholarly traceability | chapters, TOC, notes, references, tables, figures |
| Resume | most sections and chronology readable | genre correct, kind wrong | corrupted PII/compensation and flattened lists damage ATS decisions | typed fields, lists, chronology, links, PII policy |

The observed pattern is consistent: Native text extraction is stronger than Native semantic lowering. The next architecture step should therefore improve shared evidence and IR boundaries before adding more Markdown-only cleanup.

| Failure family | Likely ownership | Required direction |
|---|---|---|
| Wrong standard genre | `uparser-core/profiler.rs` | add `technical_standard` / `construction_standard` evidence and precedence |
| Contents missed | profiler + Markdown preprocessing | detect `目次`, clause leaders, and page-number patterns, including damaged leaders |
| U+FFFD/`壳` contents page | font quality + router | page/run quality scoring and region-scoped OCR/raster fallback |
| CJK substitutions | font decoder + quality detector | compare mapping candidates; flag semantically suspicious source layers without dictionary rewriting |
| False headings | Markdown analysis/preprocessing | combine typography with clause grammar and document-zone roles |
| Flattened clause lists | line grouping + structured lowering | normative-clause IR with nested item levels |
| Mandatory provisions lost | font/style extraction + structured schema | explicit mandatory flag backed by font weight and cover announcement |
| Scoring-table collapse | table grid/format/structured output | multi-row headers, row spans, stable anchors, continuation tables, schema checks |
| Annex table fragmentation | table continuation | join same-schema tables across pages and preserve continuation labels as metadata |
| Zero assets on image/vector pages | Native/core asset boundary | expose region blocks and reuse PDFium crop/asset writer |
| Paper formula/ligature loss | font decoder + formula IR | multi-character mappings, font-scoped TeX encoding, region reconstruction |
| Monograph misclassified | profiler genre precedence | add book/monograph evidence from ISBN, publisher, chapters, notes, bibliography |
| Running heads retained | repeated-margin detector | cluster normalized text by parity, band, and chapter span; use profiler decisions in lowering |
| Notes/references flattened | layout + semantic IR | link markers to note bodies; preserve bibliography entries independently of typography |
| Column prose becomes table | table admission | require grid/anchor evidence and reject coincidental column alignment |
| Resume list becomes headings | heading/list arbitration | let enumeration grammar and section role override isolated font-size changes |
| Corrupt resume fields | text-quality + field schema | validate phone/email/salary/date candidates and trigger region visual comparison |
| PII copied into diagnostics | CLI/evaluation policy | explicit redacted diagnostic mode with faithful parse unchanged by default |

## 9. Optimization Work Packages

### S0: Freeze standard goldens

Create fixture provenance and four machine-readable goldens per standard:

- cover/genre/metadata;
- contents and page targets;
- normative clause tree with mandatory flags;
- table schemas, row counts, merged-cell spans, and continuation links.

For JGJ 59, include a page-6 image/OCR golden and exact schemas for Tables A and B.1-B.19.

### S1: Engineering-standard genre and structure

- Add standard-code patterns: `GB`, `GB/T`, `JGJ`, `CJJ`, `JG`, record/filing numbers, and year suffixes.
- Combine cover phrases such as `中华人民共和国国家标准`, `行业标准`, `公告`, `前言`, `目次`, `本标准用词说明`, and `条文说明`.
- Emit `technical_standard` with tags for national/industry/construction/safety/inspection rather than forcing bid/contract classes.
- Detect chapter, section, clause, item, and subitem grammar independently of font size.
- Populate TOC and repeated-margin profile fields from actual decisions.

### S2: Mixed native/image page fallback

- Score text quality per page and per font run, not only per document.
- Treat U+FFFD density, repeated improbable glyphs, and image dominance as fallback evidence.
- Preserve clean native pages and route only failed pages/regions to local OCR or a configured remote OCR protocol.
- If OCR/rasterization is unavailable, emit a structured warning and an image-region block rather than corrupted text without qualification.
- Merge OCR contents results with detected section/page columns and suppress leader noise.

### S3: CJK font and text quality

- Trace the selected CMap/encoding source for every suspicious CJK run.
- Prefer evidence-backed ToUnicode/embedded cmap alternatives with higher mapped-glyph coverage.
- Detect systematic glyph substitutions and repeated leader-glyph mappings.
- Do not repair Chinese words through a dictionary. Use OCR comparison only for low-confidence regions and retain provenance/confidence.
- Add punctuation, full-width symbol, Roman numeral, unit, and rare/traditional glyph negative tests.

### S4: Normative clause IR

- Introduce a structured clause node with number, title/text, level, children, page, bbox, style, and mandatory flag.
- Parse `1.0.1`, `5.1`, `1`, `1)`, and deeper variants using geometry and indentation.
- Keep cross-page clause continuations attached to the originating clause.
- Determine mandatory status from source font weight plus the cover's declared mandatory-clause list; require agreement or emit a warning.
- Render readable nested Markdown while preserving the richer structure in document JSON.

### S5: Dense scoring and annex tables

- Detect multi-row/rotated headers and vertical group labels before ordinary row clustering.
- Infer stable column anchors from headers, totals, and repeated score columns across the full table.
- Preserve row spans and merged group cells in structured output; expand repeated values only in Markdown when necessary.
- Split concatenated serial-number/item groups into physical rows using baseline clusters and score-column alignment.
- Validate every row against the table schema; reject or warn instead of silently emitting variable column counts.
- Join `续表` pages by caption id and schema fingerprint.
- Keep `检查项目合计`, `检查人`, and `检查日期` as footer rows belonging to the same table.

### S6: Asset and region preservation

- **DONE for Image XObjects:** emit ordered image blocks with MediaBox-based page coordinates.
- **DONE for Image XObjects:** with PDFium enabled, render only claimed pages and crop regions through the existing content-addressed asset writer.
- **DONE:** honor `--no-assets` by avoiding rasterization and filesystem writes.
- **DONE:** when PDFium is unavailable, retain image geometry and emit explicit `asset_unavailable` warning/capability evidence.
- **DONE:** explicitly scanned image-backed pages become full-page assets; true blank pages are excluded, and real book cover/back-cover outputs are verified.
- **DONE for rectangle/bar charts:** reuse the table-safe vector-chart detector and materialize accepted regions as `VectorChart` assets.
- **DONE for painted-path figures:** collect top-level and nested-Form curved, diagonal, and closed axis-aligned painted paths, including painted `re` rectangles; cluster them with sweep-line connected components; reject page frames, prose panels, outlined-glyph swarms, uniform grids, stable financial value columns, and pure-axis multi-row text schedules; materialize accepted regions through PDFium.
- **DONE:** bind caption blocks to figure assets through optional `asset_caption` IR, preserving the original caption block in reading order. The matcher supports wrapped captions and one caption shared by horizontally aligned sibling assets, while rejecting table/source/note captions and distant geometry.

### A0: Shared evidence and quality map

- Produce page-, region-, line-, font-run-, and field-level evidence with page/bbox/font/CMap provenance.
- Compute independent confidence for text mapping, reading order, block role, table structure, and asset availability. Do not collapse them into one document score.
- Make warnings first-class output attached to the affected region. Auto routing may choose Native globally while escalating only failed pages/regions.
- Keep source text immutable; repairs are alternate candidates with method, confidence, and trace.

### A1: Shared semantic document IR

- Add typed nodes for title, heading, paragraph, list/list-item, formula, figure, table, clause, note, reference, resume field, chronology item, and page chrome.
- Preserve page, bbox, reading-order edges, style, source spans, confidence, and parent/child relationships.
- Run heading-versus-list, table-versus-columns, header-versus-body, and note-versus-prose arbitration before Markdown rendering.
- Render Markdown as a lossy view; use document JSON for merged cells, mandatory flags, note links, structured resume fields, and repair provenance.

### B0: Academic paper completion

- Execute the detailed P0-P5 plan in `PDF_ACADEMIC_NATIVE_REPAIR_EXECUTION_PLAN.md`: ToUnicode/ligatures, formula IR, alternating headers, vector figures, table termination, URL continuation, and genre guards.
- Share font-run quality, note/reference IR, page-chrome filtering, and asset materialization with monographs rather than adding paper-only forks.

### M0: Monograph structure, notes, and references

- Detect `book`/`academic_monograph` using title verso, ISBN/DOI, publisher, chapter progression, notes, bibliography, index, and front/back matter.
- Build TOC entries by joining wrapped titles and retaining printed page labels; distinguish contents occurrences inside body prose.
- Link superscript/numeric markers to page notes or endnotes and keep each bibliography record as one typed entry across line/page breaks.
- Filter running heads using parity-aware normalized clusters so alternating book title/chapter title patterns are supported.
- Add Traditional Chinese, Japanese, Latin-ligature, punctuation, and numeric-range goldens.

### M1: Monograph tables and illustrations

- Require explicit rule/grid/repeated-anchor evidence before converting columns to a table.
- Reconstruct financial tables from physical row baselines, accounting labels, repeated year/currency columns, totals, and continuation captions.
- Preserve scanned covers, plates, photos, diagrams, and vector figures as ordered figure/asset nodes with captions and page references.

### R0: Resume field and chronology IR

- Model contact/target/summary/skills/employment/project/education/achievement as typed fields while preserving the source layout.
- Parse repeated numbered skills and duties as lists; do not promote enumerated items to document headings without title grammar.
- Normalize dates into optional machine fields, retain verbatim values, and warn on overlaps, reversed ranges, or future dates without rewriting them.
- Validate high-risk phone/email/salary/URL candidates. Suspicious mappings trigger region raster/OCR comparison or an explicit warning.
- Add an opt-in `--redact-pii` diagnostic/export layer covering phone, email, address, identity numbers, and compensation; faithful parsing remains the default.

## 10. Per-Format Acceptance Gates

| Gate | GB 50870-2013 | JGJ 59-2011 |
|---|---:|---:|
| Correct genre | technical/construction standard | technical/construction inspection standard |
| TOC detected | yes | yes |
| U+FFFD output | 0 | 0 |
| Corrupt `壳` leader glyphs | 0 | 0 |
| Clause identifiers retained | 100% golden | 100% golden |
| Mandatory clauses | `5.2.1`, `7.2.2` flagged | `4.0.1`, `5.0.3` flagged |
| Simple table exact match | Table 3.0.2 | summary/grade tables |
| Complex table schema | Appendix A golden | A and B.1-B.19 goldens |
| Scoring table captions | N/A | 19/19 |
| Scoring table row/cell structure | N/A | exact golden, no collapsed mega rows |
| Image-dominant failed pages | warning/asset or OCR | page 6 clean OCR or explicit asset/warning |
| Output determinism | zero hash drift | zero hash drift |

### Academic papers

- Correct academic genre and section order on both fixtures.
- Exact golden text for ligatures and multi-character ToUnicode mappings; zero unexplained U+FFFD.
- Formula regions match the agreed LaTeX/text golden, with no body/header contamination.
- Repeated headers/folios are absent by default; vector figures and captions produce ordered assets.

### Traditional Chinese monographs

- Both fixtures classify as `book` or `academic_monograph`; visible contents are represented as structured TOC entries.
- Running-head/page-number false positives are zero against page-level goldens.
- Note marker-to-body links and bibliography entry boundaries reach 100% on sampled chapter/reference pages.
- `The/Their/Theory`, numeric ranges, Traditional Chinese, Japanese names, and punctuation match font/visual goldens; U+FFFD is zero.
- Real table schemas match selected financial goldens; the false Wenzhou prose table is rejected.
- Image-only cover/back-cover pages produce assets or explicit `asset_unavailable` regions.

### Resume

- Genre and kind both resolve to resume; clean native resumes prefer Native unless visual quality evidence justifies escalation.
- Name/contact/target/skills/employment/projects/education/links achieve 100% field presence on the golden.
- Numbered skills/duties are lists with stable item counts; no list item is H1-H3.
- Phone, email, salary, and dates either match the visual golden or carry a field-level warning; no silent alphanumeric corruption.
- Reading order is correct per page/column; chronology retains source order and exposes normalized dates separately.
- Redacted diagnostic mode leaks none of the configured PII classes.

Global release guards remain:

- all Native tests pass;
- Native line coverage remains at least 90%;
- 200-document Overall/NID/TEDS/MHS do not decrease;
- every changed public prediction is manually classified;
- five-round median CV is at most 3%;
- median Native PDF performance regresses by no more than 5%;
- no new full-page OCR for clean native-text pages.
- quality gates are reported both globally and by paper/standard/book/resume strata so a large easy class cannot hide a severe specialist regression;
- every fallback has page/region evidence and a bounded raster/OCR budget;
- raw fixtures and resume outputs remain local and excluded from public artifacts unless explicitly sanitized.

## 11. Priority and Execution Order

1. **PARTIAL P0:** seven local outputs and code-level regression fixtures are frozen; page/region semantic and sanitized visual goldens remain.
2. **PARTIAL P0:** shared warnings and document-level evidence are implemented; general region-level quality/provenance maps remain.
3. **PARTIAL P0:** JGJ page 6 now has bounded PDFium/Tesseract fallback; TOC column reconstruction/confidence and suspicious resume-field visual comparison remain.
4. **DONE P0:** resume heading-versus-list and Wenzhou table-versus-column arbitration are fixed and real-sample verified.
5. **PARTIAL P0:** JGJ B.1-B.19 physical item rows are restored and real-sample verified; explicit span/cross-page schema IR and selected monograph financial-table reconstruction remain.
6. **PARTIAL P1:** genre precedence, TOC, list semantics, academic exact-header filtering, long-book recurrence filtering, and technical-standard clause/mandatory/annex block roles are implemented; hierarchy links, chapter semantic IR, and parity metadata remain.
7. **PARTIAL P1:** source-backed CM OT1 `ff/fi/fl/ffi/ffl` recovery is implemented and real-sample verified; EC/T1 high-slot vectors, arbitrary embedded-font glyph-name recovery, and numeric-range repair remain.
8. **PARTIAL P1:** equation-shaped false tables and formula headings degrade to readable plain text; source-backed tagged-PDF roles remain preserved. Untagged high-confidence display lines now emit `Formula`/`equation` blocks with token geometry, confidence, and no fabricated LaTeX; cross-line regions, fraction/matrix grouping, normalized structure, untagged notes/references, clause hierarchy/style provenance, and resume-field IR remain.
9. **DONE P1:** embedded/scanned image regions plus rectangle/bar and general painted-path vector regions materialize through the existing asset pipeline with exact page geometry; no-PDFium builds emit `asset_unavailable`, and image/chart blocks carry backward-compatible caption relations when the same-page semantic and geometric evidence is unambiguous.
10. **PARTIAL P2:** resume PII-redacted CLI output is implemented; typed chronology normalization/validation remains.
11. **PARTIAL GATES:** 891 Native tests, 398 Core feature tests, 47 CLI tests, 2 contract tests, 10 native-document tests, and the mandatory real parses pass; `1805` repeat output hashes are identical; stratified 200-document quality, coverage, repeated performance, and competitor G-C gates remain.

## 12. Current Conclusion

The bounded implementation materially improves all four tested strata: all eight documents now resolve to the expected genre/kind compatibility model, both standards and the modern-business book expose TOCs, standard clauses and declared mandatory provisions have typed block roles, clean resumes route Native and retain six numbered skills, academic running headers and wrapped arXiv links are repaired, long-book repeated chrome is sharply reduced, and false tables in both the Wenzhou book and FreeToken equation block are eliminated. On `1805.06610v1`, the known corrupted CM ligature forms fell from at least 37 to 0 and its vector state diagram is preserved. On `2608.16157v1`, glued academic headings fell from 11 to 0, equation headings from 1 to 0, the two real tables remained present, and all five figures now materialize as visually verified vector assets. Cross-format negatives keep both standards, the resume, and the Wenzhou book at zero new vector-chart regions, while the modern-business financial statement is rejected as a chart.

The execution plan is nevertheless **not fully complete**. JGJ's page-6 replacement is operational but not yet character/structure-golden, and its now-separated scoring rows still need explicit span/cross-page schema IR. Standard clauses still need hierarchy/style provenance; formula lines now have typed token geometry but cross-line fractions/matrices and normalized semantics remain, while notes/references, typed resume fields/chronology, and semantic reconstruction of internal figure panels/chart data still require richer IR. Embedded, scanned, and painted-path figure regions are preserved and explicitly linked to captions, but that relationship does not by itself reconstruct the figure's internal semantics.

Architecture V2 therefore has stronger sample accuracy and performance than the previous Release on these eight documents, but it is **not proven to exceed LiteParse, Anydoc, and pdf-inspector overall**. The frozen competitor dominance report remains `INSUFFICIENT`; no claim changes until common quality goldens, repeated performance runs, coverage, and G-C all pass. JGJ 59's remaining safety-impact work is explicit normative/table IR and the page-6 structured OCR golden.

## 13. v0.4.0-rc.1 Release Readiness (2026-08-23)

The Architecture V2 source and product crates now identify as `0.4.0-rc.1`.
The release layer includes a reproducible Windows packager, version/tag
consistency validation, versioned EXE/PDFium/ZIP assets, SHA-256 checksums, and
a tag-triggered GitHub Release workflow. Skill downloads now use versioned
caches so a pin change cannot silently reuse an older executable; Windows also
retrieves the matching PDFium DLL when the release publishes it.

This is a **release candidate, not a stable release**. The code regression
matrix and eight-document audit are suitable for controlled evaluation, but
the global guards listed above remain blocking: public redistribution licensing
is unresolved (`UNLICENSED`), the Windows binary is unsigned and rejected by
the current machine's Application Control policy, the stratified 200-document
quality/coverage/repeated-performance runs are incomplete, and competitor G-C
is still `FAIL`/`INSUFFICIENT`. A stable tag and any overall superiority claim
remain prohibited until those gates pass.
