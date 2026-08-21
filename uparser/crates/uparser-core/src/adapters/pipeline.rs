//! `pipeline` protocol adapter (T-5.6): the traditional multi-model
//! layout->OCR->formula->table flow, aligned with MinerU's `pipeline`
//! backend (`opensource/MinerU/mineru/backend/pipeline/`). The
//! engineering point of this adapter isn't the flow itself (VLM
//! protocols already do detect-then-recognize) — it's that each of the
//! four stages independently declares a `StageBackend`
//! (ARCHITECTURE.md §11.2):
//!
//! - `layout`/`ocr`/`formula`: PP-DocLayoutV2/PaddleOCR-torch/Unimernet —
//!   all torch models with no confirmed ONNX export (T-5.2's decision,
//!   confirmed via `mineru_report.md`'s source-level research into
//!   `mineru/utils/enum_class.py` and `model_init.py`). Default `Remote`,
//!   dispatched against Pipeline Model Serving (`pipeline_serving.rs`) —
//!   a new lightweight REST contract, not chat-completions.
//! - `table`: SLANet-Plus/UnetTableModel — MinerU already loads these as
//!   ONNX (`mineru/model/table/...`), so this is the only stage allowed
//!   to run `Local` by default, via `onnx_table.rs`'s `ort`-backed
//!   inference (gated behind the `pipeline-local-table` feature since
//!   `ort` isn't default-on, same posture as `pdfium`/`native`).
//!
//! Per §11.4: if a `Remote` stage is configured but unreachable, this
//! adapter fails loudly (a `PageError`) rather than silently falling
//! back to running a torch model client-side (which isn't even possible
//! here — there's no local torch inference path in this Rust core).
//!
//! `provides_reading_order = false`: MinerU's own `para_split`
//! heuristic isn't ported here; `parse_page` instead applies
//! `reading_order.rs`'s geometric fallback (P6) directly, same as
//! `paddleocr`.

use super::pipeline_serving::{
    FormulaStageRequest, FormulaStageResponse, LayoutStageRequest, LayoutStageResponse,
    OcrStageRequest, OcrStageResponse, StageImage, TableStageRequest, TableStageResponse,
};
use super::{
    ModelStage, ParseCtx, PipelineConfig, PostprocessSignals, ProtocolAdapter, RawOutputFormat,
    RemoteEndpointSpec, ResourceHint, StageBackend, StageBackendChoice,
};
use crate::category_map::{self, PIPELINE_LAYOUT_CATEGORIES};
use crate::imaging;
use crate::ingest::RenderedPage;
use crate::otsl;
use crate::types::{Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, PageError};
use async_trait::async_trait;
use base64::Engine as _;
use std::time::Duration;

/// Categories skipped entirely at the ocr/formula/table recognition
/// step — analogous to mineru-vlm's `SKIP_CONTENT`. `image`/`chart`
/// regions have no text/table/formula content to recognize; `discarded`
/// is MinerU's own drop-this-region category.
const SKIP_RECOGNITION: &[&str] = &["image", "chart", "discarded"];

pub struct PipelineAdapter {
    pub layout_endpoint: String,
    pub ocr_endpoint: String,
    pub formula_endpoint: String,
    pub table_endpoint: String,
    pub table_backend: StageBackendChoice,
    pub table_model_path: String,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl Default for PipelineAdapter {
    fn default() -> Self {
        Self {
            layout_endpoint: "http://localhost:9001/v1/pipeline/layout".to_string(),
            ocr_endpoint: "http://localhost:9001/v1/pipeline/ocr".to_string(),
            formula_endpoint: "http://localhost:9001/v1/pipeline/formula".to_string(),
            table_endpoint: "http://localhost:9001/v1/pipeline/table".to_string(),
            table_backend: StageBackendChoice::Local,
            table_model_path: "models/table/slanet_plus.onnx".to_string(),
            timeout: Duration::from_secs(60),
            max_retries: 2,
        }
    }
}

impl PipelineAdapter {
    /// Apply CLI-sourced overrides (T-5.1) on top of the defaults above.
    /// `layout`/`ocr`/`formula` have no `Local` implementation
    /// (`allows_local = false`, §11.2) — a `Local` override for those is
    /// intentionally not represented here (nothing to switch to); only
    /// their endpoints and `table`'s backend/path/endpoint are
    /// overridable.
    pub fn apply_config(&mut self, cfg: &PipelineConfig) {
        if let Some(endpoint) = &cfg.layout_endpoint {
            self.layout_endpoint = endpoint.clone();
        }
        if let Some(endpoint) = &cfg.ocr_endpoint {
            self.ocr_endpoint = endpoint.clone();
        }
        if let Some(endpoint) = &cfg.formula_endpoint {
            self.formula_endpoint = endpoint.clone();
        }
        if let Some(backend) = cfg.table_backend {
            self.table_backend = backend;
        }
        if let Some(path) = &cfg.table_model_path {
            self.table_model_path = path.clone();
        }
    }

