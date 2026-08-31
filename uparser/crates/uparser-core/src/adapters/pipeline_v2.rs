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
use crate::{pipeline_formula, pipeline_layout, pipeline_ocr, pipeline_table, tensor_wire};
use async_trait::async_trait;
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::OnceLock;
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
    pub bare_layout_endpoint: Option<String>,
    pub formula_detection_endpoint: String,
    pub ocr_endpoint: String,
    pub bare_ocr_endpoint_base: Option<String>,
    pub ocr_dictionary_path: Option<String>,
    ocr_dictionary: OnceLock<Result<pipeline_ocr::CtcDictionary, String>>,
    pub formula_recognition_endpoint: String,
    pub bare_formula_endpoint: Option<String>,
    pub formula_tokenizer_path: Option<String>,
    formula_decoder: OnceLock<Result<pipeline_formula::FormulaDecoder, String>>,
    pub table_endpoint: String,
    pub bare_table_endpoint_base: Option<String>,
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
            bare_layout_endpoint: None,
            formula_detection_endpoint: format!("{base}/v2/pipeline/mfd:batch"),
            ocr_endpoint: format!("{base}/v2/pipeline/ocr:batch"),
            bare_ocr_endpoint_base: None,
            ocr_dictionary_path: None,
            ocr_dictionary: OnceLock::new(),
            formula_recognition_endpoint: format!("{base}/v2/pipeline/mfr:batch"),
            bare_formula_endpoint: None,
            formula_tokenizer_path: None,
            formula_decoder: OnceLock::new(),
            table_endpoint: format!("{base}/v2/pipeline/table:batch"),
            bare_table_endpoint_base: None,
            language: "ch".to_owned(),
            timeout: Duration::from_secs(180),
            max_retries: 2,
        }
    }

    pub fn set_endpoint_base(&mut self, endpoint_base: String) {
        let replacement = Self::from_endpoint_base(endpoint_base);
        self.endpoint_base = replacement.endpoint_base;
        self.layout_endpoint = replacement.layout_endpoint;
        self.bare_layout_endpoint = replacement.bare_layout_endpoint;
        self.formula_detection_endpoint = replacement.formula_detection_endpoint;
        self.ocr_endpoint = replacement.ocr_endpoint;
        self.bare_ocr_endpoint_base = replacement.bare_ocr_endpoint_base;
        self.ocr_dictionary_path = replacement.ocr_dictionary_path;
        self.ocr_dictionary = OnceLock::new();
        self.formula_recognition_endpoint = replacement.formula_recognition_endpoint;
        self.bare_formula_endpoint = replacement.bare_formula_endpoint;
        self.formula_tokenizer_path = replacement.formula_tokenizer_path;
        self.formula_decoder = OnceLock::new();
        self.table_endpoint = replacement.table_endpoint;
        self.bare_table_endpoint_base = replacement.bare_table_endpoint_base;
    }

    pub fn apply_config(&mut self, config: &PipelineConfig) {
        if let Some(endpoint) = &config.layout_endpoint {
            self.layout_endpoint = endpoint.clone();
        }
        if let Some(endpoint) = &config.bare_layout_endpoint {
            self.bare_layout_endpoint = Some(endpoint.clone());
        }
        if let Some(endpoint) = &config.formula_detection_endpoint {
            self.formula_detection_endpoint = endpoint.clone();
        }
        if let Some(endpoint) = &config.ocr_endpoint {
            self.ocr_endpoint = endpoint.clone();
        }
        if let Some(endpoint) = &config.bare_ocr_endpoint_base {
            self.bare_ocr_endpoint_base = Some(endpoint.trim_end_matches('/').to_owned());
        }
        if let Some(path) = &config.ocr_dictionary_path {
            self.ocr_dictionary_path = Some(path.clone());
            self.ocr_dictionary = OnceLock::new();
        }
        if let Some(endpoint) = &config.formula_endpoint {
            self.formula_recognition_endpoint = endpoint.clone();
        }
        if let Some(endpoint) = &config.bare_formula_endpoint {
            self.bare_formula_endpoint = Some(endpoint.clone());
        }
        if let Some(path) = &config.formula_tokenizer_path {
            self.formula_tokenizer_path = Some(path.clone());
            self.formula_decoder = OnceLock::new();
        }
        if let Some(endpoint) = &config.table_endpoint {
            self.table_endpoint = endpoint.clone();
        }
        if let Some(endpoint) = &config.bare_table_endpoint_base {
            self.bare_table_endpoint_base = Some(endpoint.trim_end_matches('/').to_owned());
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

    async fn dispatch_bare_layout(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
        endpoint: &str,
    ) -> Result<(LayoutResult, FormulaDetectionResult), PageError> {
        let (inputs, original) = pipeline_layout::preprocess(&page.png_bytes, 800, 800)
            .map_err(|error| page_error(page, "layout_preprocess", error.to_string()))?;
        if original != (page.width, page.height) {
            return Err(page_error(
                page,
                "layout_preprocess",
                format!(
                    "decoded image dimensions {original:?} differ from rendered page {}x{}",
                    page.width, page.height
                ),
            ));
        }
        let body = tensor_wire::encode(&inputs)
            .map_err(|error| page_error(page, "layout_preprocess", error.to_string()))?;
        let _permit = ctx.acquire_permit().await;
        let response = ctx
            .dispatch_binary(endpoint, body, self.timeout, self.max_retries)
            .await
            .map_err(|error| page_error(page, "layout_forward", error.to_string()))?;
        let outputs = tensor_wire::decode(&response)
            .map_err(|error| page_error(page, "layout_decode", error.to_string()))?;
        let detections = pipeline_layout::decode(&outputs, page.width, page.height, 0.45)
            .map_err(|error| page_error(page, "layout_decode", error.to_string()))?;

        let page_id = format!("page-{}", page.page_num);
        let regions: Vec<Region> = detections
            .into_iter()
            .enumerate()
            .map(|(index, detection)| Region {
                region_id: format!("{page_id}/layout-{index}"),
                label: detection.label.to_owned(),
                bbox: detection.bbox,
                polygon: None,
                confidence: Some(detection.score.clamp(0.0, 1.0)),
                coordinate_space: CoordinateSpace::RenderPixels,
            })
            .collect();
        let formulas = regions
            .iter()
            .filter(|region| matches!(region.label.as_str(), "inline_formula" | "display_formula"))
            .cloned()
            .collect();
        Ok((
            LayoutResult { regions },
            FormulaDetectionResult { regions: formulas },
        ))
    }

    async fn dispatch_bare_formula(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
        endpoint: &str,
        regions: &[Region],
    ) -> Result<FormulaRecognitionResult, PageError> {
        if regions.is_empty() {
            return Ok(FormulaRecognitionResult { spans: vec![] });
        }
        let tokenizer_path = self.formula_tokenizer_path.as_deref().ok_or_else(|| {
            page_error(
                page,
                "formula_config",
                "bare formula inference requires a formula tokenizer YAML path".to_owned(),
            )
        })?;
        let decoder = self
            .formula_decoder
            .get_or_init(|| {
                pipeline_formula::FormulaDecoder::from_inference_yaml(tokenizer_path)
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| page_error(page, "formula_config", error.clone()))?;

        let image = image::load_from_memory(&page.png_bytes)
            .map_err(|error| page_error(page, "formula_preprocess", error.to_string()))?
            .to_rgb8();
        let mut batch_data = Vec::with_capacity(regions.len() * 384 * 384 * 4);
        for region in regions {
            let [left, top, right, bottom] = region.bbox;
            let left = left.floor().clamp(0.0, page.width as f32) as u32;
            let top = top.floor().clamp(0.0, page.height as f32) as u32;
            let right = right.ceil().clamp(0.0, page.width as f32) as u32;
            let bottom = bottom.ceil().clamp(0.0, page.height as f32) as u32;
            if right <= left || bottom <= top {
                return Err(page_error(
                    page,
                    "formula_preprocess",
                    format!("formula region {} has an empty crop", region.region_id),
                ));
            }
            let crop =
                image::imageops::crop_imm(&image, left, top, right - left, bottom - top).to_image();
            let mut encoded = Vec::new();
            image::DynamicImage::ImageRgb8(crop)
                .write_to(&mut Cursor::new(&mut encoded), image::ImageFormat::Png)
                .map_err(|error| page_error(page, "formula_preprocess", error.to_string()))?;
            let input = pipeline_formula::preprocess(&encoded)
                .map_err(|error| page_error(page, "formula_preprocess", error.to_string()))?;
            batch_data.extend_from_slice(&input.tensors[0].data);
        }
        let inputs = tensor_wire::TensorBundle {
            metadata: Default::default(),
            tensors: vec![tensor_wire::Tensor {
                name: "pixel_values".to_owned(),
                dtype: tensor_wire::TensorDType::F32,
                shape: vec![regions.len(), 1, 384, 384],
                data: batch_data,
            }],
        };
        let body = tensor_wire::encode(&inputs)
            .map_err(|error| page_error(page, "formula_preprocess", error.to_string()))?;
        let _permit = ctx.acquire_permit().await;
        let response = ctx
            .dispatch_binary(endpoint, body, self.timeout, self.max_retries)
            .await
            .map_err(|error| page_error(page, "formula_forward", error.to_string()))?;
        let outputs = tensor_wire::decode(&response)
            .map_err(|error| page_error(page, "formula_decode", error.to_string()))?;
        let latex = decoder
            .decode(&outputs)
            .map_err(|error| page_error(page, "formula_decode", error.to_string()))?;
        if latex.len() != regions.len() {
            return Err(page_error(
                page,
                "formula_decode",
                format!(
                    "model returned {} formulas for {} regions",
                    latex.len(),
                    regions.len()
                ),
            ));
        }
        Ok(FormulaRecognitionResult {
            spans: regions
                .iter()
                .zip(latex)
                .map(|(region, latex)| FormulaSpan {
                    region_id: region.region_id.clone(),
                    latex,
                    confidence: None,
                    bbox: Some(region.bbox),
                })
                .collect(),
        })
    }

    async fn dispatch_bare_ocr(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
        endpoint_base: &str,
        regions: &[Region],
        formula_regions: &[Region],
        crop_padding: u32,
    ) -> Result<OcrResult, PageError> {
        let dictionary_path = self.ocr_dictionary_path.as_deref().ok_or_else(|| {
            page_error(
                page,
                "ocr_config",
                "bare OCR requires a character dictionary path".to_owned(),
            )
        })?;
        let dictionary = self
            .ocr_dictionary
            .get_or_init(|| {
                pipeline_ocr::CtcDictionary::from_path(dictionary_path)
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| page_error(page, "ocr_config", error.clone()))?;
        let image = image::load_from_memory(&page.png_bytes)
            .map_err(|error| page_error(page, "ocr_preprocess", error.to_string()))?
            .to_rgb8();
        let detector_endpoint = format!("{endpoint_base}/v1/models/pp_ocrv6_det:infer");
        let recognizer_endpoint = format!("{endpoint_base}/v1/models/pp_ocrv6_rec:infer");
        let mut spans = Vec::new();
        for region in regions.iter().filter(|region| {
            !matches!(
                region.label.as_str(),
                "image" | "chart" | "table" | "inline_formula" | "display_formula"
            )
        }) {
            let [left, top, right, bottom] = region.bbox;
            let crop_left = left.floor().clamp(0.0, page.width as f32) as u32;
            let crop_top = top.floor().clamp(0.0, page.height as f32) as u32;
            let crop_right = right.ceil().clamp(0.0, page.width as f32) as u32;
            let crop_bottom = bottom.ceil().clamp(0.0, page.height as f32) as u32;
            if crop_right <= crop_left || crop_bottom <= crop_top {
                continue;
            }
            let source_crop = image::imageops::crop_imm(
                &image,
                crop_left,
                crop_top,
                crop_right - crop_left,
                crop_bottom - crop_top,
            )
            .to_image();
            let crop = if crop_padding == 0 {
                source_crop
            } else {
                let mut padded = image::RgbImage::from_pixel(
                    source_crop.width() + crop_padding * 2,
                    source_crop.height() + crop_padding * 2,
                    image::Rgb([255, 255, 255]),
                );
                image::imageops::replace(
                    &mut padded,
                    &source_crop,
                    i64::from(crop_padding),
                    i64::from(crop_padding),
                );
                padded
            };
            let formula_boxes: Vec<[f32; 4]> = formula_regions
                .iter()
                .map(|formula| {
                    [
                        formula.bbox[0] - crop_left as f32 + crop_padding as f32,
                        formula.bbox[1] - crop_top as f32 + crop_padding as f32,
                        formula.bbox[2] - crop_left as f32 + crop_padding as f32,
                        formula.bbox[3] - crop_top as f32 + crop_padding as f32,
                    ]
                })
                .collect();
            let detection_crop = pipeline_ocr::mask_detection_regions(&crop, &formula_boxes);
            let detection_input = pipeline_ocr::preprocess_detection(&detection_crop)
                .map_err(|error| page_error(page, "ocr_preprocess", error.to_string()))?;
            let detection_output = self
                .dispatch_bare_tensor(
                    page,
                    ctx,
                    &detector_endpoint,
                    "ocr_detect_forward",
                    &detection_input.tensors,
                )
                .await?;
            let detections =
                pipeline_ocr::decode_detection(&detection_output, crop.dimensions(), 0.3, 0.6, 1.5)
                    .map_err(|error| page_error(page, "ocr_detect_decode", error.to_string()))?;
            let detections = if crop_padding > 0 {
                pipeline_ocr::merge_detections(&detections)
            } else {
                detections
            };
            let mut kept = Vec::new();
            let mut recognition_crops = Vec::new();
            for detection in detections.iter().flat_map(|detection| {
                pipeline_ocr::split_detection_around_formulas(detection, &formula_boxes)
            }) {
                let page_points = detection.points.map(|point| {
                    [
                        point[0] - crop_padding as f32 + crop_left as f32,
                        point[1] - crop_padding as f32 + crop_top as f32,
                    ]
                });
                recognition_crops.push(
                    pipeline_ocr::crop_detection(&crop, &detection)
                        .map_err(|error| page_error(page, "ocr_crop", error.to_string()))?,
                );
                kept.push((detection, page_points));
            }
            if recognition_crops.is_empty() {
                continue;
            }
            let recognition_input = pipeline_ocr::preprocess_recognition(&recognition_crops)
                .map_err(|error| page_error(page, "ocr_preprocess", error.to_string()))?;
            let recognition_output = self
                .dispatch_bare_tensor(
                    page,
                    ctx,
                    &recognizer_endpoint,
                    "ocr_recognize_forward",
                    &recognition_input,
                )
                .await?;
            let recognized = pipeline_ocr::decode_ctc(&recognition_output, dictionary)
                .map_err(|error| page_error(page, "ocr_recognize_decode", error.to_string()))?;
            if recognized.len() != kept.len() {
                return Err(page_error(
                    page,
                    "ocr_recognize_decode",
                    format!(
                        "recognizer returned {} rows for {} crops",
                        recognized.len(),
                        kept.len()
                    ),
                ));
            }
            for ((detection, points), recognition) in kept.into_iter().zip(recognized) {
                if recognition.text.trim().is_empty() {
                    continue;
                }
                spans.push(OcrSpan {
                    span_id: format!("{}/ocr-{}", region.region_id, spans.len()),
                    parent_region_id: Some(region.region_id.clone()),
                    polygon: Polygon {
                        points: points.to_vec(),
                    },
                    text: recognition.text,
                    language: Some(self.language.clone()),
                    confidence: Some(
                        (detection.confidence * recognition.confidence).clamp(0.0, 1.0),
                    ),
                    coordinate_space: CoordinateSpace::RenderPixels,
                });
            }
        }
        Ok(OcrResult { spans })
    }

    async fn dispatch_bare_tensor(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
        endpoint: &str,
        stage: &str,
        inputs: &tensor_wire::TensorBundle,
    ) -> Result<tensor_wire::TensorBundle, PageError> {
        let body = tensor_wire::encode(inputs)
            .map_err(|error| page_error(page, stage, error.to_string()))?;
        let _permit = ctx.acquire_permit().await;
        let response = ctx
            .dispatch_binary(endpoint, body, self.timeout, self.max_retries)
            .await
            .map_err(|error| page_error(page, stage, error.to_string()))?;
        tensor_wire::decode(&response).map_err(|error| page_error(page, stage, error.to_string()))
    }

    async fn recognize_bare_ocr_crops(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
        endpoint_base: &str,
        dictionary: &pipeline_ocr::CtcDictionary,
        crops: &[image::RgbImage],
        stage: &str,
    ) -> Result<Vec<pipeline_ocr::CtcRecognition>, PageError> {
        if crops.is_empty() {
            return Ok(Vec::new());
        }
        let input = pipeline_ocr::preprocess_recognition(crops)
            .map_err(|error| page_error(page, "table_orientation_preprocess", error.to_string()))?;
        let endpoint = format!("{endpoint_base}/v1/models/pp_ocrv6_rec:infer");
        let output = self
            .dispatch_bare_tensor(page, ctx, &endpoint, stage, &input)
            .await?;
        pipeline_ocr::decode_ctc(&output, dictionary)
            .map_err(|error| page_error(page, "table_orientation_decode", error.to_string()))
    }

    async fn dispatch_bare_tables(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
        endpoint_base: &str,
        regions: &[Region],
        ocr_spans: &[OcrSpan],
        formula_spans: &[FormulaSpan],
    ) -> Result<TableRecognitionResult, PageError> {
        let page_image = image::load_from_memory(&page.png_bytes)
            .map_err(|error| page_error(page, "table_preprocess", error.to_string()))?
            .to_rgb8();
        let mut tables = Vec::with_capacity(regions.len());
        for region in regions {
            let [left, top, right, bottom] = region.bbox;
            let crop_left = left.floor().clamp(0.0, page.width as f32) as u32;
            let crop_top = top.floor().clamp(0.0, page.height as f32) as u32;
            let crop_right = right.ceil().clamp(0.0, page.width as f32) as u32;
            let crop_bottom = bottom.ceil().clamp(0.0, page.height as f32) as u32;
            if crop_right <= crop_left || crop_bottom <= crop_top {
                return Err(page_error(
                    page,
                    "table_preprocess",
                    format!("table region {} has an empty crop", region.region_id),
                ));
            }
            let original_crop = image::imageops::crop_imm(
                &page_image,
                crop_left,
                crop_top,
                crop_right - crop_left,
                crop_bottom - crop_top,
            )
            .to_image();
            let source_spans = ocr_spans
                .iter()
                .filter(|span| span.parent_region_id.as_deref() == Some(region.region_id.as_str()))
                .collect::<Vec<_>>();
            let mut rotation = 0;
            let mut local_spans = source_spans
                .iter()
                .map(|span| pipeline_table::TableTextSpan {
                    id: span.span_id.clone(),
                    polygon: span
                        .polygon
                        .points
                        .iter()
                        .map(|point| [point[0] - crop_left as f32, point[1] - crop_top as f32])
                        .collect(),
                    text: span.text.clone(),
                })
                .collect::<Vec<_>>();

            if is_table_rotation_candidate(&source_spans) {
                let dictionary_path = self.ocr_dictionary_path.as_deref().ok_or_else(|| {
                    page_error(
                        page,
                        "table_orientation_config",
                        "bare table orientation requires a character dictionary path".to_owned(),
                    )
                })?;
                let dictionary = self
                    .ocr_dictionary
                    .get_or_init(|| {
                        pipeline_ocr::CtcDictionary::from_path(dictionary_path)
                            .map_err(|error| error.to_string())
                    })
                    .as_ref()
                    .map_err(|error| page_error(page, "table_orientation_config", error.clone()))?;
                let sampled = orientation_sample_indices(local_spans.len());
                let raw_crops = sampled
                    .iter()
                    .filter_map(|&index| {
                        crop_table_text_span(&original_crop, &local_spans[index].polygon)
                    })
                    .collect::<Vec<_>>();
                if raw_crops.len() >= 5 {
                    let ccw_crops = raw_crops
                        .iter()
                        .map(image::imageops::rotate270)
                        .collect::<Vec<_>>();
                    let cw_crops = raw_crops
                        .iter()
                        .map(image::imageops::rotate90)
                        .collect::<Vec<_>>();
                    let zero = self.recognize_bare_ocr_crops(
                        page,
                        ctx,
                        endpoint_base,
                        dictionary,
                        &raw_crops,
                        "table_orientation_recognize_0",
                    );
                    let ccw = self.recognize_bare_ocr_crops(
                        page,
                        ctx,
                        endpoint_base,
                        dictionary,
                        &ccw_crops,
                        "table_orientation_recognize_90",
                    );
                    let cw = self.recognize_bare_ocr_crops(
                        page,
                        ctx,
                        endpoint_base,
                        dictionary,
                        &cw_crops,
                        "table_orientation_recognize_270",
                    );
                    let (zero, ccw, cw) = tokio::join!(zero, ccw, cw);
                    let zero = zero?;
                    let ccw = ccw?;
                    let cw = cw?;
                    rotation = select_table_rotation([
                        orientation_score(&zero),
                        orientation_score(&ccw),
                        orientation_score(&cw),
                    ]);

                    if rotation != 0 {
                        let rotated_crops = local_spans
                            .iter()
                            .filter_map(|span| {
                                crop_table_text_span(&original_crop, &span.polygon).map(|crop| {
                                    if rotation == 90 {
                                        image::imageops::rotate270(&crop)
                                    } else {
                                        image::imageops::rotate90(&crop)
                                    }
                                })
                            })
                            .collect::<Vec<_>>();
                        let recognized = self
                            .recognize_bare_ocr_crops(
                                page,
                                ctx,
                                endpoint_base,
                                dictionary,
                                &rotated_crops,
                                "table_rotated_ocr_recognize",
                            )
                            .await?;
                        if recognized.len() == local_spans.len() {
                            for (span, recognition) in local_spans.iter_mut().zip(recognized) {
                                span.text = recognition.text;
                            }
                        }
                        let (width, height) = original_crop.dimensions();
                        for span in &mut local_spans {
                            for point in &mut span.polygon {
                                *point = rotate_table_point(
                                    *point,
                                    width as f32,
                                    height as f32,
                                    rotation,
                                );
                            }
                        }
                    }
                }
            }

            let crop = match rotation {
                90 => image::imageops::rotate270(&original_crop),
                270 => image::imageops::rotate90(&original_crop),
                _ => original_crop.clone(),
            };
            let classifier_input = pipeline_table::preprocess_classifier(&crop)
                .map_err(|error| page_error(page, "table_preprocess", error.to_string()))?;
            let slanet_input = pipeline_table::preprocess_slanet(&crop)
                .map_err(|error| page_error(page, "table_preprocess", error.to_string()))?;
            let unet_input = pipeline_table::preprocess_unet(&crop)
                .map_err(|error| page_error(page, "table_preprocess", error.to_string()))?;
            let classifier_endpoint = format!("{endpoint_base}/v1/models/paddle_table_cls:infer");
            let slanet_endpoint = format!("{endpoint_base}/v1/models/slanet_plus:infer");
            let unet_endpoint = format!("{endpoint_base}/v1/models/unet_table_structure:infer");
            let classifier = self.dispatch_bare_tensor(
                page,
                ctx,
                &classifier_endpoint,
                "table_classifier_forward",
                &classifier_input,
            );
            let slanet = self.dispatch_bare_tensor(
                page,
                ctx,
                &slanet_endpoint,
                "table_slanet_forward",
                &slanet_input,
            );
            let unet = self.dispatch_bare_tensor(
                page,
                ctx,
                &unet_endpoint,
                "table_unet_forward",
                &unet_input,
            );
            let (classifier, slanet, unet) = tokio::join!(classifier, slanet, unet);
            let classification = pipeline_table::decode_classifier(&classifier?)
                .map_err(|error| page_error(page, "table_classifier_decode", error.to_string()))?;
            let slanet = pipeline_table::decode_slanet(&slanet?, crop.dimensions())
                .map_err(|error| page_error(page, "table_slanet_decode", error.to_string()))?;
            let wired = pipeline_table::decode_unet(&unet?, crop.dimensions())
                .map_err(|error| page_error(page, "table_unet_decode", error.to_string()))?;

            if rotation == 0 {
                local_spans.extend(formula_spans.iter().filter_map(|span| {
                    let bbox = span.bbox?;
                    let latex = span.latex.trim();
                    let center = [(bbox[0] + bbox[2]) * 0.5, (bbox[1] + bbox[3]) * 0.5];
                    if latex.is_empty()
                        || center[0] < region.bbox[0]
                        || center[0] > region.bbox[2]
                        || center[1] < region.bbox[1]
                        || center[1] > region.bbox[3]
                    {
                        return None;
                    }
                    Some(pipeline_table::TableTextSpan {
                        id: format!("{}/table-formula", span.region_id),
                        polygon: vec![
                            [bbox[0] - crop_left as f32, bbox[1] - crop_top as f32],
                            [bbox[2] - crop_left as f32, bbox[1] - crop_top as f32],
                            [bbox[2] - crop_left as f32, bbox[3] - crop_top as f32],
                            [bbox[0] - crop_left as f32, bbox[3] - crop_top as f32],
                        ],
                        text: format!("${latex}$"),
                    })
                }));
            }
            let mut wired_cells = wired.cells;
            pipeline_table::bind_spans_to_cells(&mut wired_cells, &local_spans);
            let wired_candidate = pipeline_table::TableCandidate {
                html: pipeline_table::render_wired_html(&wired_cells),
                cells: wired_cells,
            };
            let mut wireless_cells = slanet.cells;
            pipeline_table::bind_spans_to_cells(&mut wireless_cells, &local_spans);
            let wireless_candidate = pipeline_table::TableCandidate {
                html: pipeline_table::render_slanet_html(&slanet.tokens, &wireless_cells),
                cells: wireless_cells,
            };
            let ocr_texts: Vec<String> = local_spans.iter().map(|span| span.text.clone()).collect();
            let selection = if classification.model == pipeline_table::SelectedTableModel::Wireless
                && classification.confidence >= 0.9
            {
                pipeline_table::TableSelection {
                    model: pipeline_table::SelectedTableModel::Wireless,
                    reason: "high_confidence_wireless_classifier",
                }
            } else {
                pipeline_table::select_candidate(&wired_candidate, &wireless_candidate, &ocr_texts)
            };
            if std::env::var_os("UPARSER_PIPELINE_TABLE_DEBUG").is_some() {
                eprintln!(
                    "table-debug page={} region={} crop={}x{} rotation={} class={:?} class_confidence={} selection={:?} reason={} wired_cells={} wireless_cells={} wired_html={} wireless_html={}",
                    page.page_num,
                    region.region_id,
                    crop.width(),
                    crop.height(),
                    rotation,
                    classification.model,
                    classification.confidence,
                    selection.model,
                    selection.reason,
                    wired_candidate.cells.len(),
                    wireless_candidate.cells.len(),
                    wired_candidate.html,
                    wireless_candidate.html,
                );
            }
            let selected = match selection.model {
                pipeline_table::SelectedTableModel::Wired => &wired_candidate,
                pipeline_table::SelectedTableModel::Wireless => &wireless_candidate,
            };
            let cells = selected
                .cells
                .iter()
                .enumerate()
                .map(|(index, cell)| {
                    let bbox = cell.bbox.map(|bbox| {
                        let points = [
                            [bbox[0], bbox[1]],
                            [bbox[2], bbox[3]],
                            [bbox[4], bbox[5]],
                            [bbox[6], bbox[7]],
                        ]
                        .map(|point| {
                            inverse_rotate_table_point(
                                point,
                                original_crop.width() as f32,
                                original_crop.height() as f32,
                                rotation,
                            )
                        });
                        let xs = points.map(|point| point[0]);
                        let ys = points.map(|point| point[1]);
                        [
                            xs.into_iter().fold(f32::INFINITY, f32::min) + crop_left as f32,
                            ys.into_iter().fold(f32::INFINITY, f32::min) + crop_top as f32,
                            xs.into_iter().fold(f32::NEG_INFINITY, f32::max) + crop_left as f32,
                            ys.into_iter().fold(f32::NEG_INFINITY, f32::max) + crop_top as f32,
                        ]
                    });
                    TableCell {
                        cell_id: format!("{}/cell-{index}", region.region_id),
                        row: cell.row,
                        column: cell.column,
                        row_span: cell.row_span,
                        column_span: cell.column_span,
                        bbox,
                        text: cell.text.clone(),
                        confidence: None,
                    }
                })
                .collect();
            tables.push(RecognizedTable {
                region_id: region.region_id.clone(),
                html: selected.html.clone(),
                structure_tokens: vec![],
                cells,
                classifier_label: Some(
                    match classification.model {
                        pipeline_table::SelectedTableModel::Wired => "wired",
                        pipeline_table::SelectedTableModel::Wireless => "wireless",
                    }
                    .to_owned(),
                ),
                rotation_degrees: rotation,
                confidence: Some(classification.confidence),
            });
        }
        Ok(TableRecognitionResult { tables })
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

        let (layout, formulas) = if let Some(endpoint) = &self.bare_layout_endpoint {
            self.dispatch_bare_layout(page, ctx, endpoint).await?
        } else {
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
            (layout?, formulas?)
        };
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

        let ocr = async {
            if let Some(endpoint_base) = &self.bare_ocr_endpoint_base {
                self.dispatch_bare_ocr(
                    page,
                    ctx,
                    endpoint_base,
                    &layout.regions,
                    &formulas.regions,
                    50,
                )
                .await
            } else {
                self.dispatch_stage::<_, OcrResult>(
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
                )
                .await
            }
        };
        let recognized_formulas = async {
            if let Some(endpoint) = &self.bare_formula_endpoint {
                self.dispatch_bare_formula(page, ctx, endpoint, &formulas.regions)
                    .await
            } else {
                self.dispatch_stage::<_, FormulaRecognitionResult>(
                    page,
                    ctx,
                    &self.formula_recognition_endpoint,
                    "formula_recognize",
                    FormulaRecognitionInput {
                        page: encoded.clone(),
                        formula_regions: formulas.regions.clone(),
                    },
                )
                .await
            }
        };
        let table_ocr = async {
            if table_ocr_regions.is_empty() {
                return Ok(OcrResult { spans: vec![] });
            }
            if let Some(endpoint_base) = &self.bare_ocr_endpoint_base {
                self.dispatch_bare_ocr(
                    page,
                    ctx,
                    endpoint_base,
                    &table_ocr_regions,
                    &formulas.regions,
                    0,
                )
                .await
            } else {
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
            }
        };
        let (ocr, recognized_formulas, table_ocr) =
            tokio::join!(ocr, recognized_formulas, table_ocr);
        let ocr = ocr?;
        let recognized_formulas = recognized_formulas?;
        let table_ocr = table_ocr?;

        let tables = if table_regions.is_empty() {
            TableRecognitionResult { tables: vec![] }
        } else if let Some(endpoint_base) = &self.bare_table_endpoint_base {
            self.dispatch_bare_tables(
                page,
                ctx,
                endpoint_base,
                &table_regions,
                &table_ocr.spans,
                &recognized_formulas.spans,
            )
            .await?
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

fn polygon_bounds(points: &[[f32; 2]]) -> Option<[f32; 4]> {
    if points.is_empty() {
        return None;
    }
    Some(points.iter().fold(
        [
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        ],
        |result, point| {
            [
                result[0].min(point[0]),
                result[1].min(point[1]),
                result[2].max(point[0]),
                result[3].max(point[1]),
            ]
        },
    ))
}

fn crop_table_text_span(image: &image::RgbImage, points: &[[f32; 2]]) -> Option<image::RgbImage> {
    let [left, top, right, bottom] = polygon_bounds(points)?;
    let left = left.floor().clamp(0.0, image.width() as f32) as u32;
    let top = top.floor().clamp(0.0, image.height() as f32) as u32;
    let right = right.ceil().clamp(0.0, image.width() as f32) as u32;
    let bottom = bottom.ceil().clamp(0.0, image.height() as f32) as u32;
    (right > left && bottom > top)
        .then(|| image::imageops::crop_imm(image, left, top, right - left, bottom - top).to_image())
}

fn is_table_rotation_candidate(spans: &[&OcrSpan]) -> bool {
    let vertical = spans
        .iter()
        .filter_map(|span| polygon_bounds(&span.polygon.points))
        .filter(|bbox| {
            let width = bbox[2] - bbox[0];
            let height = bbox[3] - bbox[1];
            height > 0.0 && width / height < 0.8
        })
        .count();
    vertical >= 3 && vertical as f32 >= spans.len() as f32 * 0.28
}

fn orientation_sample_indices(length: usize) -> Vec<usize> {
    const LIMIT: usize = 18;
    if length <= LIMIT {
        return (0..length).collect();
    }
    (0..LIMIT)
        .map(|index| ((index as f32 * (length - 1) as f32) / (LIMIT - 1) as f32).round() as usize)
        .collect()
}

fn orientation_score(recognitions: &[pipeline_ocr::CtcRecognition]) -> (f32, usize, usize) {
    let valid = recognitions
        .iter()
        .filter(|recognition| !recognition.text.trim().is_empty())
        .collect::<Vec<_>>();
    let characters = valid
        .iter()
        .map(|recognition| recognition.text.chars().count())
        .sum();
    if valid.len() < 5 {
        return (0.0, valid.len(), characters);
    }
    (
        valid
            .iter()
            .map(|recognition| recognition.confidence)
            .sum::<f32>()
            / valid.len() as f32,
        valid.len(),
        characters,
    )
}

fn select_table_rotation(scores: [(f32, usize, usize); 3]) -> u16 {
    if scores[0].0 >= 0.9 {
        return 0;
    }
    let best = (0..scores.len())
        .max_by(|left, right| {
            scores[*left]
                .0
                .total_cmp(&scores[*right].0)
                .then_with(|| scores[*left].1.cmp(&scores[*right].1))
                .then_with(|| scores[*left].2.cmp(&scores[*right].2))
        })
        .unwrap_or(0);
    if best != 0 && scores[best].0 - scores[0].0 < 0.08 {
        0
    } else {
        [0, 90, 270][best]
    }
}

fn rotate_table_point(point: [f32; 2], width: f32, height: f32, rotation: u16) -> [f32; 2] {
    match rotation {
        90 => [point[1], width - point[0]],
        270 => [height - point[1], point[0]],
        _ => point,
    }
}

fn inverse_rotate_table_point(point: [f32; 2], width: f32, height: f32, rotation: u16) -> [f32; 2] {
    match rotation {
        90 => [width - point[1], point[0]],
        270 => [point[1], height - point[0]],
        _ => point,
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
    indices.sort_by_key(|index| {
        let native = regions[*index]
            .region_id
            .rsplit_once("/layout-")
            .and_then(|(_, suffix)| suffix.parse::<usize>().ok());
        native.unwrap_or(regions.len() + ranks[*index] as usize)
    });
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
    let inline_parents: HashMap<String, String> = result
        .regions
        .iter()
        .filter(|region| region.label == "inline_formula")
        .filter_map(|formula| {
            result
                .regions
                .iter()
                .filter(|candidate| {
                    !matches!(
                        candidate.label.as_str(),
                        "inline_formula" | "display_formula" | "table" | "image" | "chart"
                    ) && bbox_contains_center(candidate.bbox, formula.bbox)
                })
                .min_by(|left, right| {
                    let area = |region: &Region| {
                        (region.bbox[2] - region.bbox[0]) * (region.bbox[3] - region.bbox[1])
                    };
                    area(left).total_cmp(&area(right))
                })
                .map(|parent| (formula.region_id.clone(), parent.region_id.clone()))
        })
        .collect();
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
    let mut inline_by_parent: HashMap<String, Vec<&FormulaSpan>> = HashMap::new();
    for (formula_id, parent_id) in &inline_parents {
        if let Some(formula) = formulas.get(formula_id) {
            inline_by_parent
                .entry(parent_id.clone())
                .or_default()
                .push(formula);
        }
    }
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
        if region.label == "inline_formula" && inline_parents.contains_key(&region.region_id) {
            continue;
        }
        let (category, warning) = category_map::map_pipeline_category(&region.label);
        if let Some(warning) = warning {
            ctx.warn(format!("pipeline-v2 page {}: {warning}", page.page_num));
        }
        let bbox = bbox_px(page, region.bbox)?;
        let mut spans = spans_by_region
            .remove(&region.region_id)
            .unwrap_or_default();
        let vertical_span_count = spans
            .iter()
            .filter(|span| {
                let bbox = span.polygon.points.iter().fold(
                    [
                        f32::INFINITY,
                        f32::INFINITY,
                        f32::NEG_INFINITY,
                        f32::NEG_INFINITY,
                    ],
                    |result, point| {
                        [
                            result[0].min(point[0]),
                            result[1].min(point[1]),
                            result[2].max(point[0]),
                            result[3].max(point[1]),
                        ]
                    },
                );
                let width = bbox[2] - bbox[0];
                let height = bbox[3] - bbox[1];
                width > 0.0 && height / width > 2.0
            })
            .count();
        let is_vertical = region.label == "vertical_text"
            || (!spans.is_empty() && vertical_span_count as f32 / spans.len() as f32 > 0.8);
        if is_vertical {
            spans.sort_by(|left, right| {
                let anchor = |span: &OcrSpan| {
                    span.polygon
                        .points
                        .iter()
                        .fold([f32::NEG_INFINITY, f32::INFINITY], |result, point| {
                            [result[0].max(point[0]), result[1].min(point[1])]
                        })
                };
                let left = anchor(left);
                let right = anchor(right);
                right[0]
                    .total_cmp(&left[0])
                    .then_with(|| left[1].total_cmp(&right[1]))
            });
        } else {
            spans.sort_by_key(span_sort_key);
        }
        let inline_formulas = inline_by_parent
            .remove(&region.region_id)
            .unwrap_or_default();
        let assembled = if is_vertical {
            spans
                .iter()
                .map(|span| span.text.trim())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            let mut text_events: Vec<(f32, f32, f32, String)> = spans
                .iter()
                .filter_map(|span| {
                    let text = span.text.trim();
                    if text.is_empty() {
                        return None;
                    }
                    let min_x = span
                        .polygon
                        .points
                        .iter()
                        .map(|point| point[0])
                        .fold(f32::INFINITY, f32::min);
                    let min_y = span
                        .polygon
                        .points
                        .iter()
                        .map(|point| point[1])
                        .fold(f32::INFINITY, f32::min);
                    let max_y = span
                        .polygon
                        .points
                        .iter()
                        .map(|point| point[1])
                        .fold(f32::NEG_INFINITY, f32::max);
                    Some((min_y, min_x, (max_y - min_y).max(1.0), text.to_owned()))
                })
                .collect();
            text_events.extend(inline_formulas.into_iter().filter_map(|formula| {
                let bbox = formula.bbox?;
                (!formula.latex.trim().is_empty()).then(|| {
                    (
                        bbox[1],
                        bbox[0],
                        (bbox[3] - bbox[1]).max(1.0),
                        format!("${}$", formula.latex.trim()),
                    )
                })
            }));
            text_events.sort_by(|left, right| {
                left.0
                    .total_cmp(&right.0)
                    .then_with(|| left.1.total_cmp(&right.1))
            });
            let mut lines: Vec<Vec<(f32, f32, f32, String)>> = Vec::new();
            for event in text_events {
                if let Some(line) = lines.last_mut() {
                    let mean_top = line.iter().map(|item| item.0).sum::<f32>() / line.len() as f32;
                    let max_height = line.iter().map(|item| item.2).fold(event.2, f32::max);
                    if (event.0 - mean_top).abs() <= max_height * 0.5 {
                        line.push(event);
                        continue;
                    }
                }
                lines.push(vec![event]);
            }
            lines
                .into_iter()
                .map(|mut line| {
                    line.sort_by(|left, right| left.1.total_cmp(&right.1));
                    line.into_iter()
                        .map(|event| event.3)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut text = (!assembled.is_empty()).then_some(assembled);
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
    fn table_orientation_gate_and_score_match_mineru_thresholds() {
        let make_span = |index: usize, width: f32, height: f32| OcrSpan {
            span_id: format!("span-{index}"),
            parent_region_id: Some("table-1".into()),
            polygon: Polygon {
                points: vec![[0.0, 0.0], [width, 0.0], [width, height], [0.0, height]],
            },
            text: "字".into(),
            language: Some("ch".into()),
            confidence: Some(0.9),
            coordinate_space: CoordinateSpace::RenderPixels,
        };
        let spans = (0..10)
            .map(|index| make_span(index, if index < 3 { 7.0 } else { 20.0 }, 10.0))
            .collect::<Vec<_>>();
        assert!(is_table_rotation_candidate(
            &spans.iter().collect::<Vec<_>>()
        ));

        let recognitions = (0..5)
            .map(|_| pipeline_ocr::CtcRecognition {
                text: "中文".into(),
                confidence: 0.8,
            })
            .collect::<Vec<_>>();
        assert_eq!(orientation_score(&recognitions), (0.8, 5, 10));
        assert_eq!(orientation_sample_indices(100).len(), 18);
        assert_eq!(
            select_table_rotation([(0.70, 5, 10), (0.79, 5, 10), (0.60, 6, 12)]),
            90
        );
        assert_eq!(
            select_table_rotation([(0.70, 5, 10), (0.77, 6, 12), (0.60, 5, 10)]),
            0
        );
    }

    #[test]
    fn table_rotation_coordinate_transform_roundtrips() {
        let point = [23.0, 17.0];
        for rotation in [0, 90, 270] {
            let rotated = rotate_table_point(point, 100.0, 60.0, rotation);
            assert_eq!(
                inverse_rotate_table_point(rotated, 100.0, 60.0, rotation),
                point
            );
        }
    }

    #[test]
    fn bare_layout_native_order_precedes_geometric_fallback() {
        let page = rendered_page();
        let mut right = region();
        right.region_id = "page-1/layout-0".into();
        right.bbox = [140.0, 10.0, 180.0, 190.0];
        let mut left = region();
        left.region_id = "page-1/layout-1".into();
        left.bbox = [20.0, 10.0, 60.0, 190.0];

        let order = region_reading_order(&page, &[right, left]).unwrap();

        assert_eq!(order, ["page-1/layout-0", "page-1/layout-1"]);
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

    #[test]
    fn inline_formula_is_embedded_between_neighboring_ocr_spans() {
        let page = rendered_page();
        let ctx = ParseCtx::with_mock(Arc::new(MockDispatch::new()), Arc::new(Semaphore::new(1)));
        let result = PageAnalyzeResult {
            regions: vec![
                Region {
                    region_id: "text-1".into(),
                    label: "text".into(),
                    bbox: [10.0, 10.0, 190.0, 50.0],
                    polygon: None,
                    confidence: Some(0.99),
                    coordinate_space: CoordinateSpace::RenderPixels,
                },
                Region {
                    region_id: "formula-1".into(),
                    label: "inline_formula".into(),
                    bbox: [80.0, 12.0, 105.0, 32.0],
                    polygon: None,
                    confidence: Some(0.98),
                    coordinate_space: CoordinateSpace::RenderPixels,
                },
            ],
            ocr_spans: vec![
                OcrSpan {
                    span_id: "left".into(),
                    parent_region_id: Some("text-1".into()),
                    polygon: Polygon {
                        points: vec![[10.0, 10.0], [70.0, 10.0], [70.0, 30.0], [10.0, 30.0]],
                    },
                    text: "left".into(),
                    language: Some("en".into()),
                    confidence: Some(0.99),
                    coordinate_space: CoordinateSpace::RenderPixels,
                },
                OcrSpan {
                    span_id: "right".into(),
                    parent_region_id: Some("text-1".into()),
                    polygon: Polygon {
                        points: vec![[110.0, 10.0], [190.0, 10.0], [190.0, 30.0], [110.0, 30.0]],
                    },
                    text: "right".into(),
                    language: Some("en".into()),
                    confidence: Some(0.99),
                    coordinate_space: CoordinateSpace::RenderPixels,
                },
            ],
            formula_spans: vec![FormulaSpan {
                region_id: "formula-1".into(),
                latex: "x^2".into(),
                confidence: Some(0.98),
                bbox: Some([80.0, 12.0, 105.0, 32.0]),
            }],
            tables: vec![],
            reading_order: vec!["text-1".into(), "formula-1".into()],
            markdown: None,
            assets: vec![],
        };

        let blocks = build_blocks(&page, &ctx, result).unwrap();

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text.as_deref(), Some("left $x^2$ right"));
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
