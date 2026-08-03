//! Unified intermediate representation (IR), per ARCHITECTURE.md §5.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Normalization basis a protocol's raw coordinates are expressed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSystem {
    Norm0To1000,
    Norm0To1,
    PixelAbs,
}

/// Shape of a block's location, orthogonal to `CoordinateSystem`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Geometry {
    Rect([f32; 4]),
    Polygon(Vec<[f32; 2]>),
}

/// Which coordinate frame a block's geometry is relative to. Two-stage
/// protocols (e.g. mineru-vlm) produce content blocks whose coordinates are
/// relative to a cropped sub-image, not the page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoordFrame {
    Page,
    Crop {
        parent_block: usize,
        crop_bbox_px: [i32; 4],
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    pub bbox_px: Option<[i32; 4]>,
    pub font_size: Option<f32>,
    pub is_inline_formula: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeHint {
    SameParagraph,
    NewParagraph,
    TitleLevel(u8),
}

/// Provenance of a block — which protocol path produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockSource {
    NativeTextLayer,
    StructuredNative,
    OneShotVlm,
    LayoutThenRecognize,
    OcrPipeline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub geom: Geometry,
    pub geom_frame: CoordFrame,
    pub bbox_px: Option<[i32; 4]>,
    pub category_raw: String,
    pub category: Option<String>,
    pub reading_order: Option<u32>,
    pub text: Option<String>,
    pub html: Option<String>,
    pub latex: Option<String>,
    #[serde(default)]
    pub spans: Vec<Span>,
    pub merge_hint: Option<MergeHint>,
    pub confidence: Option<f32>,
    pub source: BlockSource,
    pub error: Option<String>,
    /// Raw PNG bytes of a cropped image/chart region, populated by
    /// protocol adapters for image-category blocks (see
    /// `image_link_gap_report.md`) and cleared by `assets::write_page_assets`
    /// once written to disk. `#[serde(skip)]` — this must never leak into
    /// JSON output, defensively, even if a caller forgets to run the
    /// write step; `asset_path` (below) is what callers actually see.
    #[serde(skip)]
    pub asset_bytes: Option<Vec<u8>>,
    /// Path (relative to the source document, e.g. `"doc_images/<hash>.png"`)
    /// the asset was written to, populated by `assets::write_page_assets`.
    /// `#[serde(default)]` so a `ParseResult` cached before this field
    /// existed still deserializes cleanly.
    #[serde(default)]
    pub asset_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub page_num: u32,
    pub width_px: u32,
    pub height_px: u32,
    #[serde(default)]
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageError {
    pub page_num: u32,
    pub message: String,
    pub stage: Option<String>,
}

/// How the protocol used for a parse was decided.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutedBy {
    Explicit,
    Auto,
}

/// Coarse document-type classification, per ARCHITECTURE.md §13.3.
/// Populated by `profiler.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    Book,
    Resume,
    Slide,
    Report,
    Spreadsheet,
    AcademicPaper,
    Unknown,
}

/// Which kind of content dominates the document, driving router.rs's
/// protocol recommendation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentMix {
    TextDominant,
    TableDense,
    ImageDense,
    Mixed,
}

/// How deep the profiler went to produce a given `PageProfile`/
/// `DocumentProfile` — L3 (deep semantic classification via a model
/// call) is opt-in and not implemented by this phase's `profiler.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileLevel {
    L1,
    L2,
    L3,
}

/// L3-only semantic subtype — always `None` at L1/L2. Declared for
/// shape-compatibility with ARCHITECTURE.md §13.3's spec; no L1/L2 code
/// path populates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableSubtype {
    Academic,
    Financial,
    DataReport,
    Unknown,
}

/// L3-only semantic subtype — always `None` at L1/L2, same caveat as
/// `TableSubtype`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChartSubtype {
    TrendLine,
    Bar,
    Pie,
    Scatter,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageProfile {
    pub text_density: f32,
    pub image_density: f32,
    pub has_table_region: bool,
    pub table_subtype: Option<TableSubtype>,
    pub has_chart_region: bool,
    pub chart_subtype: Option<ChartSubtype>,
    pub profile_level: ProfileLevel,
}