    fn stage_endpoint(&self, base: &str, tag: &str, block_index: Option<usize>) -> String {
        match block_index {
            Some(i) => format!("{base}#{tag}:{i}"),
            None => format!("{base}#{tag}"),
        }
    }
}

fn image_to_stage_image(png_bytes: &[u8]) -> StageImage {
    StageImage {
        png_base64: base64::engine::general_purpose::STANDARD.encode(png_bytes),
    }
}

struct PendingBlock {
    bbox_px: [i32; 4],
    category_raw: String,
    category: String,
}

#[async_trait]
impl ProtocolAdapter for PipelineAdapter {
    fn name(&self) -> &'static str {
        "pipeline"
    }

    fn coordinate_system(&self) -> CoordinateSystem {
        CoordinateSystem::PixelAbs
    }

    fn provides_reading_order(&self) -> bool {
        false
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
        vec![
            ModelStage {
                stage_name: "layout",
                default_backend: StageBackend::Remote(RemoteEndpointSpec {
                    endpoint_env_var: "PIPELINE_LAYOUT_ENDPOINT",
                }),
                allows_local: false,
                resource_hint: ResourceHint::Heavy,
            },
            ModelStage {
                stage_name: "ocr",
                default_backend: StageBackend::Remote(RemoteEndpointSpec {
                    endpoint_env_var: "PIPELINE_OCR_ENDPOINT",
                }),
                allows_local: false,
                resource_hint: ResourceHint::Heavy,
            },
            ModelStage {
                stage_name: "formula",
                default_backend: StageBackend::Remote(RemoteEndpointSpec {
                    endpoint_env_var: "PIPELINE_FORMULA_ENDPOINT",
                }),
                allows_local: false,
                resource_hint: ResourceHint::Heavy,
            },
            ModelStage {
                stage_name: "table",
                default_backend: StageBackend::Local(super::LocalModelSpec {
                    model_path: Some(self.table_model_path.clone()),
                }),
                allows_local: true,
                resource_hint: ResourceHint::Lightweight,
            },
        ]
    }

