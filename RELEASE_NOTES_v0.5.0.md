# UParser v0.5.0

Observability, protocol-trust labelling, CI hardening, and a new
`navidc-ocr` deployment path on top of `v0.4.0`.

## Highlights

- **Per-phase timing in `ParseResult.timing`** (previously always empty):
  `analyze_ms`/`plan_ms`/`cache_lookup_ms`/`ingest_ms`/`model_ms`/
  `postprocess_ms`/`assets_ms`/`cache_write_ms`/`total_ms`. A phase that did
  not run has no key (a cache hit has no `model_ms`), and `total_ms` is
  measured, not summed. New `--stats` prints a one-line summary (including
  render time) to stderr.
- **Protocol validation tiers.** `uparser protocols` now reports each
  protocol's `validation` (`verified-live` / `offline-only` /
  `speculative-contract` / `test-double`) and `last_verified` evidence.
  Parsing with a protocol below `verified-live` adds a caveat to
  `capability_notes`.
- **Blank pages are a first-class signal.** Pages with no content are
  reported in `warnings` and on stderr; `--fail-on-blank-pages <N>` exits 3
  when the count exceeds the budget.
- **`--table-format auto|gfm|source-html`.** `source-html` passes a model's
  own HTML table through verbatim (keeps in-cell `<br>` and spans). `auto`
  still renders exactly as before; the default is not flipped until
  re-measured.
- **`uparser doctor all`** probes every protocol that has a configured
  endpoint (config.toml / `UPARSER_ENDPOINT`), skipping unconfigured ones
  instead of probing shared built-in defaults.
- **`navidc-ocr`** deployment script (`deploy_navidc_ocr.sh`).
- `cargo clippy --workspace --all-targets -- -D warnings` now passes. CI
  runs a `default` / `native` / `native,pdfium` feature matrix.
- The `uparser` skill resolves the latest published release dynamically;
  its version is no longer pinned.

## Windows assets

- `uparser-v0.5.0-windows-x86_64.exe`
- `uparser-v0.5.0-windows-x86_64-pdfium.dll`
- `uparser-v0.5.0-windows-x86_64.zip` (contains `uparser.exe`, `pdfium.dll`,
  release notes, a source/build manifest, and third-party licenses/attribution)

Checksums for every asset are published as `SHA256SUMS`.

Keep `pdfium.dll` next to `uparser.exe` when using PDF rasterization, OCR, or
vision-model protocols. Pure native text-layer PDF parsing does not depend on
PDFium.

## Linux assets

None. `.github/workflows/uparser-v2.yml` is still failing. `v0.3.0` remains
the newest release with a `linux-x86_64` asset.

## Known release blockers (unchanged)

- The product crates remain `UNLICENSED`.
- The Windows executable is unsigned; Windows Application Control policy may
  reject it until it is signed by an allowed publisher.
- The `--table-format` A/B measurement (A.3 in
  `ARCHITECTURE_V2_REMEDIATION_PLAN.md`) is blocked on model-endpoint
  availability.
