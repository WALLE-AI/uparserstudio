//! Agent-first CLI contract per ARCHITECTURE.md §6.1: stdout carries the
//! result only, all logs/progress go to stderr, and exit codes are
//! semantic so an Agent can branch on them without parsing prose.

use crate::adapters::{AdapterOverrides, PipelineConfig, Registry, StageBackendChoice};
use crate::cache;
use crate::render;
use clap::{Parser, Subcommand, ValueEnum};
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// Minimum gap between progress lines printed to stderr for a
/// non-streaming `parse` — avoids flooding stderr for a document with
/// hundreds of pages while still updating at a human-readable cadence.
/// The very last page always prints regardless of this gap.
const PROGRESS_PRINT_MIN_INTERVAL: Duration = Duration::from_millis(900);
/// Defaults for the two scheduler-tuning flags, named so the `native`
/// path can tell "the user asked for this" from "clap filled it in".
const DEFAULT_WINDOW_SIZE: usize = 64;
const DEFAULT_MAX_CONCURRENCY: usize = 16;

pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_USAGE: i32 = 1;
pub const EXIT_DEPENDENCY: i32 = 2;
pub const EXIT_PARTIAL: i32 = 3;
pub const EXIT_INTERNAL: i32 = 4;

#[derive(Parser)]
#[command(name = "uparser", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
// `Parse` genuinely has many CLI-flag-derived fields (clap subcommand
// variants, not a hot-path data structure) — boxing individual fields to
// shrink the enum would complicate clap's derive parsing for no runtime
// benefit; `Command` is only ever constructed once per process, matched
// once in `run()`.
#[allow(clippy::large_enum_variant)]
pub enum Command {
    Parse {
        path: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        /// Write the successful aggregate result to this file instead of
        /// stdout. Errors continue to use the normal stdout/stderr contract.
        #[arg(long)]
        output: Option<String>,
        /// Which producer supplies Markdown for a native PDF. `engine`
        /// (default) keeps the native engine's own Markdown, which is what
        /// the published native benchmark score was measured on; `canonical`
        /// renders that PDF from the canonical model instead, the same way
        /// every other source is rendered. Has no effect on structured
        /// sources or model protocols — those have one renderer either way —
        /// nor on non-Markdown output.
        #[arg(long, value_enum, default_value_t = MarkdownSource::Engine)]
        markdown_source: MarkdownSource,
        /// Execution family. Omit for backward-compatible auto routing or
        /// when selecting a concrete adapter through `--protocol`.
        #[arg(long, value_enum)]
        mode: Option<ParseMode>,
        /// Protocol name (`native`, `tesseract`, `mineru-vlm`, `dots-ocr`,
        /// `generic-vlm`, `monkeyocr-v2`, `pipeline`, `paddleocr`,
        /// `paddlex-structure`, `mock`), or `auto`
        /// (the default) to run the Profiler+Router first and pick one
        /// automatically (per ARCHITECTURE.md §13.5). Defaulting to `auto`
        /// rather than `mock` keeps an Agent that omits `--protocol` from
        /// silently getting placeholder output — `mock` is now explicit-only.
        #[arg(long)]
        protocol: Option<String>,
        /// Override the adapter's default endpoint (ignored by adapters
        /// with no endpoint, e.g. `mock`/`native`).
        #[arg(long)]
        endpoint: Option<String>,
        /// Override the adapter's default model name (same scope as
        /// `--endpoint`).
        #[arg(long)]
        model: Option<String>,
        /// Number of pages rasterized+processed together before the
        /// window's page buffers are dropped and the next window begins
        /// (bounds peak memory to ~O(window) page images, not O(total)).
        /// Default 64 so any document up to 64 pages runs as a single
        /// barrier-free window — the inter-window barrier drains
        /// in-flight concurrency to zero, so a window smaller than the
        /// document only hurts throughput on large docs. At runtime the
        /// effective window is raised to at least `--max-concurrency`
        /// (a window smaller than the concurrency budget can never
        /// saturate it). Lower it only to cap memory on huge documents.
        #[arg(long, default_value_t = DEFAULT_WINDOW_SIZE)]
        window_size: usize,
        /// Max concurrent model requests in flight across the whole
        /// document (page-level + per-block, sharing one budget).
        /// Default 16 — the empirically-measured sweet spot against a
        /// remote vLLM backend (the prior default of 4 left the endpoint
        /// badly under-fed; MinerU's own http client defaults to 100).
        /// Raise toward 32-100 for a beefier endpoint, lower for a
        /// fragile/shared one.
        #[arg(long, default_value_t = DEFAULT_MAX_CONCURRENCY)]
        max_concurrency: usize,
        /// `pipeline`-only per-stage backend/endpoint overrides
        /// (ARCHITECTURE.md §11.2/T-5.1). Ignored by every other
        /// protocol. `layout`/`ocr`/`formula` have no `Local`
        /// implementation — passing `local` for those is a usage error.
        #[arg(long, value_enum)]
        layout_backend: Option<StageBackendChoice>,
        #[arg(long)]
        layout_endpoint: Option<String>,
        /// Bare PP-DocLayoutV2 tensor endpoint. When set, Rust owns image
        /// preprocessing, detection decoding, filtering and formula-region
        /// extraction; the service performs model forward only.
        #[arg(long)]
        bare_layout_endpoint: Option<String>,
        /// Pipeline V2 formula-detection (MFD) batch endpoint.
        #[arg(long)]
        formula_detection_endpoint: Option<String>,
        #[arg(long, value_enum)]
        ocr_backend: Option<StageBackendChoice>,
        #[arg(long)]
        ocr_endpoint: Option<String>,
        /// Base URL of the bare OCR detector/recognizer tensor service.
        #[arg(long)]
        bare_ocr_endpoint_base: Option<String>,
        /// PP-OCR recognition character dictionary. Required together with
        /// --bare-ocr-endpoint-base.
        #[arg(long)]
        ocr_dictionary_path: Option<String>,
        #[arg(long, value_enum)]
        formula_backend: Option<StageBackendChoice>,
        #[arg(long)]
        formula_endpoint: Option<String>,
        /// Bare PP-FormulaNet-plus-M tensor endpoint. Rust owns crop,
        /// preprocessing, tokenizer decoding and LaTeX repair.
        #[arg(long)]
        bare_formula_endpoint: Option<String>,
        /// PP-FormulaNet inference YAML containing the tokenizer JSON.
        /// Required together with --bare-formula-endpoint.
        #[arg(long)]
        formula_tokenizer_path: Option<String>,
        /// Pipeline V2 model stages are service-only. `remote` is accepted
        /// for compatibility; `local` is rejected.
        #[arg(long, value_enum)]
        table_backend: Option<StageBackendChoice>,
        /// Pipeline V2 table-recognition batch endpoint.
        #[arg(long)]
        table_endpoint: Option<String>,
        /// Base URL of the bare tensor service. Rust invokes table classifier,
        /// SLANet and UNet forwards and owns all table postprocessing.
        #[arg(long)]
        bare_table_endpoint_base: Option<String>,
        #[arg(long)]
        table_model_path: Option<String>,
        /// OCR language forwarded to the model service (default: ch).
        #[arg(long)]
        pipeline_language: Option<String>,
        /// Bypass the content-hash cache (T-9.1) entirely — forces a
        /// real re-parse even if an identical `(bytes, protocol,
        /// endpoint, model)` fingerprint was cached from a prior run.
        #[arg(long)]
        no_cache: bool,
        /// Emit NDJSON to stdout incrementally, one line per completed
        /// processing window, instead of one aggregate JSON/Markdown
        /// document at the end (T-9.2 / ARCHITECTURE.md §2.2). Each line
        /// is `{"window_pages": [...], "window_errors": [...]}`.
        #[arg(long)]
        stream: bool,
        /// Skip `postprocess::merge_paragraphs_by_geometry` and return
        /// each adapter's raw per-block output unmerged — mainly for
        /// diffing "raw protocol output" against post-processed output
        /// when debugging a merge decision.
        #[arg(long)]
        no_postprocess: bool,
        /// Only parse these 1-indexed page numbers, e.g. `1-5`, `3`, or
        /// `1,5,10-12`. Applied after ingestion, before dispatching to
        /// the scheduler — lets you validate a protocol/endpoint against
        /// one page of a large document without waiting for every
        /// earlier page first. Omit to parse every page.
        #[arg(long)]
        pages: Option<String>,
        /// Directory image/chart-category block crops get written to,
        /// overriding the default `<source_stem>_images/` next to the
        /// source document (mirrors MinerU's own `images/` output
        /// convention — see `image_link_gap_report.md`). Ignored if
        /// `--no-assets` is set.
        #[arg(long)]
        assets_dir: Option<String>,
        /// Skip writing image assets to disk entirely — every block's
        /// `asset_path` stays unset and `to_markdown` never emits an
        /// `![](...)` link. An explicit opt-out for callers that don't
        /// want the filesystem side effect image-asset writing
        /// introduces by default.
        #[arg(long)]
        no_assets: bool,
        /// Override the DPI used to rasterize PDF pages before handing
        /// them to a visual-page protocol (`native`/image-only inputs are
        /// unaffected). Defaults to `runner::DEFAULT_RASTER_DPI` (200,
        /// matching MinerU's own `DEFAULT_PDF_IMAGE_DPI` — see D7 in
        /// `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`); a lower value
        /// trades OCR/table/formula recognition fidelity for smaller
        /// images and less bandwidth to a remote endpoint.
        #[arg(long)]
        raster_dpi: Option<u16>,
        /// Redact common email, mainland-China phone, and resident-ID
        /// values in emitted CLI output. Parsing and cached results remain
        /// faithful to the source.
        #[arg(long)]
        redact_pii: bool,
        /// Drop footnotes, endnotes and speaker notes (`native` structured
        /// formats only). They are extracted by default.
        #[arg(long)]
        no_notes: bool,
        /// Include page headers and footers in the body (`native` structured
        /// formats only). Excluded by default because they repeat on every
        /// page and pollute extracted text.
        #[arg(long)]
        headers_footers: bool,
        /// Reject an input larger than this many MiB before parsing it
        /// (`native` structured formats only). Guards against a hostile or
        /// accidental oversized document; defaults to the engine's own 256
        /// MiB budget.
        #[arg(long)]
        max_input_mib: Option<u64>,
    },
    /// Run the Profiler only (no protocol adapter, no full parse) and
    /// print the resulting DocumentProfile as JSON. Per ARCHITECTURE.md
    /// §13.5's Agent-first philosophy: an Agent can inspect the routing
    /// decision before committing to a full (expensive) parse.
    Classify { path: String },
    /// Detect, analyze and route without executing the selected parser.
    Plan {
        path: String,
        #[arg(long, value_enum)]
        mode: Option<ParseMode>,
        #[arg(long)]
        protocol: Option<String>,
        #[arg(long, value_enum, default_value_t = crate::router::RoutePreference::Quality)]
        prefer: crate::router::RoutePreference,
    },
    /// Content-hash cache management (T-9.1).
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    /// Per-protocol health check (T-9.3): for HTTP-backed protocols,
    /// probes the given/default endpoint's reachability; for `pipeline`,
    /// also reports local CPU/memory as a non-binding Local/Remote
    /// suggestion. Diagnostic only — never gates `parse`.
    Doctor {
        protocol: String,
        #[arg(long)]
        endpoint: Option<String>,
    },
    /// List every built-in adapter's capabilities (coordinate system,
    /// reading-order/signal support, per-stage resource hints) as JSON
    /// (T-9.4) — introspection an Agent can use before choosing
    /// `--protocol`.
    Protocols,
}

