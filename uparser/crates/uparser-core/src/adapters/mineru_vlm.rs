//! mineru-vlm two-stage protocol adapter, per T-1.9. Protocol details
//! (prompts, image preprocessing, sampling params, category vocab) are
//! confirmed against `mineru_vl_utils` v0.1.14 — the newer >=1.0.5 series
//! pinned by `opensource/MinerU` wasn't available locally to verify
//! against; see the P1 plan's caveat.
//!
//! `provides_reading_order = false` and `emitted_signals = all false`
//! are deliberate deviations from `DEVELOPMENT_PLAN.md`'s assumptions
//! for this adapter: v0.1.14's pure `vlm` backend returns plain-string
//! content with no explicit reading-order or span/merge_hint signal —
//! those likely belong to the `hybrid`/`pipeline` backends' `para_split`
//! step, which this adapter doesn't port.

use super::{
    ModelStage, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat, RemoteEndpointSpec,
    ResourceHint, StageBackend,
};
use crate::category_map::{self, MINERU_VLM_CATEGORIES};
use crate::formula_repair;
use crate::geometry;
use crate::imaging;
use crate::ingest::RenderedPage;
use crate::otsl;
use crate::output_parse;
use crate::transport::ChatCompletionRequest;
use crate::types::{Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, PageError};
use async_trait::async_trait;
use futures::future::join_all;
use serde_json::Value;
use std::time::Duration;

const LAYOUT_IMAGE_SIZE: u32 = 1036;
const MAX_EDGE_RATIO: f32 = 50.0;
const MIN_EDGE: u32 = 28;
const IOU_DEDUPE_THRESHOLD: f32 = 0.8;

/// Native categories for which stage 2 (content extraction) is skipped
/// entirely — confirmed from `mineru_vl_utils` v0.1.14.
const SKIP_CONTENT: &[&str] = &["image", "list", "equation_block"];

pub struct MineruVlmAdapter {
    /// Base chat-completions endpoint. Not reachable yet from the CLI
    /// (no `--endpoint` flag wired in this pass) — present so the
    /// request-building logic is complete and ready for that wiring.
    pub endpoint_base: String,
    pub model: String,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl Default for MineruVlmAdapter {
    fn default() -> Self {
        Self {
            endpoint_base: "http://localhost:8000/v1/chat/completions".to_string(),
            model: "mineru-vlm".to_string(),
            timeout: Duration::from_secs(60),
            max_retries: 2,
        }
    }
}

impl MineruVlmAdapter {
    fn stage1_endpoint(&self) -> String {
        format!("{}#stage1", self.endpoint_base)
    }

    fn stage2_endpoint(&self, block_index: usize) -> String {
        format!("{}#stage2:{block_index}", self.endpoint_base)
    }

    fn request(
        &self,
        endpoint: String,
        prompt: &str,
        image_data_url: &str,
        sampling: Value,
    ) -> ChatCompletionRequest {
        let messages = vec![
            serde_json::json!({"role": "system", "content": "You are a helpful assistant."}),
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": image_data_url}},
                    {"type": "text", "text": prompt},
                ],
            }),
        ];
        ChatCompletionRequest {
            endpoint,
            model: self.model.clone(),
            messages,
            sampling,
            timeout: self.timeout,
            max_retries: self.max_retries,
        }
    }

    fn layout_sampling() -> Value {
        serde_json::json!({
            "temperature": 0.0,
            "top_p": 0.01,
            "top_k": 1,
            "no_repeat_ngram_size": 100,
            // Required: vLLM's OpenAI-compatible endpoint defaults to
            // stripping special tokens from decoded text, which would eat
            // the `<|box_start|>`/`<|ref_start|>` wrapper tokens the
            // custom_token grammar depends on.
            "skip_special_tokens": false,
        })
    }

    /// Per-category stage-2 prompt/sampling, confirmed from
    /// `mineru_vl_utils` v0.1.14's `DEFAULT_PROMPTS`/`DEFAULT_SAMPLING_PARAMS`.
    fn stage2_prompt_and_sampling(category_raw: &str) -> (&'static str, Value) {
        match category_raw {
            "table" => (
                "\nTable Recognition:",
                serde_json::json!({
                    "presence_penalty": 1.0,
                    "frequency_penalty": 0.005,
                    "skip_special_tokens": false,
                }),
            ),
            "equation" => (
                "\nFormula Recognition:",
                serde_json::json!({
                    "presence_penalty": 1.0,
                    "frequency_penalty": 0.05,
                    "skip_special_tokens": false,
                }),
            ),
            _ => (
                "\nText Recognition:",
                serde_json::json!({
                    "presence_penalty": 1.0,
                    "frequency_penalty": 0.05,
                    "skip_special_tokens": false,
                }),
            ),
        }
    }
}

