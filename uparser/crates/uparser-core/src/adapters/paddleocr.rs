//! `paddleocr` protocol adapter (T-6.4): a plain detection+recognition
//! OCR service with **no layout/category concept** — per
//! ARCHITECTURE.md §3.4, PaddleOCR/PaddleX serving returns text boxes
//! (arbitrary-point polygons, not always axis-aligned rects) plus
//! recognized text and confidence, nothing else. Every block maps to
//! category `"text"` unconditionally (there is no native vocabulary to
//! map from — T-6.4's "无分类映射为 text").
//!
//! **T-6.1's service-contract confirmation**: PaddleOCR/PaddleX has
//! several possible deployment shapes (PaddleX high-performance serving,
//! PaddleServing, PaddleOCR's own simple HTTP demo) with no single
//! canonical wire format — `ARCHITECTURE.md` §8 explicitly flags this as
//! unresolved and defers the concrete choice to implementation time. No
//! real deployment of any of them is available in this sandbox to
//! reverse-engineer against (same category of gap as P5's Pipeline Model
//! Serving). This adapter therefore documents and implements its own
//! reasonable, minimal REST contract (`PaddleOcrRequest`/`Response`
//! below) as the one this project would ask a real deployment to speak
//! — reusing `pipeline_serving::StageImage`'s base64-PNG shape rather
//! than inventing a third image encoding — and is offline-tested via
//! `MockDispatch`, same as every other adapter's dispatch.
//!
//! `dispatch()`-equivalent: like `pipeline`'s Remote stages, this isn't
//! chat-completions-shaped, so it goes through `ParseCtx::dispatch_rest`
//! rather than `dispatch()` (§3.4's "覆盖走专属 REST").
//!
//! `provides_reading_order = false`, per §3.4 ("PaddleOCR 生态本身也没有
//!自带的阅读顺序重建"). Until `scheduler.rs` generically wires
//! `reading_order.rs` based on this flag (deferred — the same
//! not-yet-scheduler-wired gap `postprocess.rs` has had since P1), this
//! adapter calls `reading_order::assign_reading_order` directly inside
//! `parse_page` so a real caller gets a usable order today; the flag
//! itself stays `false` to honestly describe the model's own output,
//! not this adapter's applied fallback.

use super::pipeline_serving::StageImage;
use super::{ModelStage, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat};
use crate::geometry;
use crate::ingest::RenderedPage;
use crate::reading_order;
use crate::types::{Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, PageError};
use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// This project's own documented request/response contract for a
/// PaddleOCR/PaddleX-shaped detect+recognize service — see this module's
/// doc comment for why no single upstream-canonical contract exists to
/// copy instead.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PaddleOcrRequest {
    pub image: StageImage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaddleOcrBox {
    /// Arbitrary-point detection polygon in page-pixel coordinates
    /// (PaddleOCR's detector emits quads, not always axis-aligned).
    pub polygon_px: Vec<[f32; 2]>,
    pub text: String,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaddleOcrResponse {
    pub boxes: Vec<PaddleOcrBox>,
}

pub struct PaddleOcrAdapter {
    pub endpoint: String,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl Default for PaddleOcrAdapter {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:8868/predict/ocr_system".to_string(),
            timeout: Duration::from_secs(60),
            max_retries: 2,
        }
    }
}