#[derive(Subcommand)]
pub enum CacheAction {
    /// Print entry count and total size on disk.
    Stat,
    /// Delete the entire cache directory.
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Json,
    Markdown,
    DocumentJson,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MarkdownSource {
    Engine,
    Canonical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ParseMode {
    Auto,
    Native,
    Protocol,
    Pipeline,
}

/// Run the parsed CLI invocation, returning the process exit code. All
/// diagnostics are written to stderr as a side effect; the result (or a
/// structured error object, for `--format json`) is written to stdout.
pub fn run(cli: Cli) -> i32 {
    match cli.command {
        Command::Parse {
            path,
            format,
            output,
            markdown_source,
            mode,
            protocol,
            endpoint,
            model,
            window_size,
            max_concurrency,
            layout_backend,
            layout_endpoint,
            bare_layout_endpoint,
            formula_detection_endpoint,
            ocr_backend,
            ocr_endpoint,
            bare_ocr_endpoint_base,
            ocr_dictionary_path,
            formula_backend,
            formula_endpoint,
            bare_formula_endpoint,
            formula_tokenizer_path,
            table_backend,
            table_endpoint,
            bare_table_endpoint_base,
            table_model_path,
            pipeline_language,
            no_cache,
            stream,
            no_postprocess,
            pages,
            assets_dir,
            no_assets,
            raster_dpi,
            redact_pii,
            no_notes,
            headers_footers,
            max_input_mib,
        } => {
            let protocol = match resolve_mode(mode, protocol.as_deref()) {
                Ok(protocol) => protocol,
                Err(error) => {
                    return emit_error(
                        format,
                        EXIT_USAGE,
                        "invalid_mode_selection",
                        &error,
                        protocol.as_deref().unwrap_or("auto"),
                        Some("route"),
                    );
                }
            };
            let wanted_pages = match pages.as_deref().map(crate::page_range::parse_page_range) {
                Some(Ok(pages)) => Some(pages),
                Some(Err(e)) => {
                    return emit_error(
                        format,
                        EXIT_USAGE,
                        "invalid_pages",
                        &e,
                        &protocol,
                        Some("pages"),
                    );
                }
                None => None,
            };

            for (stage, backend) in [
                ("layout", layout_backend),
                ("ocr", ocr_backend),
                ("formula", formula_backend),
                ("table", table_backend),
            ] {
                if backend == Some(StageBackendChoice::Local) {
                    return emit_error(
                        format,
                        EXIT_USAGE,
                        "unsupported_stage_backend",
                        &format!(
                            "pipeline V2's `{stage}` stage is model-service-only; \
                             no model runtime is linked into the Rust client"
                        ),
                        &protocol,
                        Some(stage),
                    );
                }
            }

            if table_model_path.is_some() {
                return emit_error(
                    format,
                    EXIT_USAGE,
                    "unsupported_stage_backend",
                    "pipeline V2 does not load table models in the Rust process; configure the \
                     model service and use --table-endpoint instead",
                    &protocol,
                    Some("table"),
                );
            }

            if bare_formula_endpoint.is_some() != formula_tokenizer_path.is_some() {
                return emit_error(
                    format,
                    EXIT_USAGE,
                    "invalid_pipeline_config",
                    "--bare-formula-endpoint and --formula-tokenizer-path must be provided together",
                    &protocol,
                    Some("formula"),
                );
            }
            if bare_ocr_endpoint_base.is_some() != ocr_dictionary_path.is_some() {
                return emit_error(
                    format,
                    EXIT_USAGE,
                    "invalid_pipeline_config",
                    "--bare-ocr-endpoint-base and --ocr-dictionary-path must be provided together",
                    &protocol,
                    Some("ocr"),
                );
            }

            let pipeline_config = PipelineConfig {
                layout_backend: None,
                layout_endpoint,
                bare_layout_endpoint,
                formula_detection_endpoint,
                ocr_backend: None,
                ocr_endpoint,
                bare_ocr_endpoint_base,
                ocr_dictionary_path,
                formula_backend: None,
                formula_endpoint,
                bare_formula_endpoint,
                formula_tokenizer_path,
                table_backend,
                table_endpoint,
                bare_table_endpoint_base,
                table_model_path,
                language: pipeline_language,
            };
            run_parse(
                path,
                format,
                output,
                markdown_source,
                protocol,
                endpoint,
                model,
                window_size,
                max_concurrency,
                pipeline_config,
                no_cache,
                stream,
                no_postprocess,
                wanted_pages,
                assets_dir,
                no_assets,
                raster_dpi,
                redact_pii,
                no_notes,
                headers_footers,
                max_input_mib,
            )
        }
        Command::Classify { path } => run_classify(path),
        Command::Plan {
            path,
            mode,
            protocol,
            prefer,
        } => match resolve_mode(mode, protocol.as_deref()) {
            Ok(protocol) => run_plan(path, protocol, prefer),
            Err(error) => emit_error(
                OutputFormat::Json,
                EXIT_USAGE,
                "invalid_mode_selection",
                &error,
                protocol.as_deref().unwrap_or("auto"),
                Some("route"),
            ),
        },
        Command::Cache { action } => run_cache(action),
        Command::Doctor { protocol, endpoint } => run_doctor(protocol, endpoint),
        Command::Protocols => run_protocols(),
    }
}

fn resolve_mode(mode: Option<ParseMode>, protocol: Option<&str>) -> Result<String, String> {
    let protocol = protocol.filter(|value| !value.trim().is_empty());
    match (mode, protocol) {
        (None, None) => Ok("auto".to_owned()),
        (None, Some(protocol)) => Ok(protocol.to_owned()),
        (Some(ParseMode::Auto), None | Some("auto")) => Ok("auto".to_owned()),
        (Some(ParseMode::Native), None | Some("native")) => Ok("native".to_owned()),
        (Some(ParseMode::Pipeline), None | Some("pipeline")) => Ok("pipeline".to_owned()),
        (Some(ParseMode::Protocol), Some(protocol)) => {
            let Some(spec) = crate::protocol_spec::get(protocol) else {
                return Err(format!("unknown model protocol: {protocol}"));
            };
            if spec.mode != crate::protocol_spec::ModeKind::ModelProtocol {
                return Err(format!(
                    "--mode protocol requires a model-protocol adapter, got {protocol}"
                ));
            }
            Ok(protocol.to_owned())
        }
        (Some(ParseMode::Protocol), None) => {
            Err("--mode protocol requires --protocol <name>".to_owned())
        }
        (Some(mode), Some(protocol)) => Err(format!(
            "--mode {} conflicts with --protocol {protocol}",
            match mode {
                ParseMode::Auto => "auto",
                ParseMode::Native => "native",
                ParseMode::Protocol => "protocol",
                ParseMode::Pipeline => "pipeline",
            }
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_parse(
    path: String,
    format: OutputFormat,
    output_path: Option<String>,
    markdown_source: MarkdownSource,
    protocol: String,
    endpoint: Option<String>,
    model: Option<String>,
    window_size: usize,
    max_concurrency: usize,
    pipeline_config: PipelineConfig,
    no_cache: bool,
    stream: bool,
    no_postprocess: bool,
    wanted_pages: Option<Vec<u32>>,
    assets_dir: Option<String>,
    no_assets: bool,
    raster_dpi: Option<u16>,
    redact_pii: bool,
    no_notes: bool,
    headers_footers: bool,
    max_input_mib: Option<u64>,
) -> i32 {
    if stream && output_path.is_some() {
        return emit_error(
            format,
            EXIT_USAGE,
            "invalid_output_selection",
            "--output cannot be combined with --stream",
            &protocol,
            Some("output"),
        );
    }
    if !Path::new(&path).exists() {
        return emit_error(
            format,
            EXIT_DEPENDENCY,
            "file_not_found",
            &format!("no such file: {path}"),
            &protocol,
            None,
        );
    }

    let file_bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            return emit_error(
                format,
                EXIT_DEPENDENCY,
                "read_failed",
                &e.to_string(),
                &protocol,
                None,
            );
        }
    };

    let mut document_options = uparser_document_engine::ParseOptions {
        include_notes: !no_notes,
        include_headers_footers: headers_footers,
        include_assets: !no_assets,
        ..uparser_document_engine::ParseOptions::default()
    };
    if let Some(mib) = max_input_mib {
        document_options.limits.max_input_bytes = mib.saturating_mul(1024 * 1024);
    }

    let preflight_source = crate::frontend::PreflightSource::new(
        std::sync::Arc::<[u8]>::from(file_bytes),
        Some(&path),
    );
    let detected_format = preflight_source.format();
    let cancellation = crate::frontend::CancellationToken::default();
    let prepare_runtime = match if protocol == "native" {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
    } else {
        tokio::runtime::Runtime::new()
    } {
        Ok(runtime) => runtime,
        Err(error) => {
            return emit_error(
                format,
                EXIT_INTERNAL,
                "runtime_init_failed",
                &error.to_string(),
                &protocol,
                Some("preflight"),
            );
        }
    };
    let signal_cancellation = cancellation.clone();
    prepare_runtime.spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancellation.cancel();
        }
    });
    let prepared = match prepare_runtime.block_on(crate::runner::prepare_with_options(
        preflight_source,
        Some(&protocol),
        crate::router::RoutePreference::Quality,
        cancellation.clone(),
        &document_options,
    )) {
        Ok(prepared) => prepared,
        Err(error) => {
            // A structured failure carries a machine-readable kind
            // (`native_document.encrypted`, `.resource_limit`, …); report it
            // as the error object's `stage` so an agent can branch on the
            // cause instead of parsing the message.
            let stage = match &error {
                crate::runner::PrepareError::Analysis {
                    stage: Some(stage), ..
                } => stage,
                _ => "preflight",
            };
            return emit_error(
                format,
                EXIT_USAGE,
                "preflight_failed",
                &error.to_string(),
                &protocol,
                Some(stage),
            );
        }
    };
    let effective_protocol = prepared.plan.route.protocol.clone();
    if protocol == "auto" {
        eprintln!(
            "auto: routed to {:?} ({})",
            effective_protocol, prepared.plan.route.reason
        );
    }
    // Resolve endpoint/model after routing so auto uses the selected protocol's config.
    let (endpoint, model) =
        crate::agent_config::resolve_endpoint_model(&effective_protocol, endpoint, model);

    // `document-json` used to be rejected here for every protocol but
    // `native`, and for every format but a structured source, because the
    // canonical document could only come from a structured frontend. Since
    // O5.2 the IR can be lifted into one (`ascend`), so the format is
    // available everywhere and this gate is gone.

    if protocol == "auto"
        && effective_protocol != "native"
        && effective_protocol != "tesseract"
        && endpoint.is_none()
    {
        eprintln!(
            "hint: auto selected '{effective_protocol}', which needs a model endpoint, but \
             none is configured - set UPARSER_ENDPOINT (or --endpoint / config.toml)"
        );
    }
    if effective_protocol == "native" {
        for (flag, given) in [
            ("--pages", wanted_pages.is_some()),
            ("--stream", stream),
            ("--window-size", window_size != DEFAULT_WINDOW_SIZE),
            (
                "--max-concurrency",
                max_concurrency != DEFAULT_MAX_CONCURRENCY,
            ),
            ("--raster-dpi", raster_dpi.is_some()),
        ] {
            if given {
                eprintln!("warning: {flag} has no effect on native whole-document execution");
            }
        }
    }

    let execution = crate::runner::ExecutionOptions {
        endpoint,
        model,
        window_size,
        max_concurrency,
        pipeline_config,
        no_cache,
        no_postprocess,
        pages: wanted_pages,
        assets_dir: assets_dir.map(std::path::PathBuf::from),
        no_assets,
        raster_dpi,
        document_options,
        // Engine Markdown is rendered straight from the native/document
        // engine artifact, so the compatibility Page/Block IR and image
        // materialization are pure overhead for this request shape.
        markdown_only: effective_protocol == "native"
            && format == OutputFormat::Markdown
            && markdown_source == MarkdownSource::Engine,
        cancellation,
    };

    let hooks = if stream && effective_protocol != "native" {
        crate::runner::ExecutionHooks {
            on_window: Some(std::sync::Arc::new(move |pages, errors, warnings| {
                let line = serde_json::json!({
                    "window_pages": pages,
                    "window_errors": errors,
                    "window_warnings": warnings,
                });
                let rendered = serde_json::to_string(&line)
                    .expect("runner window output is always serializable");
                emit_line(&redact_output_if_requested(rendered, redact_pii));
            })),
            on_progress: None,
        }
    } else {
        let last_print = std::sync::Arc::new(std::sync::Mutex::new(
            std::time::Instant::now()
                .checked_sub(PROGRESS_PRINT_MIN_INTERVAL)
                .unwrap_or_else(std::time::Instant::now),
        ));
        crate::runner::ExecutionHooks {
            on_window: None,
            on_progress: Some(std::sync::Arc::new(move |event| {
                if event.total <= 1 {
                    return;
                }
                let is_last = event.completed == event.total;
                let should_print = is_last || {
                    let mut last = last_print.lock().expect("progress mutex not poisoned");
                    if last.elapsed() >= PROGRESS_PRINT_MIN_INTERVAL {
                        *last = std::time::Instant::now();
                        true
                    } else {
                        false
                    }
                };
                if should_print {
                    eprintln!(
                        "progress: {}/{} pages (page {} {})",
                        event.completed,
                        event.total,
                        event.page_num,
                        if event.ok { "ok" } else { "error" }
                    );
                }
            })),
        }
    };

    let mut outcome = match prepare_runtime.block_on(crate::runner::execute_with_hooks(
        prepared, &execution, &hooks,
    )) {
        Ok(outcome) => outcome,
        Err(error) => {
            let (code, error_code, stage) = match &error {
                crate::runner::ExecutionError::UnknownProtocol(_) => {
                    (EXIT_USAGE, "unknown_protocol", Some("route"))
                }
                crate::runner::ExecutionError::Ingest(_) => {
                    (EXIT_DEPENDENCY, "ingest_failed", Some("ingest"))
                }
                crate::runner::ExecutionError::Structured(_) => (
                    EXIT_DEPENDENCY,
                    "native_parse_failed",
                    Some("native_document"),
                ),
                crate::runner::ExecutionError::Native(_) => {
                    (EXIT_INTERNAL, "native_parse_failed", Some("native"))
                }
                crate::runner::ExecutionError::Assets(_) => {
                    (EXIT_DEPENDENCY, "asset_write_failed", Some("assets"))
                }
                crate::runner::ExecutionError::Cache(_) => {
                    (EXIT_DEPENDENCY, "cache_write_failed", Some("cache"))
                }
                crate::runner::ExecutionError::InvalidStageGraph(_) => {
                    (EXIT_USAGE, "invalid_stage_graph", Some("stage_graph"))
                }
                crate::runner::ExecutionError::Cancelled => {
                    (EXIT_PARTIAL, "cancelled", Some("runner"))
                }
            };
            return emit_error(
                format,
                code,
                error_code,
                &error.to_string(),
                &effective_protocol,
                stage,
            );
        }
    };
    if outcome.cache_hit {
        eprintln!("cache: hit");
    }

    let has_errors = !outcome.result.page_errors.is_empty();
    if !stream || effective_protocol == "native" {
        // Asset materialization has to happen before rendering, because the
        // canonical renderer points `![](…)` at the path the asset was
        // written to; it is a no-op for a document with no assets.
        if !no_assets && let Some(document) = outcome.document.as_mut() {
            let directory = execution
                .assets_dir
                .clone()
                .unwrap_or_else(|| crate::assets::default_assets_dir(&path));
            if let Err(error) = crate::assets::write_document_assets(document, &directory) {
                eprintln!("warning: failed to write document assets: {error}");
            }
        }
        let render_input = render::RenderInput {
            result: &outcome.result,
            engine_markdown: outcome.engine_markdown.as_deref(),
            document: outcome.document.as_ref(),
            source_format: detected_format,
        };
        let output = match format {
            OutputFormat::Json => render::to_json(&outcome.result),
            OutputFormat::Markdown => render::render_markdown(
                &render_input,
                match markdown_source {
                    MarkdownSource::Engine => render::MarkdownSource::Engine,
                    MarkdownSource::Canonical => render::MarkdownSource::Canonical,
                },
            ),
            OutputFormat::DocumentJson => match render::render_document_json(&render_input) {
                Ok(json) => json,
                Err(error) => {
                    return emit_error(
                        format,
                        EXIT_INTERNAL,
                        "serialization_failed",
                        &error.to_string(),
                        &effective_protocol,
                        Some("render"),
                    );
                }
            },
        };
        let output = redact_output_if_requested(output, redact_pii);
        if let Err(error) = emit_parse_output(&output, output_path.as_deref()) {
            return emit_error(
                format,
                EXIT_DEPENDENCY,
                "output_write_failed",
                &error.to_string(),
                &effective_protocol,
                Some("output"),
            );
        }
    }

    if has_errors {
        emit_page_error_diagnostics(&outcome.result.page_errors);
        EXIT_PARTIAL
    } else {
        EXIT_SUCCESS
    }
}

fn emit_page_error_diagnostics(errors: &[crate::types::PageError]) {
    for error in errors {
        eprintln!(
            "error: page={} stage={} message={}",
            error.page_num,
            error.stage.as_deref().unwrap_or("unknown"),
            error.message
        );
    }
}

fn redact_output_if_requested(text: String, redact: bool) -> String {
    if !redact {
        return text;
    }
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    static RESIDENT_ID: OnceLock<Regex> = OnceLock::new();
    static PHONE: OnceLock<Regex> = OnceLock::new();

    let email = EMAIL.get_or_init(|| {
        Regex::new(r"(?i)[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}")
            .expect("PII email regex is valid")
    });
    let resident_id = RESIDENT_ID.get_or_init(|| {
        Regex::new(r"(^|[^0-9])[0-9]{17}[0-9Xx]([^0-9]|$)").expect("PII resident-ID regex is valid")
    });
    let phone = PHONE.get_or_init(|| {
        Regex::new(r"(^|[^0-9])1[3-9][0-9]{9}([^0-9]|$)").expect("PII phone regex is valid")
    });

    let text = email.replace_all(&text, "[REDACTED_EMAIL]");
    let text = resident_id.replace_all(&text, "$1[REDACTED_ID]$2");
    phone
        .replace_all(&text, "$1[REDACTED_PHONE]$2")
        .into_owned()
}

/// Print a result line to stdout, treating a closed pipe as a normal end of
/// output rather than a panic.
///
/// `println!` panics when stdout is gone, which is exactly what happens under
/// `uparser … | head` — an agent driving this as a subprocess would see a
/// Rust panic trace on stderr for an entirely ordinary situation.
fn emit_line(text: &str) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    if writeln!(handle, "{text}").is_err() {
        return;
    }
    let _ = handle.flush();
}

