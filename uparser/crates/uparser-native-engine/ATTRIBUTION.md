# Attribution

`uparser-native-engine` is vendored (internalized) from the open-source
**pdf-inspector** project by the Firecrawl team.

- Upstream: https://github.com/firecrawl/pdf-inspector
- License: MIT (see `LICENSE`, © 2026 Firecrawl) — retained verbatim.
- Vendored from commit: `3fb545284bec8bfd86cf2a1445083b8e2832625b`
  (`git describe`: `v0.7.0-145-g3fb5452`, package version `0.1.7`).
- Vendored on: 2026-08-04.

## Why vendored

uparser's `native` protocol needs a self-contained, pure-Rust,
zero-external-service PDF text-extraction engine with no PDFium binary
dependency. pdf-inspector's architecture (lopdf-based, no OCR,
classification-first, markdown-oriented) matches that need exactly. See
`../../NATIVE_ENGINE_INTERNALIZATION_DESIGN.md` (repo root) for the full
rationale and execution plan.

## Local modifications relative to upstream

- `Cargo.toml`: renamed package to `uparser-native-engine`; `[lib] name =
  uparser_native_engine`; `crate-type = ["lib"]` (dropped `cdylib`);
  removed the `[[bin]]` targets (`pdf2md`/`detect-pdf`/`dump_ops`); dropped
  the optional `pyo3`/`python` feature and dependency.
- Removed `src/bin/` (CLI binaries — uparser has its own CLI) and
  `src/python.rs` (PyO3 binding — unused; uparser calls the Rust API
  directly).
- `external/bcmaps/` retained (loaded at runtime relative to
  `CARGO_MANIFEST_DIR` by `tounicode.rs` on non-wasm builds).
- Upstream `README.md` kept as `UPSTREAM_README.md`.

Enhancements that make `native` *exceed* pdf-inspector on
`opendataloader-bench` live in the uparser side (adapter + `postprocess`/
`content_normalize`), not in this vendored core — see the design doc §4.6,
so this directory can be re-synced against upstream with minimal conflict.
