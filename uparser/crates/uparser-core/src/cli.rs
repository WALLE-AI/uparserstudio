//! Agent-first CLI contract per ARCHITECTURE.md §6.1: stdout carries the
//! result only, all logs/progress go to stderr, and exit codes are
//! semantic so an Agent can branch on them without parsing prose.

use crate::adapters::{AdapterOverrides, PipelineConfig, Registry, StageBackendChoice};
use crate::cache::{self, ParamFingerprint};
use crate::ingest::RenderedPage;
use crate::render;
use crate::scheduler::Scheduler;
use crate::transport::Transport;
use crate::types::{ParseResult, RoutedBy};
use clap::{Parser, Subcommand, ValueEnum};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Default cache freshness window (T-9.1): 24h. Chosen as a reasonable
/// default for "Agent re-reads the same document within a session" —
/// long enough to survive a multi-turn conversation, short enough that a
/// stale entry doesn't linger for weeks. `--no-cache` bypasses it
/// entirely for forced re-verification.
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

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
pub enum Command {
    Parse {
        path: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        /// Protocol name (`mock`, `mineru-vlm`, `dots-ocr`, `monkeyocr-v2`,
        /// `native`), or `auto` to run the Profiler+Router first and pick
        /// one automatically (per ARCHITECTURE.md §13.5).
        #[arg(long, default_value = "mock")]
        protocol: String,
        /// Override the adapter's default endpoint (ignored by adapters
        /// with no endpoint, e.g. `mock`/`native`).
        #[arg(long)]
        endpoint: Option<String>,
        /// Override the adapter's default model name (same scope as
        /// `--endpoint`).
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 16)]
        window_size: usize,
        #[arg(long, default_value_t = 4)]
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
    },
    /// Run the Profiler only (no protocol adapter, no full parse) and
    /// print the resulting DocumentProfile as JSON. Per ARCHITECTURE.md
    /// §13.5's Agent-first philosophy: an Agent can inspect the routing
    /// decision before committing to a full (expensive) parse.
    Classify { path: String },
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
}

