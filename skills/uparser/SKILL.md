---
name: uparser
description: Parse, classify, plan, and route PDF, Office, OpenDocument, EPUB, RTF, CSV/TSV, and image inputs with the uparser Architecture V2 CLI. Use for Markdown or structured JSON extraction, OCR/VLM selection, tables, formulas, reading order, assets, endpoint diagnostics, document ingestion, and failed or garbled extraction. Trigger even when uparser is not named if the task needs reliable mixed-format document parsing; do not use for ordinary text-file reading.
---

# uparser

`uparser` is a subprocess built for coding agents. Contract:

- **stdout** = the requested result only (JSON / Markdown / NDJSON), never logs.
- **stderr** = routing, progress, warnings. Never merge it into stdout before `jq`.
- **exit code** = `0` ok · `1` bad request or unusable input · `2` environment/dependency · `3` partial (usable, check `page_errors`) · `4` internal defect.
- On failure, stdout still carries `{"error":{"code","message","protocol","stage"}}` for `--format json`.

## Start here

```bash
UP=$(scripts/ensure_uparser.sh)        # resolves AND upgrades; or: export UPARSER_BIN=/path/to/uparser
"$UP" --version                        # need >= 0.4.0-rc.1 (has `plan`, `--mode`, `--output`)
```

`ensure_uparser.sh` checks GitHub Releases at most once per 6 h (`UPARSER_VERSION_TTL`), caches the
answer, and serves the last known good version offline. It picks the newest release that actually
carries an asset for *your* platform, and it never downgrades a newer binary you already have — an
older one on `PATH` is superseded (the new copy goes to the cache; the file on `PATH` is left alone).
Pin with `UPARSER_VERSION=0.3.0`; skip the check with `UPARSER_OFFLINE=1`. `$UPARSER_BIN` always wins
and is never version-checked. `scripts/find_uparser.sh` still exists but only builds from source — it
does no version check, so prefer `ensure_uparser.sh`.

Then pick one line — do not over-plan a document you already understand:

| You know | Run |
|---|---|
| Quality matters more than latency (**the default**) | `scripts/uparser-parse.sh f.pdf` — probes your configured model endpoints, runs the best reachable one |
| Born-digital PDF, want text/Markdown fast, offline | `"$UP" parse f.pdf --mode native --format markdown` |
| DOCX/PPTX/XLSX/ODF/EPUB/RTF/CSV | `"$UP" parse f.docx --mode native --format markdown` |
| …and you need lossless structure (lists, table grids, notes) | `--mode native --format document-json` |
| Scanned PDF / PNG / JPEG, local only | `"$UP" doctor tesseract && "$UP" parse f.pdf --protocol tesseract --format markdown` |
| Scanned or complex layout, VLM endpoint available | `"$UP" parse f.pdf --mode protocol --protocol mineru-vlm --endpoint <url> --model <name> --format json` |
| Nothing — unknown file, or cost matters | `"$UP" plan f.pdf --prefer quality` first, then execute what it chose |

`--mode auto` (the default when neither `--mode` nor `--protocol` is given) runs the whole chain and picks for you. It is a ranker over observed evidence, not an oracle — when the answer must be auditable, keep `route_decision` from the result.

`auto` ranks on compiled/local capability only. It **never probes an endpoint**, its model candidate
is hardwired to `mineru-vlm` (so it can never choose `navidc-ocr`, `monkeyocr-v2` or `pipeline`), and
on a born-digital PDF it scores `native` above every model. To actually use the best *reachable*
model protocol, use `scripts/uparser-parse.sh`, or `doctor` plus an explicit `--protocol` — not `auto`.

## Recipes

```bash
# Inspect before spending money/time. Both are cheap and never call a model.
"$UP" classify f.pdf | jq '{quality:.source_quality, kind, dominant_content}'
"$UP" plan f.pdf --prefer quality | jq '{picked:.plan.route.protocol, why:.plan.route.reason,
  rejected:[.plan.route.candidates[]|select(.feasible|not)|{protocol,rejection}]}'

# Validate a remote protocol BEFORE parsing. "Feasible" in plan != endpoint running.
# No --endpoint needed once config.toml is set; it echoes the endpoint it resolved.
"$UP" doctor mineru-vlm | jq '{endpoint, reachable}'

# Machine consumption: text + geometry + category per block.
"$UP" parse f.pdf --mode native --format json --no-assets \
  | jq -r '.pages[].blocks[] | select(.text) | .text'

# Tables only (HTML strings), formulas only (LaTeX).
jq -r '.pages[].blocks[]|select(.category=="table").html' result.json
jq -r '.pages[].blocks[]|select(.category=="equation").latex' result.json

# One page of a big document, to validate an endpoint cheaply.
"$UP" parse big.pdf --pages 3 --protocol mineru-vlm --endpoint "$UPARSER_ENDPOINT"

# Long document, incremental consumption (one JSON object per completed window).
"$UP" parse big.pdf --stream --window-size 16 | while read -r line; do ...; done

# Reproducible measurement / benchmarking.
"$UP" parse f.pdf --no-cache --no-assets --output out.json

# Batch: keep going past a single bad file, and record which failed.
for f in docs/*.pdf; do
  "$UP" parse "$f" --mode native --format markdown --output "out/$(basename "$f" .pdf).md" \
    || echo "$f exit=$?" >> out/failures.txt
done
```

