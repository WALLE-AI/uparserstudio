//! Shared detect -> analyze -> route -> preprocess planning chain.

use crate::frontend::{DocumentFormat, PreflightSource};
use crate::router::{
    RouteCandidate, RouteDecision, RouteOrigin, RoutePreference, RouteReasonCode,
    RoutingEnvironment,
};
use crate::types::{ContentMix, DocumentProfile, ParseResult, RoutedBy};
use crate::{adapters, assets, cache, ingest, postprocess, scheduler, transport};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Default PDF rasterization DPI for visual-page protocols, matching
/// MinerU's own `DEFAULT_PDF_IMAGE_DPI` (`opensource/MinerU/mineru/utils/pdf_image_tools.py`).
/// Previously hardcoded to `150` here — a 25% lower linear resolution
/// than what OCR/table/formula models are tuned against — with no way to
/// override it. See D7 in `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`.
pub const DEFAULT_RASTER_DPI: u16 = 200;

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
    /// Overrides `PreprocessPlan::raster_dpi` for visual-page protocols
    /// when set. `None` (the default) uses the plan's own DPI, which for
    /// PDF/converted-structured inputs is `DEFAULT_RASTER_DPI`. Has no
    /// effect on `native`/already-image inputs, which don't rasterize at
    /// all. See D7 in `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`.
    pub raster_dpi: Option<u16>,
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
            raster_dpi: None,
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
        options
            .raster_dpi
            .or(plan.preprocess.raster_dpi)
            .unwrap_or(DEFAULT_RASTER_DPI),
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
            let mut document = if document_options_require_reparse(&options.document_options) {
                uparser_document_engine::parse_document(
                    source.bytes(),
                    source.format(),
                    &options.document_options,
                )
                .map_err(|error| ExecutionError::Structured(error.to_string()))?
            } else {
                document
            };
            if !options.document_options.include_assets {
                for asset in &mut document.assets {
                    asset.bytes = None;
                }
            }
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
            #[cfg(feature = "pdfium")]
            let ocr_request = hybrid_ocr_request(&artifact, options.pages.as_deref());
            let (mut result, mut engine_markdown) =
                crate::adapters::native::NativeAdapter::parse_pdf_artifact(
                    &source_path,
                    source.bytes(),
                    artifact,
                );
            #[cfg(feature = "pdfium")]
            if apply_hybrid_ocr(source.bytes(), &mut result, ocr_request).await {
                // Engine markdown still contains the broken native text. Let the
                // CLI render the merged page IR instead when OCR replaced a page.
                engine_markdown = None;
            }
            attach_execution_metadata(&mut result, analysis.profile, plan, routed_by, options);
            #[cfg(feature = "pdfium")]
            materialize_native_image_assets(
                source.bytes(),
                &mut result,
                options.no_assets,
                options.document_options.limits.max_asset_bytes,
            );
            #[cfg(not(feature = "pdfium"))]
            annotate_unavailable_native_image_assets(&mut result, options.no_assets);
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

#[cfg(all(feature = "native", not(feature = "pdfium")))]
fn annotate_unavailable_native_image_assets(result: &mut ParseResult, no_assets: bool) {
    if no_assets {
        return;
    }
    let regions = result
        .pages
        .iter()
        .flat_map(|page| &page.blocks)
        .filter(|block| matches!(block.category.as_deref(), Some("image" | "chart")))
        .count();
    if regions == 0 {
        return;
    }
    result.warnings.push(format!(
        "asset_unavailable: {regions} native image region(s) retained with geometry; PDFium support is not compiled"
    ));
    result.capability_notes.push(format!(
        "native image assets: materialized=0, unavailable={regions}, reason=pdfium_feature_disabled"
    ));
}

fn attach_execution_metadata(
    result: &mut ParseResult,
    profile: DocumentProfile,
    plan: RunPlan,
    routed_by: RoutedBy,
    options: &ExecutionOptions,
) {
    let is_technical_standard =
        profile.genre.primary == crate::types::DocumentGenre::TechnicalStandard;
    result.routed_by = routed_by;
    result.document_profile = Some(profile);
    result.route_decision = Some(plan.route);
    result.preprocess_plan = Some(plan.preprocess);
    result.model_endpoint = options.endpoint.clone();
    result.model_name = options.model.clone();
    if is_technical_standard {
        annotate_technical_standard_ir(result);
    }
}

