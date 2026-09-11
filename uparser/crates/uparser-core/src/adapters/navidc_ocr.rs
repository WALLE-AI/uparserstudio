//! NaviDC-OCR two-stage protocol adapter, per
//! `NAVIDC_OCR_ADAPTER_EXECUTION_PLAN.md`. Protocol details (layout
//! grammar, category vocab, per-category prompts, skip list, sampling
//! params) are read directly from the vendored
//! `opensource/NaviDC-OCR/NaviOCR/vlm_utils/{structs.py,NaviOCR_client.py}`
//! and `src/vlm_magic_model.py` — no other project's parser is reused
//! for the wire grammar itself (`<box:...><label:...><direction>` shares
//! no syntax with mineru-vlm's `custom_token` or MonkeyOCRv2's
//! Python-literal grammar).
//!
//! **Known upstream gap** (plan §2.1): `new_vlm_client()` declares
//! support for an `http-client` backend, but the vendored repository has
//! no committed `vlm_client/http_client.py` and no confirmed real
//! endpoint was available to this session — this adapter's standard
//! OpenAI-chat-completions request/response contract, sampling
//! parameters, and per-category prompt selection are therefore
//! **offline-verified only** (same status dots-ocr/MonkeyOCRv2 started
//! at), not confirmed against a live NaviDC-OCR vLLM deployment. Real
//! endpoint validation (plan §6.4) is a follow-up. One specific,
//! documented uncertainty: the real client's `prompts.get(block.type) or
//! prompts["default"]` lookup only has dedicated prompt entries for
//! `table`/`code`/`seal`/`char` — `equation`/`equation_block` fall back
//! to the plain text-extraction prompt unless a caller supplies a custom
//! prompt override. This adapter instead gives `"equation"` its own
//! LaTeX-extraction prompt (matching the plan's directive table), which
//! must be re-confirmed once a real endpoint is available.
//!
//! **Deliberate v1 simplification** (plan §F's own explicit allowance:
//! "第一版不完整复制上游 MagicModel 的 Python 层级对象... 只移植会影响
//! 最终 Markdown 正确性的关系规则，保持数据不丢失"): `list` and
//! `equation_block` are containers in the real protocol whose children
//! this adapter does not reconstruct. Rather than dropping them (losing
//! geometry/reading-order data) or fabricating placeholder text, they're
//! kept as ordinary blocks with `text: None` — the same choice
//! `mineru_vlm.rs` already makes for its own `list`/`image_block`/
//! `equation_block` skip-content categories.

use super::{
    ModelStage, NavidcLayoutMode, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat,
    RemoteEndpointSpec, ResourceHint, StageBackend, extract_chat_content,
};
use crate::category_map::{self, NAVIDC_OCR_CATEGORIES};
use crate::formula_repair;
use crate::geometry;
use crate::imaging;
use crate::ingest::RenderedPage;
use crate::otsl;
use crate::output_parse;
use crate::robustness;
use crate::transport::ChatCompletionRequest;
use crate::types::{Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, PageError};
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

const LAYOUT_IMAGE_SIZE: u32 = 1036;
const MAX_EDGE_RATIO: f32 = 50.0;
const MIN_EDGE: u32 = 28;

/// Detection mode: axis-aligned 4-number rects.
/// Stage-2 output budget. 4096 matches the upstream model card's own
/// Quick Start (`max_new_tokens=4096`); upstream's client otherwise
/// leaves it unset and lets the server decide.
///
/// Deliberately **not** 8192: a request whose `max_tokens` equals the
/// server's `--max-model-len` is rejected outright (vLLM: "'max_tokens'
/// is too large: 8192 ... your request has 32 input tokens"), and 8192
/// is the most common max-model-len for this 1.2B checkpoint. Found by
/// a real vLLM deployment where every stage-2 block failed with a 400
/// while stage-1 (already 4096) succeeded.
const STAGE2_MAX_TOKENS: u32 = 4096;

