//! Versioned contract for the MinerU-current pipeline compatibility service.
//!
//! V1 remains available in `pipeline_serving`; V2 preserves geometry and model
//! provenance and supports page batches and per-item failure isolation.

use super::{
    ModelStage, ParseCtx, PipelineConfig, PostprocessSignals, ProtocolAdapter, RawOutputFormat,
    RemoteEndpointSpec, ResourceHint, StageBackend,
};
use crate::category_map::{self, PIPELINE_LAYOUT_CATEGORIES};
use crate::imaging;
use crate::ingest::RenderedPage;
use crate::types::{
    Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, MergeHint, PageError,
};
use async_trait::async_trait;
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

pub const PIPELINE_V2_SCHEMA_VERSION: &str = "uparser.pipeline.v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSpace {
    RenderPixels,
    PagePoints,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageDimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodedImage {
    pub media_type: String,
    pub base64_data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageImage {
    pub page_id: String,
    pub image: EncodedImage,
    pub dimensions: ImageDimensions,
    #[serde(default)]
    pub rotation_degrees: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Polygon {
    pub points: Vec<[f32; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub region_id: String,
    pub label: String,
    pub bbox: [f32; 4],
    pub polygon: Option<Polygon>,
    pub confidence: Option<f32>,
    pub coordinate_space: CoordinateSpace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub name: String,
    pub revision: String,
    pub weight_sha256: Option<String>,
    pub runtime: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageWarning {
    pub code: String,
    pub message: String,
    pub region_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub region_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchRequest<T> {
    pub schema_version: String,
    pub request_id: String,
    pub items: Vec<T>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchItemResult<T> {
    pub page_id: String,
    pub result: Option<T>,
    pub error: Option<StageError>,
    #[serde(default)]
    pub warnings: Vec<StageWarning>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchResponse<T> {
    pub schema_version: String,
    pub request_id: String,
    pub model: ModelMetadata,
    pub items: Vec<BatchItemResult<T>>,
}

pub type LayoutBatchRequest = BatchRequest<PageImage>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutResult {
    pub regions: Vec<Region>,
}

pub type LayoutBatchResponse = BatchResponse<LayoutResult>;
pub type FormulaDetectionBatchRequest = BatchRequest<PageImage>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaDetectionResult {
    pub regions: Vec<Region>,
}

pub type FormulaDetectionBatchResponse = BatchResponse<FormulaDetectionResult>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrPageInput {
    pub page: PageImage,
    pub layout_regions: Vec<Region>,
    #[serde(default)]
    pub formula_regions: Vec<Region>,
    pub language: String,
}

pub type OcrBatchRequest = BatchRequest<OcrPageInput>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrSpan {
    pub span_id: String,
    pub parent_region_id: Option<String>,
    pub polygon: Polygon,
    pub text: String,
    pub language: Option<String>,
    pub confidence: Option<f32>,
    pub coordinate_space: CoordinateSpace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrResult {
    pub spans: Vec<OcrSpan>,
}

pub type OcrBatchResponse = BatchResponse<OcrResult>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaRecognitionInput {
    pub page: PageImage,
    pub formula_regions: Vec<Region>,
}

pub type FormulaRecognitionBatchRequest = BatchRequest<FormulaRecognitionInput>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaSpan {
    pub region_id: String,
    pub latex: String,
    pub confidence: Option<f32>,
    pub bbox: Option<[f32; 4]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaRecognitionResult {
    pub spans: Vec<FormulaSpan>,
}

pub type FormulaRecognitionBatchResponse = BatchResponse<FormulaRecognitionResult>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableRecognitionInput {
    pub page: PageImage,
    pub table_regions: Vec<Region>,
    pub ocr_spans: Vec<OcrSpan>,
    #[serde(default)]
    pub formula_spans: Vec<FormulaSpan>,
}

pub type TableRecognitionBatchRequest = BatchRequest<TableRecognitionInput>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableCell {
    pub cell_id: String,
    pub row: u32,
    pub column: u32,
    pub row_span: u32,
    pub column_span: u32,
    pub bbox: Option<[f32; 4]>,
    pub text: String,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecognizedTable {
    pub region_id: String,
    pub html: String,
    pub structure_tokens: Vec<String>,
    pub cells: Vec<TableCell>,
    pub classifier_label: Option<String>,
    pub rotation_degrees: u16,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableRecognitionResult {
    pub tables: Vec<RecognizedTable>,
}

pub type TableRecognitionBatchResponse = BatchResponse<TableRecognitionResult>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageAnalyzeInput {
    pub page: PageImage,
    pub language: String,
    pub formula_enabled: bool,
    pub table_enabled: bool,
}

pub type PageAnalyzeBatchRequest = BatchRequest<PageAnalyzeInput>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageAnalyzeResult {
    pub regions: Vec<Region>,
    pub ocr_spans: Vec<OcrSpan>,
    pub formula_spans: Vec<FormulaSpan>,
    pub tables: Vec<RecognizedTable>,
    pub reading_order: Vec<String>,
    #[serde(default)]
    pub markdown: Option<String>,
    #[serde(default)]
    pub assets: Vec<GeneratedAsset>,
}

pub type PageAnalyzeBatchResponse = BatchResponse<PageAnalyzeResult>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeneratedAsset {
    pub path: String,
    pub media_type: String,
    pub base64_data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodedDocument {
    pub document_id: String,
    pub media_type: String,
    pub base64_data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentAnalyzeInput {
    pub document: EncodedDocument,
    pub language: String,
    pub formula_enabled: bool,
    pub table_enabled: bool,
}

pub type DocumentAnalyzeBatchRequest = BatchRequest<DocumentAnalyzeInput>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentAnalyzeResult {
    pub markdown: String,
    #[serde(default)]
    pub assets: Vec<GeneratedAsset>,
}

pub type DocumentAnalyzeBatchResponse = BatchResponse<DocumentAnalyzeResult>;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ContractValidationError {
    #[error("schema_version must be {PIPELINE_V2_SCHEMA_VERSION}")]
    SchemaVersion,
    #[error("request_id must not be empty")]
    EmptyRequestId,
    #[error("batch must contain at least one item")]
    EmptyBatch,
    #[error("page_id must not be empty")]
    EmptyPageId,
    #[error("image dimensions must be non-zero")]
    InvalidDimensions,
    #[error("rotation_degrees must be one of 0, 90, 180, 270")]
    InvalidRotation,
    #[error("image media_type must be image/png or image/jpeg")]
    UnsupportedMediaType,
    #[error("base64 image data must not be empty")]
    EmptyImage,
    #[error("region bbox must contain finite ordered coordinates")]
    InvalidBbox,
    #[error("region confidence must be between 0 and 1")]
    InvalidConfidence,
    #[error("region polygon must contain at least three finite points")]
    InvalidPolygon,
    #[error("document_id must not be empty")]
    EmptyDocumentId,
    #[error("document media_type must be application/pdf")]
    UnsupportedDocumentMediaType,
    #[error("base64 document data must not be empty")]
    EmptyDocument,
    #[error("batch response must contain exactly one item")]
    InvalidResponseCardinality,
    #[error("batch item must contain exactly one of result or error")]
    InvalidBatchOutcome,
    #[error("response identifier does not match the request")]
    ResponseIdentifierMismatch,
}

impl<T> BatchRequest<T> {
    pub fn validate_envelope(&self) -> Result<(), ContractValidationError> {
        if self.schema_version != PIPELINE_V2_SCHEMA_VERSION {
            return Err(ContractValidationError::SchemaVersion);
        }
        if self.request_id.trim().is_empty() {
            return Err(ContractValidationError::EmptyRequestId);
        }
        if self.items.is_empty() {
            return Err(ContractValidationError::EmptyBatch);
        }
        Ok(())
    }
}

impl PageImage {
    pub fn validate(&self) -> Result<(), ContractValidationError> {
        if self.page_id.trim().is_empty() {
            return Err(ContractValidationError::EmptyPageId);
        }
        if self.dimensions.width == 0 || self.dimensions.height == 0 {
            return Err(ContractValidationError::InvalidDimensions);
        }
        if !matches!(self.rotation_degrees, 0 | 90 | 180 | 270) {
            return Err(ContractValidationError::InvalidRotation);
        }
        if !matches!(self.image.media_type.as_str(), "image/png" | "image/jpeg") {
            return Err(ContractValidationError::UnsupportedMediaType);
        }
        if self.image.base64_data.is_empty() {
            return Err(ContractValidationError::EmptyImage);
        }
        Ok(())
    }
}

impl Region {
    pub fn validate(&self) -> Result<(), ContractValidationError> {
        let [x0, y0, x1, y1] = self.bbox;
        if !self.bbox.iter().all(|value| value.is_finite()) || x1 <= x0 || y1 <= y0 {
            return Err(ContractValidationError::InvalidBbox);
        }
        if self
            .confidence
            .is_some_and(|confidence| !(0.0..=1.0).contains(&confidence))
        {
            return Err(ContractValidationError::InvalidConfidence);
        }
        if self.polygon.as_ref().is_some_and(|polygon| {
            polygon.points.len() < 3
                || !polygon
                    .points
                    .iter()
                    .flatten()
                    .all(|coordinate| coordinate.is_finite())
        }) {
            return Err(ContractValidationError::InvalidPolygon);
        }
        Ok(())
    }
}

impl EncodedDocument {
    pub fn validate(&self) -> Result<(), ContractValidationError> {
        if self.document_id.trim().is_empty() {
            return Err(ContractValidationError::EmptyDocumentId);
        }
        if self.media_type != "application/pdf" {
            return Err(ContractValidationError::UnsupportedDocumentMediaType);
        }
        if self.base64_data.is_empty() {
            return Err(ContractValidationError::EmptyDocument);
        }
        Ok(())
    }
}

impl<T> BatchItemResult<T> {
    pub fn validate_outcome(&self) -> Result<(), ContractValidationError> {
        if (self.result.is_some()) == (self.error.is_some()) {
            return Err(ContractValidationError::InvalidBatchOutcome);
        }
        Ok(())
    }
}

impl<T> BatchResponse<T> {
    fn into_single(
        mut self,
        request_id: &str,
        page_id: &str,
    ) -> Result<(T, Vec<StageWarning>), String> {
        if self.schema_version != PIPELINE_V2_SCHEMA_VERSION || self.request_id != request_id {
            return Err(ContractValidationError::ResponseIdentifierMismatch.to_string());
        }
        if self.items.len() != 1 {
            return Err(ContractValidationError::InvalidResponseCardinality.to_string());
        }
        let item = self.items.remove(0);
        if item.page_id != page_id {
            return Err(ContractValidationError::ResponseIdentifierMismatch.to_string());
        }
        item.validate_outcome().map_err(|error| error.to_string())?;
        if let Some(error) = item.error {
            return Err(format!("{}: {}", error.code, error.message));
        }
        Ok((
            item.result
                .expect("validated batch outcome contains a result"),
            item.warnings,
        ))
    }
}

/// Rust-owned Pipeline V2 workflow. Model execution remains remote, while
/// stage ordering, failure handling, region ownership and IR construction are
/// implemented here without a MinerU/PaddleOCR client-side runtime.
pub struct PipelineV2Adapter {
    pub endpoint_base: String,
    pub layout_endpoint: String,
    pub formula_detection_endpoint: String,
    pub ocr_endpoint: String,
    pub formula_recognition_endpoint: String,
    pub table_endpoint: String,
    pub language: String,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl Default for PipelineV2Adapter {
    fn default() -> Self {
        let endpoint_base = "http://localhost:9001".to_owned();
        Self::from_endpoint_base(endpoint_base)
    }
}

impl PipelineV2Adapter {
    pub fn from_endpoint_base(endpoint_base: String) -> Self {
        let base = endpoint_base.trim_end_matches('/').to_owned();
        Self {
            endpoint_base: base.clone(),
            layout_endpoint: format!("{base}/v2/pipeline/layout:batch"),
            formula_detection_endpoint: format!("{base}/v2/pipeline/mfd:batch"),
            ocr_endpoint: format!("{base}/v2/pipeline/ocr:batch"),
            formula_recognition_endpoint: format!("{base}/v2/pipeline/mfr:batch"),
            table_endpoint: format!("{base}/v2/pipeline/table:batch"),
            language: "ch".to_owned(),
            timeout: Duration::from_secs(180),
            max_retries: 2,
        }
    }

    pub fn set_endpoint_base(&mut self, endpoint_base: String) {
        let replacement = Self::from_endpoint_base(endpoint_base);
        self.endpoint_base = replacement.endpoint_base;
        self.layout_endpoint = replacement.layout_endpoint;
        self.formula_detection_endpoint = replacement.formula_detection_endpoint;
        self.ocr_endpoint = replacement.ocr_endpoint;
        self.formula_recognition_endpoint = replacement.formula_recognition_endpoint;
        self.table_endpoint = replacement.table_endpoint;
    }

    pub fn apply_config(&mut self, config: &PipelineConfig) {
        if let Some(endpoint) = &config.layout_endpoint {
            self.layout_endpoint = endpoint.clone();
        }
        if let Some(endpoint) = &config.formula_detection_endpoint {
            self.formula_detection_endpoint = endpoint.clone();
        }
        if let Some(endpoint) = &config.ocr_endpoint {
            self.ocr_endpoint = endpoint.clone();
        }
        if let Some(endpoint) = &config.formula_endpoint {
            self.formula_recognition_endpoint = endpoint.clone();
        }
        if let Some(endpoint) = &config.table_endpoint {
            self.table_endpoint = endpoint.clone();
        }
        if let Some(language) = &config.language {
            self.language = language.clone();
        }
    }

    fn encoded_page(page: &RenderedPage) -> PageImage {
        PageImage {
            page_id: format!("page-{}", page.page_num),
            image: EncodedImage {
                media_type: "image/png".to_owned(),
                base64_data: base64::engine::general_purpose::STANDARD.encode(&page.png_bytes),
            },
            dimensions: ImageDimensions {
                width: page.width,
                height: page.height,
            },
            rotation_degrees: 0,
        }
    }

    async fn dispatch_stage<I, O>(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
        endpoint: &str,
        stage: &str,
        item: I,
    ) -> Result<O, PageError>
    where
        I: Serialize,
        O: DeserializeOwned,
    {
        let request_id = format!("pipeline-v2-{}-{stage}", page.page_num);
        let page_id = format!("page-{}", page.page_num);
        let request = BatchRequest {
            schema_version: PIPELINE_V2_SCHEMA_VERSION.to_owned(),
            request_id: request_id.clone(),
            items: vec![item],
        };
        let body = serde_json::to_value(request).expect("Pipeline V2 request is serializable");
        let _permit = ctx.acquire_permit().await;
        let value = ctx
            .dispatch_rest(endpoint, body, self.timeout, self.max_retries)
            .await
            .map_err(|error| page_error(page, stage, error.to_string()))?;
        let response: BatchResponse<O> = serde_json::from_value(value)
            .map_err(|error| page_error(page, stage, format!("malformed response: {error}")))?;
        let (result, warnings) = response
            .into_single(&request_id, &page_id)
            .map_err(|error| page_error(page, stage, error))?;
        for warning in warnings {
            ctx.warn(format!(
                "pipeline-v2 page {} stage {stage}: {}: {}",
                page.page_num, warning.code, warning.message
            ));
        }
        Ok(result)
    }

    async fn run_workflow(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
    ) -> Result<PageAnalyzeResult, PageError> {
        crate::stage_graph::PIPELINE_V2_STAGE_GRAPH
            .validate()
            .map_err(|error| page_error(page, "stage_graph", error.to_string()))?;
        let encoded = Self::encoded_page(page);
        encoded
            .validate()
            .map_err(|error| page_error(page, "input", error.to_string()))?;

        let layout = self.dispatch_stage::<_, LayoutResult>(
            page,
            ctx,
            &self.layout_endpoint,
            "layout",
            encoded.clone(),
        );
        let formulas = self.dispatch_stage::<_, FormulaDetectionResult>(
            page,
            ctx,
            &self.formula_detection_endpoint,
            "formula_detect",
            encoded.clone(),
        );
        let (layout, formulas) = tokio::join!(layout, formulas);
        let layout = layout?;
        let formulas = formulas?;
        validate_regions(page, "layout", &layout.regions)?;
        validate_regions(page, "formula_detect", &formulas.regions)?;

        // Table regions never receive OCR spans from the page-level OCR call:
        // the OCR backend's `TEXT_REGION_LABELS` deliberately excludes
        // "table" (MinerU's own page-level OCR skips tables too, since the
        // table model owns its own text recognition dependency). A second,
        // table-scoped OCR dispatch is required — relabeling each table
        // region so it passes the OCR backend's region-type filter — mirrors
        // the reference `PipelinePageAnalyzer` orchestration in
        // `page_analyzer.py`. Without this, `TableRecognitionInput.ocr_spans`
        // is structurally empty for every table and the table model has no
        // text to bind to its predicted cell structure (see
        // `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`, D1).
        let table_regions = layout
            .regions
            .iter()
            .filter(|region| category_map::map_pipeline_category(&region.label).0 == "table")
            .cloned()
            .collect::<Vec<_>>();
        let table_ocr_regions = table_regions
            .iter()
            .cloned()
            .map(|region| Region {
                label: "text".to_owned(),
                ..region
            })
            .collect::<Vec<_>>();

        let ocr = self.dispatch_stage::<_, OcrResult>(
            page,
            ctx,
            &self.ocr_endpoint,
            "ocr",
            OcrPageInput {
                page: encoded.clone(),
                layout_regions: layout.regions.clone(),
                formula_regions: formulas.regions.clone(),
                language: self.language.clone(),
            },
        );
        let recognized_formulas = self.dispatch_stage::<_, FormulaRecognitionResult>(
            page,
            ctx,
            &self.formula_recognition_endpoint,
            "formula_recognize",
            FormulaRecognitionInput {
                page: encoded.clone(),
                formula_regions: formulas.regions.clone(),
            },
        );
        let table_ocr = async {
            if table_ocr_regions.is_empty() {
                return Ok(OcrResult { spans: vec![] });
            }
            self.dispatch_stage::<_, OcrResult>(
                page,
                ctx,
                &self.ocr_endpoint,
                "table_ocr",
                OcrPageInput {
                    page: encoded.clone(),
                    layout_regions: table_ocr_regions,
                    formula_regions: formulas.regions.clone(),
                    language: self.language.clone(),
                },
            )
            .await
        };
        let (ocr, recognized_formulas, table_ocr) =
            tokio::join!(ocr, recognized_formulas, table_ocr);
        let ocr = ocr?;
        let recognized_formulas = recognized_formulas?;
        let table_ocr = table_ocr?;

        let tables = if table_regions.is_empty() {
            TableRecognitionResult { tables: vec![] }
        } else {
            self.dispatch_stage::<_, TableRecognitionResult>(
                page,
                ctx,
                &self.table_endpoint,
                "table",
                TableRecognitionInput {
                    page: encoded,
                    table_regions: table_regions.clone(),
                    ocr_spans: table_ocr.spans.clone(),
                    formula_spans: recognized_formulas.spans.clone(),
                },
            )
            .await?
        };

        let mut all_ocr_spans = ocr.spans;
        all_ocr_spans.extend(table_ocr.spans);

        // The layout model can independently detect its own
        // `inline_formula`/`display_formula` regions (e.g. legacy class
        // ids 8/9 in `compat.py::LEGACY_LAYOUT_LABELS`), duplicating what
        // the dedicated MFD (formula detection) model finds more
        // precisely for the same area. Left unfiltered, these duplicates
        // survive as blank blocks: they're excluded from page-level OCR
        // (formula regions aren't in `TEXT_REGION_LABELS`) and never
        // receive a `latex` value (MFR only recognizes MFD-origin
        // regions), so they occupy a reading-order slot and skew XY-cut
        // for nothing. Mirrors `compat.py::merge_layout_and_mfd`'s
        // IoMin ≥ 0.7 dedup — see D4 in
        // `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`.
        let mut regions = layout
            .regions
            .into_iter()
            .filter(|region| {
                let is_formula_label =
                    matches!(region.label.as_str(), "inline_formula" | "display_formula");
                !is_formula_label
                    || !formulas
                        .regions
                        .iter()
                        .any(|mfd| intersection_over_min_area(region.bbox, mfd.bbox) >= 0.7)
            })
            .collect::<Vec<_>>();
        let mut known_ids = regions
            .iter()
            .map(|region| region.region_id.clone())
            .collect::<HashSet<_>>();
        for region in formulas.regions {
            let belongs_to_table = table_regions
                .iter()
                .any(|table| bbox_contains_center(table.bbox, region.bbox));
            if !belongs_to_table && known_ids.insert(region.region_id.clone()) {
                regions.push(region);
            }
        }
        let order = region_reading_order(page, &regions)?;
        Ok(PageAnalyzeResult {
            regions,
            ocr_spans: all_ocr_spans,
            formula_spans: recognized_formulas.spans,
            tables: tables.tables,
            reading_order: order,
            markdown: None,
            assets: vec![],
        })
    }
}

#[async_trait]
impl ProtocolAdapter for PipelineV2Adapter {
    fn name(&self) -> &'static str {
        "pipeline"
    }

    fn coordinate_system(&self) -> CoordinateSystem {
        CoordinateSystem::PixelAbs
    }

    fn provides_reading_order(&self) -> bool {
        true
    }

    fn category_vocab(&self) -> &[&'static str] {
        PIPELINE_LAYOUT_CATEGORIES
    }

    fn raw_output_format(&self) -> RawOutputFormat {
        RawOutputFormat::OcrBoxes
    }

    fn emitted_signals(&self) -> PostprocessSignals {
        PostprocessSignals::default()
    }

    fn model_stages(&self) -> Vec<ModelStage> {
        [
            "layout",
            "formula_detect",
            "ocr",
            "formula_recognize",
            "table",
        ]
        .into_iter()
        .map(|stage_name| ModelStage {
            stage_name,
            default_backend: StageBackend::Remote(RemoteEndpointSpec {
                endpoint_env_var: "UPARSER_PIPELINE_ENDPOINT",
            }),
            allows_local: false,
            resource_hint: ResourceHint::Heavy,
        })
        .collect()
    }

    async fn parse_page(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
    ) -> Result<Vec<Block>, PageError> {
        let result = self.run_workflow(page, ctx).await?;
        build_blocks(page, ctx, result)
    }
}

fn page_error(page: &RenderedPage, stage: &str, message: String) -> PageError {
    PageError {
        page_num: page.page_num,
        message,
        stage: Some(stage.to_owned()),
    }
}

fn validate_regions(page: &RenderedPage, stage: &str, regions: &[Region]) -> Result<(), PageError> {
    for region in regions {
        region
            .validate()
            .map_err(|error| page_error(page, stage, error.to_string()))?;
        if region.coordinate_space != CoordinateSpace::RenderPixels {
            return Err(page_error(
                page,
                stage,
                "page adapter requires render_pixels coordinates".to_owned(),
            ));
        }
    }
    Ok(())
}

fn bbox_px(page: &RenderedPage, bbox: [f32; 4]) -> Result<[i32; 4], PageError> {
    let x0 = bbox[0].floor().max(0.0).min(page.width as f32) as i32;
    let y0 = bbox[1].floor().max(0.0).min(page.height as f32) as i32;
    let x1 = bbox[2].ceil().max(0.0).min(page.width as f32) as i32;
    let y1 = bbox[3].ceil().max(0.0).min(page.height as f32) as i32;
    if x1 <= x0 || y1 <= y0 {
        return Err(page_error(
            page,
            "geometry",
            format!("region bbox is empty after clamping: {bbox:?}"),
        ));
    }
    Ok([x0, y0, x1, y1])
}

fn region_reading_order(page: &RenderedPage, regions: &[Region]) -> Result<Vec<String>, PageError> {
    let boxes = regions
        .iter()
        .map(|region| bbox_px(page, region.bbox))
        .collect::<Result<Vec<_>, _>>()?;
    let ranks = crate::reading_order::assign_reading_order(&boxes);
    let mut indices = (0..regions.len()).collect::<Vec<_>>();
    indices.sort_by_key(|index| ranks[*index]);
    Ok(indices
        .into_iter()
        .map(|index| regions[index].region_id.clone())
        .collect())
}

fn span_sort_key(span: &OcrSpan) -> (i32, i32) {
    let min_y = span
        .polygon
        .points
        .iter()
        .map(|point| point[1])
        .fold(f32::INFINITY, f32::min);
    let min_x = span
        .polygon
        .points
        .iter()
        .map(|point| point[0])
        .fold(f32::INFINITY, f32::min);
    (min_y.round() as i32, min_x.round() as i32)
}

fn bbox_contains_center(outer: [f32; 4], inner: [f32; 4]) -> bool {
    let center_x = (inner[0] + inner[2]) / 2.0;
    let center_y = (inner[1] + inner[3]) / 2.0;
    outer[0] <= center_x && center_x <= outer[2] && outer[1] <= center_y && center_y <= outer[3]
}

/// Intersection area divided by the smaller of the two boxes' areas.
/// Mirrors `compat.py::intersection_over_min_area` — deliberately not
/// IoU: a small, precise MFD formula box fully contained inside a larger,
/// coarser layout-model formula box should still count as "the same
/// region" even though their IoU would be low.
fn intersection_over_min_area(left: [f32; 4], right: [f32; 4]) -> f32 {
    let width = (left[2].min(right[2]) - left[0].max(right[0])).max(0.0);
    let height = (left[3].min(right[3]) - left[1].max(right[1])).max(0.0);
    let intersection = width * height;
    let left_area = (left[2] - left[0]).max(0.0) * (left[3] - left[1]).max(0.0);
    let right_area = (right[2] - right[0]).max(0.0) * (right[3] - right[1]).max(0.0);
    let denominator = left_area.min(right_area);
    if denominator > 0.0 {
        intersection / denominator
    } else {
        0.0
    }
}

fn build_blocks(
    page: &RenderedPage,
    ctx: &ParseCtx,
    result: PageAnalyzeResult,
) -> Result<Vec<Block>, PageError> {
    let mut spans_by_region: HashMap<String, Vec<OcrSpan>> = HashMap::new();
    for span in result.ocr_spans {
        if let Some(parent) = &span.parent_region_id {
            spans_by_region
                .entry(parent.clone())
                .or_default()
                .push(span);
        }
    }
    let formulas = result
        .formula_spans
        .into_iter()
        .map(|span| (span.region_id.clone(), span))
        .collect::<HashMap<_, _>>();
    let tables = result
        .tables
        .into_iter()
        .map(|table| (table.region_id.clone(), table))
        .collect::<HashMap<_, _>>();
    let order = result
        .reading_order
        .iter()
        .enumerate()
        .map(|(rank, id)| (id.clone(), rank as u32))
        .collect::<HashMap<_, _>>();

    let mut blocks = Vec::with_capacity(result.regions.len());
    for region in result.regions {
        let (category, warning) = category_map::map_pipeline_category(&region.label);
        if let Some(warning) = warning {
            ctx.warn(format!("pipeline-v2 page {}: {warning}", page.page_num));
        }
        let bbox = bbox_px(page, region.bbox)?;
        let mut spans = spans_by_region
            .remove(&region.region_id)
            .unwrap_or_default();
        spans.sort_by_key(span_sort_key);
        let mut text = (!spans.is_empty()).then(|| {
            spans
                .iter()
                .map(|span| span.text.trim())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        });
        let latex = formulas
            .get(&region.region_id)
            .map(|formula| formula.latex.clone());
        let html = tables
            .get(&region.region_id)
            .map(|table| table.html.clone());
        if latex.is_some() || html.is_some() {
            text = None;
        }
        let merge_hint = match region.label.as_str() {
            "doc_title" => Some(MergeHint::TitleLevel(1)),
            "paragraph_title" => Some(MergeHint::TitleLevel(2)),
            _ => None,
        };
        let asset_bytes = if matches!(category.as_str(), "image" | "chart") {
            ctx.crop(page, bbox)
                .ok()
                .and_then(|image| imaging::to_png_bytes(&image).ok())
        } else {
            None
        };
        blocks.push(Block {
            geom: Geometry::Rect(region.bbox),
            geom_frame: CoordFrame::Page,
            bbox_px: Some(bbox),
            category_raw: region.label,
            category: Some(category),
            reading_order: order.get(&region.region_id).copied(),
            text: text.filter(|text| !text.is_empty()),
            html,
            latex,
            spans: vec![],
            merge_hint,
            confidence: region.confidence,
            source: BlockSource::OcrPipeline,
            error: None,
            asset_bytes,
            asset_path: None,
            asset_caption: None,
        });
    }
    blocks.sort_by_key(|block| block.reading_order.unwrap_or(u32::MAX));

    // D8's blank-page finding: uparser Pipeline V2 renders 60/1651
    // OmniDocBench pages as fully blank Markdown vs. the original
    // MinerU's 2 — and this held true even for the Python
    // `page_analyzer.py`-orchestrated V2, before this file's Rust
    // orchestration existed, so it isn't a `pipeline_v2.rs`-specific
    // regression. The most likely root cause is the legacy layout
    // model classifying an entire page as `discarded` (or otherwise
    // producing regions with no OCR/latex/html/image content) on pages
    // its weaker classification handles poorly — something S3's real
    // PP-DocLayoutV2 model should reduce as a side effect, not something
    // this render-agnostic adapter code can fix outright. What was
    // previously missing: this failure mode was completely silent —
    // a blank Markdown page and a genuinely blank source page were
    // indistinguishable in `warnings`. See
    // `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`, D8.
    if blocks.is_empty() {
        ctx.warn(format!(
            "pipeline-v2 page {}: layout produced no regions at all (page will render blank)",
            page.page_num
        ));
    } else if blocks.iter().all(|block| {
        block.text.is_none()
            && block.html.is_none()
            && block.latex.is_none()
            && block.asset_bytes.is_none()
    }) {
        ctx.warn(format!(
            "pipeline-v2 page {}: {} region(s) detected but none produced renderable text/html/latex/image content (page will render blank)",
            page.page_num,
            blocks.len()
        ));
    }

    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MockDispatch;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    fn page() -> PageImage {
        PageImage {
            page_id: "doc-1/page-1".into(),
            image: EncodedImage {
                media_type: "image/png".into(),
                base64_data: "cG5n".into(),
            },
            dimensions: ImageDimensions {
                width: 100,
                height: 200,
            },
            rotation_degrees: 0,
        }
    }

    fn region() -> Region {
        Region {
            region_id: "region-1".into(),
            label: "text".into(),
            bbox: [1.0, 2.0, 30.0, 40.0],
            polygon: Some(Polygon {
                points: vec![[1.0, 2.0], [30.0, 2.0], [30.0, 40.0], [1.0, 40.0]],
            }),
            confidence: Some(0.9),
            coordinate_space: CoordinateSpace::RenderPixels,
        }
    }

    fn stage_response(request_id: &str, result: Value) -> Value {
        json!({
            "schema_version": PIPELINE_V2_SCHEMA_VERSION,
            "request_id": request_id,
            "model": {
                "name": "fixture-model",
                "revision": "fixture-revision",
                "weight_sha256": null,
                "runtime": "fixture"
            },
            "items": [{
                "page_id": "page-1",
                "result": result,
                "error": null,
                "warnings": []
            }]
        })
    }

    fn rendered_page() -> RenderedPage {
        let image = image::RgbImage::from_pixel(200, 200, image::Rgb([255, 255, 255]));
        RenderedPage {
            page_num: 1,
            width: 200,
            height: 200,
            png_bytes: imaging::to_png_bytes(&image).unwrap(),
        }
    }

    #[test]
    fn page_analyze_contract_roundtrips_without_losing_geometry() {
        let request = PageAnalyzeBatchRequest {
            schema_version: PIPELINE_V2_SCHEMA_VERSION.into(),
            request_id: "request-1".into(),
            items: vec![PageAnalyzeInput {
                page: page(),
                language: "ch".into(),
                formula_enabled: true,
                table_enabled: true,
            }],
        };

        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: PageAnalyzeBatchRequest = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, request);
        assert_eq!(decoded.validate_envelope(), Ok(()));
        assert_eq!(decoded.items[0].page.validate(), Ok(()));
    }

    #[test]
    fn python_and_rust_share_page_analyze_request_fixture() {
        let fixture = include_str!("../../../../../pipeline/fixtures/page-analyze-request.json");
        let request: PageAnalyzeBatchRequest = serde_json::from_str(fixture).unwrap();

        request.validate_envelope().unwrap();
        request.items[0].page.validate().unwrap();
        assert_eq!(request.items[0].page.page_id, "fixture/page-1");
        assert!(request.items[0].formula_enabled);
        assert!(request.items[0].table_enabled);
    }

    #[test]
    fn invalid_geometry_and_confidence_are_rejected() {
        let mut invalid = region();
        invalid.bbox = [10.0, 0.0, 2.0, 4.0];
        assert_eq!(
            invalid.validate(),
            Err(ContractValidationError::InvalidBbox)
        );

        let mut invalid = region();
        invalid.confidence = Some(1.1);
        assert_eq!(
            invalid.validate(),
            Err(ContractValidationError::InvalidConfidence)
        );
    }

    #[test]
    fn batch_item_can_isolate_a_region_failure() {
        let response = BatchItemResult::<LayoutResult> {
            page_id: "doc-1/page-1".into(),
            result: None,
            error: Some(StageError {
                code: "layout_failed".into(),
                message: "model error".into(),
                retryable: true,
                region_id: None,
            }),
            warnings: vec![],
        };

        let encoded = serde_json::to_value(&response).unwrap();
        assert_eq!(encoded["error"]["retryable"], true);
        assert!(encoded["result"].is_null());
    }

    #[test]
    fn batch_item_requires_exactly_one_outcome() {
        let empty = BatchItemResult::<LayoutResult> {
            page_id: "page-1".into(),
            result: None,
            error: None,
            warnings: vec![],
        };
        assert_eq!(
            empty.validate_outcome(),
            Err(ContractValidationError::InvalidBatchOutcome)
        );

        let both = BatchItemResult {
            page_id: "page-1".into(),
            result: Some(LayoutResult { regions: vec![] }),
            error: Some(StageError {
                code: "failed".into(),
                message: "failure".into(),
                retryable: false,
                region_id: None,
            }),
            warnings: vec![],
        };
        assert_eq!(
            both.validate_outcome(),
            Err(ContractValidationError::InvalidBatchOutcome)
        );
    }

    #[test]
    fn document_contract_rejects_non_pdf_payloads() {
        let document = EncodedDocument {
            document_id: "doc-1".into(),
            media_type: "image/png".into(),
            base64_data: "cG5n".into(),
        };
        assert_eq!(
            document.validate(),
            Err(ContractValidationError::UnsupportedDocumentMediaType)
        );
    }

    #[tokio::test]
    async fn rust_orchestrates_the_complete_v2_ocr_workflow() {
        let adapter = PipelineV2Adapter::from_endpoint_base("http://pipeline.test".into());
        let mock = Arc::new(MockDispatch::new());
        mock.seed(
            &adapter.layout_endpoint,
            stage_response(
                "pipeline-v2-1-layout",
                json!({
                    "regions": [
                        {
                            "region_id": "text-1",
                            "label": "text",
                            "bbox": [10.0, 10.0, 190.0, 50.0],
                            "polygon": null,
                            "confidence": 0.99,
                            "coordinate_space": "render_pixels"
                        },
                        {
                            "region_id": "table-1",
                            "label": "table",
                            "bbox": [10.0, 70.0, 190.0, 120.0],
                            "polygon": null,
                            "confidence": 0.95,
                            "coordinate_space": "render_pixels"
                        }
                    ]
                }),
            ),
        );
        mock.seed(
            &adapter.formula_detection_endpoint,
            stage_response(
                "pipeline-v2-1-formula_detect",
                json!({
                    "regions": [{
                        "region_id": "formula-1",
                        "label": "interline_equation",
                        "bbox": [10.0, 140.0, 190.0, 175.0],
                        "polygon": null,
                        "confidence": 0.93,
                        "coordinate_space": "render_pixels"
                    }]
                }),
            ),
        );
        mock.seed(
            &adapter.ocr_endpoint,
            stage_response(
                "pipeline-v2-1-ocr",
                json!({
                    "spans": [{
                        "span_id": "span-1",
                        "parent_region_id": "text-1",
                        "polygon": {"points": [[10.0, 10.0], [190.0, 10.0], [190.0, 30.0], [10.0, 30.0]]},
                        "text": "Pipeline V2 text",
                        "language": "en",
                        "confidence": 0.98,
                        "coordinate_space": "render_pixels"
                    }]
                }),
            ),
        );
        mock.seed(
            &adapter.formula_recognition_endpoint,
            stage_response(
                "pipeline-v2-1-formula_recognize",
                json!({
                    "spans": [{
                        "region_id": "formula-1",
                        "latex": "x^2 + y^2",
                        "confidence": 0.97,
                        "bbox": [10.0, 140.0, 190.0, 175.0]
                    }]
                }),
            ),
        );
        // Table regions never receive spans from the page-level OCR call
        // (the OCR backend's `TEXT_REGION_LABELS` excludes "table"), so a
        // second, table-scoped OCR dispatch is required — this is the
        // response to that second call.
        mock.seed(
            &adapter.ocr_endpoint,
            stage_response(
                "pipeline-v2-1-table_ocr",
                json!({
                    "spans": [{
                        "span_id": "table-span-1",
                        "parent_region_id": "table-1",
                        "polygon": {"points": [[10.0, 70.0], [190.0, 70.0], [190.0, 120.0], [10.0, 120.0]]},
                        "text": "A",
                        "language": "ch",
                        "confidence": 0.95,
                        "coordinate_space": "render_pixels"
                    }]
                }),
            ),
        );
        mock.seed(
            &adapter.table_endpoint,
            stage_response(
                "pipeline-v2-1-table",
                json!({
                    "tables": [{
                        "region_id": "table-1",
                        "html": "<table><tr><td>A</td></tr></table>",
                        "structure_tokens": ["<table>", "<tr>", "<td>", "</td>", "</tr>", "</table>"],
                        "cells": [{
                            "cell_id": "cell-1",
                            "row": 0,
                            "column": 0,
                            "row_span": 1,
                            "column_span": 1,
                            "bbox": [10.0, 70.0, 190.0, 120.0],
                            "text": "A",
                            "confidence": 0.96
                        }],
                        "classifier_label": "wired",
                        "rotation_degrees": 0,
                        "confidence": 0.96
                    }]
                }),
            ),
        );

        let ctx = ParseCtx::with_mock(mock.clone(), Arc::new(Semaphore::new(4)));
        let blocks = adapter.parse_page(&rendered_page(), &ctx).await.unwrap();

        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].text.as_deref(), Some("Pipeline V2 text"));
        assert_eq!(
            blocks[1].html.as_deref(),
            Some("<table><tr><td>A</td></tr></table>")
        );
        assert_eq!(blocks[2].latex.as_deref(), Some("x^2 + y^2"));
        assert_eq!(
            blocks
                .iter()
                .map(|block| block.reading_order)
                .collect::<Vec<_>>(),
            vec![Some(0), Some(1), Some(2)]
        );
        assert!(
            blocks
                .iter()
                .all(|block| block.source == BlockSource::OcrPipeline)
        );

        // Prove the table stage genuinely received the table-scoped OCR
        // spans (parent_region_id "table-1", text "A") and not the
        // page-level OCR response (parent_region_id "text-1") — the
        // regression this test exists to catch (see
        // `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`, D1).
        let table_requests = mock.recorded_requests(&adapter.table_endpoint);
        assert_eq!(table_requests.len(), 1);
        let sent_spans = table_requests[0]["items"][0]["ocr_spans"]
            .as_array()
            .expect("table request carries an ocr_spans array");
        assert_eq!(sent_spans.len(), 1);
        assert_eq!(sent_spans[0]["parent_region_id"], "table-1");
        assert_eq!(sent_spans[0]["text"], "A");

        // Two independent dispatches actually reached the OCR endpoint
        // (page-level "ocr" stage + table-scoped "table_ocr" stage).
        let ocr_requests = mock.recorded_requests(&adapter.ocr_endpoint);
        assert_eq!(ocr_requests.len(), 2);
    }

    /// D4: the layout model's own duplicate formula detection (a
    /// `display_formula` region overlapping the same area an independent
    /// MFD detection already covers) must be dropped — otherwise it
    /// survives as a blank block (no OCR, since formula regions aren't in
    /// `TEXT_REGION_LABELS`; no latex, since MFR only recognizes
    /// MFD-origin regions) that occupies a reading-order slot for
    /// nothing. See `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md`, D4.
    #[tokio::test]
    async fn duplicate_layout_formula_region_is_dropped_in_favor_of_mfd_detection() {
        let adapter = PipelineV2Adapter::from_endpoint_base("http://pipeline.test".into());
        let mock = Arc::new(MockDispatch::new());
        mock.seed(
            &adapter.layout_endpoint,
            stage_response(
                "pipeline-v2-1-layout",
                json!({
                    "regions": [
                        {
                            "region_id": "text-1",
                            "label": "text",
                            "bbox": [10.0, 10.0, 190.0, 50.0],
                            "polygon": null,
                            "confidence": 0.99,
                            "coordinate_space": "render_pixels"
                        },
                        {
                            "region_id": "layout-formula-1",
                            "label": "display_formula",
                            "bbox": [10.0, 140.0, 190.0, 175.0],
                            "polygon": null,
                            "confidence": 0.5,
                            "coordinate_space": "render_pixels"
                        }
                    ]
                }),
            ),
        );
        mock.seed(
            &adapter.formula_detection_endpoint,
            stage_response(
                "pipeline-v2-1-formula_detect",
                json!({
                    "regions": [{
                        "region_id": "mfd-formula-1",
                        "label": "display_formula",
                        "bbox": [10.0, 140.0, 190.0, 175.0],
                        "polygon": null,
                        "confidence": 0.93,
                        "coordinate_space": "render_pixels"
                    }]
                }),
            ),
        );
        mock.seed(
            &adapter.ocr_endpoint,
            stage_response(
                "pipeline-v2-1-ocr",
                json!({
                    "spans": [{
                        "span_id": "span-1",
                        "parent_region_id": "text-1",
                        "polygon": {"points": [[10.0, 10.0], [190.0, 10.0], [190.0, 30.0], [10.0, 30.0]]},
                        "text": "Pipeline V2 text",
                        "language": "en",
                        "confidence": 0.98,
                        "coordinate_space": "render_pixels"
                    }]
                }),
            ),
        );
        mock.seed(
            &adapter.formula_recognition_endpoint,
            stage_response(
                "pipeline-v2-1-formula_recognize",
                json!({
                    "spans": [{
                        "region_id": "mfd-formula-1",
                        "latex": "x^2 + y^2",
                        "confidence": 0.97,
                        "bbox": [10.0, 140.0, 190.0, 175.0]
                    }]
                }),
            ),
        );

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(4)));
        let blocks = adapter.parse_page(&rendered_page(), &ctx).await.unwrap();

        // Only the text block and the single MFD-origin formula block
        // survive — the duplicate layout-origin "display_formula" region
        // (same bbox, no latex) is dropped, not kept as a second,
        // content-free "display_formula" block.
        assert_eq!(blocks.len(), 2);
        let formula_blocks = blocks
            .iter()
            .filter(|block| block.category_raw == "display_formula")
            .collect::<Vec<_>>();
        assert_eq!(formula_blocks.len(), 1);
        assert_eq!(formula_blocks[0].latex.as_deref(), Some("x^2 + y^2"));
    }

    /// D8: a page whose layout stage returns zero regions previously
    /// rendered as silently-blank Markdown with no signal anywhere that
    /// distinguishes it from a genuinely blank source page. It should now
    /// surface a warning.
    #[tokio::test]
    async fn layout_returning_no_regions_at_all_surfaces_a_warning() {
        let adapter = PipelineV2Adapter::from_endpoint_base("http://pipeline.test".into());
        let mock = Arc::new(MockDispatch::new());
        mock.seed(
            &adapter.layout_endpoint,
            stage_response("pipeline-v2-1-layout", json!({"regions": []})),
        );
        mock.seed(
            &adapter.formula_detection_endpoint,
            stage_response("pipeline-v2-1-formula_detect", json!({"regions": []})),
        );
        mock.seed(
            &adapter.ocr_endpoint,
            stage_response("pipeline-v2-1-ocr", json!({"spans": []})),
        );
        mock.seed(
            &adapter.formula_recognition_endpoint,
            stage_response("pipeline-v2-1-formula_recognize", json!({"spans": []})),
        );

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(4)));
        let blocks = adapter.parse_page(&rendered_page(), &ctx).await.unwrap();

        assert!(blocks.is_empty());
        assert!(
            ctx.warnings_snapshot()
                .iter()
                .any(|warning| warning.contains("layout produced no regions at all")),
        );
    }

    /// D8: a page with detected regions that nonetheless produce zero
    /// renderable content (no OCR text, no latex, no table HTML, no
    /// image crop — e.g. a region the OCR backend's region-type filter
    /// excludes and that isn't itself a table/formula/image) is the same
    /// "silently blank" failure mode as the zero-region case, just with
    /// regions present. It should also warn.
    #[tokio::test]
    async fn regions_with_no_renderable_content_surface_a_warning() {
        let adapter = PipelineV2Adapter::from_endpoint_base("http://pipeline.test".into());
        let mock = Arc::new(MockDispatch::new());
        mock.seed(
            &adapter.layout_endpoint,
            stage_response(
                "pipeline-v2-1-layout",
                json!({
                    "regions": [{
                        "region_id": "discarded-1",
                        "label": "discarded",
                        "bbox": [10.0, 10.0, 190.0, 50.0],
                        "polygon": null,
                        "confidence": 0.6,
                        "coordinate_space": "render_pixels"
                    }]
                }),
            ),
        );
        mock.seed(
            &adapter.formula_detection_endpoint,
            stage_response("pipeline-v2-1-formula_detect", json!({"regions": []})),
        );
        mock.seed(
            &adapter.ocr_endpoint,
            stage_response("pipeline-v2-1-ocr", json!({"spans": []})),
        );
        mock.seed(
            &adapter.formula_recognition_endpoint,
            stage_response("pipeline-v2-1-formula_recognize", json!({"spans": []})),
        );

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(4)));
        let blocks = adapter.parse_page(&rendered_page(), &ctx).await.unwrap();

        assert_eq!(blocks.len(), 1);
        assert!(ctx.warnings_snapshot().iter().any(|warning| {
            warning.contains("none produced renderable text/html/latex/image content")
        }));
    }
}