fn annotate_technical_standard_ir(result: &mut ParseResult) {
    let declaration_text = result
        .pages
        .iter()
        .flat_map(|page| &page.blocks)
        .filter_map(|block| block.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    let mandatory_ids = declared_mandatory_clause_ids(&declaration_text);
    let mut clause_count = 0usize;
    let mut mandatory_count = 0usize;
    let mut annex_count = 0usize;

    for block in result.pages.iter_mut().flat_map(|page| &mut page.blocks) {
        let Some(text) = block.text.as_deref() else {
            continue;
        };
        if let Some(identifier) = leading_normative_clause_id(text)
            && !text.contains("强制性条文")
        {
            clause_count += 1;
            if mandatory_ids.contains(identifier) {
                block.category = Some("mandatory_clause".to_owned());
                mandatory_count += 1;
            } else {
                block.category = Some("normative_clause".to_owned());
            }
        } else if is_annex_heading(text) {
            block.category = Some("annex_heading".to_owned());
            annex_count += 1;
        }
    }

    if clause_count > 0 || annex_count > 0 {
        let mandatory_list = mandatory_ids.into_iter().collect::<Vec<_>>().join(",");
        result.capability_notes.push(format!(
            "technical-standard IR: clauses={clause_count}, mandatory={mandatory_count} [{mandatory_list}], annex_headings={annex_count}"
        ));
    }
}

fn declared_mandatory_clause_ids(text: &str) -> BTreeSet<String> {
    let clause_pattern =
        regex::Regex::new(r"\b\d+(?:\.\d+){2,}\b").expect("static normative clause regex is valid");
    let mut identifiers = BTreeSet::new();
    for (phrase_start, _) in text.match_indices("强制性条文") {
        let prefix_start = text[..phrase_start]
            .char_indices()
            .rev()
            .nth(120)
            .map(|(index, _)| index)
            .unwrap_or(0);
        for capture in clause_pattern.find_iter(&text[prefix_start..phrase_start]) {
            identifiers.insert(capture.as_str().to_owned());
        }
    }
    identifiers
}

fn leading_normative_clause_id(text: &str) -> Option<&str> {
    let trimmed = text.trim_start_matches(|character: char| {
        character.is_whitespace() || matches!(character, '#' | '*' | '_')
    });
    let end = trimmed
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit() || *character == '.')
        .map(|(index, character)| index + character.len_utf8())
        .last()?;
    let candidate = trimmed[..end].trim_end_matches('.');
    let mut components = candidate.split('.');
    let valid = components.clone().count() >= 3
        && components.all(|component| {
            !component.is_empty()
                && component
                    .chars()
                    .all(|character| character.is_ascii_digit())
        });
    valid.then_some(candidate)
}

fn is_annex_heading(text: &str) -> bool {
    let compact: String = text
        .trim_start_matches(|character: char| {
            character.is_whitespace() || matches!(character, '#' | '*' | '_')
        })
        .chars()
        .filter(|character| !character.is_whitespace())
        .take(5)
        .collect();
    let mut characters = compact.chars();
    matches!((characters.next(), characters.next(), characters.next()),
        (Some('附'), Some('录' | '錄'), Some(letter)) if letter.is_ascii_uppercase())
}

fn document_options_require_reparse(options: &uparser_document_engine::ParseOptions) -> bool {
    let defaults = uparser_document_engine::ParseOptions::default();
    options.include_notes != defaults.include_notes
        || options.include_headers_footers != defaults.include_headers_footers
        || options.limits.max_input_bytes != defaults.limits.max_input_bytes
        || options.limits.max_entry_bytes != defaults.limits.max_entry_bytes
        || options.limits.max_total_uncompressed_bytes
            != defaults.limits.max_total_uncompressed_bytes
        || options.limits.max_archive_entries != defaults.limits.max_archive_entries
        || options.limits.max_xml_depth != defaults.limits.max_xml_depth
        || options.limits.max_record_depth != defaults.limits.max_record_depth
        || options.limits.max_xml_nodes != defaults.limits.max_xml_nodes
        || options.limits.max_expansion != defaults.limits.max_expansion
        || options.limits.max_asset_bytes != defaults.limits.max_asset_bytes
        || options.limits.max_text_bytes != defaults.limits.max_text_bytes
}