/// Run the parsed CLI invocation, returning the process exit code. All
/// diagnostics are written to stderr as a side effect; the result (or a
/// structured error object, for `--format json`) is written to stdout.
pub fn run(cli: Cli) -> i32 {
    match cli.command {
        Command::Parse {
            path,
            format,
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
        } => {
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
                protocol,
                endpoint,
                model,
                window_size,
                max_concurrency,
                pipeline_config,
                no_cache,
                stream,
            )
        }
        Command::Classify { path } => run_classify(path),
        Command::Cache { action } => run_cache(action),
        Command::Doctor { protocol, endpoint } => run_doctor(protocol, endpoint),
        Command::Protocols => run_protocols(),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_parse(
    path: String,
    format: OutputFormat,
    protocol: String,
    endpoint: Option<String>,
    model: Option<String>,
    window_size: usize,
    max_concurrency: usize,
    pipeline_config: PipelineConfig,
    no_cache: bool,
    stream: bool,
) -> i32 {
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

    // `--protocol auto`: run Profiler+Router first (per ARCHITECTURE.md
    // §13.1a/§13.5) and substitute the recommended protocol name into the
    // rest of this function — same registry lookup / native branch below.
    let effective_protocol = if protocol == "auto" {
        let (chosen, reason) = resolve_auto_protocol(&path, &file_bytes);
        eprintln!("auto: routed to {chosen:?} ({reason})");
        chosen
    } else {
        protocol.clone()
    };

    if effective_protocol == "native" {
        // Caching/`--stream` aren't wired into the `native` whole-document
        // path (T-9.1/T-9.2 scope): its entry point bypasses the
        // scheduler entirely (see `run_parse_native`'s own doc comment),
        // and it has no network round-trip to amortize the way every
        // other protocol does — the main cost caching exists to avoid.
        return run_parse_native(path, format, file_bytes);
    }

    let fingerprint = ParamFingerprint {
        protocol: effective_protocol.clone(),
        endpoint: endpoint.clone(),
        model: model.clone(),
    };
    let cache_key = cache::cache_key(&file_bytes, &fingerprint);
    let cache_dir = cache::default_cache_dir();
    if !no_cache && let Some(cached) = cache::get(&cache_dir, &cache_key, DEFAULT_CACHE_TTL) {
        eprintln!("cache: hit for {cache_key}");
        let has_errors = !cached.page_errors.is_empty();
        let output = match format {
            OutputFormat::Json => render::to_json(&cached),
            OutputFormat::Markdown => render::to_markdown(&cached),
        };
        println!("{output}");
        return if has_errors {
            EXIT_PARTIAL
        } else {
            EXIT_SUCCESS
        };
    }

    let registry = Registry::with_builtins();
    let overrides = AdapterOverrides {
        endpoint,
        model,
        pipeline: Some(pipeline_config),
    };
    let Some(adapter) = registry.build(&effective_protocol, &overrides) else {
        return emit_error(
            format,
            EXIT_USAGE,
            "unknown_protocol",
            &format!("unknown protocol: {effective_protocol}"),
            &effective_protocol,
            None,
        );
    };

    let source_sha256 = {
        let mut hasher = Sha256::new();
        hasher.update(&file_bytes);
        format!("{:x}", hasher.finalize())
    };
    let pages = rasterize_or_fallback(&path, &file_bytes);

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            return emit_error(
                format,
                EXIT_INTERNAL,
                "runtime_init_failed",
                &e.to_string(),
                &effective_protocol,
                None,
            );
        }
    };

    let transport = Arc::new(Transport::new());
    let permits = Arc::new(Semaphore::new(max_concurrency.max(1)));
    let scheduler = Scheduler::new(window_size.max(1));

    let (result_pages, page_errors) = if stream {
        runtime.block_on(scheduler.run_streaming(
            adapter,
            transport,
            permits,
            pages,
            |window_pages, window_errors| {
                let line = serde_json::json!({
                    "window_pages": window_pages,
                    "window_errors": window_errors,
                });
                println!(
                    "{}",
                    serde_json::to_string(&line).expect("NDJSON line is always serializable")
                );
            },
        ))
    } else {
        runtime.block_on(scheduler.run(adapter, transport, permits, pages))
    };
    let has_errors = !page_errors.is_empty();

    let result = ParseResult {
        source_path: path.clone(),
        source_sha256,
        protocol: effective_protocol.clone(),
        routed_by: if protocol == "auto" {
            RoutedBy::Auto
        } else {
            RoutedBy::Explicit
        },
        document_profile: None,
        model_endpoint: None,
        model_name: None,
        pages: result_pages,
        page_errors,
        capability_notes: vec![],
        warnings: vec![],
        timing: Default::default(),
    };

    if !no_cache && let Err(e) = cache::put(&cache_dir, &cache_key, &result) {
        eprintln!("warning: failed to write cache entry: {e}");
    }

    if !stream {
        let output = match format {
            OutputFormat::Json => render::to_json(&result),
            OutputFormat::Markdown => render::to_markdown(&result),
        };
        println!("{output}");
    }

    if has_errors {
        EXIT_PARTIAL
    } else {
        EXIT_SUCCESS
    }
}

/// Rasterize the input as a real multi-page PDF when the `pdfium`
/// feature is compiled in and rasterization succeeds. Otherwise, if the
/// input is itself a decodable raster image (PNG/JPEG/etc. — e.g. a page
/// already rendered externally), treat it as one real page with its real
/// dimensions. Only if neither applies (a PDF without `pdfium`, or
/// genuinely non-image garbage bytes) fall back to a degenerate 1x1
/// placeholder page — the original P0 Gate-G0 behavior, preserved
/// exactly for that case so `mock`/tests using arbitrary non-image bytes
/// are unaffected.
///
/// Getting this wrong is not cosmetic: a real VLM adapter denormalizes
/// model bbox output against `page.width`/`page.height` — silently
/// reporting `1x1` for what's actually e.g. a 1275x1651 image collapses
/// every bbox to a single degenerate point, corrupting every stage-2
/// crop (confirmed by a real end-to-end run against a live vLLM
/// endpoint: every block came back `"[Non-Text]"` until this was fixed).
#[cfg_attr(not(feature = "pdfium"), allow(unused_variables))]
fn rasterize_or_fallback(path: &str, file_bytes: &[u8]) -> Vec<RenderedPage> {
    #[cfg(feature = "pdfium")]
    {
        if let Ok(pages) = crate::ingest::rasterize(path, 150.0)
            && !pages.is_empty()
        {
            return pages;
        }
    }

    if let Ok(img) = image::load_from_memory(file_bytes) {
        return vec![RenderedPage {
            page_num: 1,
            width: img.width(),
            height: img.height(),
            png_bytes: file_bytes.to_vec(),
        }];
    }

    vec![RenderedPage {
        page_num: 1,
        width: 1,
        height: 1,
        png_bytes: file_bytes.to_vec(),
    }]
}

