//! Shared detect -> analyze -> route -> preprocess planning chain.

use crate::frontend::{DocumentFormat, PreflightSource};
use crate::router::{
    RouteCandidate, RouteDecision, RouteOrigin, RoutePreference, RouteReasonCode,
    RoutingEnvironment,
};
use crate::types::{ContentMix, DocumentProfile, ParseResult, RoutedBy};
use crate::{adapters, assets, cache, ingest, postprocess, scheduler, transport};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

pub enum AnalysisArtifacts {
    None,
    Structured(uparser_document_engine::CanonicalDocument),
    #[cfg(feature = "native")]
    Pdf(uparser_native_engine::PdfProcessResult),
}

pub struct AnalysisReport {
    pub profile: DocumentProfile,
    pub artifacts: AnalysisArtifacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputChannel {
    SourceSemantic,
    PdfText,
    VisualPages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionPlan {
    None,
    LibreOfficeToPdf,
    DirectImage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ParseHints {
    pub preserve_toc: bool,
    pub preserve_headings: bool,
    pub preserve_numbered_clauses: bool,
    pub emphasize_tables: bool,
    pub emphasize_formulas: bool,
    pub emphasize_charts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreprocessPlan {
    pub input_channel: InputChannel,
    pub conversion: ConversionPlan,
    pub raster_dpi: Option<u16>,
    pub detect_orientation: bool,
    pub deskew: bool,
    pub parse_hints: ParseHints,
    pub reused_artifacts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunPlan {
    pub route: RouteDecision,
    pub preprocess: PreprocessPlan,
}

pub struct PreparedRun {
    pub source: PreflightSource,
    pub analysis: AnalysisReport,
    pub plan: RunPlan,
}

#[derive(Debug, Clone)]
pub struct ExecutionOptions {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub window_size: usize,
    pub max_concurrency: usize,
    pub pipeline_config: adapters::PipelineConfig,
    pub no_cache: bool,
    pub no_postprocess: bool,
    pub pages: Option<Vec<u32>>,
    pub assets_dir: Option<PathBuf>,
    pub no_assets: bool,
    pub document_options: uparser_document_engine::ParseOptions,
    pub cancellation: crate::frontend::CancellationToken,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            endpoint: None,
            model: None,
            window_size: 64,
            max_concurrency: 16,
            pipeline_config: adapters::PipelineConfig::default(),
            no_cache: false,
            no_postprocess: false,
            pages: None,
            assets_dir: None,
            no_assets: false,
            document_options: uparser_document_engine::ParseOptions::default(),
            cancellation: crate::frontend::CancellationToken::default(),
        }
    }
}

pub struct ParseOutcome {
    pub result: ParseResult,
    pub document: Option<uparser_document_engine::CanonicalDocument>,
    pub engine_markdown: Option<String>,
    pub cache_hit: bool,
}

pub type WindowCallback =
    Arc<dyn Fn(&[crate::types::Page], &[crate::types::PageError], &[String]) + Send + Sync>;
pub type ProgressCallback = Arc<dyn Fn(&scheduler::PageProgress) + Send + Sync>;

#[derive(Clone, Default)]
pub struct ExecutionHooks {
    pub on_window: Option<WindowCallback>,
    pub on_progress: Option<ProgressCallback>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("unknown protocol: {0}")]
    UnknownProtocol(String),
    #[error("ingestion failed: {0}")]
    Ingest(String),
    #[error("native parse failed: {0}")]
    Native(String),
    #[error("structured document parse failed: {0}")]
    Structured(String),
    #[error("asset write failed: {0}")]
    Assets(String),
    #[error("cache write failed: {0}")]
    Cache(String),
    #[error("invalid execution graph: {0}")]
    InvalidStageGraph(String),
    #[error("execution cancelled")]
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("unsupported or unknown input format")]
    UnknownFormat,
    #[error("document analysis failed: {0}")]
    Analysis(String),
    #[error("document preparation cancelled")]
    Cancelled,
    #[error("unknown protocol: {0}")]
    UnknownProtocol(String),
    #[error("protocol {protocol} cannot process {format:?}: {reason}")]
    Unreachable {
        protocol: String,
        format: DocumentFormat,
        reason: String,
    },
}

pub async fn analyze(source: &PreflightSource) -> Result<AnalysisReport, PrepareError> {
    analyze_with_cancellation(source, &crate::frontend::CancellationToken::default()).await
}

pub async fn analyze_with_cancellation(
    source: &PreflightSource,
    cancellation: &crate::frontend::CancellationToken,
) -> Result<AnalysisReport, PrepareError> {
    if cancellation.is_cancelled() {
        return Err(PrepareError::Cancelled);
    }
    let report = analyze_inner(source)?;
    if cancellation.is_cancelled() {
        return Err(PrepareError::Cancelled);
    }
    Ok(report)
}

fn analyze_inner(source: &PreflightSource) -> Result<AnalysisReport, PrepareError> {
    let format = source.format();
    if format == DocumentFormat::Unknown {
        return Err(PrepareError::UnknownFormat);
    }
    if format == DocumentFormat::Pdf {
        #[cfg(feature = "native")]
        {
            let artifact = uparser_native_engine::process_pdf_mem(source.bytes())
                .map_err(|error| PrepareError::Analysis(error.to_string()))?;
            let profile = crate::profiler::profile_l2_result(&artifact, format);
            return Ok(AnalysisReport {
                profile,
                artifacts: AnalysisArtifacts::Pdf(artifact),
            });
        }
        #[cfg(not(feature = "native"))]
        {
            return Ok(AnalysisReport {
                profile: crate::profiler::profile_l1(format),
                artifacts: AnalysisArtifacts::None,
            });
        }
    }
    if is_structured(format) {
        let document = uparser_document_engine::parse_document(
            source.bytes(),
            format,
            &uparser_document_engine::ParseOptions::default(),
        )
        .map_err(|error| PrepareError::Analysis(error.to_string()))?;
        let profile = crate::profiler::profile_structured_document(&document);
        return Ok(AnalysisReport {
            profile,
            artifacts: AnalysisArtifacts::Structured(document),
        });
    }
    Ok(AnalysisReport {
        profile: crate::profiler::profile_l1(format),
        artifacts: AnalysisArtifacts::None,
    })
}

pub async fn prepare(
    source: PreflightSource,
    requested_protocol: Option<&str>,
) -> Result<PreparedRun, PrepareError> {
    prepare_with_preference(source, requested_protocol, RoutePreference::Quality).await
}

pub async fn prepare_with_preference(
    source: PreflightSource,
    requested_protocol: Option<&str>,
    preference: RoutePreference,
) -> Result<PreparedRun, PrepareError> {
    prepare_with_preference_and_cancellation(
        source,
        requested_protocol,
        preference,
        crate::frontend::CancellationToken::default(),
    )
    .await
}

pub async fn prepare_with_preference_and_cancellation(
    source: PreflightSource,
    requested_protocol: Option<&str>,
    preference: RoutePreference,
    cancellation: crate::frontend::CancellationToken,
) -> Result<PreparedRun, PrepareError> {
    let mut analysis = analyze_with_cancellation(&source, &cancellation).await?;
    if requested_protocol.is_none() || requested_protocol == Some("auto") {
        crate::semantic::enrich_from_environment_with_cancellation(
            &mut analysis,
            cancellation.clone(),
        )
        .await
        .map_err(|_| PrepareError::Cancelled)?;
    }
    if cancellation.is_cancelled() {
        return Err(PrepareError::Cancelled);
    }
    let route = match requested_protocol.filter(|name| *name != "auto") {
        Some(protocol) => explicit_route(protocol, &analysis.profile)?,
        None => crate::router::route_with_preference(
            &analysis.profile,
            RoutingEnvironment::default(),
            preference,
        ),
    };
    let preprocess = preprocess_plan(source.format(), &analysis.profile, &route.protocol)?;
    Ok(PreparedRun {
        source,
        analysis,
        plan: RunPlan { route, preprocess },
    })
}

pub async fn execute(
    prepared: PreparedRun,
    options: &ExecutionOptions,
) -> Result<ParseOutcome, ExecutionError> {
    execute_with_hooks(prepared, options, &ExecutionHooks::default()).await
}

pub async fn execute_with_hooks(
    prepared: PreparedRun,
    options: &ExecutionOptions,
    hooks: &ExecutionHooks,
) -> Result<ParseOutcome, ExecutionError> {
    let PreparedRun {
        source,
        analysis,
        plan,
    } = prepared;
    let source_path = source.filename_hint().unwrap_or("<memory>").to_owned();
    let protocol = plan.route.protocol.clone();
    let routed_by = match plan.route.origin {
        RouteOrigin::Explicit => RoutedBy::Explicit,
        RouteOrigin::Auto => RoutedBy::Auto,
    };

    if protocol == "native" {
        return execute_native(source, analysis, plan, options, source_path, routed_by).await;
    }

    let fingerprint = cache::ParamFingerprint {
        protocol: protocol.clone(),
        endpoint: options.endpoint.clone(),
        model: options.model.clone(),
        execution: Some(execution_fingerprint(options, &plan)),
    };
    let cache_key = cache::cache_key(source.bytes(), &fingerprint);
    let cache_dir = cache::default_cache_dir();
    if !options.no_cache
        && let Some(mut result) = cache::get(&cache_dir, &cache_key, DEFAULT_CACHE_TTL)
    {
        attach_execution_metadata(&mut result, analysis.profile, plan, routed_by, options);
        return Ok(ParseOutcome {
            result,
            document: None,
            engine_markdown: None,
            cache_hit: true,
        });
    }

    let registry = adapters::Registry::with_builtins();
    let overrides = adapters::AdapterOverrides {
        endpoint: options.endpoint.clone(),
        model: options.model.clone(),
        pipeline: Some(options.pipeline_config.clone()),
    };
    let adapter = registry
        .build(&protocol, &overrides)
        .ok_or_else(|| ExecutionError::UnknownProtocol(protocol.clone()))?;
    validate_execution_shape(adapter.spec())?;
    if options.cancellation.is_cancelled() {
        return Err(ExecutionError::Cancelled);
    }
    let mut page_source = materialize_page_source(
        &source,
        plan.preprocess.raster_dpi.unwrap_or(150),
        options.pages.as_deref(),
        options.cancellation.clone(),
    )
    .await?;
    if options.cancellation.is_cancelled() {
        return Err(ExecutionError::Cancelled);
    }

    let scheduler =
        scheduler::Scheduler::new(options.window_size.max(options.max_concurrency).max(1))
            .with_cancellation(options.cancellation.clone());
    let transport = Arc::new(transport::Transport::new());
    let permits = Arc::new(Semaphore::new(options.max_concurrency.max(1)));
    let on_window = hooks.on_window.clone();
    let on_progress = hooks.on_progress.clone();
    let assets_dir = (!options.no_assets).then(|| {
        options
            .assets_dir
            .clone()
            .unwrap_or_else(|| assets::default_assets_dir(&source_path))
    });
    let (pages, page_errors, warnings) = scheduler
        .run_source(
            adapter,
            transport,
            permits,
            page_source.as_mut(),
            move |pages, errors, warnings| {
                let Some(on_window) = &on_window else {
                    return;
                };
                let mut pages = postprocess_pages(pages.to_vec(), options.no_postprocess);
                if let Some(directory) = &assets_dir
                    && let Err(error) = assets::write_page_assets(&mut pages, directory)
                {
                    eprintln!("warning: failed to write streamed image assets: {error}");
                }
                on_window(&pages, errors, warnings);
            },
            move |event| {
                if let Some(on_progress) = &on_progress {
                    on_progress(event);
                }
            },
        )
        .await
        .map_err(|error| match error {
            crate::frontend::PageSourceError::Cancelled => ExecutionError::Cancelled,
            crate::frontend::PageSourceError::Production(message) => {
                ExecutionError::Ingest(message)
            }
        })?;
    let pages = postprocess_pages(pages, options.no_postprocess);
    let mut result = ParseResult {
        source_path: source_path.clone(),
        source_sha256: source.digest().to_owned(),
        protocol,
        routed_by: routed_by.clone(),
        document_profile: Some(analysis.profile),
        route_decision: Some(plan.route),
        preprocess_plan: Some(plan.preprocess),
        model_endpoint: options.endpoint.clone(),
        model_name: options.model.clone(),
        pages,
        page_errors,
        capability_notes: Vec::new(),
        warnings,
        timing: Default::default(),
    };
    write_result_assets(&mut result, &source_path, options)?;
    if !options.no_cache {
        cache::put(&cache_dir, &cache_key, &result)
            .map_err(|error| ExecutionError::Cache(error.to_string()))?;
    }
    Ok(ParseOutcome {
        result,
        document: None,
        engine_markdown: None,
        cache_hit: false,
    })
}

fn validate_execution_shape(
    spec: &crate::protocol_spec::ProtocolSpec,
) -> Result<(), ExecutionError> {
    if spec.shape == crate::protocol_spec::ProtocolShape::StageGraph {
        crate::stage_graph::PIPELINE_STAGE_GRAPH
            .validate()
            .map_err(|error| ExecutionError::InvalidStageGraph(error.to_string()))?;
    }
    Ok(())
}

fn execution_fingerprint(options: &ExecutionOptions, plan: &RunPlan) -> String {
    serde_json::to_string(&serde_json::json!({
        "plan": plan,
        "window_size": options.window_size,
        "max_concurrency": options.max_concurrency,
        "pipeline": options.pipeline_config,
        "no_postprocess": options.no_postprocess,
        "pages": options.pages,
        "assets_dir": options.assets_dir,
        "no_assets": options.no_assets,
        "document": {
            "include_assets": options.document_options.include_assets,
            "include_notes": options.document_options.include_notes,
            "include_headers_footers": options.document_options.include_headers_footers,
            "max_input_bytes": options.document_options.limits.max_input_bytes,
            "max_entry_bytes": options.document_options.limits.max_entry_bytes,
            "max_total_uncompressed_bytes": options.document_options.limits.max_total_uncompressed_bytes,
            "max_archive_entries": options.document_options.limits.max_archive_entries,
            "max_xml_depth": options.document_options.limits.max_xml_depth,
            "max_record_depth": options.document_options.limits.max_record_depth,
            "max_xml_nodes": options.document_options.limits.max_xml_nodes,
            "max_expansion": options.document_options.limits.max_expansion,
            "max_asset_bytes": options.document_options.limits.max_asset_bytes,
            "max_text_bytes": options.document_options.limits.max_text_bytes,
        }
    }))
    .expect("execution fingerprint consists only of serializable values")
}

fn postprocess_pages(
    pages: Vec<crate::types::Page>,
    no_postprocess: bool,
) -> Vec<crate::types::Page> {
    if no_postprocess {
        pages
    } else {
        pages
            .into_iter()
            .map(|page| crate::types::Page {
                blocks: postprocess::merge_paragraphs_by_geometry(page.blocks),
                ..page
            })
            .collect()
    }
}

async fn execute_native(
    source: PreflightSource,
    analysis: AnalysisReport,
    plan: RunPlan,
    options: &ExecutionOptions,
    source_path: String,
    routed_by: RoutedBy,
) -> Result<ParseOutcome, ExecutionError> {
    match analysis.artifacts {
        AnalysisArtifacts::Structured(document) => {
            let document = if document_options_are_default(&options.document_options) {
                document
            } else {
                uparser_document_engine::parse_document(
                    source.bytes(),
                    source.format(),
                    &options.document_options,
                )
                .map_err(|error| ExecutionError::Structured(error.to_string()))?
            };
            let mut result =
                crate::structured::to_parse_result(&document, &source_path, source.bytes());
            attach_execution_metadata(&mut result, analysis.profile, plan, routed_by, options);
            write_result_assets(&mut result, &source_path, options)?;
            Ok(ParseOutcome {
                result,
                document: Some(document),
                engine_markdown: None,
                cache_hit: false,
            })
        }
        #[cfg(feature = "native")]
        AnalysisArtifacts::Pdf(artifact) => {
            let (mut result, engine_markdown) =
                crate::adapters::native::NativeAdapter::parse_pdf_artifact(
                    &source_path,
                    source.bytes(),
                    artifact,
                );
            attach_execution_metadata(&mut result, analysis.profile, plan, routed_by, options);
            write_result_assets(&mut result, &source_path, options)?;
            Ok(ParseOutcome {
                result,
                document: None,
                engine_markdown,
                cache_hit: false,
            })
        }
        AnalysisArtifacts::None => Err(ExecutionError::Native(
            "native route has no reusable document artifact".to_owned(),
        )),
    }
}

fn attach_execution_metadata(
    result: &mut ParseResult,
    profile: DocumentProfile,
    plan: RunPlan,
    routed_by: RoutedBy,
    options: &ExecutionOptions,
) {
    result.routed_by = routed_by;
    result.document_profile = Some(profile);
    result.route_decision = Some(plan.route);
    result.preprocess_plan = Some(plan.preprocess);
    result.model_endpoint = options.endpoint.clone();
    result.model_name = options.model.clone();
}

fn document_options_are_default(options: &uparser_document_engine::ParseOptions) -> bool {
    let defaults = uparser_document_engine::ParseOptions::default();
    options.include_assets == defaults.include_assets
        && options.include_notes == defaults.include_notes
        && options.include_headers_footers == defaults.include_headers_footers
        && options.limits.max_input_bytes == defaults.limits.max_input_bytes
        && options.limits.max_entry_bytes == defaults.limits.max_entry_bytes
        && options.limits.max_total_uncompressed_bytes
            == defaults.limits.max_total_uncompressed_bytes
        && options.limits.max_archive_entries == defaults.limits.max_archive_entries
        && options.limits.max_xml_depth == defaults.limits.max_xml_depth
        && options.limits.max_record_depth == defaults.limits.max_record_depth
        && options.limits.max_xml_nodes == defaults.limits.max_xml_nodes
        && options.limits.max_expansion == defaults.limits.max_expansion
        && options.limits.max_asset_bytes == defaults.limits.max_asset_bytes
        && options.limits.max_text_bytes == defaults.limits.max_text_bytes
}

fn write_result_assets(
    result: &mut ParseResult,
    source_path: &str,
    options: &ExecutionOptions,
) -> Result<(), ExecutionError> {
    if options.no_assets {
        return Ok(());
    }
    let directory = options
        .assets_dir
        .clone()
        .unwrap_or_else(|| assets::default_assets_dir(source_path));
    assets::write_block_assets(result, &directory)
        .map(|_| ())
        .map_err(|error| ExecutionError::Assets(error.to_string()))
}

async fn materialize_page_source(
    source: &PreflightSource,
    dpi: u16,
    selected_pages: Option<&[u32]>,
    cancellation: crate::frontend::CancellationToken,
) -> Result<Box<dyn crate::frontend::PageSource>, ExecutionError> {
    if source.bytes().is_empty() {
        return Err(ExecutionError::Ingest("input is empty".to_owned()));
    }
    let bytes = source.bytes();
    match source.format() {
        DocumentFormat::Png | DocumentFormat::Jpeg => {
            let image = image::load_from_memory(bytes)
                .map_err(|error| ExecutionError::Ingest(error.to_string()))?;
            let mut pages = vec![ingest::RenderedPage {
                page_num: 1,
                width: image.width(),
                height: image.height(),
                png_bytes: bytes.to_vec(),
            }];
            if let Some(selected) = selected_pages {
                pages.retain(|page| selected.contains(&page.page_num));
            }
            Ok(Box::new(crate::frontend::MemoryPageSource::new(
                source.format(),
                source.digest(),
                pages,
                cancellation,
            )))
        }
        DocumentFormat::Pdf => crate::frontend::pdf_page_source(
            source.shared_bytes(),
            source.digest(),
            dpi as f32,
            selected_pages,
            cancellation,
        )
        .map_err(|error| ExecutionError::Ingest(error.to_string())),
        format if is_structured(format) => {
            let conversion_cancellation = cancellation.clone();
            let pdf = tokio::select! {
                biased;
                _ = conversion_cancellation.cancelled() => {
                    return Err(ExecutionError::Cancelled);
                }
                result = ingest::normalize_format(bytes, format) => {
                    result.map_err(|error| ExecutionError::Ingest(error.to_string()))?
                }
            };
            crate::frontend::pdf_page_source(
                Arc::<[u8]>::from(pdf),
                source.digest(),
                dpi as f32,
                selected_pages,
                cancellation,
            )
            .map_err(|error| ExecutionError::Ingest(error.to_string()))
        }
        _ => Err(ExecutionError::Ingest(
            "unsupported input cannot be materialized as pages".to_owned(),
        )),
    }
}

fn explicit_route(
    protocol: &str,
    profile: &DocumentProfile,
) -> Result<RouteDecision, PrepareError> {
    let protocol = canonical_protocol(protocol)
        .ok_or_else(|| PrepareError::UnknownProtocol(protocol.to_owned()))?;
    let environment = RoutingEnvironment::default();
    let family_available = match protocol {
        "native" => environment.native || is_structured(profile.source_format),
        // Explicit mode selection is an operator assertion that the remote
        // stage deployment is available; auto remains conservative.
        "pipeline" => true,
        _ => environment.model_protocol,
    };
    if !family_available {
        return Err(PrepareError::Unreachable {
            protocol: protocol.to_owned(),
            format: profile.source_format,
            reason: "required compiled/runtime capability is unavailable".to_owned(),
        });
    }
    if protocol == "native"
        && matches!(
            profile.source_quality,
            crate::types::SourceQuality::Scanned
                | crate::types::SourceQuality::ImageOnly
                | crate::types::SourceQuality::Unknown
        )
    {
        return Err(PrepareError::Unreachable {
            protocol: protocol.to_owned(),
            format: profile.source_format,
            reason: "no reliable native text or source semantics".to_owned(),
        });
    }
    Ok(RouteDecision {
        protocol: protocol.to_owned(),
        origin: RouteOrigin::Explicit,
        reason: format!("explicit protocol {protocol}"),
        confidence: 1.0,
        candidates: vec![RouteCandidate {
            protocol: protocol.to_owned(),
            score: 100,
            feasible: true,
            reason_codes: vec![RouteReasonCode::GenreStructure],
            rejection: None,
        }],
    })
}

fn canonical_protocol(protocol: &str) -> Option<&'static str> {
    Some(match protocol {
        "native" => "native",
        "mineru-vlm" => "mineru-vlm",
        "dots-ocr" => "dots-ocr",
        "generic-vlm" => "generic-vlm",
        "monkeyocr-v2" => "monkeyocr-v2",
        "paddleocr" => "paddleocr",
        "paddlex-structure" => "paddlex-structure",
        "pipeline" => "pipeline",
        "mock" => "mock",
        _ => return None,
    })
}