#[cfg(all(feature = "native", feature = "pdfium"))]
fn hybrid_ocr_request(
    artifact: &uparser_native_engine::PdfProcessResult,
    selected_pages: Option<&[u32]>,
) -> Option<(Vec<u32>, String)> {
    let mut pages: std::collections::BTreeSet<u32> = artifact
        .ocr_reasons_by_page
        .iter()
        .filter(|entry| {
            entry.reasons.iter().any(|reason| {
                matches!(
                    reason.as_str(),
                    uparser_native_engine::OCR_REASON_SUSPECTED_GARBLED_TEXT
                        | uparser_native_engine::OCR_REASON_SCANNED
                )
            }) && selected_pages
                .map(|selected| selected.contains(&entry.page))
                .unwrap_or(true)
        })
        .map(|entry| entry.page)
        .collect();
    pages.extend(
        artifact
            .positioned_items
            .iter()
            .filter(|item| {
                uparser_native_engine::looks_like_gbk_utf8_mojibake(&item.text)
                    && selected_pages
                        .map(|selected| selected.contains(&item.page))
                        .unwrap_or(true)
            })
            .map(|item| item.page),
    );
    if pages.is_empty() {
        return None;
    }
    let pages = pages.into_iter().collect();
    let language = std::env::var("UPARSER_OCR_LANG").unwrap_or_else(|_| {
        let text = artifact.markdown.as_deref().unwrap_or_default();
        if text.chars().any(is_cjk) {
            if likely_traditional_chinese(text) {
                "chi_tra+eng".to_owned()
            } else {
                "chi_sim+eng".to_owned()
            }
        } else {
            "eng".to_owned()
        }
    });
    Some((pages, language))
}

#[cfg(all(feature = "native", feature = "pdfium"))]
async fn apply_hybrid_ocr(
    pdf_bytes: &[u8],
    result: &mut ParseResult,
    request: Option<(Vec<u32>, String)>,
) -> bool {
    let Some((pages, language)) = request else {
        return false;
    };
    let adapter = crate::adapters::local_tesseract::TesseractAdapter::with_language(&language);
    if !adapter.has_language() {
        result.warnings.push(format!(
            "hybrid OCR skipped pages {:?}: Tesseract language data {language} is unavailable",
            pages
        ));
        return false;
    }
    let rendered = match ingest::rasterize_pdf_page_numbers(pdf_bytes, 300.0, &pages) {
        Ok(rendered) => rendered,
        Err(error) => {
            result.warnings.push(format!(
                "hybrid OCR skipped pages {:?}: PDFium rasterization failed: {error}",
                pages
            ));
            return false;
        }
    };
    let mut replaced = Vec::new();
    for page in rendered {
        match adapter.recognize_page(&page).await {
            Ok(mut blocks) if !blocks.is_empty() => {
                let reconciled =
                    reconcile_ocr_toc_with_native(&result.pages, page.page_num, &mut blocks);
                if let Some(existing) = result
                    .pages
                    .iter()
                    .find(|candidate| candidate.page_num == page.page_num)
                {
                    blocks.extend(
                        existing
                            .blocks
                            .iter()
                            .filter(|block| {
                                matches!(block.category.as_deref(), Some("image" | "chart"))
                            })
                            .cloned(),
                    );
                    blocks.sort_by_key(|block| {
                        let bbox = block.bbox_px.unwrap_or([0; 4]);
                        (bbox[1], bbox[0])
                    });
                }
                let replacement = crate::types::Page {
                    page_num: page.page_num,
                    width_px: page.width,
                    height_px: page.height,
                    blocks,
                };
                if let Some(existing) = result
                    .pages
                    .iter_mut()
                    .find(|candidate| candidate.page_num == page.page_num)
                {
                    *existing = replacement;
                } else {
                    result.pages.push(replacement);
                }
                replaced.push(page.page_num);
                if reconciled > 0 {
                    result.capability_notes.push(format!(
                        "OCR TOC reconciliation: {reconciled} entries aligned with native body headings on page {}",
                        page.page_num
                    ));
                }
            }
            Ok(_) => result.warnings.push(format!(
                "hybrid OCR produced no text for page {}",
                page.page_num
            )),
            Err(error) => result.page_errors.push(error),
        }
    }
    if replaced.is_empty() {
        return false;
    }
    result.pages.sort_by_key(|page| page.page_num);
    result.capability_notes.push(format!(
        "hybrid page OCR: native text preserved; Tesseract ({language}) replaced pages {:?}",
        replaced
    ));
    true
}