fn emit_parse_output(text: &str, output: Option<&str>) -> std::io::Result<()> {
    if let Some(path) = output {
        std::fs::write(path, text)
    } else {
        emit_line(text);
        Ok(())
    }
}

/// the resulting `DocumentProfile` as JSON to stdout.
fn run_classify(path: String) -> i32 {
    if !Path::new(&path).exists() {
        return emit_error(
            OutputFormat::Json,
            EXIT_DEPENDENCY,
            "file_not_found",
            &format!("no such file: {path}"),
            "classify",
            None,
        );
    }

    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return emit_error(
                OutputFormat::Json,
                EXIT_DEPENDENCY,
                "read_failed",
                &e.to_string(),
                "classify",
                None,
            );
        }
    };

    let source =
        crate::frontend::PreflightSource::new(std::sync::Arc::<[u8]>::from(bytes), Some(&path));
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            return emit_error(
                OutputFormat::Json,
                EXIT_INTERNAL,
                "runtime_init_failed",
                &error.to_string(),
                "classify",
                Some("preflight"),
            );
        }
    };
    let profile = match runtime.block_on(crate::runner::analyze(&source)) {
        Ok(report) => report.profile,
        Err(error) => {
            return emit_error(
                OutputFormat::Json,
                EXIT_USAGE,
                "analysis_failed",
                &error.to_string(),
                "classify",
                Some("analyze"),
            );
        }
    };

    let json =
        serde_json::to_string_pretty(&profile).expect("DocumentProfile is always serializable");
    println!("{json}");
    EXIT_SUCCESS
}