    async fn parse_page(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
    ) -> Result<Vec<Block>, PageError> {
        crate::stage_graph::PIPELINE_STAGE_GRAPH
            .validate()
            .map_err(|error| PageError {
                page_num: page.page_num,
                message: error.to_string(),
                stage: Some("stage_graph".to_owned()),
            })?;
        // Stage 1: layout, whole page, always Remote (no Local
        // implementation exists for this stage).
        let layout_req = serde_json::to_value(LayoutStageRequest {
            image: image_to_stage_image(&page.png_bytes),
        })
        .expect("LayoutStageRequest always serializable");
        let layout_endpoint = self.stage_endpoint(&self.layout_endpoint, "layout", None);
        let layout_resp = crate::shape_executor::rest_stage(
            page,
            ctx,
            &layout_endpoint,
            layout_req,
            self.timeout,
            self.max_retries,
            "layout",
        )
        .await?;
        let layout: LayoutStageResponse =
            serde_json::from_value(layout_resp).map_err(|e| PageError {
                page_num: page.page_num,
                message: format!("malformed layout stage response: {e}"),
                stage: Some("layout".into()),
            })?;

        let pending: Vec<PendingBlock> = layout
            .boxes
            .into_iter()
            .map(|b| {
                let (category, warning) = category_map::map_pipeline_category(&b.category);
                if let Some(w) = warning {
                    ctx.warn(format!("pipeline page {}: {w}", page.page_num));
                }
                PendingBlock {
                    bbox_px: b.bbox_px,
                    category_raw: b.category,
                    category,
                }
            })
            .collect();

        // Stages 2-4: per-region recognition, concurrent within the page
        // (bounded by the shared document-level permit budget) — same
        // shape as mineru-vlm's stage 2.
        let futures_iter = pending.iter().enumerate().map(|(index, p)| {
            let skip = SKIP_RECOGNITION.contains(&p.category.as_str());
            async move {
                if skip {
                    return (index, Ok(None));
                }

                // No permit acquired here: cropping is cheap CPU work,
                // and the actual permit-guarded network dispatch happens
                // inside each `recognize_*` call below, right before its
                // own `ctx.dispatch_rest()` (or not at all, for the
                // Local table backend, which has no network request to
                // bound).
                let crop_img = match ctx.crop(page, p.bbox_px) {
                    Ok(img) => img,
                    Err(e) => return (index, Err(e)),
                };

                if p.category == "table" {
                    return (
                        index,
                        self.recognize_table(&crop_img, page.page_num, ctx).await,
                    );
                }
                if p.category == "equation" {
                    return (index, self.recognize_formula(&crop_img, index, ctx).await);
                }
                (index, self.recognize_text(&crop_img, index, ctx).await)
            }
        });
        let mut outcome_by_index = crate::shape_executor::collect_indexed(futures_iter).await;

        // §11.5/M8: MinerU's own pipeline reading order depends on
        // `para_split` heuristics this project doesn't port; per the
        // architecture doc's explicit choice, `pipeline` shares
        // `reading_order.rs`'s geometric fallback with `paddleocr`
        // instead (see `reading_order.rs`'s module doc for why this is
        // applied directly here rather than scheduler-wired).
        let bboxes: Vec<[i32; 4]> = pending.iter().map(|p| p.bbox_px).collect();
        let reading_orders = crate::reading_order::assign_reading_order(&bboxes);

        let mut blocks = Vec::with_capacity(pending.len());
        for (index, p) in pending.iter().enumerate() {
            let outcome = outcome_by_index.remove(&index).unwrap_or(Ok(None));
            let (text, html, latex, error) = match outcome {
                Ok(None) => (None, None, None, None),
                Ok(Some(RecognizedContent::Text(t))) => (Some(t), None, None, None),
                Ok(Some(RecognizedContent::Html(h))) => (None, Some(h), None, None),
                Ok(Some(RecognizedContent::Latex(l))) => (None, None, Some(l), None),
                Err(e) => (None, None, None, Some(e)),
            };

            // SKIP_RECOGNITION correctly skips the OCR/formula/table
            // stage dispatch for "image"/"chart" regions, but crop-and-
            // preserve is a separate step MinerU does unconditionally
            // for these categories — do it here so `render.rs` has
            // pixels to link to (see `image_link_gap_report.md`).
            let asset_bytes = if matches!(p.category.as_str(), "image" | "chart") {
                ctx.crop(page, p.bbox_px)
                    .ok()
                    .and_then(|img| imaging::to_png_bytes(&img).ok())
            } else {
                None
            };

            let [x0, y0, x1, y1] = p.bbox_px;
            blocks.push(Block {
                geom: Geometry::Rect([x0 as f32, y0 as f32, x1 as f32, y1 as f32]),
                geom_frame: CoordFrame::Page,
                bbox_px: Some(p.bbox_px),
                category_raw: p.category_raw.clone(),
                category: Some(p.category.clone()),
                reading_order: Some(reading_orders[index]),
                text,
                html,
                latex,
                spans: vec![],
                merge_hint: None,
                confidence: None,
                source: BlockSource::LayoutThenRecognize,
                error,
                asset_bytes,
                asset_path: None,
            });
        }

        Ok(blocks)
    }
}

enum RecognizedContent {
    Text(String),
    Html(String),
    Latex(String),
}

impl PipelineAdapter {
    async fn recognize_text(
        &self,
        crop: &image::RgbImage,
        index: usize,
        ctx: &ParseCtx,
    ) -> Result<Option<RecognizedContent>, String> {
        let png = imaging::to_png_bytes(crop)?;
        let req = serde_json::to_value(OcrStageRequest {
            image: image_to_stage_image(&png),
        })
        .expect("OcrStageRequest always serializable");
        let endpoint = self.stage_endpoint(&self.ocr_endpoint, "ocr", Some(index));
        let _permit = ctx.acquire_permit().await;
        let resp = ctx
            .dispatch_rest(&endpoint, req, self.timeout, self.max_retries)
            .await
            .map_err(|e| e.to_string())?;
        let parsed: OcrStageResponse = serde_json::from_value(resp)
            .map_err(|e| format!("malformed ocr stage response: {e}"))?;
        Ok(Some(RecognizedContent::Text(parsed.text)))
    }