#[async_trait]
impl ProtocolAdapter for PaddleOcrAdapter {
    fn name(&self) -> &'static str {
        "paddleocr"
    }

    fn coordinate_system(&self) -> CoordinateSystem {
        CoordinateSystem::PixelAbs
    }

    fn provides_reading_order(&self) -> bool {
        false
    }

    fn category_vocab(&self) -> &[&'static str] {
        // No native layout classification: every block is "text".
        &["text"]
    }

    fn raw_output_format(&self) -> RawOutputFormat {
        RawOutputFormat::OcrBoxes
    }

    fn emitted_signals(&self) -> PostprocessSignals {
        PostprocessSignals::default()
    }

    fn model_stages(&self) -> Vec<ModelStage> {
        vec![ModelStage {
            stage_name: "ocr",
            default_backend: super::StageBackend::Remote(super::RemoteEndpointSpec {
                endpoint_env_var: "PADDLEOCR_ENDPOINT",
            }),
            allows_local: false,
            resource_hint: super::ResourceHint::Heavy,
        }]
    }

    async fn parse_page(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
    ) -> Result<Vec<Block>, PageError> {
        let req = PaddleOcrRequest {
            image: StageImage {
                png_base64: base64::engine::general_purpose::STANDARD.encode(&page.png_bytes),
            },
        };
        let endpoint = format!("{}#ocr", self.endpoint);
        let resp = ctx
            .dispatch_rest(
                &endpoint,
                serde_json::to_value(req).expect("always serializable"),
                self.timeout,
                self.max_retries,
            )
            .await
            .map_err(|e| PageError {
                page_num: page.page_num,
                message: e.to_string(),
                stage: Some("ocr".into()),
            })?;
        let parsed: PaddleOcrResponse = serde_json::from_value(resp).map_err(|e| PageError {
            page_num: page.page_num,
            message: format!("malformed paddleocr response: {e}"),
            stage: Some("ocr".into()),
        })?;

        let bboxes: Vec<[i32; 4]> = parsed
            .boxes
            .iter()
            .map(|b| {
                let bounds = geometry::geometry_bounds(&Geometry::Polygon(b.polygon_px.clone()));
                [
                    bounds[0].round() as i32,
                    bounds[1].round() as i32,
                    bounds[2].round() as i32,
                    bounds[3].round() as i32,
                ]
            })
            .collect();
        let reading_orders = reading_order::assign_reading_order(&bboxes);

        let blocks = parsed
            .boxes
            .into_iter()
            .zip(bboxes)
            .zip(reading_orders)
            .map(|((b, bbox_px), order)| Block {
                geom: Geometry::Polygon(b.polygon_px),
                geom_frame: CoordFrame::Page,
                bbox_px: Some(bbox_px),
                category_raw: String::new(),
                category: Some("text".to_string()),
                reading_order: Some(order),
                text: Some(b.text),
                html: None,
                latex: None,
                spans: vec![],
                merge_hint: None,
                confidence: b.confidence,
                source: BlockSource::OcrPipeline,
                error: None,
                asset_bytes: None,
                asset_path: None,
            })
            .collect();

        Ok(blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MockDispatch;
    use image::{Rgb, RgbImage};
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    fn fake_page(width: u32, height: u32) -> RenderedPage {
        let img = RgbImage::from_pixel(width, height, Rgb([255, 255, 255]));
        RenderedPage {
            page_num: 1,
            width,
            height,
            png_bytes: crate::imaging::to_png_bytes(&img).unwrap(),
        }
    }

    #[tokio::test]
    async fn single_request_orchestration_offline_via_mock_dispatch() {
        let adapter = PaddleOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());
        let endpoint = format!("{}#ocr", adapter.endpoint);
        mock.seed(
            &endpoint,
            serde_json::to_value(PaddleOcrResponse {
                boxes: vec![
                    PaddleOcrBox {
                        polygon_px: vec![[0.0, 0.0], [90.0, 0.0], [90.0, 40.0], [0.0, 40.0]],
                        text: "top".into(),
                        confidence: Some(0.98),
                    },
                    PaddleOcrBox {
                        polygon_px: vec![[0.0, 100.0], [90.0, 100.0], [90.0, 140.0], [0.0, 140.0]],
                        text: "bottom".into(),
                        confidence: Some(0.95),
                    },
                ],
            })
            .unwrap(),
        );

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(4)));
        let page = fake_page(200, 300);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds");
        assert_eq!(blocks.len(), 2);

        for b in &blocks {
            assert_eq!(b.category.as_deref(), Some("text"));
            assert!(matches!(b.geom, Geometry::Polygon(_)));
            assert!(b.bbox_px.is_some());
        }

        // Reading order must place the top box before the bottom box.
        let top = blocks
            .iter()
            .find(|b| b.text.as_deref() == Some("top"))
            .unwrap();
        let bottom = blocks
            .iter()
            .find(|b| b.text.as_deref() == Some("bottom"))
            .unwrap();
        assert!(top.reading_order.unwrap() < bottom.reading_order.unwrap());
    }

    #[tokio::test]
    async fn missing_seed_yields_page_error() {
        let adapter = PaddleOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());
        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let page = fake_page(100, 100);

        let result = adapter.parse_page(&page, &ctx).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().stage.as_deref(), Some("ocr"));
    }

    #[test]
    fn declares_expected_protocol_metadata() {
        let adapter = PaddleOcrAdapter::default();
        assert_eq!(adapter.name(), "paddleocr");
        assert_eq!(adapter.coordinate_system(), CoordinateSystem::PixelAbs);
        assert!(!adapter.provides_reading_order());
        assert_eq!(adapter.category_vocab(), &["text"]);
        assert_eq!(adapter.raw_output_format(), RawOutputFormat::OcrBoxes);
    }
}