/// `native`'s real entry point is the whole-document `parse_document()`,
/// not the per-page `ProtocolAdapter::parse_page()` every other adapter
/// implements (a deliberate P4 design decision — see `adapters/native.rs`),
/// so it can't go through the scheduler-based flow above.
#[cfg(feature = "native")]
fn run_parse_native(path: String, format: OutputFormat, file_bytes: Vec<u8>) -> i32 {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            return emit_error(
                format,
                EXIT_INTERNAL,
                "runtime_init_failed",
                &e.to_string(),
                "native",
                None,
            );
        }
    };

    let adapter = crate::adapters::native::NativeAdapter;
    let result = match runtime.block_on(adapter.parse_document(&path, &file_bytes)) {
        Ok(r) => r,
        Err(e) => {
            return emit_error(
                format,
                EXIT_INTERNAL,
                "native_parse_failed",
                &e.message,
                "native",
                e.stage.as_deref(),
            );
        }
    };

    let has_errors = !result.page_errors.is_empty();
    let output = match format {
        OutputFormat::Json => render::to_json(&result),
        OutputFormat::Markdown => render::to_markdown(&result),
    };
    println!("{output}");

    if has_errors {
        EXIT_PARTIAL
    } else {
        EXIT_SUCCESS
    }
}

#[cfg(not(feature = "native"))]
fn run_parse_native(_path: String, format: OutputFormat, _file_bytes: Vec<u8>) -> i32 {
    emit_error(
        format,
        EXIT_USAGE,
        "unsupported_protocol",
        "the `native` protocol requires building with `--features native`",
        "native",
        None,
    )
}

/// Runs the Profiler only (no protocol adapter, no scheduler) and prints
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

    let format = crate::ingest::detect_format(&bytes, Some(&path));
    let profile = profile_best_effort(&bytes, format);

    let json =
        serde_json::to_string_pretty(&profile).expect("DocumentProfile is always serializable");
    println!("{json}");
    EXIT_SUCCESS
}

/// L2 profiling when possible (native feature + PDF input), falling back
/// to L1 otherwise. Shared by `run_classify` and `resolve_auto_protocol`.
#[cfg_attr(not(feature = "native"), allow(unused_variables))]
fn profile_best_effort(
    bytes: &[u8],
    format: crate::ingest::DocumentFormat,
) -> crate::types::DocumentProfile {
    #[cfg(feature = "native")]
    {
        if format == crate::ingest::DocumentFormat::Pdf {
            match tokio::runtime::Runtime::new() {
                Ok(runtime) => match runtime.block_on(crate::profiler::profile_l2(bytes, format)) {
                    Ok(p) => return p,
                    Err(e) => eprintln!("warning: L2 profiling failed ({e}), falling back to L1"),
                },
                Err(e) => eprintln!(
                    "warning: failed to init runtime for L2 profiling ({e}), falling back to L1"
                ),
            }
        }
    }
    #[cfg(not(feature = "native"))]
    eprintln!(
        "note: built without the `native` feature — only L1 (format-based) profiling is available"
    );

    crate::profiler::profile_l1(format)
}

/// `--protocol auto` support: Profiler + Router, per ARCHITECTURE.md
/// §13.1a/§13.5.
fn resolve_auto_protocol(path: &str, bytes: &[u8]) -> (String, String) {
    let format = crate::ingest::detect_format(bytes, Some(path));
    let profile = profile_best_effort(bytes, format);
    let decision = crate::router::route(&profile);
    (decision.protocol.to_string(), decision.reason)
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
    if format == OutputFormat::Json {
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
    match protocol {
        "mineru-vlm" => {
            Some(crate::adapters::mineru_vlm::MineruVlmAdapter::default().endpoint_base)
        }
        "dots-ocr" => Some(crate::adapters::dots_ocr::DotsOcrAdapter::default().endpoint_base),
        "monkeyocr-v2" => {
            Some(crate::adapters::monkeyocr_v2::MonkeyOcrV2Adapter::default().endpoint_base)
        }
        "paddleocr" => Some(crate::adapters::paddleocr::PaddleOcrAdapter::default().endpoint),
        _ => None,
    }
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
            serde_json::json!({
                "name": adapter.name(),
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