/// Pull the assistant message's text content out of an OpenAI-style
/// chat-completion response.
fn extract_content(resp: &Value) -> Option<&str> {
    resp["choices"][0]["message"]["content"].as_str()
}

fn wrap_display_math(latex: &str) -> String {
    let trimmed = latex.trim();
    if trimmed.starts_with("\\[") || trimmed.starts_with("$$") {
        trimmed.to_string()
    } else {
        format!("\\[\n{trimmed}\n\\]")
    }
}

struct PendingBlock {
    bbox_px: [i32; 4],
    category_raw: String,
    category: String,
    angle: Option<u32>,
}

#[async_trait]
impl ProtocolAdapter for MineruVlmAdapter {
    fn name(&self) -> &'static str {
        "mineru-vlm"
    }

    fn coordinate_system(&self) -> CoordinateSystem {
        CoordinateSystem::Norm0To1000
    }

    fn provides_reading_order(&self) -> bool {
        false
    }

    fn category_vocab(&self) -> &[&'static str] {
        MINERU_VLM_CATEGORIES
    }

    fn raw_output_format(&self) -> RawOutputFormat {
        RawOutputFormat::CustomToken
    }

    fn emitted_signals(&self) -> PostprocessSignals {
        PostprocessSignals::default()
    }

    fn model_stages(&self) -> Vec<ModelStage> {
        vec![ModelStage {
            stage_name: "vlm",
            default_backend: StageBackend::Remote(RemoteEndpointSpec {
                endpoint_env_var: "MINERU_VLM_ENDPOINT",
            }),
            allows_local: false,
            resource_hint: ResourceHint::Heavy,
        }]
    }

    async fn parse_page(
        &self,
        page: &RenderedPage,
        ctx: &ParseCtx,
    ) -> Result<Vec<Block>, PageError> {
        let page_img = image::load_from_memory(&page.png_bytes).map_err(|e| PageError {
            page_num: page.page_num,
            message: format!("failed to decode rasterized page: {e}"),
            stage: Some("decode".into()),
        })?;
        let page_rgb = imaging::to_rgb(&page_img);

        // Stage 1: layout detection on the whole page, hard-resized
        // (non-aspect-preserving) to LAYOUT_IMAGE_SIZE.
        let layout_img = imaging::hard_resize(&page_rgb, LAYOUT_IMAGE_SIZE, LAYOUT_IMAGE_SIZE);
        let layout_data_url = imaging::to_base64_data_url(&layout_img).map_err(|e| PageError {
            page_num: page.page_num,
            message: format!("failed to encode layout image: {e}"),
            stage: Some("layout".into()),
        })?;
        let layout_req = self.request(
            self.stage1_endpoint(),
            "\nLayout Detection:",
            &layout_data_url,
            Self::layout_sampling(),
        );
        let layout_resp = ctx.dispatch(layout_req).await.map_err(|e| PageError {
            page_num: page.page_num,
            message: e.to_string(),
            stage: Some("layout".into()),
        })?;
        let layout_content = extract_content(&layout_resp).unwrap_or("");
        let (layout_boxes, warnings) = output_parse::parse_custom_tokens(layout_content);
        for w in &warnings {
            eprintln!("mineru-vlm page {}: {w}", page.page_num);
        }

        // Denormalize + category-map, then dedupe near-identical boxes.
        let mut pending: Vec<PendingBlock> = Vec::with_capacity(layout_boxes.len());
        for lb in &layout_boxes {
            let bbox_px = geometry::denormalize_0to1000_bbox(lb.bbox_1000, page.width, page.height);
            let (category, warning) = category_map::map_mineru_vlm_category(&lb.category_raw);
            if let Some(w) = warning {
                eprintln!("mineru-vlm page {}: {w}", page.page_num);
            }
            pending.push(PendingBlock {
                bbox_px,
                category_raw: lb.category_raw.clone(),
                category,
                angle: lb.angle,
            });
        }

        let bboxes: Vec<[i32; 4]> = pending.iter().map(|p| p.bbox_px).collect();
        let kept_indices = geometry::dedupe_by_iou(&bboxes, IOU_DEDUPE_THRESHOLD);
        let pending: Vec<PendingBlock> = kept_indices
            .into_iter()
            .map(|i| {
                // Safe: `kept_indices` are indices into the original `pending`.
                let p = &pending[i];
                PendingBlock {
                    bbox_px: p.bbox_px,
                    category_raw: p.category_raw.clone(),
                    category: p.category.clone(),
                    angle: p.angle,
                }
            })
            .collect();

        // Stage 2: per-block content extraction, concurrent within the
        // page (bounded by the shared document-level permit budget).
        let futures_iter = pending.iter().enumerate().map(|(index, p)| {
            let skip = SKIP_CONTENT.contains(&p.category_raw.as_str());
            async move {
                if skip {
                    return (index, Ok(None));
                }

                let _permit = ctx.acquire_permit().await;
                let crop_img = match ctx.crop(page, p.bbox_px) {
                    Ok(img) => img,
                    Err(e) => return (index, Err(e)),
                };
                let rotated = match p.angle {
                    Some(a @ (90 | 180 | 270)) => imaging::rotate_90n(&crop_img, a),
                    _ => crop_img,
                };
                let resized = imaging::resize_by_need(&rotated, MAX_EDGE_RATIO, MIN_EDGE);
                let data_url = match imaging::to_base64_data_url(&resized) {
                    Ok(u) => u,
                    Err(e) => return (index, Err(e)),
                };

                let (prompt, sampling) = Self::stage2_prompt_and_sampling(&p.category_raw);
                let req = self.request(self.stage2_endpoint(index), prompt, &data_url, sampling);
                match ctx.dispatch(req).await {
                    Ok(resp) => {
                        let content = extract_content(&resp).unwrap_or("").to_string();
                        (index, Ok(Some(content)))
                    }
                    Err(e) => (index, Err(e.to_string())),
                }
            }
        });
        let stage2_results = join_all(futures_iter).await;

        let mut content_by_index: std::collections::HashMap<usize, Result<Option<String>, String>> =
            stage2_results.into_iter().collect();

        let mut blocks = Vec::with_capacity(pending.len());
        for (index, p) in pending.iter().enumerate() {
            let outcome = content_by_index.remove(&index).unwrap_or(Ok(None));
            let (text, html, latex, error) = match outcome {
                Ok(None) => (None, None, None, None),
                Ok(Some(content)) => match p.category_raw.as_str() {
                    "table" => (None, Some(otsl::to_html(&content)), None, None),
                    "equation" => {
                        let repaired =
                            formula_repair::repair_chain(formula_repair::DEFAULT_CHAIN, &content);
                        (None, None, Some(wrap_display_math(&repaired)), None)
                    }
                    _ => (Some(content), None, None, None),
                },
                Err(e) => (None, None, None, Some(e)),
            };

            let [x0, y0, x1, y1] = p.bbox_px;
            blocks.push(Block {
                geom: Geometry::Rect([x0 as f32, y0 as f32, x1 as f32, y1 as f32]),
                geom_frame: CoordFrame::Page,
                bbox_px: Some(p.bbox_px),
                category_raw: p.category_raw.clone(),
                category: Some(p.category.clone()),
                reading_order: None,
                text,
                html,
                latex,
                spans: vec![],
                merge_hint: None,
                confidence: None,
                source: BlockSource::LayoutThenRecognize,
                error,
            });
        }

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

    fn chat_response(content: &str) -> Value {
        serde_json::json!({
            "choices": [{"message": {"content": content}}]
        })
    }

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
    async fn two_stage_orchestration_offline_via_mock_dispatch() {
        let adapter = MineruVlmAdapter::default();
        let mock = Arc::new(MockDispatch::new());

        let layout = "\
<|box_start|>0 0 200 100<|box_end|><|ref_start|>text<|ref_end|>
<|box_start|>0 200 200 400<|box_end|><|ref_start|>table<|ref_end|>
<|box_start|>0 500 200 600<|box_end|><|ref_start|>equation<|ref_end|>
<|box_start|>0 700 200 900<|box_end|><|ref_start|>image<|ref_end|>";
        mock.seed(&adapter.stage1_endpoint(), chat_response(layout));
        mock.seed(&adapter.stage2_endpoint(0), chat_response("Hello world"));
        mock.seed(
            &adapter.stage2_endpoint(1),
            chat_response("<fcel>a<fcel>b<nl><fcel>c<fcel>d"),
        );
        mock.seed(&adapter.stage2_endpoint(2), chat_response(r"\frac{1}{2"));
        // Deliberately no seed for stage2_endpoint(3) — the "image"
        // category block must never dispatch a stage-2 request.

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(4)));
        let page = fake_page(400, 1000);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds");
        assert_eq!(blocks.len(), 4);

        let text_block = &blocks[0];
        assert_eq!(text_block.category.as_deref(), Some("text"));
        assert_eq!(text_block.text.as_deref(), Some("Hello world"));
        assert_eq!(text_block.bbox_px, Some([0, 0, 80, 100]));
        assert!(text_block.error.is_none());

        let table_block = &blocks[1];
        assert_eq!(table_block.category.as_deref(), Some("table"));
        assert_eq!(
            table_block.html.as_deref(),
            Some("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>")
        );
        assert_eq!(table_block.bbox_px, Some([0, 200, 80, 400]));

        let equation_block = &blocks[2];
        assert_eq!(equation_block.category.as_deref(), Some("equation"));
        assert_eq!(
            equation_block.latex.as_deref(),
            Some("\\[\n\\frac{1}{2}\n\\]")
        );
        assert_eq!(equation_block.bbox_px, Some([0, 500, 80, 600]));

        let image_block = &blocks[3];
        assert_eq!(image_block.category.as_deref(), Some("image"));
        assert!(image_block.text.is_none());
        assert!(image_block.html.is_none());
        assert!(image_block.latex.is_none());
        assert!(
            image_block.error.is_none(),
            "image category must skip stage 2 entirely, not dispatch-and-fail"
        );
        assert_eq!(image_block.bbox_px, Some([0, 700, 80, 900]));
    }

    #[tokio::test]
    async fn missing_stage1_seed_yields_page_error() {
        let adapter = MineruVlmAdapter::default();
        let mock = Arc::new(MockDispatch::new());
        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let page = fake_page(100, 100);

        let result = adapter.parse_page(&page, &ctx).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().stage.as_deref(), Some("layout"));
    }

    #[test]
    fn declares_expected_protocol_metadata() {
        let adapter = MineruVlmAdapter::default();
        assert_eq!(adapter.name(), "mineru-vlm");
        assert_eq!(adapter.coordinate_system(), CoordinateSystem::Norm0To1000);
        assert!(!adapter.provides_reading_order());
        assert_eq!(adapter.raw_output_format(), RawOutputFormat::CustomToken);
        let signals = adapter.emitted_signals();
        assert!(!signals.spans && !signals.merge_hint && !signals.font_size);
    }
}
