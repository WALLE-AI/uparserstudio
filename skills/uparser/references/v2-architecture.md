# Architecture V2 execution reference

Read this only when debugging or extending uparser behavior. For ordinary document parsing, follow `SKILL.md`.

## Stable invariants

1. `PreflightSource` owns authoritative content/signature-first format detection and the source digest.
2. `runner::analyze` produces a serializable `DocumentProfile` plus non-serialized reusable `AnalysisArtifacts`.
3. Auto alone may run conditional L3 semantic enrichment. Explicit selection still performs format detection, required L1/L2 analysis, reachability, and preprocessing planning.
4. `RouteDecision` contains the selected protocol, origin, score evidence, confidence, and rejected candidates.
5. `PreprocessPlan` is resolved after routing and records input channel, conversion, raster DPI, hints, and reused artifacts.
6. Materialization occurs only for the selected path: source semantics, PDF text artifact, or visual pages.
7. Every successful common-IR result carries `document_profile`, `route_decision`, and `preprocess_plan`.

## Execution flow

```text
PreflightSource
  -> analyze_with_cancellation
       PDF + native feature -> PdfProcessResult + L2 profile
       structured input     -> CanonicalDocument + structural profile
       image/other          -> L1 profile
  -> optional L3 enrichment (auto only)
  -> explicit_route OR route_with_preference
  -> preprocess_plan
  -> PreparedRun { source, analysis, plan }
  -> execute_with_hooks
       native   -> consume reusable artifact directly
       protocol -> validate ProtocolSpec shape, materialize PageSource,
                   schedule windows with one concurrency budget
       pipeline -> validate StageGraph before page materialization
  -> postprocess -> assets -> metadata -> result/cache
```

Native intentionally returns before the model-protocol cache/registry/scheduler path. This is an execution specialization inside the unified runner contract, not a second CLI/API behavior contract.

## Family boundary

Classify by who performs fusion:

- Model protocol: one endpoint or adapter returns a complete page result, even if the service internally uses multiple models.
- Pipeline: core invokes and combines multiple typed stage outputs itself.

Do not place client-side layout/crop/OCR/merge orchestration into a model protocol merely because all endpoints belong to one vendor ecosystem.

## Artifact reuse

- Native PDF execution consumes the exact `PdfProcessResult` created during analysis. It must not call the PDF parser a second time.
- Native structured execution consumes the analyzed `CanonicalDocument`; a controlled reparse is allowed only when execution-specific document options differ from analysis defaults.
- Visual paths may reuse profile/source artifacts for planning, but lossy conversion and rasterization are deferred until after routing.

When diagnosing a suspected duplicate parse, verify these artifacts and the selected `PreprocessPlan.reused_artifacts` before adding a bypass.

## Protocol declaration boundary

`ProtocolSpec` owns the declarative mode, shape, transport, preprocessing, decoder, coordinates, ordering, default endpoint, and feature requirement. Adding a protocol that fits an existing shape should primarily add a spec and adapter-specific codec behavior. Avoid adding protocol-name branches to router, scheduler, cache, or renderer unless the behavior truly changes a shared contract.

## Cancellation and partial results

One cancellation token spans analysis, optional L3, Office conversion, page materialization, scheduling, and in-flight HTTP work. Model execution isolates page failures and can return exit `3` with usable pages plus `page_errors`. Native whole-document failures are not scheduler partials.

## Renderer and asset boundaries

- Engine Markdown is the native fidelity default.
- Canonical Markdown is an explicit shared-IR comparison path.
- Asset discovery belongs to parsers/adapters; bounded materialization and filesystem writing belong to the runner/assets layer.
- `--no-assets` must prevent both writes and avoidable PDF rasterization.
- Output-only PII redaction occurs after parsing/rendering and must not alter cached or source-faithful results.
