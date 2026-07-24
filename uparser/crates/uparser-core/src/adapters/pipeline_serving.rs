//! Pipeline Model Serving REST contract, per ARCHITECTURE.md §11.3 — a
//! new, custom, non-chat-completions contract for `pipeline`'s three
//! `Remote`-by-default stages (`layout`/`ocr`/`formula`). Request bodies
//! carry a base64 PNG image (or region crop) plus a task discriminant;
//! response bodies are stage-shaped structured results, not free text.
//!
//! No reference deployment of this contract exists anywhere to test
//! against (MinerU's real layout/OCR/formula weights aren't vendored in
//! this repo — see P5's plan). These types are the documented contract a
//! real deployment would implement; `adapters/pipeline.rs`'s Remote-stage
//! dispatch is exercised offline via `MockDispatch`, keyed the same way
//! every other adapter's stage dispatch already is.

use serde::{Deserialize, Serialize};

/// Shared by all three request shapes: a base64-encoded PNG of the whole
/// page (`layout`) or a cropped region (`ocr`/`formula`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageImage {
    pub png_base64: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutStageRequest {
    pub image: StageImage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutBox {
    /// Native `pipeline` layout category — see
    /// `category_map::PIPELINE_LAYOUT_CATEGORIES`.
    pub category: String,
    /// Pixel-absolute `[x0, y0, x1, y1]` on the page image submitted.
    pub bbox_px: [i32; 4],
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutStageResponse {
    pub boxes: Vec<LayoutBox>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrStageRequest {
    pub image: StageImage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrStageResponse {
    pub text: String,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaStageRequest {
    pub image: StageImage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaStageResponse {
    pub latex: String,
}

/// Only dispatched when `table` is explicitly configured `Remote`
/// (default is `Local` via `onnx_table.rs` — ARCHITECTURE.md §11.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableStageRequest {
    pub image: StageImage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableStageResponse {
    /// OTSL token sequence or literal `<table>...` HTML — same shape
    /// `otsl::to_html` already accepts from mineru-vlm's stage 2.
    pub otsl: String,
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
    fn layout_response_roundtrip() {
        roundtrip(&LayoutStageResponse {
            boxes: vec![LayoutBox {
                category: "text".into(),
                bbox_px: [0, 0, 10, 10],
                confidence: Some(0.9),
            }],
        });
    }

    #[test]
    fn ocr_response_roundtrip() {
        roundtrip(&OcrStageResponse {
            text: "hello".into(),
            confidence: None,
        });
    }

    #[test]
    fn formula_response_roundtrip() {
        roundtrip(&FormulaStageResponse {
            latex: "\\frac{1}{2}".into(),
        });
    }

    #[test]
    fn table_response_roundtrip() {
        roundtrip(&TableStageResponse {
            otsl: "<fcel>a".into(),
        });
    }
}
