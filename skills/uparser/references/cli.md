# uparser CLI reference

Generated from `uparser --help` at version `uparser 0.4.0-rc.1`. Regenerate after a CLI change; `SKILL.md` covers the flags that matter day to day.

## `uparser`

```text
Usage: uparser <COMMAND>

Commands:
  parse      
  classify   Run the Profiler only (no protocol adapter, no full parse) and print the resulting DocumentProfile as JSON. Per ARCHITECTURE.md §13.5's Agent-first philosophy: an Agent can inspect the routing decision before committing to a full (expensive) parse
  plan       Detect, analyze and route without executing the selected parser
  cache      Content-hash cache management (T-9.1)
  doctor     Per-protocol health check (T-9.3): for HTTP-backed protocols, probes the given/default endpoint's reachability; for `pipeline`, also reports local CPU/memory as a non-binding Local/Remote suggestion. Diagnostic only — never gates `parse`
  protocols  List every built-in adapter's capabilities (coordinate system, reading-order/signal support, per-stage resource hints) as JSON (T-9.4) — introspection an Agent can use before choosing `--protocol`
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

## `uparser parse`

```text
Usage: uparser parse [OPTIONS] <PATH>

Arguments:
  <PATH>
          

Options:
      --format <FORMAT>
          [default: json]
          [possible values: json, markdown, document-json]

      --output <OUTPUT>
          Write the successful aggregate result to this file instead of stdout. Errors continue to use the normal stdout/stderr contract

      --markdown-source <MARKDOWN_SOURCE>
          Which producer supplies Markdown for a native PDF. `canonical` (default) renders it from the canonical model, the same way every other source is rendered; `engine-legacy` keeps the native engine's own Markdown writer, retained for one release so a regression has somewhere to fall back to. Has no effect on structured sources or model protocols — those have one renderer either way — nor on non-Markdown output

          Possible values:
          - canonical
          - engine-legacy: The native engine's own Markdown writer. Kept for one release as a fallback, hence the name; `engine` still resolves to it so an existing invocation does not break on the rename
          
          [default: canonical]

      --mode <MODE>
          Execution family. Omit for backward-compatible auto routing or when selecting a concrete adapter through `--protocol`
          
          [possible values: auto, native, protocol, pipeline]

      --protocol <PROTOCOL>
          Protocol name (`native`, `tesseract`, `mineru-vlm`, `dots-ocr`, `generic-vlm`, `monkeyocr-v2`, `navidc-ocr`, `pipeline`, `paddleocr`, `paddlex-structure`, `mock`), or `auto` (the default) to run the Profiler+Router first and pick one automatically (per ARCHITECTURE.md §13.5). Defaulting to `auto` rather than `mock` keeps an Agent that omits `--protocol` from silently getting placeholder output — `mock` is now explicit-only

      --endpoint <ENDPOINT>
          Override the adapter's default endpoint (ignored by adapters with no endpoint, e.g. `mock`/`native`)

      --model <MODEL>
          Override the adapter's default model name (same scope as `--endpoint`)

      --window-size <WINDOW_SIZE>
          Number of pages rasterized+processed together before the window's page buffers are dropped and the next window begins (bounds peak memory to ~O(window) page images, not O(total)). Default 64 so any document up to 64 pages runs as a single barrier-free window — the inter-window barrier drains in-flight concurrency to zero, so a window smaller than the document only hurts throughput on large docs. At runtime the effective window is raised to at least `--max-concurrency` (a window smaller than the concurrency budget can never saturate it). Lower it only to cap memory on huge documents
          
          [default: 64]

      --max-concurrency <MAX_CONCURRENCY>
          Max concurrent model requests in flight across the whole document (page-level + per-block, sharing one budget). Default 16 — the empirically-measured sweet spot against a remote vLLM backend (the prior default of 4 left the endpoint badly under-fed; MinerU's own http client defaults to 100). Raise toward 32-100 for a beefier endpoint, lower for a fragile/shared one
          
          [default: 16]

      --layout-backend <LAYOUT_BACKEND>
          `pipeline`-only per-stage backend/endpoint overrides (ARCHITECTURE.md §11.2/T-5.1). Ignored by every other protocol. `layout`/`ocr`/`formula` have no `Local` implementation — passing `local` for those is a usage error
          
          [possible values: local, remote]

      --layout-endpoint <LAYOUT_ENDPOINT>
          

      --bare-layout-endpoint <BARE_LAYOUT_ENDPOINT>
          Bare PP-DocLayoutV2 tensor endpoint. When set, Rust owns image preprocessing, detection decoding, filtering and formula-region extraction; the service performs model forward only

      --formula-detection-endpoint <FORMULA_DETECTION_ENDPOINT>
          Pipeline V2 formula-detection (MFD) batch endpoint

      --ocr-backend <OCR_BACKEND>
          [possible values: local, remote]

      --ocr-endpoint <OCR_ENDPOINT>
          

      --bare-ocr-endpoint-base <BARE_OCR_ENDPOINT_BASE>
          Base URL of the bare OCR detector/recognizer tensor service

      --ocr-dictionary-path <OCR_DICTIONARY_PATH>
          PP-OCR recognition character dictionary. Required together with --bare-ocr-endpoint-base

      --formula-backend <FORMULA_BACKEND>
          [possible values: local, remote]

      --formula-endpoint <FORMULA_ENDPOINT>
          

      --bare-formula-endpoint <BARE_FORMULA_ENDPOINT>
          Bare PP-FormulaNet-plus-M tensor endpoint. Rust owns crop, preprocessing, tokenizer decoding and LaTeX repair

      --formula-tokenizer-path <FORMULA_TOKENIZER_PATH>
          PP-FormulaNet inference YAML containing the tokenizer JSON. Required together with --bare-formula-endpoint

      --table-backend <TABLE_BACKEND>
          Pipeline V2 model stages are service-only. `remote` is accepted for compatibility; `local` is rejected
          
          [possible values: local, remote]

      --table-endpoint <TABLE_ENDPOINT>
          Pipeline V2 table-recognition batch endpoint

      --bare-table-endpoint-base <BARE_TABLE_ENDPOINT_BASE>
          Base URL of the bare tensor service. Rust invokes table classifier, SLANet and UNet forwards and owns all table postprocessing

      --table-model-path <TABLE_MODEL_PATH>
          

      --pipeline-language <PIPELINE_LANGUAGE>
          OCR language forwarded to the model service (default: ch)

      --layout-mode <LAYOUT_MODE>
          `navidc-ocr` stage-1 layout mode. `detection` (default) returns axis-aligned rects; `segmentation` returns multi-point polygons for photographed/perspective-distorted pages, cropped with a polygon mask rather than a bounding rect. Keyed into the cache
          
          [possible values: detection, segmentation]

      --no-cache
          Bypass the content-hash cache (T-9.1) entirely — forces a real re-parse even if an identical `(bytes, protocol, endpoint, model)` fingerprint was cached from a prior run

      --stream
          Emit NDJSON to stdout incrementally, one line per completed processing window, instead of one aggregate JSON/Markdown document at the end (T-9.2 / ARCHITECTURE.md §2.2). Each line is `{"window_pages": [...], "window_errors": [...]}`

      --no-postprocess
          Skip `postprocess::merge_paragraphs_by_geometry` and return each adapter's raw per-block output unmerged — mainly for diffing "raw protocol output" against post-processed output when debugging a merge decision

      --pages <PAGES>
          Only parse these 1-indexed page numbers, e.g. `1-5`, `3`, or `1,5,10-12`. Applied after ingestion, before dispatching to the scheduler — lets you validate a protocol/endpoint against one page of a large document without waiting for every earlier page first. Omit to parse every page

      --assets-dir <ASSETS_DIR>
          Directory image/chart-category block crops get written to, overriding the default `<source_stem>_images/` next to the source document (mirrors MinerU's own `images/` output convention — see `image_link_gap_report.md`). Ignored if `--no-assets` is set

      --no-assets
          Skip writing image assets to disk entirely — every block's `asset_path` stays unset and `to_markdown` never emits an `![](...)` link. An explicit opt-out for callers that don't want the filesystem side effect image-asset writing introduces by default

      --raster-dpi <RASTER_DPI>
          Override the DPI used to rasterize PDF pages before handing them to a visual-page protocol (`native`/image-only inputs are unaffected). Defaults to `runner::DEFAULT_RASTER_DPI` (200, matching MinerU's own `DEFAULT_PDF_IMAGE_DPI` — see D7 in `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`); a lower value trades OCR/table/formula recognition fidelity for smaller images and less bandwidth to a remote endpoint

      --redact-pii
          Redact common email, mainland-China phone, and resident-ID values in emitted CLI output. Parsing and cached results remain faithful to the source

      --no-notes
          Drop footnotes, endnotes and speaker notes (`native` structured formats only). They are extracted by default

      --headers-footers
          Include page headers and footers in the body (`native` structured formats only). Excluded by default because they repeat on every page and pollute extracted text

      --max-input-mib <MAX_INPUT_MIB>
          Reject an input larger than this many MiB before parsing it (`native` structured formats only). Guards against a hostile or accidental oversized document; defaults to the engine's own 256 MiB budget

  -h, --help
          Print help (see a summary with '-h')
```

## `uparser classify`

```text
Run the Profiler only (no protocol adapter, no full parse) and print the resulting DocumentProfile as JSON. Per ARCHITECTURE.md §13.5's Agent-first philosophy: an Agent can inspect the routing decision before committing to a full (expensive) parse

Usage: uparser classify <PATH>

Arguments:
  <PATH>  

Options:
  -h, --help  Print help
```

## `uparser plan`

```text
Detect, analyze and route without executing the selected parser

Usage: uparser plan [OPTIONS] <PATH>

Arguments:
  <PATH>  

Options:
      --mode <MODE>          [possible values: auto, native, protocol, pipeline]
      --protocol <PROTOCOL>  
      --prefer <PREFER>      [default: quality] [possible values: quality, speed, cost]
  -h, --help                 Print help
```

## `uparser doctor`

```text
Per-protocol health check (T-9.3): for HTTP-backed protocols, probes the given/default endpoint's reachability; for `pipeline`, also reports local CPU/memory as a non-binding Local/Remote suggestion. Diagnostic only — never gates `parse`

Usage: uparser doctor [OPTIONS] <PROTOCOL>

Arguments:
  <PROTOCOL>  

Options:
      --endpoint <ENDPOINT>  
  -h, --help                 Print help
```

## `uparser protocols`

```text
List every built-in adapter's capabilities (coordinate system, reading-order/signal support, per-stage resource hints) as JSON (T-9.4) — introspection an Agent can use before choosing `--protocol`

Usage: uparser protocols

Options:
  -h, --help  Print help
```

## `uparser cache stat`

```text
Print entry count and total size on disk

Usage: uparser cache stat

Options:
  -h, --help  Print help
```

## `uparser cache clear`

```text
Delete the entire cache directory

Usage: uparser cache clear

Options:
  -h, --help  Print help
```