    async fn recognize_formula(
        &self,
        crop: &image::RgbImage,
        index: usize,
        ctx: &ParseCtx,
    ) -> Result<Option<RecognizedContent>, String> {
        let png = imaging::to_png_bytes(crop)?;
        let req = serde_json::to_value(FormulaStageRequest {
            image: image_to_stage_image(&png),
        })
        .expect("FormulaStageRequest always serializable");
        let endpoint = self.stage_endpoint(&self.formula_endpoint, "formula", Some(index));
        let _permit = ctx.acquire_permit().await;
        let resp = ctx
            .dispatch_rest(&endpoint, req, self.timeout, self.max_retries)
            .await
            .map_err(|e| e.to_string())?;
        let parsed: FormulaStageResponse = serde_json::from_value(resp)
            .map_err(|e| format!("malformed formula stage response: {e}"))?;
        Ok(Some(RecognizedContent::Latex(parsed.latex)))
    }

    /// `table`'s dispatch, per its per-instance `table_backend`
    /// (default `Local`). Per §11.4: `Remote` unreachability is a clean
    /// error, never a silent `Local` fallback — and the reverse holds
    /// too, since there's no torch-model client-side path to fall back
    /// to anyway.
    async fn recognize_table(
        &self,
        crop: &image::RgbImage,
        page_num: u32,
        ctx: &ParseCtx,
    ) -> Result<Option<RecognizedContent>, String> {
        match self.table_backend {
            StageBackendChoice::Remote => {
                let png = imaging::to_png_bytes(crop)?;
                let req = serde_json::to_value(TableStageRequest {
                    image: image_to_stage_image(&png),
                })
                .expect("TableStageRequest always serializable");
                let endpoint = self.stage_endpoint(&self.table_endpoint, "table", None);
                let _permit = ctx.acquire_permit().await;
                let resp = ctx
                    .dispatch_rest(&endpoint, req, self.timeout, self.max_retries)
                    .await
                    .map_err(|e| e.to_string())?;
                let parsed: TableStageResponse = serde_json::from_value(resp)
                    .map_err(|e| format!("malformed table stage response: {e}"))?;
                let (html, warnings) = otsl::to_html(&parsed.otsl);
                for w in &warnings {
                    ctx.warn(format!("pipeline page {page_num}: {w}"));
                }
                Ok(Some(RecognizedContent::Html(html)))
            }
            StageBackendChoice::Local => self.recognize_table_local(crop),
        }
    }

    #[cfg(feature = "pipeline-local-table")]
    fn recognize_table_local(
        &self,
        crop: &image::RgbImage,
    ) -> Result<Option<RecognizedContent>, String> {
        // Real SLANet/table-structure ONNX weights aren't vendored
        // anywhere in this repo (see onnx_table.rs's module doc) — this
        // path is validated against a synthetic fixture, not real
        // accuracy. A real deployment would map the model's raw output
        // into OTSL tokens before handing off to `otsl::to_html`; there
        // is no such mapping to write without real weights to observe,
        // so this surfaces the raw tensor length as a placeholder rather
        // than fabricating table structure.
        let path = std::path::Path::new(&self.table_model_path);
        let output =
            super::onnx_table::run_local_table_model(path, crop).map_err(|e| e.to_string())?;
        Ok(Some(RecognizedContent::Html(format!(
            "<!-- local ONNX table stage: {} output values, no OTSL decoding without real weights -->",
            output.len()
        ))))
    }