pub fn preprocess_plan(
    format: DocumentFormat,
    profile: &DocumentProfile,
    protocol: &str,
) -> Result<PreprocessPlan, PrepareError> {
    let native = protocol == "native";
    let (input_channel, conversion, raster_dpi, reused_artifacts) = if native {
        if format == DocumentFormat::Pdf {
            (
                InputChannel::PdfText,
                ConversionPlan::None,
                None,
                vec!["native_pdf_analysis".to_owned()],
            )
        } else if is_structured(format) {
            (
                InputChannel::SourceSemantic,
                ConversionPlan::None,
                None,
                vec!["canonical_source_document".to_owned()],
            )
        } else {
            return Err(PrepareError::Unreachable {
                protocol: protocol.to_owned(),
                format,
                reason: "native does not accept image-only input".to_owned(),
            });
        }
    } else if format == DocumentFormat::Pdf {
        (
            InputChannel::VisualPages,
            ConversionPlan::None,
            Some(150),
            vec!["sampled_pages".to_owned()],
        )
    } else if matches!(format, DocumentFormat::Png | DocumentFormat::Jpeg) {
        (
            InputChannel::VisualPages,
            ConversionPlan::DirectImage,
            None,
            Vec::new(),
        )
    } else if is_structured(format) {
        (
            InputChannel::VisualPages,
            ConversionPlan::LibreOfficeToPdf,
            Some(150),
            vec!["canonical_source_document".to_owned()],
        )
    } else {
        return Err(PrepareError::UnknownFormat);
    };
    Ok(PreprocessPlan {
        input_channel,
        conversion,
        raster_dpi,
        detect_orientation: matches!(
            profile.source_quality,
            crate::types::SourceQuality::Scanned | crate::types::SourceQuality::ImageOnly
        ),
        deskew: matches!(profile.source_quality, crate::types::SourceQuality::Scanned),
        parse_hints: ParseHints {
            preserve_toc: profile.structure.has_toc.unwrap_or(false),
            preserve_headings: profile.structure.heading_depth.is_some(),
            preserve_numbered_clauses: profile.structure.numbered_clause_density > 0.02,
            emphasize_tables: profile.dominant_content == ContentMix::TableDense,
            emphasize_formulas: matches!(
                profile.genre.primary,
                crate::types::DocumentGenre::AcademicPaper
                    | crate::types::DocumentGenre::FinancialReport
            ),
            emphasize_charts: profile
                .page_profiles
                .iter()
                .any(|page| page.has_chart_region),
        },
        reused_artifacts,
    })
}

