# Native OCR distribution benchmark

Generated: `2026-08-24T17:49:46.669229+00:00`
Binary: `/home/dataset1/gaojing/llm/uparserstudio/uparser/target/release/uparser` (`b5daaa07f2c04eb7210f38809a99dee2fbdd3fd46456a7296e84eb65d0764b35`)

## opendataloader-bench

| Run | Samples | Success | Fail | Non-empty | Median s | P95 s | Max s |
|---|---:|---:|---:|---:|---:|---:|---:|
| `uparser-native-no-ocr-v2-20260824` | 200 | 200 | 0 | 200 | 0.0364 | 0.0492 | 0.0722 |
| `uparser-native-hybrid-ocr-v2-20260824` | 200 | 200 | 0 | 200 | 0.0361 | 0.0471 | 2.5663 |

Official quality summaries:

- `uparser-native-no-ocr-v2-20260824`: `{"overall_mean": 0.8758177972145255, "nid_mean": 0.918556872888086, "nid_s_mean": 0.9054621526070734, "teds_mean": 0.8388831697783845, "teds_s_mean": 0.8732142828097786, "mhs_mean": 0.7827095632548434, "mhs_s_mean": 0.8559046719574482}`
- `uparser-native-hybrid-ocr-v2-20260824`: `{"overall_mean": 0.8686887613258026, "nid_mean": 0.913944545000013, "nid_s_mean": 0.9008498247190007, "teds_mean": 0.8388831697783845, "teds_s_mean": 0.8732142828097786, "mhs_mean": 0.7646801354242401, "mhs_s_mean": 0.8372130831724015}`

Changed outputs between no-OCR and hybrid runs: 2
- `01030000000018`: 1615 -> 1124 bytes
- `01030000000113`: 3153 -> 1488 bytes

## OmniDocBench

OmniDocBench inputs are page images. The no-OCR native run measures the explicit format/reachability boundary; the OCR run is the in-process tesseract protocol, not native PDF hybrid fallback.

| Run | Samples | Success | Fail | Non-empty | Median s | P95 s | Max s |
|---|---:|---:|---:|---:|---:|---:|---:|
| `uparser-native-no-ocr-v2-20260824` | 1651 | 0 | 1651 | 0 | 0.0051 | 0.0084 | 0.0387 |
| `uparser-tesseract-local-v2-20260824` | 1651 | 1651 | 0 | 1651 | 1.8591 | 12.5108 | 236.1460 |

### `uparser-tesseract-local-v2-20260824` timing by data source

| Data source | Samples | Median s | P95 s | Max s |
|---|---:|---:|---:|---:|
| PPT2PDF | 253 | 0.6547 | 2.1661 | 6.6541 |
| academic_literature | 215 | 2.2113 | 4.3892 | 9.1388 |
| book | 276 | 1.6100 | 3.8889 | 8.4341 |
| colorful_textbook | 159 | 1.7167 | 8.4551 | 14.4129 |
| exam_paper | 193 | 2.3792 | 6.7146 | 21.8935 |
| historical_document | 5 | 2.3129 | 5.7413 | 5.7413 |
| magazine | 149 | 2.0964 | 6.6182 | 7.4526 |
| newspaper | 151 | 13.2216 | 56.5401 | 236.1460 |
| note | 118 | 0.7071 | 1.5872 | 2.5844 |
| research_report | 132 | 3.1432 | 6.3517 | 8.2104 |

Official quality summaries:

- `uparser-native-no-ocr-v2-20260824`: `{"text_edit": 1, "formula_edit": 1, "table_teds": 0, "table_teds_structure": 0, "reading_order_edit": 1}`

Official evaluation status:

- `uparser-native-no-ocr-v2-20260824`: completed official result loaded from disk
- `uparser-tesseract-local-v2-20260824`: official evaluation stopped after about 31 minutes at 1058/1651 pages because repeated 300/420 second matcher fallbacks made completion impractical; parser predictions are complete

Official OCR evaluation attempt: 1058/1651 pages in about 31 minutes; not completed (pathological quick-match timeouts on dense newspaper and textbook OCR output).
