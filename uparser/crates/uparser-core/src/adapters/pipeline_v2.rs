//! Versioned contract for the MinerU-current pipeline compatibility service.
//!
//! V1 remains available in `pipeline_serving`; V2 preserves geometry and model
//! provenance and supports page batches and per-item failure isolation.

use serde::{Deserialize, Serialize};

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
}

pub type PageAnalyzeBatchResponse = BatchResponse<PageAnalyzeResult>;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