#[cfg(all(feature = "native", feature = "pdfium"))]
fn reconcile_ocr_toc_with_native(
    native_pages: &[crate::types::Page],
    ocr_page_num: u32,
    blocks: &mut [crate::types::Block],
) -> usize {
    let mut headings = std::collections::BTreeMap::<String, (String, u32)>::new();
    for page in native_pages
        .iter()
        .filter(|page| page.page_num != ocr_page_num)
    {
        for block in &page.blocks {
            let Some(text) = block.text.as_deref() else {
                continue;
            };
            let Some((identifier, title)) = split_numbered_heading(text) else {
                continue;
            };
            if title.chars().count() <= 40
                && title.chars().filter(|character| is_cjk(*character)).count() >= 2
                && !title
                    .chars()
                    .any(|character| matches!(character, '。' | '；' | ';' | '：' | ':'))
                && !title.contains("应符合")
            {
                let replace = headings
                    .get(&identifier)
                    .is_none_or(|(current, _)| title.chars().count() < current.chars().count());
                if replace {
                    headings.insert(identifier, (title, page.page_num));
                }
            }
        }
    }

    let matches: Vec<(usize, String, String, u32, Option<u32>)> = blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            let (identifier, _) = split_numbered_heading(block.text.as_deref()?)?;
            let (title, body_page) = headings.get(&identifier)?;
            Some((
                index,
                identifier,
                title.clone(),
                *body_page,
                trailing_page_number(block.text.as_deref()?),
            ))
        })
        .collect();
    if matches.len() < 5 {
        return 0;
    }

    let mut offset_counts = std::collections::BTreeMap::<u32, usize>::new();
    for (_, _, _, body_page, printed_page) in &matches {
        if let Some(printed_page) = printed_page
            && let Some(offset) = body_page.checked_sub(*printed_page)
            && offset <= 10
        {
            *offset_counts.entry(offset).or_default() += 1;
        }
    }
    let page_offset = offset_counts
        .into_iter()
        .max_by_key(|(offset, count)| (*count, std::cmp::Reverse(*offset)))
        .map(|(offset, _)| offset)
        .unwrap_or(0);

    for (index, identifier, title, body_page, ocr_page) in &matches {
        let printed_page = ocr_page
            .filter(|page| {
                body_page
                    .checked_sub(*page)
                    .is_some_and(|offset| offset <= 3)
            })
            .unwrap_or_else(|| body_page.saturating_sub(page_offset));
        let repaired = format!("{identifier} {title} {printed_page}");
        let block = &mut blocks[*index];
        block.text = Some(repaired.clone());
        block.category_raw = "toc_entry".to_owned();
        block.category = Some("list".to_owned());
        block.spans = vec![crate::types::Span {
            text: repaired,
            bbox_px: block.bbox_px,
            font_size: None,
            is_inline_formula: false,
        }];
    }
    let mut reconciled = matches.len();

    let appendix_a_title = native_pages
        .iter()
        .flat_map(|page| &page.blocks)
        .filter_map(|block| block.text.as_deref())
        .find_map(|text| {
            let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
            normalized
                .strip_prefix("表 A ")
                .map(str::trim)
                .filter(|title| title.contains("评分汇总表"))
                .map(str::to_owned)
        });
    let appendix_b_tables = native_pages
        .iter()
        .flat_map(|page| &page.blocks)
        .filter_map(|block| block.text.as_deref())
        .filter(|text| {
            let normalized: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            normalized.starts_with("表B.") && normalized.contains("检查评分表")
        })
        .count();
    if let Some(appendix_a_title) = appendix_a_title {
        for block in blocks.iter_mut() {
            let Some(text) = block.text.as_deref() else {
                continue;
            };
            let printed_page = trailing_page_number(text).unwrap_or(0);
            let repaired = if text.contains("评分汇总表") {
                Some(format!("附录 A {appendix_a_title} {printed_page}"))
            } else if appendix_b_tables >= 5 && text.contains("分项检查评分表") {
                appendix_a_title
                    .strip_suffix("检查评分汇总表")
                    .map(|prefix| format!("附录 B {prefix}分项检查评分表 {printed_page}"))
            } else {
                None
            };
            let Some(repaired) = repaired else {
                continue;
            };
            block.text = Some(repaired.clone());
            block.category_raw = "toc_entry".to_owned();
            block.category = Some("list".to_owned());
            block.spans = vec![crate::types::Span {
                text: repaired,
                bbox_px: block.bbox_px,
                font_size: None,
                is_inline_formula: false,
            }];
            reconciled += 1;
        }
    }
    reconciled
}

