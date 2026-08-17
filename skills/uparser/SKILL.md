---
name: uparser
description: Parse documents into structured Markdown or JSON with the `uparser` CLI — a Rust tool built specifically for coding agents to invoke as a subprocess. Handles PDF, Word (.docx/.doc), PowerPoint (.pptx/.ppt), Excel (.xlsx/.xls/.xlsm/.xlsb), OpenDocument (.odt/.ods/.odp), EPUB, RTF, CSV/TSV and images — every non-PDF format parses fully offline, with no LibreOffice, no model and no network. Use this skill WHENEVER the user wants to extract text, tables, formulas, reading order, slides, spreadsheet cells or images from any of those; convert a document to Markdown; OCR or VLM-parse a scanned document; classify a document before parsing; or feed document content into a RAG/LLM pipeline — even if they don't say "uparser" by name. Also use it when choosing between a fast local parse and a high-accuracy vision-model parse, or when a previous document-parsing attempt gave garbled/empty output.
---

# uparser

`uparser` is a unified document-parsing CLI (Rust). It turns a document into a clean **Markdown** or a structured **JSON** intermediate representation (blocks with bounding boxes, categories, tables, formulas, reading order). It is designed to be driven by coding agents as a subprocess: **stdout = the result, stderr = logs, and the exit code is semantic** (see Output contract).

Pick the parsing engine with `--protocol`. The two you'll use most:

- **`native`** — pure-Rust, **zero model, no GPU, no network**. Milliseconds per document. Two engines share this one name: a PDF text-layer engine for **born-digital PDFs**, and a **structured-document engine** that reads Word/PowerPoint/Excel/OpenDocument/EPUB/RTF/CSV *from their own source structure* (see [Non-PDF documents](#non-pdf-documents-office--opendocument--epub--rtf--csv)). Cannot read scanned/image-only pages (no OCR).
- **`mineru-vlm`** — highest accuracy (best reading order + tables). Requires an **OpenAI-compatible vLLM endpoint** serving a MinerU2.5 vision model. Use for **scanned documents, complex layouts, or when table/figure fidelity matters**.

> **`--protocol` defaults to `auto`** (a Profiler+Router picks the engine: any non-PDF document format → `native` offline, always; born-digital PDF → `native`; scan/complex PDF → a VLM). So `uparser parse doc.pdf` or `uparser parse deck.pptx` with no flags does the right thing. `mock` (placeholder output, *not* your document) is now **explicit-only** — request it deliberately with `--protocol mock`. Note: for `auto` to reach the offline `native` path the binary must be the `native`-enabled build (the shipped prebuilt is); if `auto` routes to a VLM it still needs an endpoint (it prints a clear stderr hint when none is configured).

## Agent helper scripts (one call, correct defaults)

If you'd rather not assemble flags yourself, the skill ships two wrappers that
make the right decisions for you. Both auto-resolve the binary (download/build
on first use) and **never fall into the `mock` trap**.

```bash
# Smart parse: file in → Markdown out. Picks `native` (offline) when no VLM
# endpoint is known, or `auto` (endpoint injected) when one is. Exit code is
# the binary's own (0/1/2/3/4). Extra flags pass through and win.
scripts/uparser-parse.sh report.pdf                 # → Markdown on stdout
scripts/uparser-parse.sh scan.pdf --format json     # override anything
UPARSER_ENDPOINT=http://host:port/v1/chat/completions scripts/uparser-parse.sh scan.pdf

# Preflight: is the binary usable, which protocols exist, is my endpoint up?
# Prints one compact JSON line to stdout (exit 0 usable / 2 not) — branch on it.
scripts/uparser-check.sh
scripts/uparser-check.sh --protocol mineru-vlm --endpoint http://host:port/v1/chat/completions
# → {"binary":"...","ok":true,"protocols":[...],"endpoint":"...","endpoint_reachable":true}
```

Windows equivalents: `scripts\uparser-parse.ps1`, `scripts\uparser-check.ps1`.
Endpoint/model for `uparser-parse` are resolved from `--endpoint`/`--model`,
then `$UPARSER_ENDPOINT`/`$UPARSER_MODEL`, then the config file (below).

Batch a folder (Markdown per file) with a plain loop — mixed formats are fine, each one
routes itself:
```bash
for f in docs/*; do scripts/uparser-parse.sh "$f" > "${f%.*}.md"; done
```

Prefer the raw `uparser` binary directly when you want full control — the
sections below document it. The helpers are just a convenience layer on top.

## Getting the binary (auto-downloaded — usually nothing to do)

You only need to install this skill. The binary is fetched on first use:
`scripts/ensure_uparser.sh` (Linux/WSL/macOS) / `scripts/ensure_uparser.ps1`
(Windows) resolves `uparser` in this order and prints its path:

1. `uparser` already on PATH → use it;
2. a previously downloaded copy in `~/.cache/uparser/bin/` → reuse it;
3. otherwise **download the version-pinned prebuilt from GitHub Releases**
   (`WALLE-AI/uparserstudio`, currently `v0.3.0` on Linux x86_64 and `v0.2.0`
   on Windows — the pins are per-platform, each tracking the newest release
   that actually published an asset for it), trying the
   direct URL then the `ghfast.top` mirror, verifying `SHA256SUMS`, and
   smoke-testing it;
4. if no prebuilt fits the platform (non-x86_64, glibc < 2.35, Windows with no
   published `.exe`), it falls back to building from source.

The config-driven wrappers (`scripts/uparser-run.sh` / `.ps1`) call this
automatically, so `uparser-run.sh parse ...` just works on a fresh machine.
Env overrides: `UPARSER_VERSION`, `UPARSER_REPO`, `UPARSER_HOME` (cache root).

**Building from source instead** (e.g. to add `pdfium` for the VLM/OCR
protocols — the prebuilt already includes it):
```bash
cargo build --release --features native,pdfium   # from the uparser/ workspace
```
`scripts/find_uparser.sh` locates or builds it and prints the path.

## Quick start

```bash
# Fast, local, born-digital PDF → Markdown:
uparser parse --protocol native --format markdown report.pdf > report.md

# Highest quality via a vision model (needs a vLLM endpoint):
uparser parse --protocol mineru-vlm \
  --endpoint http://127.0.0.1:19122/v1/chat/completions \
  --model MinerU2.5-2604-1.2B \
  --format markdown scan.pdf > scan.md

# Let uparser choose the engine automatically:
uparser parse --protocol auto --format markdown mystery.pdf > out.md

# Any Office/OpenDocument/EPUB/RTF/CSV file — offline, no flags needed:
uparser parse --format markdown deck.pptx > deck.md
uparser parse --format document-json report.docx > report.json   # lossless structure
```

## Choosing a protocol

Decide with this table. When unsure, use `--protocol auto` (the default), or run `uparser classify <file>` first — but note `classify` profiles **PDFs**; on a non-PDF document it reports `unknown` and tells you nothing, because those never need routing in the first place.

| Situation | Use | Why |
|---|---|---|
| Born-digital PDF, need speed, no GPU | `native` | ms/page, zero deps; text-layer extraction |
| Scanned / image-only PDF | `mineru-vlm` (or another VLM) | `native` has no OCR → empty output on scans |
| Complex tables / figures matter | `mineru-vlm` | best table (OTSL→HTML) + reading order |
| Don't know the document type | `auto` (the default) or `classify` first | non-PDF formats → `native` always; born-digital PDF → `native`; else → VLM |
| Office / OpenDocument / EPUB / RTF / CSV input | `native` (the `auto` default already picks it) | parsed in-process from the source structure — **no LibreOffice, no model, no network**. See the next section |
| Image input (png/jpg) | a VLM protocol | there is no text layer to read |

Other protocols: `dots-ocr`, `monkeyocr-v2` (single/two-stage VLMs, need their own endpoint), `pipeline` (traditional layout→OCR→formula→table), `paddleocr`. Run `uparser protocols` to introspect every adapter's capabilities as JSON. See `references/protocols.md` for details on all of them.

## Non-PDF documents (Office / OpenDocument / EPUB / RTF / CSV)

These do **not** go through PDF conversion, rasterization or a model. `native` reads
each format's own structure in-process, so a `.docx` heading is a heading because the
file says so — not because a layout model guessed it from pixels. **No LibreOffice, no
`soffice`, no network, no GPU.** This is also what plain `uparser parse <file>` does:
`auto` routes every format below straight to `native`.

| Input | Extensions | Read as |
|---|---|---|
| Word | `.docx`, `.doc` (legacy binary) | headings, paragraphs, lists, tables, footnotes/endnotes, images (`.doc` is text/paragraphs/tables only — see gaps) |
| PowerPoint | `.pptx`, `.ppt` (legacy binary) | one unit per slide, outline lists, speaker notes, images |
| Excel | `.xlsx`, `.xls`, `.xlsm`, `.xlsb`, `.xla`, `.xlam` | one unit per sheet, cells as a table (no rasterization at all) |
| OpenDocument | `.odt`, `.ods`, `.odp` | same shapes as their OOXML counterparts |
| EPUB | `.epub` | one unit per chapter, spine order, internal links resolved |
| RTF | `.rtf` | paragraphs, character styles, tables, images |
| Delimited text | `.csv`, `.tsv`, `.tab` | a single table |

Format detection is **signature-first, extension-second**: a `.docx` misnamed `report.pdf`
is still parsed as DOCX. The exception is delimited text — CSV/TSV have no magic bytes,
so they are recognized **by extension only**. A CSV named `data.txt` will not be
recognized (it falls through to the PDF/VLM path); rename it or pass the real extension.

### Pick the output format deliberately

| `--format` | What you get | Use it for |
|---|---|---|
| `markdown` | flattened Markdown: headings, nested lists, GFM tables, `![]()` images | reading, RAG chunks, diffing |
| `json` (default) | the **page/block IR** shared with the PDF/VLM protocols: one page per unit, blocks with `category` (`title`/`text`/`table`), tables as HTML (with `rowspan`/`colspan`), inline emphasis kept as Markdown inside `text` | uniform handling across PDF *and* Office in one pipeline |
| `document-json` | the **lossless canonical document**: `units[]` with `kind` (`page`/`slide`/`sheet`/`chapter`/`flow`) and `label`, nested list structure, table grids with explicit covered-cell slots, `notes[]`, `assets[]`, and per-format `warnings[]` | anything that needs real structure — slide-by-slide, sheet-by-sheet, footnote linkage, table geometry |

`--format document-json` is only valid for these formats; on a PDF it exits 1 with
`unsupported_output_format`.

```bash
uparser parse --format markdown       deck.pptx > deck.md
uparser parse --format document-json  book.epub > book.json   # units[].kind == "chapter"
uparser parse --format json           sheet.xlsx               # page IR, one page per sheet
```

### Flags that only apply here

- `--no-notes` — drop footnotes, endnotes and speaker notes (extracted by default).
- `--headers-footers` — include running headers/footers (excluded by default: they repeat on every page and pollute extracted text).
- `--max-input-mib <N>` — reject an oversized input *before* parsing it.

`--pages`, `--stream`, `--max-concurrency` and `--window-size` belong to the
model/scheduler path and have **no effect** on `native` — the whole document is parsed in
one pass and returned whole. Passing one prints a `warning:` line on stderr saying so, so
you never mistake "returned everything" for "selected what I asked for". To keep only some
slides/sheets/chapters, filter `units[]` (or `pages[]`) yourself from the JSON.

`uparser classify` is not useful here — it is a PDF profiler and reports
`source_format: "unknown"` for these. There is nothing to decide: they always parse
offline with `native`.

### Read the `warnings` array

Every structured parse reports what it could *not* recover, as a warning rather than
silently. Surface these to the user when they matter — e.g. a legacy `.ppt` reports that
table cells came back as separate paragraphs. In `--format document-json` they are
`{code, part, message}` objects (`UnsupportedFeature`, `AssetDropped`,
`BrokenRelationship`, `TruncatedContent`, …); in `--format json` they are strings in
`warnings`.

Known gaps worth knowing before you promise a user something:

| Format | Gap |
|---|---|
| `.doc` (legacy) | text, paragraphs and tables only — **no bold/italic, no heading levels** |
| `.ppt` (legacy) | table cells come back as separate paragraphs, not a table; EMF/WMF pictures are not decoded (bitmap ones are) |
| `.rtf` | list *types* are not parsed; every list renders as an ordered list |

### Failure modes and exit codes

| Situation | Exit | What to do |
|---|---|---|
| corrupt file, or a format nothing here handles | 1 | stop feeding this file — retrying unchanged will not help |
| password-protected / encrypted | 2 | it is not readable without the password; uparser cannot decrypt it |
| larger than the input budget, or a resource limit tripped (deeply nested records, huge spans) | 2 | raise `--max-input-mib`, or accept the file is hostile/degenerate |
| parsed with recoverable losses | 0 | check `warnings` |

### When you *would* want a VLM on an Office file

Only when the file is a wrapper around scanned images (e.g. a DOCX whose "content" is a
full-page photo). Forcing a VLM protocol converts the file to PDF first via **LibreOffice**
(`soffice`) — if it is not installed you get exit code 2 and
`required conversion tool "soffice" was not found on PATH`. For ordinary Office files
this path is strictly worse: slower, needs a GPU endpoint, and throws away structure the
source file already states.

## Output contract (important for agents)

- **stdout** carries the result only: the Markdown (`--format markdown`) or the JSON `ParseResult` (`--format json`, default). Redirect it to a file or capture it.
- **stderr** carries logs, progress, and warnings. Never parse stdout+stderr together.
- **Exit codes** are semantic — branch on them:

| Code | Meaning | Agent action |
|---|---|---|
| 0 | success | use the result |
| 1 | the request can't be served as given — bad flags/args, or a document that is corrupt or of an unsupported format | fix the command, or stop feeding this file; **retrying unchanged will not help** |
| 2 | dependency/environment condition (LibreOffice missing, endpoint unreachable, file encrypted, input over a resource budget) | fix the environment (supply a password, raise `--max-input-mib`, start the endpoint), then retry |
| 3 | partial success (some pages failed) | result is usable; inspect `page_errors` in JSON |
| 4 | internal error | report; retry with `--no-cache` |

- On `--format json`, errors are a structured object: `{"error":{"code":...,"message":...,"protocol":...,"stage":...}}`.
- `--format json` results include `pages[].blocks[]` (text/html/latex/asset_path, `category`, `bbox_px`, `reading_order`), `page_errors`, and `warnings`.

## Common recipes

```bash
# JSON IR for a RAG pipeline (blocks with bboxes/categories):
uparser parse --protocol native --format json paper.pdf > paper.json

# Only certain pages of a large document:
uparser parse --protocol mineru-vlm --endpoint <url> --model <m> --pages 1-3,7 big.pdf

# Classify first (no model call) to decide routing / cost:
uparser classify paper.pdf        # → DocumentProfile JSON (kind, dominant_content, per-page)

# Check an endpoint is reachable before a big run:
uparser doctor mineru-vlm --endpoint http://127.0.0.1:19122/v1/chat/completions

# Force a fresh parse (skip the content-hash cache):
uparser parse --protocol native --no-cache doc.pdf

# Stream incremental NDJSON for a large doc (one line per window):
uparser parse --protocol mineru-vlm --endpoint <url> --model <m> --stream huge.pdf
```

## Images in Markdown

By default, image/figure regions are cropped and written to `<source_stem>_images/` next to the source, and referenced in the Markdown as `![](images/<hash>.png)` (MinerU-style). Override the folder with `--assets-dir <dir>`, or pass `--no-assets` to skip the filesystem side effect entirely (no `![]()` links).

The same applies to images *embedded* in a structured document (DOCX/PPTX/ODF/EPUB/RTF, and bitmap pictures in legacy `.ppt`): they are written out content-addressed, keeping their original extension, and both the Markdown link and the `document-json` `assets[].path` point at the written file. A link is only emitted when the asset was actually written — you will never get an `![](asset-1f3c…)` that resolves to nothing.

## Key flags (see `parse --help` for all)

- `--protocol <native|mineru-vlm|dots-ocr|monkeyocr-v2|pipeline|paddleocr|auto|mock>`
- `--format <markdown|json|document-json>` (default `json`; `document-json` is the lossless
  structured contract for non-PDF documents — see that section)
- `--endpoint <url>` / `--model <name>` — for the VLM/OCR protocols
- `--assets-dir <dir>`, `--no-assets` — where embedded/cropped images go, or skip them
- `--no-cache`, `--no-postprocess`
- **Model/scheduler path only** (no effect on `native`, which warns if you pass them):
  `--pages <1-5,7>`, `--stream`, `--max-concurrency <N>` (default 16; 32–100 for a beefy
  endpoint), `--window-size <N>` (default 64; lower only to cap memory on huge docs)
- **Non-PDF documents only:** `--no-notes` (drop footnotes/endnotes/speaker notes),
  `--headers-footers` (include running headers/footers, excluded by default because they
  repeat on every page), `--max-input-mib <N>` (reject an oversized input before parsing it)

## Configuring endpoints (avoid retyping `--endpoint`/`--model`)

**The binary resolves `--endpoint`/`--model` itself** — no wrapper needed. For
`parse` and `doctor`, when a flag is omitted it falls back, in order:

1. the explicit `--endpoint` / `--model` flag (always wins);
2. the `UPARSER_ENDPOINT` / `UPARSER_MODEL` environment variables;
3. `~/.config/uparser/config.toml` (override the path with `UPARSER_CONFIG`),
   the `[<protocol>]` section — keyed by the *effective* protocol, so
   `--protocol auto` that routes to `mineru-vlm` picks up `[mineru-vlm]`.

```bash
# Set once, then omit the flags on every call:
export UPARSER_ENDPOINT=http://10.0.0.5:19122/v1/chat/completions
export UPARSER_MODEL=MinerU2.5-2604-1.2B
uparser parse --protocol mineru-vlm --format markdown doc.pdf   # no --endpoint/--model
```
```toml
# …or a config file (template: references/config.example.toml):
[mineru-vlm]
endpoint = "http://10.0.0.5:19122/v1/chat/completions"
model    = "MinerU2.5-2604-1.2B"
```

The `scripts/uparser-run.sh` / `.ps1` wrappers predate this and inject the same
values; they're now **optional** (useful only for an older binary that lacks
native config support). Prefer env vars or the config file with the raw binary.

## Windows

There is no prebuilt Windows binary. Two options:

- **WSL2 (simplest):** use the Linux binary/bundle unchanged inside WSL2 Ubuntu.
- **Native Windows build:** run `scripts/build-windows.ps1` (needs rustup+MSVC
  toolchain + VS C++ Build Tools). Try `-Features native` first (pure Rust, no
  PDFium download); add `pdfium` for the VLM/OCR protocols. Native Windows build
  is not yet CI-verified: `native`/`parse` should work; `doctor pipeline`'s
  memory report is Linux-only and returns null on Windows (non-fatal).

## Performance notes (measured on opendataloader-bench)

- `native` ≈ 0.05 s/doc, no GPU; strong quality on born-digital docs. Use it as the default fast path.
- `mineru-vlm` ≈ best overall quality (reading order + tables) but ~1–2 s/page and needs a GPU-backed endpoint. Repeated identical parses hit the content-hash cache (near-instant) unless `--no-cache`.

For the full protocol reference, capability matrix, and endpoint setup, read `references/protocols.md`.