fn run_plan(path: String, protocol: String, preference: crate::router::RoutePreference) -> i32 {
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return emit_error(
                OutputFormat::Json,
                EXIT_DEPENDENCY,
                "read_failed",
                &error.to_string(),
                &protocol,
                Some("read"),
            );
        }
    };
    let source =
        crate::frontend::PreflightSource::new(std::sync::Arc::<[u8]>::from(bytes), Some(&path));
    let runtime = tokio::runtime::Runtime::new().expect("runtime initialization");
    match runtime.block_on(crate::runner::prepare_with_preference(
        source,
        Some(&protocol),
        preference,
    )) {
        Ok(prepared) => {
            let output = serde_json::json!({
                "format": prepared.source.detection(),
                "profile": prepared.analysis.profile,
                "plan": prepared.plan,
                "preference": preference,
            });
            emit_line(&serde_json::to_string_pretty(&output).expect("plan is serializable"));
            EXIT_SUCCESS
        }
        Err(error) => emit_error(
            OutputFormat::Json,
            EXIT_USAGE,
            "plan_failed",
            &error.to_string(),
            &protocol,
            Some("plan"),
        ),
    }
}

fn emit_error(
    format: OutputFormat,
    code: i32,
    error_code: &str,
    message: &str,
    protocol: &str,
    stage: Option<&str>,
) -> i32 {
    eprintln!("error: {message}");
    if matches!(format, OutputFormat::Json | OutputFormat::DocumentJson) {
        let err = serde_json::json!({
            "error": {
                "code": error_code,
                "message": message,
                "protocol": protocol,
                "stage": stage,
            }
        });
        println!(
            "{}",
            serde_json::to_string(&err).expect("error object is serializable")
        );
    }
    code
}