#[cfg(all(feature = "native", feature = "pdfium"))]
fn split_numbered_heading(text: &str) -> Option<(String, String)> {
    let text = text.trim();
    let prefix_end = text
        .char_indices()
        .take_while(|(_, character)| {
            character.is_ascii_digit() || *character == '.' || character.is_whitespace()
        })
        .map(|(index, character)| index + character.len_utf8())
        .last()?;
    let identifier: String = text[..prefix_end]
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .trim_matches('.')
        .to_owned();
    let segments: Vec<&str> = identifier.split('.').collect();
    if !(1..=2).contains(&segments.len())
        || segments
            .iter()
            .any(|segment| segment.is_empty() || !segment.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let title = text[prefix_end..]
        .trim_start_matches(|character: char| {
            character.is_whitespace() || "“\"'".contains(character)
        })
        .trim()
        .to_owned();
    (!title.is_empty()).then_some((identifier, title))
}

#[cfg(all(feature = "native", feature = "pdfium"))]
fn trailing_page_number(text: &str) -> Option<u32> {
    text.split_whitespace()
        .next_back()?
        .trim_matches(|character: char| !character.is_ascii_digit())
        .parse()
        .ok()
}

#[cfg(all(feature = "native", feature = "pdfium"))]
fn is_cjk(character: char) -> bool {
    matches!(character, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}')
}

#[cfg(all(feature = "native", feature = "pdfium"))]
fn likely_traditional_chinese(text: &str) -> bool {
    const TRADITIONAL_MARKERS: &str = "與為於國業術書臺灣溫體學會後發這個來時說";
    const SIMPLIFIED_MARKERS: &str = "与为于国业术书台湾温体学会后发这个来时说";
    let traditional = text
        .chars()
        .filter(|character| TRADITIONAL_MARKERS.contains(*character))
        .count();
    let simplified = text
        .chars()
        .filter(|character| SIMPLIFIED_MARKERS.contains(*character))
        .count();
    traditional > simplified
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

#[cfg(all(feature = "native", feature = "pdfium"))]
fn materialize_native_image_assets(
    pdf_bytes: &[u8],
    result: &mut ParseResult,
    no_assets: bool,
    max_asset_bytes: usize,
) {
    if no_assets {
        return;
    }
    let pages: Vec<u32> = result
        .pages
        .iter()
        .filter(|page| page.blocks.iter().any(is_materializable_native_image))
        .map(|page| page.page_num)
        .collect();
    if pages.is_empty() {
        return;
    }
    let rendered = match ingest::rasterize_pdf_page_numbers(pdf_bytes, 150.0, &pages) {
        Ok(rendered) => rendered,
        Err(error) => {
            result.warnings.push(format!(
                "native image assets skipped on pages {pages:?}: PDFium rasterization failed: {error}"
            ));
            return;
        }
    };

    let mut materialized = 0usize;
    let mut skipped = 0usize;
    for rendered_page in rendered {
        let Some(page) = result
            .pages
            .iter_mut()
            .find(|page| page.page_num == rendered_page.page_num)
        else {
            continue;
        };
        let decoded = match image::load_from_memory(&rendered_page.png_bytes) {
            Ok(decoded) => decoded,
            Err(error) => {
                result.warnings.push(format!(
                    "native image assets skipped on page {}: rendered PNG decode failed: {error}",
                    rendered_page.page_num
                ));
                continue;
            }
        };
        for block in &mut page.blocks {
            if !is_materializable_native_image(block) {
                continue;
            }
            match crop_native_image_asset(
                &decoded,
                rendered_page.width,
                rendered_page.height,
                page.width_px,
                page.height_px,
                block,
            ) {
                Some(bytes) if bytes.len() <= max_asset_bytes => {
                    block.asset_bytes = Some(bytes);
                    materialized += 1;
                }
                Some(_) | None => skipped += 1,
            }
        }
    }
    if materialized > 0 {
        clear_materialized_asset_warning(result);
    }
    result.capability_notes.push(format!(
        "native image assets: materialized={materialized}, skipped={skipped}, dpi=150"
    ));
}

#[cfg(all(feature = "native", feature = "pdfium"))]
fn clear_materialized_asset_warning(result: &mut ParseResult) {
    if let Some(profile) = result.document_profile.as_mut() {
        profile
            .warnings
            .retain(|warning| !warning.contains("no materialized figure assets"));
    }
}

#[cfg(all(feature = "native", feature = "pdfium"))]
fn is_materializable_native_image(block: &crate::types::Block) -> bool {
    if !matches!(block.category.as_deref(), Some("image" | "chart")) || block.asset_bytes.is_some()
    {
        return false;
    }
    let crate::types::Geometry::Rect(rect) = &block.geom else {
        return false;
    };
    let [x0, y0, x1, y1] = *rect;
    let width = (x1 - x0).abs();
    let height = (y1 - y0).abs();
    width >= 8.0 && height >= 8.0 && width * height >= 256.0
}

#[cfg(all(feature = "native", feature = "pdfium"))]
fn crop_native_image_asset(
    page_image: &image::DynamicImage,
    rendered_width: u32,
    rendered_height: u32,
    page_width: u32,
    page_height: u32,
    block: &crate::types::Block,
) -> Option<Vec<u8>> {
    if page_width == 0 || page_height == 0 || rendered_width == 0 || rendered_height == 0 {
        return None;
    }
    let crate::types::Geometry::Rect(rect) = &block.geom else {
        return None;
    };
    let [x0, y0, x1, y1] = *rect;
    let scale_x = rendered_width as f32 / page_width as f32;
    let scale_y = rendered_height as f32 / page_height as f32;
    let left = (x0.min(x1) * scale_x).floor().max(0.0) as u32;
    let top = (y0.min(y1) * scale_y).floor().max(0.0) as u32;
    let right = (x0.max(x1) * scale_x).ceil().max(0.0) as u32;
    let bottom = (y0.max(y1) * scale_y).ceil().max(0.0) as u32;
    let left = left.min(rendered_width);
    let top = top.min(rendered_height);
    let right = right.min(rendered_width);
    let bottom = bottom.min(rendered_height);
    if right <= left || bottom <= top {
        return None;
    }
    let crop = page_image.crop_imm(left, top, right - left, bottom - top);
    let mut output = std::io::Cursor::new(Vec::new());
    crop.write_to(&mut output, image::ImageFormat::Png).ok()?;
    Some(output.into_inner())
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
        "tesseract" => "tesseract",
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
            Some(DEFAULT_RASTER_DPI),
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
            Some(DEFAULT_RASTER_DPI),
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
    fn minimal_text_pdf() -> Vec<u8> {
        let content = "BT /F1 12 Tf 72 720 Td (Hello release) Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref_offset = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    fn text_block(text: &str) -> crate::types::Block {
        crate::types::Block {
            geom: crate::types::Geometry::Rect([0.0, 0.0, 10.0, 10.0]),
            geom_frame: crate::types::CoordFrame::Page,
            bbox_px: Some([0, 0, 10, 10]),
            category_raw: "text".into(),
            category: Some("text".into()),
            reading_order: None,
            text: Some(text.into()),
            html: None,
            latex: None,
            spans: vec![],
            merge_hint: None,
            confidence: None,
            source: crate::types::BlockSource::NativeTextLayer,
            error: None,
            asset_bytes: None,
            asset_path: None,
            asset_caption: None,
        }
    }

    #[test]
    fn mandatory_declaration_promotes_matching_standard_clauses() {
        let mut result = ParseResult {
            source_path: "standard.pdf".into(),
            source_sha256: "sha".into(),
            protocol: "native".into(),
            routed_by: RoutedBy::Explicit,
            document_profile: None,
            route_decision: None,
            preprocess_plan: None,
            model_endpoint: None,
            model_name: None,
            pages: vec![crate::types::Page {
                page_num: 1,
                width_px: 100,
                height_px: 100,
                blocks: vec![
                    text_block("其中，第 4.0.1、5.0.3 条为强制性条文，必须严格执行。"),
                    text_block("5.0.3 条为强制性条文，必须严格执行。"),
                    text_block("4.0.1 建筑施工安全检查评定中，保证项目应全数检查。"),
                    text_block("4.0.2 评分应采用扣减分值的方法。"),
                    text_block("5.0.3 当评定等级为不合格时，必须限期整改。"),
                    text_block("附录 A 建筑施工安全检查评分汇总表"),
                ],
            }],
            page_errors: vec![],
            capability_notes: vec![],
            warnings: vec![],
            timing: Default::default(),
        };

        annotate_technical_standard_ir(&mut result);

        let categories: Vec<_> = result.pages[0]
            .blocks
            .iter()
            .map(|block| block.category.as_deref())
            .collect();
        assert_eq!(categories[1], Some("text"));
        assert_eq!(categories[2], Some("mandatory_clause"));
        assert_eq!(categories[3], Some("normative_clause"));
        assert_eq!(categories[4], Some("mandatory_clause"));
        assert_eq!(categories[5], Some("annex_heading"));
        assert!(result.capability_notes[0].contains("mandatory=2 [4.0.1,5.0.3]"));
    }

    #[test]
    fn standard_semantic_helpers_reject_lists_and_accept_traditional_annexes() {
        assert_eq!(leading_normative_clause_id("3.2.4 条款正文"), Some("3.2.4"));
        assert_eq!(leading_normative_clause_id("1 列表项目"), None);
        assert_eq!(leading_normative_clause_id("2026.08 日期"), None);
        assert!(is_annex_heading("## 附錄 B 評分表"));
        assert!(!is_annex_heading("附表数据"));
    }

    #[cfg(feature = "native")]
    #[tokio::test]
    async fn native_runner_reuses_pdf_artifact_and_preserves_engine_markdown() {
        let bytes: Arc<[u8]> = minimal_text_pdf().into();
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
    fn asset_bytes_can_be_dropped_without_reparsing_the_document() {
        let mut options = uparser_document_engine::ParseOptions::default();
        options.include_assets = false;
        assert!(!document_options_require_reparse(&options));

        options.include_notes = false;
        assert!(document_options_require_reparse(&options));
    }

    #[cfg(all(feature = "native", not(feature = "pdfium")))]
    #[test]
    fn native_image_geometry_is_explicitly_unavailable_without_pdfium() {
        let mut image = text_block("placeholder");
        image.category = Some("image".into());
        image.text = None;
        let mut result = ParseResult {
            source_path: "paper.pdf".into(),
            source_sha256: "sha".into(),
            protocol: "native".into(),
            routed_by: RoutedBy::Explicit,
            document_profile: None,
            route_decision: None,
            preprocess_plan: None,
            model_endpoint: None,
            model_name: None,
            pages: vec![crate::types::Page {
                page_num: 1,
                width_px: 100,
                height_px: 100,
                blocks: vec![image],
            }],
            page_errors: vec![],
            capability_notes: vec![],
            warnings: vec![],
            timing: Default::default(),
        };

        annotate_unavailable_native_image_assets(&mut result, false);

        assert!(result.warnings[0].starts_with("asset_unavailable: 1"));
        assert!(result.capability_notes[0].contains("unavailable=1"));
    }

    #[cfg(all(feature = "native", feature = "pdfium"))]
    #[test]
    fn native_image_crop_scales_page_geometry_and_emits_real_png() {
        let mut block = text_block("placeholder");
        block.geom = crate::types::Geometry::Rect([10.0, 5.0, 40.0, 25.0]);
        block.bbox_px = Some([10, 5, 40, 25]);
        block.category_raw = "ImageXObject".into();
        block.category = Some("image".into());
        block.text = None;
        assert!(is_materializable_native_image(&block));

        let page = image::DynamicImage::new_rgb8(200, 100);
        let bytes = crop_native_image_asset(&page, 200, 100, 100, 50, &block).unwrap();
        let crop = image::load_from_memory(&bytes).unwrap();
        assert_eq!((crop.width(), crop.height()), (60, 40));
    }

    #[cfg(all(feature = "native", feature = "pdfium"))]
    #[test]
    fn materialized_assets_clear_only_the_obsolete_profile_warning() {
        let mut result = ParseResult {
            source_path: "paper.pdf".into(),
            source_sha256: "sha".into(),
            protocol: "native".into(),
            routed_by: RoutedBy::Explicit,
            document_profile: Some(crate::profiler::profile_l1(DocumentFormat::Pdf)),
            route_decision: None,
            preprocess_plan: None,
            model_endpoint: None,
            model_name: None,
            pages: vec![],
            page_errors: vec![],
            capability_notes: vec![],
            warnings: vec![],
            timing: Default::default(),
        };
        let warnings = &mut result.document_profile.as_mut().unwrap().warnings;
        warnings.push("paper has captions but no materialized figure assets".into());
        warnings.push("keep this warning".into());

        clear_materialized_asset_warning(&mut result);

        assert_eq!(
            result.document_profile.unwrap().warnings,
            ["keep this warning"]
        );
    }

    #[test]
    fn visual_office_plan_converts_only_after_routing() {
        let profile = crate::profiler::profile_l1(DocumentFormat::Docx);
        let plan = preprocess_plan(DocumentFormat::Docx, &profile, "mineru-vlm").unwrap();
        assert_eq!(plan.conversion, ConversionPlan::LibreOfficeToPdf);
        assert_eq!(plan.input_channel, InputChannel::VisualPages);
    }

    /// D7: PDF and converted-structured-document plans default to
    /// `DEFAULT_RASTER_DPI` (200, matching MinerU's own
    /// `DEFAULT_PDF_IMAGE_DPI`), not the previous hardcoded 150 — a 25%
    /// lower linear resolution than what OCR/table/formula models are
    /// tuned against. See `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`.
    #[test]
    fn visual_page_plans_default_to_200_dpi_matching_mineru() {
        let pdf_profile = crate::profiler::profile_l1(DocumentFormat::Pdf);
        let pdf_plan = preprocess_plan(DocumentFormat::Pdf, &pdf_profile, "mineru-vlm").unwrap();
        assert_eq!(pdf_plan.raster_dpi, Some(DEFAULT_RASTER_DPI));
        assert_eq!(DEFAULT_RASTER_DPI, 200);

        let docx_profile = crate::profiler::profile_l1(DocumentFormat::Docx);
        let docx_plan = preprocess_plan(DocumentFormat::Docx, &docx_profile, "mineru-vlm").unwrap();
        assert_eq!(docx_plan.raster_dpi, Some(DEFAULT_RASTER_DPI));
    }

    /// D7: `execute`'s DPI precedence is `options.raster_dpi` (an
    /// explicit `--raster-dpi`/`ParseOptions::raster_dpi` override) first,
    /// then the plan's own `raster_dpi`, then `DEFAULT_RASTER_DPI` as a
    /// last-resort fallback. This test exercises the exact expression
    /// `execute` uses (`options.raster_dpi.or(plan.preprocess.raster_dpi)
    /// .unwrap_or(DEFAULT_RASTER_DPI)`) directly, since driving it through
    /// a full `execute()` call would require a real PDF fixture and the
    /// `pdfium` feature (not available offline here) just to observe a
    /// DPI difference in the rasterized output.
    #[test]
    fn raster_dpi_precedence_prefers_override_then_plan_then_default() {
        fn effective(override_dpi: Option<u16>, plan_dpi: Option<u16>) -> u16 {
            override_dpi.or(plan_dpi).unwrap_or(DEFAULT_RASTER_DPI)
        }

        assert_eq!(effective(Some(96), Some(DEFAULT_RASTER_DPI)), 96);
        assert_eq!(
            effective(None, Some(DEFAULT_RASTER_DPI)),
            DEFAULT_RASTER_DPI
        );
        assert_eq!(effective(None, None), DEFAULT_RASTER_DPI);
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