## Reading the result

`--format json` top level: `source_path`, `source_sha256`, `protocol`, `routed_by`, `document_profile`,
`route_decision`, `preprocess_plan`, `model_endpoint`, `model_name`, `pages`, `page_errors`,
`capability_notes`, `warnings`, `timing`.

Each block: `geom`, `geom_frame`, `bbox_px`, `category_raw`, `category`, `reading_order`, `text`,
`html` (tables), `latex` (formulas), `spans`, `merge_hint`, `confidence`, `source`, `error`, `asset_path`.

After any exit `0` or `3`, read `warnings`, `capability_notes`, and `page_errors` — surface fidelity
losses to the user instead of silently presenting degraded text as complete.
`bbox_px` is `null` for protocols that answer with a finished document rather than geometry
(`generic-vlm`, `paddlex-structure`); don't build geometry logic on those.

`--format markdown` goes through the shared canonical renderer, which emits a table as a GFM pipe
table unless it has merged cells (row/column spans keep it as HTML). If table structure matters,
read `--format json` and take each table block's `html` — that is the protocol's own output,
undegraded. The one exception is `monkeyocr-v2`, which assembles its own document (a port of
upstream `core_runner.py`'s `result2md`): its Markdown keeps tables as HTML verbatim, does not
prefix `list` blocks with `- `, and does not escape Markdown metacharacters. For that protocol the
shared paragraph-merge and CJK punctuation normalization are also skipped, so `--no-postprocess`
changes nothing.

`--format document-json` (structured native sources only, not PDF, not model routes) is the lossless
canonical view: typed table grids with row/column spans, list structure, notes, assets. It is **not** a
superset of `--format json` — that one has the geometry, provenance, and routing metadata.
Merged table cells are only recoverable from `document-json`.

## Flags that change behaviour, not just output

- Assets are written by default to `<source_stem>_images/`. `--assets-dir <dir>` relocates them;
  `--no-assets` avoids the filesystem side effect entirely (use it whenever you only want text —
  including for `--format json`, which otherwise still writes files).
- `--markdown-source canonical` is the default; `engine-legacy` is a one-release fallback for the
  native PDF engine's own writer.
- `--redact-pii` masks email / mainland-China phone / resident-ID in emitted output only. Cache and
  parsing stay source-faithful — it is not an anonymizer.
- `--pages`, `--stream`, `--window-size` (default 64), `--max-concurrency` (default 16) apply to model
  and pipeline execution. On native whole-document execution they are ignored and warn.
- `--raster-dpi` (default 200) only affects visual-page protocols.
- Model protocols cache on `(bytes, protocol, endpoint, model, options)`; native bypasses the cache.
- Structured-native only: `--no-notes`, `--headers-footers`, `--max-input-mib`.
- Protocol-specific, ignored elsewhere and part of the cache key: `--layout-mode detection|segmentation`
  (`navidc-ocr`), `--monkeyocr-retry-repeat` + `--monkeyocr-retry-repeat-max-retries <N>`
  (`monkeyocr-v2`; off by default, matching upstream — turn it on only for a page whose output
  visibly loops).

Full flag list: `references/cli.md`. Protocol catalog and service contracts: `references/protocols.md`.
Routing/artifact internals: `references/v2-architecture.md`.

## When it fails

| Exit | Do |
|---|---|
| 1 | Fix the arguments, mode/protocol conflict, or output format. If the input itself is corrupt/unsupported, stop — retrying is pointless. |
| 2 | Repair the environment named in the message (endpoint down, LibreOffice, PDFium, Tesseract language data, encrypted PDF, missing file, unwritable output), then retry once. |
| 3 | Keep the result. Inspect `page_errors`; re-run only those pages with `--pages`. |
| 4 | Report the structured error as a defect. One retry with `--no-cache` distinguishes a stale cache entry from a real bug. |

Garbled or empty text from `--mode native` means the PDF has no reliable text layer — switch to
`--protocol tesseract` (local) or a VLM protocol. Do not retry native with different flags.

Never select `--protocol mock`; it is explicit-only placeholder output.

## Binary, endpoints, config

`scripts/ensure_uparser.sh` is the entry point: `$UPARSER_BIN` → a local workspace build → `PATH` →
version-matched cache → the newest GitHub release carrying an asset for this platform (direct, then
the ghfast.top mirror; sha256-verified and smoke-tested, and on Windows it fetches `pdfium.dll` too)
→ `cargo build --release`. The three local candidates are used only when they are not *older* than
the newest published release. `scripts/find_uparser.sh [--build]` is now only the from-source rung.
`scripts/uparser-check.sh` reports readiness (including `version`, `latest`, and the
`quality_protocol` that would actually run); `scripts/uparser-parse.sh <file>` is the one-shot
wrapper. `.ps1` equivalents exist for Windows and behave identically.

Build from `uparser/`: `cargo build --release --features native,pdfium`. PDFium is needed for page
rasterization, every vision protocol, native PDF asset crops, and local OCR — pure native text
extraction is not.

Endpoint/model resolution, applied **per key**: explicit flag → `UPARSER_ENDPOINT` / `UPARSER_MODEL`
→ `~/.config/uparser/config.toml` (or `UPARSER_CONFIG`) under the effective protocol's section → that
file's `[defaults]` section → the protocol's built-in default. The same chain resolves `api_key`
(sent as `Authorization: Bearer`; `UPARSER_API_KEY`, or `api_key_env` to name a variable instead of
inlining the secret), extra headers, `timeout_secs`, `max_retries`, and `pipeline`'s per-stage
endpoints under `[pipeline.stages]` — the last of which have no env-var equivalent and are
configurable only here. The library API and the Node/Python bindings run the identical chain.

Prefer configuring once over passing flags repeatedly:

```toml
# ~/.config/uparser/config.toml
[mineru-vlm]
endpoint = "http://127.0.0.1:19122/v1/chat/completions"
model    = "MinerU2.5-Pro-2605-1.2B"
```

## Quality-first protocol selection

`scripts/uparser-parse.sh` defaults to quality. For PDF/PNG/JPEG it probes the model protocols you
actually configured, in order `mineru-vlm → navidc-ocr → monkeyocr-v2 → pipeline → dots-ocr →
paddlex-structure → generic-vlm`, and runs the first reachable one. It never probes a protocol you
have not configured (each costs ~2.5 s to time out) and never selects `mock`. Probe results cache for
5 min (`UPARSER_PROBE_TTL`), so a batch pays one probe, not one per file; "nothing reachable" caches
for only 60 s so an endpoint that comes up mid-batch is picked up.

Which formats get a model is graded by what the format actually gives up:

| Input | Routed to | Why |
|---|---|---|
| PDF, PNG, JPEG | probed model | the visual channel is the only channel |
| PPTX, PPT, ODP | probed model *(needs LibreOffice; else `native`)* | slides are absolutely-positioned text boxes with no reading-order semantics to lose — the router itself scores a presentation +35 toward a model, −35 against native, even when structured |
| DOCX, XLSX, CSV, ODT, RTF, EPUB | `native` | they carry exact structure a model could only re-infer from pixels: real cells with spans, real list nesting. XLSX/CSV never even rasterize, and `--format document-json` (the only lossless view with row/column spans) exists for these sources only |

If nothing is reachable it falls back to `native`, **not** `auto` (`auto` assumes a model endpoint
exists and would route to the dead one).

On a born-digital PDF this trades roughly **15× wall-clock for about +0.05 overall accuracy**
(`UPARSER_LEADERBOARD.md`: `mineru-vlm` 0.9252 @ 0.682 s/doc vs `native` 0.8766 @ 0.044 s/doc). That
is the intended default. `UPARSER_PREFER=speed` restores endpoint-agnostic routing;
`UPARSER_QUALITY_ORDER="navidc-ocr mineru-vlm"` overrides the order; `UPARSER_NO_PROBE=1` disables
probing entirely.

| Env | Effect |
|---|---|
| `UPARSER_BIN` | Use exactly this binary; wins over everything, never version-checked |
| `UPARSER_VERSION` | Pin the release to resolve; no network lookup |
| `UPARSER_OFFLINE=1` | Never contact the GitHub API; serve cache/pin |
| `UPARSER_VERSION_TTL` | Seconds between release checks (default 21600) |
| `UPARSER_PRERELEASE=1` | Consider prereleases when resolving latest |
| `UPARSER_PREFER=speed` | Skip endpoint probing, use `auto` |
| `UPARSER_QUALITY_ORDER` | Space-separated probe order override |
| `UPARSER_PROBE_TTL` | Seconds to cache a successful probe (default 300) |
| `GITHUB_TOKEN` | Lifts the anonymous 60 req/h API rate limit |

Then `"$UP" parse f.pdf --protocol mineru-vlm` needs no endpoint flag at all. Full template with
every protocol and key: `references/config.example.toml`. `UPARSER_OCR_LANG` overrides OCR language
selection.
