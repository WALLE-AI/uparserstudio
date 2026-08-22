//! Agent-first CLI contract per ARCHITECTURE.md §6.1: stdout carries the
//! result only, all logs/progress go to stderr, and exit codes are
//! semantic so an Agent can branch on them without parsing prose.

use crate::adapters::{AdapterOverrides, PipelineConfig, Registry, StageBackendChoice};
use crate::cache;
use crate::render;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::Path;
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
#[command(name = "uparser")]
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
        /// Markdown rendering source. `engine` preserves native/document
        /// engine output; `canonical` uses the shared ParseResult renderer
        /// for G-N comparison. Ignored for non-Markdown output.
        #[arg(long, value_enum, default_value_t = MarkdownSource::Engine)]
        markdown_source: MarkdownSource,
        /// Execution family. Omit for backward-compatible auto routing or
        /// when selecting a concrete adapter through `--protocol`.
        #[arg(long, value_enum)]
        mode: Option<ParseMode>,
        /// Protocol name (`native`, `mineru-vlm`, `dots-ocr`,
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
        #[arg(long, value_enum)]
        ocr_backend: Option<StageBackendChoice>,
        #[arg(long)]
        ocr_endpoint: Option<String>,
        #[arg(long, value_enum)]
        formula_backend: Option<StageBackendChoice>,
        #[arg(long)]
        formula_endpoint: Option<String>,
        /// `table` is the only stage that defaults `Local` (via `ort`,
        /// requires the `pipeline-local-table` feature); `--table-backend
        /// remote` switches it to Pipeline Model Serving instead.
        #[arg(long, value_enum)]
        table_backend: Option<StageBackendChoice>,
        #[arg(long)]
        table_model_path: Option<String>,
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
            ocr_backend,
            ocr_endpoint,
            formula_backend,
            formula_endpoint,
            table_backend,
            table_model_path,
            no_cache,
            stream,
            no_postprocess,
            pages,
            assets_dir,
            no_assets,
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
            ] {
                if backend == Some(StageBackendChoice::Local) {
                    return emit_error(
                        format,
                        EXIT_USAGE,
                        "unsupported_stage_backend",
                        &format!(
                            "pipeline's `{stage}` stage has no `Local` implementation \
                             (its model has no confirmed ONNX export) — only `table` \
                             supports `--table-backend local`"
                        ),
                        &protocol,
                        Some(stage),
                    );
                }
            }

            let pipeline_config = PipelineConfig {
                layout_backend: None,
                layout_endpoint,
                ocr_backend: None,
                ocr_endpoint,
                formula_backend: None,
                formula_endpoint,
                table_backend,
                table_model_path,
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

    // The explicit native Markdown/no-assets mode needs no async services,
    // routing enrichment, compatibility IR, or execution metadata. Keep this
    // direct path aligned with lightweight converter CLIs used in benchmarks.
    if protocol == "native"
        && format == OutputFormat::Markdown
        && markdown_source == MarkdownSource::Engine
        && no_cache
        && !stream
        && !no_postprocess
        && wanted_pages.is_none()
        && assets_dir.is_none()
        && no_assets
        && window_size == DEFAULT_WINDOW_SIZE
        && max_concurrency == DEFAULT_MAX_CONCURRENCY
    {
        match native_markdown_fast_path(&path, &file_bytes, &document_options) {
            Ok(Some(markdown)) => {
                return match emit_parse_output(&markdown, output_path.as_deref()) {
                    Ok(()) => EXIT_SUCCESS,
                    Err(error) => emit_error(
                        format,
                        EXIT_DEPENDENCY,
                        "output_write_failed",
                        &error.to_string(),
                        &protocol,
                        Some("output"),
                    ),
                };
            }
            Ok(None) => {}
            Err(error) => {
                return emit_error(
                    format,
                    EXIT_DEPENDENCY,
                    "native_parse_failed",
                    &error,
                    &protocol,
                    Some("native"),
                );
            }
        }
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
    let prepared =
        match prepare_runtime.block_on(crate::runner::prepare_with_preference_and_cancellation(
            preflight_source,
            Some(&protocol),
            crate::router::RoutePreference::Quality,
            cancellation.clone(),
        )) {
            Ok(prepared) => prepared,
            Err(error) => {
                return emit_error(
                    format,
                    EXIT_USAGE,
                    "preflight_failed",
                    &error.to_string(),
                    &protocol,
                    Some("preflight"),
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

    if format == OutputFormat::DocumentJson && effective_protocol != "native" {
        return emit_error(
            format,
            EXIT_USAGE,
            "unsupported_output_format",
            "document-json requires the native protocol for a structured document",
            &effective_protocol,
            None,
        );
    }
    if format == OutputFormat::DocumentJson
        && detected_format == crate::frontend::DocumentFormat::Pdf
    {
        return emit_error(
            format,
            EXIT_USAGE,
            "unsupported_output_format",
            "document-json is available for structured native documents, not PDF",
            &effective_protocol,
            None,
        );
    }

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
        document_options,
        cancellation,
    };

    let hooks = if stream && effective_protocol != "native" {
        crate::runner::ExecutionHooks {
            on_window: Some(std::sync::Arc::new(|pages, errors, warnings| {
                let line = serde_json::json!({
                    "window_pages": pages,
                    "window_errors": errors,
                    "window_warnings": warnings,
                });
                emit_line(
                    &serde_json::to_string(&line)
                        .expect("runner window output is always serializable"),
                );
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
        let output = match format {
            OutputFormat::Json => render::to_json(&outcome.result),
            OutputFormat::Markdown => {
                if markdown_source == MarkdownSource::Canonical {
                    render::to_markdown(&outcome.result)
                } else if let Some(markdown) = outcome.engine_markdown.take() {
                    markdown
                } else if let Some(document) = outcome.document.as_mut() {
                    if !no_assets {
                        let directory = execution
                            .assets_dir
                            .clone()
                            .unwrap_or_else(|| crate::assets::default_assets_dir(&path));
                        if let Err(error) =
                            crate::assets::write_document_assets(document, &directory)
                        {
                            eprintln!("warning: failed to write document assets: {error}");
                        }
                    }
                    uparser_document_engine::render::markdown(document)
                } else {
                    render::to_markdown(&outcome.result)
                }
            }
            OutputFormat::DocumentJson => {
                let Some(document) = outcome.document.as_mut() else {
                    return emit_error(
                        format,
                        EXIT_USAGE,
                        "unsupported_output_format",
                        "document-json requires a structured native document",
                        &effective_protocol,
                        Some("render"),
                    );
                };
                if !no_assets {
                    let directory = execution
                        .assets_dir
                        .clone()
                        .unwrap_or_else(|| crate::assets::default_assets_dir(&path));
                    if let Err(error) = crate::assets::write_document_assets(document, &directory) {
                        eprintln!("warning: failed to write document assets: {error}");
                    }
                }
                match uparser_document_engine::render::document_json(document) {
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
                }
            }
        };
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
        EXIT_PARTIAL
    } else {
        EXIT_SUCCESS
    }
}

fn native_markdown_fast_path(
    path: &str,
    bytes: &[u8],
    options: &uparser_document_engine::ParseOptions,
) -> Result<Option<String>, String> {
    let detected = uparser_document_engine::detect_format(bytes, Some(path));
    if detected == uparser_document_engine::DocumentFormat::Pdf {
        #[cfg(feature = "native")]
        {
            let artifact =
                uparser_native_engine::process_pdf_mem(bytes).map_err(|error| error.to_string())?;
            if let Some(markdown) = artifact
                .markdown
                .as_deref()
                .filter(|markdown| !markdown.trim().is_empty())
            {
                return Ok(Some(markdown.to_owned()));
            }
            if artifact.positioned_items.is_empty() {
                let markdown = match artifact
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    Some(title) => format!("# {title}\n\n[Image-only PDF: OCR required]\n"),
                    None => "[Image-only PDF: OCR required]\n".to_owned(),
                };
                return Ok(Some(markdown));
            }
            return Ok(Some(uparser_native_engine::to_markdown_from_items(
                artifact.positioned_items,
                uparser_native_engine::MarkdownOptions::default(),
            )));
        }
        #[cfg(not(feature = "native"))]
        {
            return Ok(None);
        }
    }
    let document = uparser_document_engine::parse_document(bytes, detected, options)
        .map_err(|error| error.to_string())?;
    Ok(Some(uparser_document_engine::render::markdown(&document)))
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

/// `MemAvailable` from `/proc/meminfo`, in MB. `None` on non-Linux or if
/// the file/field is missing — a diagnostic heuristic, not something
/// worth a new dependency (e.g. `sysinfo`) or a hard failure over.
fn available_memory_mb() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
            return Some(kb / 1024);
        }
    }
    None
}

/// `uparser doctor` (T-9.3): reachability probe for HTTP-backed
/// protocols, or a local CPU/memory advisory for `pipeline`. Diagnostic
/// only — a failed probe never changes `parse`'s behavior.
fn run_doctor(protocol: String, endpoint: Option<String>) -> i32 {
    // Same endpoint resolution as `parse` (flag → env → config[protocol]) so a
    // pre-flight `doctor` probes the very endpoint a later `parse` would use.
    let (endpoint, _) = crate::agent_config::resolve_endpoint_model(&protocol, endpoint, None);
    if protocol == "pipeline" {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0);
        let mem_mb = available_memory_mb();
        let advice = match (cores, mem_mb) {
            (c, Some(m)) if c >= 4 && m >= 4096 => {
                "table stage's default Local (ort) backend should be fine on this machine; \
                 layout/ocr/formula remain Remote-only regardless of local resources"
            }
            _ => {
                "this machine looks resource-constrained for local ONNX inference; consider \
                 --table-backend remote (heuristic suggestion only, not enforced)"
            }
        };
        let report = serde_json::json!({
            "protocol": "pipeline",
            "local_cpu_cores": cores,
            "local_available_memory_mb": mem_mb,
            "advice": advice,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("doctor report is serializable")
        );
        return EXIT_SUCCESS;
    }

    if protocol == "mock" || protocol == "native" {
        let report = serde_json::json!({
            "protocol": protocol,
            "reachable": null,
            "note": "this protocol has no network endpoint to probe",
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