/// Document-level pre-analysis result, per ARCHITECTURE.md §13.3.
/// Populated by `profiler.rs`, consumed by `router.rs`, and carried
/// through to `ParseResult` so a caller can see what the system thought
/// this document was, not just what protocol it ended up using.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentProfile {
    pub source_format: crate::ingest::DocumentFormat,
    pub kind: DocumentKind,
    pub kind_confidence: f32,
    #[serde(default)]
    pub page_profiles: Vec<PageProfile>,
    pub dominant_content: ContentMix,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParseResult {
    pub source_path: String,
    pub source_sha256: String,
    pub protocol: String,
    pub routed_by: RoutedBy,
    pub document_profile: Option<DocumentProfile>,
    pub model_endpoint: Option<String>,
    pub model_name: Option<String>,
    #[serde(default)]
    pub pages: Vec<Page>,
    #[serde(default)]
    pub page_errors: Vec<PageError>,
    #[serde(default)]
    pub capability_notes: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub timing: HashMap<String, f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).unwrap();
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(value, &back);
    }

    #[test]
    fn geometry_roundtrip() {
        roundtrip(&Geometry::Rect([0.0, 0.0, 1.0, 1.0]));
        roundtrip(&Geometry::Polygon(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]));
    }

    #[test]
    fn coord_frame_roundtrip() {
        roundtrip(&CoordFrame::Page);
        roundtrip(&CoordFrame::Crop {
            parent_block: 2,
            crop_bbox_px: [0, 0, 100, 100],
        });
    }

    #[test]
    fn block_roundtrip() {
        let block = Block {
            geom: Geometry::Rect([0.0, 0.0, 10.0, 10.0]),
            geom_frame: CoordFrame::Page,
            bbox_px: Some([0, 0, 10, 10]),
            category_raw: "text".into(),
            category: Some("text".into()),
            reading_order: Some(0),
            text: Some("hello".into()),
            html: None,
            latex: None,
            spans: vec![Span {
                text: "hello".into(),
                bbox_px: Some([0, 0, 10, 10]),
                font_size: Some(12.0),
                is_inline_formula: false,
            }],
            merge_hint: Some(MergeHint::NewParagraph),
            confidence: Some(0.99),
            source: BlockSource::OneShotVlm,
            error: None,
            asset_bytes: None,
            asset_path: None,
        };
        let json = serde_json::to_string(&block).unwrap();
        let back: Block = serde_json::from_str(&json).unwrap();
        assert_eq!(block.category_raw, back.category_raw);
        assert_eq!(block.text, back.text);
    }

    #[test]
    fn asset_bytes_never_serialized_but_asset_path_round_trips() {
        let mut block = Block {
            geom: Geometry::Rect([0.0, 0.0, 10.0, 10.0]),
            geom_frame: CoordFrame::Page,
            bbox_px: Some([0, 0, 10, 10]),
            category_raw: "image".into(),
            category: Some("image".into()),
            reading_order: None,
            text: None,
            html: None,
            latex: None,
            spans: vec![],
            merge_hint: None,
            confidence: None,
            source: BlockSource::LayoutThenRecognize,
            error: None,
            asset_bytes: Some(vec![1, 2, 3, 4]),
            asset_path: Some("doc_images/abc123.png".into()),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(
            !json.contains("asset_bytes"),
            "asset_bytes must never appear in JSON output: {json}"
        );
        assert!(json.contains("doc_images/abc123.png"));

        let back: Block = serde_json::from_str(&json).unwrap();
        assert_eq!(back.asset_bytes, None);
        assert_eq!(back.asset_path, block.asset_path);

        // Old cached JSON (pre-dating this field) must still deserialize.
        block.asset_path = None;
        let old_shape_json = json.replace(r#","asset_path":"doc_images/abc123.png""#, "");
        let back: Block = serde_json::from_str(&old_shape_json).unwrap();
        assert_eq!(back.asset_path, None);
    }

    #[test]
    fn parse_result_roundtrip() {
        let result = ParseResult {
            source_path: "doc.pdf".into(),
            source_sha256: "abc123".into(),
            protocol: "mock".into(),
            routed_by: RoutedBy::Explicit,
            document_profile: None,
            model_endpoint: None,
            model_name: None,
            pages: vec![Page {
                page_num: 1,
                width_px: 100,
                height_px: 100,
                blocks: vec![],
            }],
            page_errors: vec![],
            capability_notes: vec![],
            warnings: vec![],
            timing: HashMap::new(),
        };
        roundtrip(&result);
    }

    #[test]
    fn document_profile_roundtrip() {
        let profile = DocumentProfile {
            source_format: crate::ingest::DocumentFormat::Pdf,
            kind: DocumentKind::Report,
            kind_confidence: 0.8,
            page_profiles: vec![PageProfile {
                text_density: 0.6,
                image_density: 0.1,
                has_table_region: false,
                table_subtype: None,
                has_chart_region: false,
                chart_subtype: None,
                profile_level: ProfileLevel::L2,
            }],
            dominant_content: ContentMix::TextDominant,
        };
        roundtrip(&profile);
    }
}