const LAYOUT_PROMPT_DETECTION: &str = "\nAnalyze the image layout.";
/// Segmentation mode: multi-point polygons for non-rectangular regions.
/// Confirmed against a live deployment — same model and endpoint as
/// Detection, selected purely by this prompt text.
const LAYOUT_PROMPT_SEGMENTATION: &str = "\nMulti-point Layout Segmentation Analysis.";
const SYSTEM_PROMPT: &str = "You are a helpful assistant.";

/// Native categories for which stage 2 (content recognition) is skipped
/// entirely — `NaviOCR_client.py`'s `prepare_for_extract` default
/// `skip_list`.
const SKIP_CONTENT: &[&str] = &["image", "list", "equation_block"];

fn stage2_prompt(category_raw: &str) -> &'static str {
    match category_raw {
        "table" => "\nThis is the image of a table. Please output the table in OTSL format.",
        "equation" => {
            "\nPlease write out the expression of the formula in the image using LaTeX format."
        }
        "code" | "algorithm" => {
            "\nThe image contains a code snippet, please output the parsing result."
        }
        "seal" => "\nSeal Recognition:",
        "char" => "\nThis is a scientific figure. Please extract the table implied by this figure.",
        _ => "\nPlease output the text content from the image.",
    }
}

pub struct NavidcOcrAdapter {
    pub endpoint_base: String,
    pub model: String,
    pub timeout: Duration,
    pub max_retries: u32,
    /// Detection (default, matching upstream's own default) or
    /// Segmentation. See `NavidcLayoutMode`.
    pub layout_mode: NavidcLayoutMode,
}

impl Default for NavidcOcrAdapter {
    fn default() -> Self {
        Self {
            endpoint_base: "http://localhost:8000/v1/chat/completions".to_string(),
            model: "StarDoc-AI/NaviDC-OCR".to_string(),
            timeout: Duration::from_secs(120),
            max_retries: 2,
            layout_mode: NavidcLayoutMode::Detection,
        }
    }
}

impl NavidcOcrAdapter {
    fn stage1_endpoint(&self) -> String {
        format!("{}#layout", self.endpoint_base)
    }

    fn stage2_endpoint(&self, block_index: usize) -> String {
        format!("{}#recognize:{block_index}", self.endpoint_base)
    }

    fn request(
        &self,
        endpoint: String,
        prompt: &str,
        image_data_url: &str,
        max_tokens: u32,
    ) -> ChatCompletionRequest {
        let messages = vec![
            serde_json::json!({"role": "system", "content": SYSTEM_PROMPT}),
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
            // Base sampling per `DEFAULT_SAMPLING_PARAMS` in
            // `NaviOCR_client.py`: greedy-ish decoding (temperature 0,
            // top_p 0.01, top_k 1). Per-category presence/frequency
            // penalty overrides are not yet wired (real endpoint not
            // available to confirm their exact request-body encoding —
            // plan §2.1).
            sampling: serde_json::json!({
                "temperature": 0,
                "top_p": 0.01,
                "max_tokens": max_tokens,
            }),
            timeout: self.timeout,
            max_retries: self.max_retries,
        }
    }
}

struct PendingBlock {
    /// Bounding rect in page pixels — always present, even for a
    /// polygon block (the envelope of its mapped points).
    bbox_px: [i32; 4],
    /// `Some` only for a Segmentation-mode polygon block; the mapped
    /// page-pixel vertices used for the masked crop.
    polygon_px: Option<Vec<[i32; 2]>>,
    category_raw: String,
    category: String,
    angle: Option<u32>,
}