/// `uparser cache stat|clear` (T-9.1).
fn run_cache(action: CacheAction) -> i32 {
    let dir = cache::default_cache_dir();
    match action {
        CacheAction::Stat => match cache::stat(&dir) {
            Ok(stats) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&stats).expect("CacheStats is serializable")
                );
                EXIT_SUCCESS
            }
            Err(e) => emit_error(
                OutputFormat::Json,
                EXIT_INTERNAL,
                "cache_stat_failed",
                &e.to_string(),
                "cache",
                None,
            ),
        },
        CacheAction::Clear => match cache::clear(&dir) {
            Ok(()) => {
                eprintln!("cache cleared: {}", dir.display());
                EXIT_SUCCESS
            }
            Err(e) => emit_error(
                OutputFormat::Json,
                EXIT_INTERNAL,
                "cache_clear_failed",
                &e.to_string(),
                "cache",
                None,
            ),
        },
    }
}

/// Each HTTP-backed built-in adapter's default endpoint, duplicated here
/// (rather than adding a `default_endpoint()` trait method just for this
/// diagnostic command) since only `doctor` needs it and every adapter's
/// concrete default is already a public field. `mock`/`native` have no
/// network endpoint to probe; `pipeline` is handled separately (its
/// doctor check is local-resource-based, not endpoint reachability).
fn default_endpoint_for(protocol: &str) -> Option<String> {
    crate::protocol_spec::get(protocol)
        .and_then(|spec| spec.default_endpoint)
        .map(str::to_owned)
}