    #[cfg(not(feature = "pipeline-local-table"))]
    fn recognize_table_local(
        &self,
        _crop: &image::RgbImage,
    ) -> Result<Option<RecognizedContent>, String> {
        Err(
            "table backend is `Local` but this build lacks the `pipeline-local-table` feature; \
             rebuild with `--features pipeline-local-table` or pass `--table-backend remote`"
                .to_string(),
        )
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
            png_bytes: imaging::to_png_bytes(&img).unwrap(),
        }
    }

    #[tokio::test]
    async fn four_stage_orchestration_offline_via_mock_dispatch() {
        let adapter = PipelineAdapter {
            table_backend: StageBackendChoice::Remote,
            ..PipelineAdapter::default()
        };
        let mock = Arc::new(MockDispatch::new());

        let layout_endpoint = adapter.stage_endpoint(&adapter.layout_endpoint, "layout", None);
        mock.seed(
            &layout_endpoint,
            serde_json::to_value(LayoutStageResponse {
                boxes: vec![
                    super::super::pipeline_serving::LayoutBox {
                        category: "text".into(),
                        bbox_px: [0, 0, 100, 50],
                        confidence: Some(0.9),
                    },
                    super::super::pipeline_serving::LayoutBox {
                        category: "table".into(),
                        bbox_px: [0, 100, 100, 200],
                        confidence: Some(0.9),
                    },
                    super::super::pipeline_serving::LayoutBox {
                        category: "interline_equation".into(),
                        bbox_px: [0, 250, 100, 300],
                        confidence: Some(0.9),
                    },
                    super::super::pipeline_serving::LayoutBox {
                        category: "image".into(),
                        bbox_px: [0, 350, 100, 400],
                        confidence: Some(0.9),
                    },
                ],
            })
            .unwrap(),
        );

        let ocr_endpoint = adapter.stage_endpoint(&adapter.ocr_endpoint, "ocr", Some(0));
        mock.seed(
            &ocr_endpoint,
            serde_json::to_value(OcrStageResponse {
                text: "hello world".into(),
                confidence: Some(0.95),
            })
            .unwrap(),
        );

        let table_endpoint = adapter.stage_endpoint(&adapter.table_endpoint, "table", None);
        mock.seed(
            &table_endpoint,
            serde_json::to_value(TableStageResponse {
                otsl: "<fcel>a<fcel>b<nl><fcel>c<fcel>d".into(),
            })
            .unwrap(),
        );

        let formula_endpoint =
            adapter.stage_endpoint(&adapter.formula_endpoint, "formula", Some(2));
        mock.seed(
            &formula_endpoint,
            serde_json::to_value(FormulaStageResponse {
                latex: "\\frac{1}{2}".into(),
            })
            .unwrap(),
        );
        // Deliberately no seed for the "image" category's ocr endpoint —
        // it must never dispatch a recognition request.

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(4)));
        let page = fake_page(400, 1000);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds");
        assert_eq!(blocks.len(), 4);

        assert_eq!(blocks[0].category.as_deref(), Some("text"));
        assert_eq!(blocks[0].text.as_deref(), Some("hello world"));
        assert!(blocks[0].error.is_none());

        assert_eq!(blocks[1].category.as_deref(), Some("table"));
        assert_eq!(
            blocks[1].html.as_deref(),
            Some("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>")
        );

        assert_eq!(blocks[2].category.as_deref(), Some("equation"));
        assert_eq!(blocks[2].latex.as_deref(), Some("\\frac{1}{2}"));

        assert_eq!(blocks[3].category.as_deref(), Some("image"));
        assert!(blocks[3].text.is_none());
        assert!(
            blocks[3].error.is_none(),
            "image category must skip recognition entirely, not dispatch-and-fail"
        );
        // image_link_gap_report.md: SKIP_RECOGNITION correctly skips
        // model dispatch for "image" content, but the pixels must still
        // be cropped and preserved for `render.rs` to link to.
        let asset_bytes = blocks[3]
            .asset_bytes
            .as_ref()
            .expect("image block must have cropped pixels attached");
        let decoded = image::load_from_memory(asset_bytes).expect("must be valid PNG bytes");
        assert_eq!((decoded.width(), decoded.height()), (100, 50));
    }

    #[tokio::test]
    async fn missing_layout_seed_yields_page_error() {
        let adapter = PipelineAdapter::default();
        let mock = Arc::new(MockDispatch::new());
        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let page = fake_page(100, 100);

        let result = adapter.parse_page(&page, &ctx).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().stage.as_deref(), Some("layout"));
    }

    /// §11.4: an unreachable Remote stage must surface as a clear
    /// per-block error, never a silent Local fallback.
    #[tokio::test]
    async fn unreachable_remote_ocr_stage_surfaces_as_block_error_not_silent_fallback() {
        let adapter = PipelineAdapter::default();
        let mock = Arc::new(MockDispatch::new());
        let layout_endpoint = adapter.stage_endpoint(&adapter.layout_endpoint, "layout", None);
        mock.seed(
            &layout_endpoint,
            serde_json::to_value(LayoutStageResponse {
                boxes: vec![super::super::pipeline_serving::LayoutBox {
                    category: "text".into(),
                    bbox_px: [0, 0, 50, 50],
                    confidence: None,
                }],
            })
            .unwrap(),
        );
        // Deliberately no seed for the ocr endpoint.

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let page = fake_page(100, 100);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds at the page level (error is per-block)");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].error.is_some());
        assert!(blocks[0].text.is_none());
    }

    #[test]
    fn declares_expected_protocol_metadata() {
        let adapter = PipelineAdapter::default();
        assert_eq!(adapter.name(), "pipeline");
        assert_eq!(adapter.coordinate_system(), CoordinateSystem::PixelAbs);
        assert!(!adapter.provides_reading_order());
        assert_eq!(adapter.raw_output_format(), RawOutputFormat::OcrBoxes);
        let stages = adapter.model_stages();
        assert_eq!(stages.len(), 4);
        assert!(stages[3].allows_local);
        assert!(!stages[0].allows_local && !stages[1].allows_local && !stages[2].allows_local);
    }
}
