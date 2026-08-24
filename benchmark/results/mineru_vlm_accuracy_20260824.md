# MinerU-VLM accuracy benchmark

Model: `MinerU2.5-Pro-2605-1.2B`

uparser: `0.4.0-rc.1`, binary SHA-256 `b5daaa07f2c04eb7210f38809a99dee2fbdd3fd46456a7296e84eb65d0764b35`

The current binary passed a 2-page live endpoint smoke test with 2/2 non-empty outputs. The full OmniDocBench predictions are the 1,651-page `architecture-v2-20260821` V2 run; they were re-evaluated with the current official evaluator on 2026-08-24.

## OmniDocBench

Edit-distance metrics are lower-is-better; TEDS is higher-is-better.

| Metric | Score |
|---|---:|
| Text edit distance | 0.069960 |
| Formula edit distance | 0.102585 |
| Table TEDS | 0.906068 |
| Table structure-only TEDS | 0.937463 |
| Table edit distance | 0.068035 |
| Reading-order edit distance | 0.136131 |

Text accuracy distribution:

| Data source | Edit distance |
|---|---:|
| Research report | 0.008168 |
| PPT2PDF | 0.029548 |
| Book | 0.044312 |
| Historical document | 0.065483 |
| Magazine | 0.068849 |
| Exam paper | 0.071212 |
| Academic literature | 0.072666 |
| Colorful textbook | 0.073581 |
| Note | 0.097406 |
| Newspaper | 0.194047 |

The weakest layout buckets are `other_layout` (0.131696) and `three_column` (0.124508). The official run evaluated all 1,651 pages and 665 tables. One page used the page-timeout fallback; TEDS had zero timeout and zero error cases.

## OpenDataLoader Bench

| Metric | MinerU-VLM V2 | Native no OCR | Delta |
|---|---:|---:|---:|
| Overall | 0.923978 | 0.875818 | +0.048160 |
| NID | 0.943310 | 0.918557 | +0.024753 |
| TEDS | 0.968228 | 0.838883 | +0.129345 |
| MHS | 0.867214 | 0.782710 | +0.084504 |

MinerU-VLM improves every aggregate OpenDataLoader metric relative to native no-OCR, with the largest gain on table structure. Its main OmniDocBench weaknesses are newspaper pages, irregular layouts, three-column layouts, and reading order.

## V2 versus previous versions

V2 is not an across-the-board accuracy improvement.

Against the previous `uparser-mineru-vlm` OpenDataLoader run, V2 changes Overall by `-0.004390`, NID by
`-0.003700`, TEDS by `+0.024334`, and MHS by `-0.010514`. Table quality improves, while aggregate,
reading-order, and heading quality regress slightly.

Against the historical OmniDocBench best `mineru-vlm-2605-surpass-e1-full`, V2 text/formula/reading-order
Edit distance regress by `+0.033229`/`+0.007745`/`+0.007677`; Table TEDS is effectively flat at
`-0.000434`. The correct release conclusion is that V2 improves architecture completeness and stability,
but does not improve overall MinerU-VLM accuracy.
