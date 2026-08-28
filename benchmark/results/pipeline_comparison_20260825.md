# Pipeline comparison (2026-08-25)

## Strict same-model A/B

The original `magic-pdf 1.3.5` pipeline and uparser Pipeline V2 use the same
DocLayout-YOLO, YOLO MFD, UniMERNet-small, PaddleOCR, SLANet-plus, and
LayoutReader weights. This comparison isolates the uparser reconstruction.

| Benchmark | Original MinerU | uparser V2 | uparser delta |
|---|---:|---:|---:|
| ODL Overall | 0.852981 | 0.800122 | -0.052859 |
| ODL NID | 0.880607 | 0.832794 | -0.047814 |
| ODL TEDS | 0.856388 | 0.836368 | -0.020020 |
| ODL MHS | 0.786227 | 0.700005 | -0.086222 |
| Omni Overall | 76.1250 | 75.7789 | -0.3460 |
| Omni Text Edit (lower is better) | 0.153716 | 0.190804 | +0.037088 |
| Omni Formula CDM | 68.6217 | 73.3099 | +4.6882 |
| Omni Table TEDS | 75.1248 | 73.1073 | -2.0175 |
| Omni Order Edit (lower is better) | 0.266186 | 0.294770 | +0.028584 |
| Omni blank pages | 2 | 60 | +58 |

Pipeline V2 is not uniformly worse: Formula CDM and structure-only table TEDS
improve. The overall regression is concentrated in text, reading order,
headings, table content assembly, and output completeness.

## MinerU 3.4.5

MinerU 3.4.5 is a separate system comparison because it requires
PP-DocLayoutV2 and current table direction, wired, and wireless models.

| Benchmark | uparser V2 | MinerU 3.4.5 | Public official reference |
|---|---:|---:|---:|
| ODL Overall | 0.800122 | 0.856821 | 0.831135 (MinerU 2.7) |
| Omni Overall | 75.7789 | 85.3391 | 86.47 |
| Omni Text Edit | 0.190804 | 0.056176 | 0.055 |
| Omni Formula CDM | 73.3099 | 79.5826 | 83.07 |
| Omni Table TEDS | 73.1073 | 82.0523 | 81.88 |
| Omni TEDS-S | 84.7475 | 88.8441 | 88.68 |
| Omni Order Edit | 0.294770 | 0.153534 | 0.153 |

MinerU 3.4.5 is `+9.5602` Omni Overall points above uparser V2 and `-1.1309`
below the public official Pipeline reference. Table metrics slightly exceed the
reference; Formula CDM (`-3.4874`) is the main remaining deficit.

## Root cause

- uparser directly joins OCR spans into Markdown instead of running MinerU's
  overlap cleanup, block repair, title merge, LayoutReader sorting, paragraph
  split, language-aware spacing, and hyphen handling.
- The table backend selects OCR spans independently for each table bbox by
  center point. Overlapping table regions can consume and emit the same spans.
- uparser produces 141,361 non-empty lines versus 30,322 for the original
  same-model pipeline, plus 60 blank pages. These output diagnostics agree with
  the formal NID/MHS/text/order regressions.

All three paths completed ODL 200/200 and OmniDocBench 1,651/1,651 without
generation failures. MinerU 3.4.5 official evaluation completed with zero page
fallbacks and zero CDM/TEDS timeouts or errors.