/// `uparser doctor` (T-9.3): reachability probe for HTTP-backed protocols.
/// Diagnostic only — a failed probe never changes `parse`'s behavior.
fn run_doctor(protocol: String, endpoint: Option<String>) -> i32 {
    // Same endpoint resolution as `parse` (flag → env → config[protocol]) so a
    // pre-flight `doctor` probes the very endpoint a later `parse` would use.
    let (mut endpoint, _) = crate::agent_config::resolve_endpoint_model(&protocol, endpoint, None);
    if protocol == "pipeline" {
        let base = endpoint
            .take()
            .unwrap_or_else(|| "http://localhost:9001".to_owned());
        endpoint = Some(format!("{}/health", base.trim_end_matches('/')));
    }

    if protocol == "mock" || protocol == "native" || protocol == "tesseract" {
        let (reachable, note) = if protocol == "tesseract" {
            let available = crate::adapters::local_tesseract::available();
            (
                Some(available),
                if available {
                    "local Tesseract executable is available"
                } else {
                    "local Tesseract executable was not found"
                },
            )
        } else {
            (None, "this protocol has no network endpoint to probe")
        };
        let report = serde_json::json!({
            "protocol": protocol,
            "reachable": reachable,
            "note": note,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("doctor report is serializable")
        );
        return EXIT_SUCCESS;
    }

    let Some(target) = endpoint.or_else(|| default_endpoint_for(&protocol)) else {
        return emit_error(
            OutputFormat::Json,
            EXIT_USAGE,
            "unknown_protocol",
            &format!("unknown protocol: {protocol}"),
            &protocol,
            None,
        );
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            return emit_error(
                OutputFormat::Json,
                EXIT_INTERNAL,
                "runtime_init_failed",
                &e.to_string(),
                &protocol,
                None,
            );
        }
    };

    // A GET against what's usually a POST-only chat-completions/REST
    // path will very likely 404/405 — that still proves the endpoint is
    // *reachable*, which is all this probes for. Only a transport-level
    // failure (refused/timeout/DNS) counts as unreachable.
    let (reachable, detail) = runtime.block_on(async {
        let client = reqwest::Client::new();
        match client
            .get(&target)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) => (true, format!("HTTP {}", resp.status())),
            Err(e) => (false, e.to_string()),
        }
    });

    let report = serde_json::json!({
        "protocol": protocol,
        "endpoint": target,
        "reachable": reachable,
        "detail": detail,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("doctor report is serializable")
    );
    EXIT_SUCCESS
}