#[async_trait]
impl ProtocolAdapter for NavidcOcrAdapter {
    fn name(&self) -> &'static str {
        "navidc-ocr"
    }

    fn coordinate_system(&self) -> CoordinateSystem {
        CoordinateSystem::Norm0To1000
    }

    fn provides_reading_order(&self) -> bool {
        true
    }

    fn category_vocab(&self) -> &[&'static str] {
        NAVIDC_OCR_CATEGORIES
    }

    fn raw_output_format(&self) -> RawOutputFormat {
        RawOutputFormat::NaviLayoutTokens
    }

    fn emitted_signals(&self) -> PostprocessSignals {
        PostprocessSignals::default()
    }

    fn model_stages(&self) -> Vec<ModelStage> {
        vec![ModelStage {
            stage_name: "vlm",
            default_backend: StageBackend::Remote(RemoteEndpointSpec {
                endpoint_env_var: "NAVIDC_OCR_ENDPOINT",
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

        // Stage 1: layout, hard-resized (non-aspect-preserving) to the
        // fixed 1036x1036 canvas — matches `layout_image_size=(1036,1036)`
        // in `NaviOCR_client.py`.
        let layout_img = imaging::hard_resize(&page_rgb, LAYOUT_IMAGE_SIZE, LAYOUT_IMAGE_SIZE);
        let layout_data_url = imaging::to_base64_data_url(&layout_img).map_err(|e| PageError {
            page_num: page.page_num,
            message: format!("failed to encode layout image: {e}"),
            stage: Some("layout".into()),
        })?;
        let layout_prompt = match self.layout_mode {
            NavidcLayoutMode::Detection => LAYOUT_PROMPT_DETECTION,
            NavidcLayoutMode::Segmentation => LAYOUT_PROMPT_SEGMENTATION,
        };
        let layout_req = self.request(
            self.stage1_endpoint(),
            layout_prompt,
            &layout_data_url,
            4096,
        );
        let layout_resp =
            crate::shape_executor::chat_stage(page, ctx, layout_req, "layout").await?;
        let layout_content = extract_chat_content(&layout_resp).map_err(|e| PageError {
            page_num: page.page_num,
            message: e,
            stage: Some("layout".into()),
        })?;
        if super::is_truncated_response(&layout_resp) {
            ctx.warn(format!(
                "navidc-ocr page {}: layout response was truncated (finish_reason: length) — recovered content below may be an incomplete prefix",
                page.page_num
            ));
        }
        let (layout_blocks, warnings) = output_parse::parse_navidc_layout_lines(layout_content);
        for w in &warnings {
            ctx.warn(format!("navidc-ocr page {}: {w}", page.page_num));
        }

        let pending: Vec<PendingBlock> = layout_blocks
            .iter()
            .map(|lb| {
                let (bbox_px, polygon_px) = match &lb.geometry_1000 {
                    Geometry::Rect(r) => (
                        geometry::map_bbox_0to1000_clamped(*r, page.width, page.height),
                        None,
                    ),
                    Geometry::Polygon(points) => {
                        let mapped =
                            geometry::map_polygon_0to1000_clamped(points, page.width, page.height);
                        let xs: Vec<f32> = mapped.iter().map(|p| p[0]).collect();
                        let ys: Vec<f32> = mapped.iter().map(|p| p[1]).collect();
                        let bbox = geometry::sanitize_bbox_px(
                            [
                                xs.iter().cloned().fold(f32::INFINITY, f32::min) as i32,
                                ys.iter().cloned().fold(f32::INFINITY, f32::min) as i32,
                                xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max) as i32,
                                ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max) as i32,
                            ],
                            page.width,
                            page.height,
                        );
                        let points_px: Vec<[i32; 2]> = mapped
                            .iter()
                            .map(|p| [p[0].round() as i32, p[1].round() as i32])
                            .collect();
                        (bbox, Some(points_px))
                    }
                };
                let (category, warning) = category_map::map_navidc_ocr_category(&lb.category_raw);
                if let Some(w) = warning {
                    ctx.warn(format!("navidc-ocr page {}: {w}", page.page_num));
                }
                PendingBlock {
                    bbox_px,
                    polygon_px,
                    category_raw: lb.category_raw.clone(),
                    category,
                    angle: lb.angle,
                }
            })
            .collect();

        // Stage 2: per-block recognition, concurrent within the page
        // (bounded by the shared document-level permit budget).
        let page_rgb_ref = &page_rgb;
        let futures_iter = pending.iter().enumerate().map(|(index, p)| {
            let skip = SKIP_CONTENT.contains(&p.category_raw.as_str());
            async move {
                if skip {
                    return (index, Ok(None));
                }

                // Crop/rotate/resize/encode first (CPU-bound, no
                // network) — acquiring the permit before this point
                // would let this page's image processing consume the
                // document-level concurrency budget without a request
                // in flight, silently reducing real request concurrency
                // below `--max-concurrency`.
                let crop_img = match &p.polygon_px {
                    Some(points) => match imaging::crop_polygon_masked(page_rgb_ref, points) {
                        Some(img) => img,
                        None => return (index, Err(format!("polygon crop {points:?} does not overlap the page at all"))),
                    },
                    None => match ctx.crop(page, p.bbox_px) {
                        Ok(img) => img,
                        Err(e) => return (index, Err(e)),
                    },
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

                let prompt = stage2_prompt(&p.category_raw);
                let req =
                    self.request(self.stage2_endpoint(index), prompt, &data_url, STAGE2_MAX_TOKENS);
                let _permit = ctx.acquire_permit().await;
                let content = match ctx.dispatch(req).await {
                    Ok(resp) => match extract_chat_content(&resp) {
                        Ok(content) => content.to_string(),
                        Err(e) => return (index, Err(e)),
                    },
                    Err(e) => return (index, Err(e.to_string())),
                };

                // Robustness (`robustness.rs`, wired the same way as
                // mineru-vlm/MonkeyOCRv2): only for plain free-text
                // content, and only after a real successful dispatch, so
                // a connectivity failure is never masked as "degenerate
                // empty content" — it already returned above via the
                // `Err` arms.
                let is_plain_text = !matches!(p.category_raw.as_str(), "table" | "equation");
                let content = if is_plain_text && robustness::is_degenerate(&content) {
                    ctx.warn(format!(
                        "navidc-ocr page {}: stage-2 content for block {index} ({}) looks degenerate (repetitive loop) — retrying with escalating temperature",
                        page.page_num, p.category_raw
                    ));
                    let policy = robustness::RetryPolicy::default();
                    let first_content = content.clone();
                    let attempt_no = std::sync::atomic::AtomicU32::new(0);
                    robustness::retry_with_temperature(&policy, 0.0, |temp| {
                        let n = attempt_no.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let seed = if n == 0 {
                            Some(first_content.clone())
                        } else {
                            None
                        };
                        let mut req = self.request(
                            self.stage2_endpoint(index),
                            prompt,
                            &data_url,
                            STAGE2_MAX_TOKENS,
                        );
                        if let Value::Object(m) = &mut req.sampling {
                            m.insert("temperature".to_string(), serde_json::json!(temp));
                        }
                        let fallback = first_content.clone();
                        async move {
                            if let Some(seed) = seed {
                                return seed;
                            }
                            let _permit = ctx.acquire_permit().await;
                            match ctx.dispatch(req).await {
                                Ok(resp) => extract_chat_content(&resp)
                                    .map(|c| c.to_string())
                                    .unwrap_or(fallback),
                                Err(_) => fallback,
                            }
                        }
                    })
                    .await
                } else {
                    content
                };

                (index, Ok(Some(content)))
            }
        });
        let mut content_by_index = crate::shape_executor::collect_indexed(futures_iter).await;

        let mut blocks = Vec::with_capacity(pending.len());
        for (index, p) in pending.iter().enumerate() {
            let outcome = content_by_index.remove(&index).unwrap_or(Ok(None));
            let (text, html, latex, error) = match outcome {
                Ok(None) => (None, None, None, None),
                Ok(Some(content)) => match p.category_raw.as_str() {
                    "table" => {
                        let (html, warnings) = otsl::to_html(&content);
                        for w in &warnings {
                            ctx.warn(format!("navidc-ocr page {}: {w}", page.page_num));
                        }
                        (None, Some(html), None, None)
                    }
                    "equation" => {
                        let repaired =
                            formula_repair::repair_chain(formula_repair::DEFAULT_CHAIN, &content);
                        (
                            None,
                            None,
                            Some(formula_repair::wrap_display_math(&repaired)),
                            None,
                        )
                    }
                    _ => (Some(content), None, None, None),
                },
                Err(e) => (None, None, None, Some(e)),
            };

            // Same shape as mineru-vlm/MonkeyOCRv2: the model is never
            // called for "image" content, but the pixels are still
            // cropped and preserved so `render.rs` has something to
            // link to (`image_link_gap_report.md`).
            let asset_bytes = if p.category_raw == "image" {
                match &p.polygon_px {
                    Some(points) => imaging::crop_polygon_masked(&page_rgb, points),
                    None => imaging::crop(&page_rgb, p.bbox_px),
                }
                .and_then(|img| imaging::to_png_bytes(&img).ok())
            } else {
                None
            };

            let geom = match &p.polygon_px {
                Some(points) => {
                    Geometry::Polygon(points.iter().map(|p| [p[0] as f32, p[1] as f32]).collect())
                }
                None => {
                    let [x0, y0, x1, y1] = p.bbox_px;
                    Geometry::Rect([x0 as f32, y0 as f32, x1 as f32, y1 as f32])
                }
            };

            blocks.push(Block {
                geom,
                geom_frame: CoordFrame::Page,
                bbox_px: Some(p.bbox_px),
                category_raw: p.category_raw.clone(),
                category: Some(p.category.clone()),
                reading_order: Some(index as u32),
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
                asset_caption: None,
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

    #[test]
    fn declares_expected_protocol_metadata() {
        let adapter = NavidcOcrAdapter::default();
        assert_eq!(adapter.name(), "navidc-ocr");
        assert_eq!(adapter.coordinate_system(), CoordinateSystem::Norm0To1000);
        assert!(adapter.provides_reading_order());
        assert_eq!(
            adapter.raw_output_format(),
            RawOutputFormat::NaviLayoutTokens
        );
        let signals = adapter.emitted_signals();
        assert!(!signals.spans && !signals.merge_hint && !signals.font_size);
    }

    #[tokio::test]
    async fn two_stage_orchestration_offline_via_mock_dispatch() {
        let adapter = NavidcOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());

        let layout = "\
<box:0 0 200 100><label:text><up>
<box:0 200 200 400><label:table><up>
<box:0 500 200 600><label:equation><up>
<box:0 700 200 900><label:image><up>";
        mock.seed(&adapter.stage1_endpoint(), chat_response(layout));
        mock.seed(&adapter.stage2_endpoint(0), chat_response("Hello world"));
        mock.seed(
            &adapter.stage2_endpoint(1),
            chat_response("<fcel>a<fcel>b<nl><fcel>c<fcel>d"),
        );
        mock.seed(&adapter.stage2_endpoint(2), chat_response(r"\frac{1}{2"));
        // Deliberately no seed for stage2_endpoint(3) — "image" must
        // never dispatch a stage-2 request.

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
        assert_eq!(text_block.reading_order, Some(0));
        assert!(text_block.error.is_none());

        let table_block = &blocks[1];
        assert_eq!(table_block.category.as_deref(), Some("table"));
        assert_eq!(
            table_block.html.as_deref(),
            Some("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>")
        );

        let equation_block = &blocks[2];
        assert_eq!(equation_block.category.as_deref(), Some("equation"));
        assert_eq!(
            equation_block.latex.as_deref(),
            Some("\\[\n\\frac{1}{2}\n\\]")
        );

        let image_block = &blocks[3];
        assert_eq!(image_block.category.as_deref(), Some("image"));
        assert!(image_block.text.is_none());
        assert!(
            image_block.error.is_none(),
            "image must skip stage 2 entirely, not dispatch-and-fail"
        );
        let asset_bytes = image_block
            .asset_bytes
            .as_ref()
            .expect("image block must have cropped pixels attached");
        image::load_from_memory(asset_bytes).expect("must be valid PNG bytes");
    }

    #[tokio::test]
    async fn skip_categories_never_dispatch_a_stage2_request() {
        let adapter = NavidcOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());

        let layout = "\
<box:0 0 200 100><label:list><up>
<box:0 200 200 400><label:equation_block><up>";
        mock.seed(&adapter.stage1_endpoint(), chat_response(layout));
        // No stage-2 seeds at all — a dispatch attempt for either block
        // would fail the mock lookup and fail this test.

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(4)));
        let page = fake_page(400, 1000);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds");
        assert_eq!(blocks.len(), 2);
        assert!(blocks.iter().all(|b| b.text.is_none() && b.error.is_none()));
    }

    /// The layout mode must actually reach the wire. Detection and
    /// Segmentation differ *only* by this prompt string against the real
    /// service, so an adapter that silently always sends the Detection
    /// prompt would make every polygon code path unreachable in
    /// production while still passing its own mock-fed polygon tests —
    /// which is exactly the state this adapter shipped in before
    /// `--layout-mode` existed.
    #[test]
    fn layout_mode_selects_the_prompt_that_actually_reaches_the_wire() {
        let detection = NavidcOcrAdapter::default();
        assert_eq!(detection.layout_mode, NavidcLayoutMode::Detection);

        let segmentation = NavidcOcrAdapter {
            layout_mode: NavidcLayoutMode::Segmentation,
            ..Default::default()
        };
        let prompt_for = |a: &NavidcOcrAdapter| match a.layout_mode {
            NavidcLayoutMode::Detection => LAYOUT_PROMPT_DETECTION,
            NavidcLayoutMode::Segmentation => LAYOUT_PROMPT_SEGMENTATION,
        };
        assert_ne!(prompt_for(&detection), prompt_for(&segmentation));
        assert!(prompt_for(&segmentation).contains("Multi-point Layout Segmentation"));
    }

    /// The registry is the only path the CLI/API can reach an adapter
    /// through, so the override must survive it *and* land in the actual
    /// request body. Asserted against the recorded wire payload rather
    /// than the adapter struct, because the registry returns a trait
    /// object whose concrete fields aren't observable — and because
    /// "the field is set" is not the same claim as "the prompt changed".
    #[tokio::test]
    async fn registry_layout_mode_override_reaches_the_real_request_body() {
        let registry = crate::adapters::Registry::with_builtins();
        let endpoint = "http://mock/v1/chat/completions";

        for (mode, expected_prompt) in [
            (NavidcLayoutMode::Detection, LAYOUT_PROMPT_DETECTION),
            (NavidcLayoutMode::Segmentation, LAYOUT_PROMPT_SEGMENTATION),
        ] {
            let adapter = registry
                .build(
                    "navidc-ocr",
                    &crate::adapters::AdapterOverrides {
                        endpoint: Some(endpoint.to_string()),
                        navidc: Some(crate::adapters::NavidcConfig {
                            layout_mode: Some(mode),
                        }),
                        ..Default::default()
                    },
                )
                .expect("navidc-ocr is registered");

            let layout_key = format!("{endpoint}#layout");
            let mock = Arc::new(MockDispatch::new());
            mock.seed(&layout_key, chat_response(""));
            let ctx = ParseCtx::with_mock(mock.clone(), Arc::new(Semaphore::new(1)));

            adapter
                .parse_page(&fake_page(100, 100), &ctx)
                .await
                .expect("parse_page succeeds");

            let sent = mock.recorded_requests(&layout_key);
            assert_eq!(sent.len(), 1, "exactly one layout request");
            let prompt = sent[0]["messages"][1]["content"][1]["text"]
                .as_str()
                .expect("stage-1 request carries a text content part");
            assert_eq!(
                prompt, expected_prompt,
                "mode {mode:?} sent the wrong prompt"
            );
        }
    }

    #[tokio::test]
    async fn segmentation_polygon_block_maps_and_crops_via_mask() {
        let adapter = NavidcOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());

        let layout = "<box:100 0 900 100 900 900 100 900><label:text><up>";
        mock.seed(&adapter.stage1_endpoint(), chat_response(layout));
        mock.seed(&adapter.stage2_endpoint(0), chat_response("polygon text"));

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(4)));
        let page = fake_page(1000, 1000);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds");
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0].geom, Geometry::Polygon(_)));
        assert_eq!(blocks[0].text.as_deref(), Some("polygon text"));
    }

    #[tokio::test]
    async fn degenerate_stage2_content_retries_with_escalating_temperature() {
        let adapter = NavidcOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());

        let layout = "<box:0 0 200 100><label:text><up>";
        mock.seed(&adapter.stage1_endpoint(), chat_response(layout));
        mock.seed(
            &adapter.stage2_endpoint(0),
            chat_response("loop loop loop loop loop loop loop loop loop loop "),
        );
        mock.seed(
            &adapter.stage2_endpoint(0),
            chat_response("a well formed sentence"),
        );

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(4)));
        let page = fake_page(400, 1000);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text.as_deref(), Some("a well formed sentence"));

        let warnings = ctx.warnings_snapshot();
        assert!(
            warnings.iter().any(|w| w.contains("degenerate")),
            "{warnings:?}"
        );
    }

    #[tokio::test]
    async fn missing_stage1_seed_yields_page_error() {
        let adapter = NavidcOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());
        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let page = fake_page(100, 100);

        let result = adapter.parse_page(&page, &ctx).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().stage.as_deref(), Some("layout"));
    }

    #[tokio::test]
    async fn noisy_layout_lines_recover_the_valid_ones() {
        let adapter = NavidcOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());

        let layout = "garbage preamble\n<box:0 0 200 100><label:text><up>\nmore garbage";
        mock.seed(&adapter.stage1_endpoint(), chat_response(layout));
        mock.seed(&adapter.stage2_endpoint(0), chat_response("Hello"));

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let page = fake_page(400, 1000);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page still recovers the valid line");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text.as_deref(), Some("Hello"));
    }

    #[tokio::test]
    async fn truncated_layout_response_surfaces_a_warning_but_still_recovers_content() {
        let adapter = NavidcOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());
        let truncated = serde_json::json!({
            "choices": [{
                "message": {"content": "<box:0 0 200 100><label:text><up>"},
                "finish_reason": "length",
            }]
        });
        mock.seed(&adapter.stage1_endpoint(), truncated);
        mock.seed(&adapter.stage2_endpoint(0), chat_response("partial"));

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let page = fake_page(400, 1000);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page still recovers a partial result");
        assert!(!blocks.is_empty());
        let warnings = ctx.warnings_snapshot();
        assert!(
            warnings.iter().any(|w| w.contains("truncated")),
            "{warnings:?}"
        );
    }

    #[tokio::test]
    async fn single_block_recognition_failure_does_not_fail_the_whole_page() {
        let adapter = NavidcOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());

        let layout = "\
<box:0 0 200 100><label:text><up>
<box:0 200 200 400><label:text><up>";
        mock.seed(&adapter.stage1_endpoint(), chat_response(layout));
        mock.seed(&adapter.stage2_endpoint(0), chat_response("first block ok"));
        // Deliberately no seed for stage2_endpoint(1) — that dispatch
        // fails, but the page must still return both blocks.

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(4)));
        let page = fake_page(400, 1000);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds despite one block failing");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text.as_deref(), Some("first block ok"));
        assert!(blocks[1].error.is_some());
    }
}