pub fn is_structured(format: DocumentFormat) -> bool {
    !matches!(
        format,
        DocumentFormat::Pdf | DocumentFormat::Png | DocumentFormat::Jpeg | DocumentFormat::Unknown
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[cfg(feature = "native")]
    #[tokio::test]
    async fn native_runner_reuses_pdf_artifact_and_preserves_engine_markdown() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../opensource/liteparse/integration_tests_data/sample.pdf"
        );
        let bytes: Arc<[u8]> = std::fs::read(fixture).unwrap().into();
        let expected = uparser_native_engine::process_pdf_mem(&bytes)
            .unwrap()
            .markdown
            .unwrap_or_default();
        let source = PreflightSource::new(bytes, Some("sample.pdf"));
        let prepared = prepare(source, Some("native")).await.unwrap();
        let outcome = execute(
            prepared,
            &ExecutionOptions {
                no_cache: true,
                no_assets: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(outcome.engine_markdown.as_deref(), Some(expected.as_str()));
        assert!(!outcome.result.pages.is_empty());
    }

    #[tokio::test]
    async fn csv_is_analyzed_before_native_plan() {
        let source = PreflightSource::new(
            Arc::<[u8]>::from(&b"name,age\nAlice,30\n"[..]),
            Some("people.csv"),
        );
        let report = analyze(&source).await.unwrap();
        assert_eq!(report.profile.source_format, DocumentFormat::Csv);
        assert_eq!(
            report.profile.genre.primary,
            crate::types::DocumentGenre::Spreadsheet
        );
    }

    #[test]
    fn visual_office_plan_converts_only_after_routing() {
        let profile = crate::profiler::profile_l1(DocumentFormat::Docx);
        let plan = preprocess_plan(DocumentFormat::Docx, &profile, "mineru-vlm").unwrap();
        assert_eq!(plan.conversion, ConversionPlan::LibreOfficeToPdf);
        assert_eq!(plan.input_channel, InputChannel::VisualPages);
    }

    #[tokio::test]
    async fn cancelled_execution_stops_before_adapter_dispatch() {
        let image = image::RgbImage::from_pixel(1, 1, image::Rgb([255, 255, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        let source = PreflightSource::new(Arc::<[u8]>::from(bytes), Some("page.png"));
        let prepared = prepare(source, Some("mock")).await.unwrap();
        let cancellation = crate::frontend::CancellationToken::default();
        cancellation.cancel();
        let options = ExecutionOptions {
            no_cache: true,
            cancellation,
            ..ExecutionOptions::default()
        };
        assert!(matches!(
            execute(prepared, &options).await,
            Err(ExecutionError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn pre_cancelled_structured_materialization_never_starts_conversion() {
        let source = PreflightSource::new(
            Arc::<[u8]>::from(&b"name,age\nAlice,30\n"[..]),
            Some("people.csv"),
        );
        let cancellation = crate::frontend::CancellationToken::default();
        cancellation.cancel();

        assert!(matches!(
            materialize_page_source(&source, 150, None, cancellation).await,
            Err(ExecutionError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn pre_cancelled_analysis_returns_a_typed_prepare_error() {
        let source = PreflightSource::new(
            Arc::<[u8]>::from(&b"name,age\nAlice,30\n"[..]),
            Some("people.csv"),
        );
        let cancellation = crate::frontend::CancellationToken::default();
        cancellation.cancel();

        assert!(matches!(
            analyze_with_cancellation(&source, &cancellation).await,
            Err(PrepareError::Cancelled)
        ));
    }
}