/// `uparser protocols` (T-9.4): every built-in adapter's declared
/// capabilities, as JSON.
fn run_protocols() -> i32 {
    let registry = Registry::with_builtins();
    let mut names = registry.names();
    names.sort_unstable();
    let overrides = AdapterOverrides::default();

    let list: Vec<serde_json::Value> = names
        .iter()
        .map(|name| {
            let adapter = registry
                .build(name, &overrides)
                .expect("name came from the registry itself");
            let signals = adapter.emitted_signals();
            let spec = crate::protocol_spec::get(name).expect("registered protocol has a spec");
            serde_json::json!({
                "name": adapter.name(),
                "mode": spec.mode,
                "shape": spec.shape,
                "transport": spec.transport,
                "default_endpoint": spec.default_endpoint,
                "coordinate_system": format!("{:?}", adapter.coordinate_system()),
                "provides_reading_order": adapter.provides_reading_order(),
                "category_vocab": adapter.category_vocab(),
                "raw_output_format": format!("{:?}", adapter.raw_output_format()),
                "emitted_signals": {
                    "spans": signals.spans,
                    "merge_hint": signals.merge_hint,
                    "font_size": signals.font_size,
                },
                "model_stages": adapter.model_stages().iter().map(|s| serde_json::json!({
                    "stage_name": s.stage_name,
                    "allows_local": s.allows_local,
                    "resource_hint": format!("{:?}", s.resource_hint),
                    "default_backend": match &s.default_backend {
                        crate::adapters::StageBackend::Local(_) => "local",
                        crate::adapters::StageBackend::Remote(_) => "remote",
                    },
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    println!(
        "{}",
        serde_json::to_string_pretty(&list).expect("protocol list is serializable")
    );
    EXIT_SUCCESS
}

#[cfg(test)]
mod pii_tests {
    use super::redact_output_if_requested;

    #[test]
    fn output_redaction_covers_common_resume_identifiers() {
        let source = "邮箱 person.name@example.com 电话13812345678 身份证11010119900307123X";
        let redacted = redact_output_if_requested(source.to_owned(), true);
        assert_eq!(
            redacted,
            "邮箱 [REDACTED_EMAIL] 电话[REDACTED_PHONE] 身份证[REDACTED_ID]"
        );
    }

    #[test]
    fn output_redaction_is_opt_in() {
        let source = "person.name@example.com";
        assert_eq!(redact_output_if_requested(source.to_owned(), false), source);
    }
}
