//! MonkeyOCRv2 two-stage protocol adapter, per T-3.4. Protocol details
//! (prompts, resize strategy, Python-literal output, bbox mapping) are
//! confirmed against the fully vendored `opensource/MonkeyOCRv2` source
//! (`parsing/core_runner.py`), no version-mismatch caveat.
//!
//! Originally added to prove Gate G3 by reusing `otsl.rs` and
//! `formula_repair.rs` verbatim. That claim is **no longer true and
//! should not be restored**: both shared modules are MinerU-derived and
//! disagree with this protocol's reference implementation in ways that
//! change output, so table/formula handling now goes through
//! `monkeyocr_post` instead (that module's doc lists each disagreement).
//! Gate G3's actual finding stands — the shared modules were reusable
//! across three protocols — it just isn't what fidelity to *this*
//! protocol requires.
//!
//! Document dewarming preprocessing (an independent local-torch model in
//! the real pipeline, unrelated to the parsing VLM) is deliberately
//! **not** implemented here, per the P3 plan's T-3.5 decision (option a:
//! skip) — this adapter does not correct skewed/photographed input.

use super::{
    ModelStage, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat, RemoteEndpointSpec,
    ResourceHint, StageBackend, extract_chat_content,
};
use crate::category_map::{self, MONKEYOCR_V2_CATEGORIES};
use crate::geometry;
use crate::imaging;
use crate::ingest::RenderedPage;
use crate::monkeyocr_post;
use crate::output_parse;
use crate::transport::ChatCompletionRequest;
use crate::types::{
    Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, MergeHint, PageError,
};
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

/// `BackendConfig.max_pixels` / `MOCR2_MAX_PIXELS`, applied by
/// `load_image` as an upper bound on **every** request's image, and
/// additionally as `get_layout`'s `min_pixels` lower bound.
const TARGET_PIXELS: u32 = 1_003_520;

/// `get_layout`'s `max_tokens`.
const LAYOUT_MAX_TOKENS: u32 = 4096;

/// `_parse_page` / `_recognize_one_block`'s `max_tokens` for the
/// per-block recognition call. Was 10000 here, which is not a budget the
/// reference implementation ever grants.
const RECOGNIZE_MAX_TOKENS: u32 = 5000;

/// `batch_inference_with_repeat_retry` / `_recognize_one_block`'s retry
/// sampling: `temperature = min(0.2 * (retries + 1), 0.8)` with a fixed
/// `top_p`.
/// `f64` rather than `f32` so the serialized value is bit-for-bit what
/// upstream's Python float arithmetic puts on the wire (`0.2`, `0.4`,
/// `0.6000000000000001`) instead of an `f32`-widening artifact like
/// `0.20000000298023224`.
const RETRY_TEMPERATURE_STEP: f64 = 0.2;
const RETRY_TEMPERATURE_CAP: f64 = 0.8;
const RETRY_TOP_P: f64 = 0.95;

const LAYOUT_PROMPT: &str =
    "Please output the categories and coordinates of the document elements in reading order.";

/// `ALL_PROMPT[label]` for stage 2 — `None` means "not in ALL_PROMPT",
/// i.e. `need_infer = False`, matching `Picture` (and any unrecognized
/// label) skipping stage 2 entirely.
fn stage2_prompt(label: &str) -> Option<&'static str> {
    match label {
        "Caption" | "List-item" | "Page-footer" | "Page-header" | "Section-header" | "Text"
        | "Title" => Some("Please output the text content from the image."),
        "Formula" => {
            Some("Please write out the expression of the formula in the image using LaTeX format.")
        }
        "Table" => Some("Please extract the table from the image and represent it in OTSL format."),
        _ => None,
    }
}

pub struct MonkeyOcrV2Adapter {
    pub endpoint_base: String,
    pub model: String,
    pub timeout: Duration,
    pub max_retries: u32,
    /// `PipelineConfig.retry_repeat` — re-issue a recognition request at
    /// an escalating temperature when the response looks like a repeat
    /// loop. **Off by default**, matching upstream, where it is an opt-in
    /// flag rather than standing behavior.
    pub retry_repeat: bool,
    /// `PipelineConfig.retry_repeat_max_retries` / `MOCR2_REC_MAX_RETRIES`.
    pub retry_repeat_max_retries: u32,
}

impl Default for MonkeyOcrV2Adapter {
    fn default() -> Self {
        let spec = crate::protocol_spec::spec_of("monkeyocr-v2");
        Self {
            endpoint_base: spec.endpoint_default(),
            model: spec.model_default(),
            timeout: spec.timeout_default(),
            max_retries: spec.default_max_retries,
            retry_repeat: false,
            retry_repeat_max_retries: 3,
        }
    }
}

impl MonkeyOcrV2Adapter {
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
        // No system message on the HTTP-client path (matches
        // `_chat_completion`'s message shape — only the local-engine
        // ChatML path adds one, which this adapter doesn't implement).
        let messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "image_url", "image_url": {"url": image_data_url}},
                {"type": "text", "text": prompt},
            ],
        })];
        ChatCompletionRequest {
            endpoint,
            model: self.model.clone(),
            messages,
            sampling: serde_json::json!({
                "temperature": 0,
                "max_tokens": max_tokens,
            }),
            timeout: self.timeout,
            max_retries: self.max_retries,
        }
    }
}

struct PendingBlock {
    bbox_px: [i32; 4],
    label: String,
    category: String,
}

#[async_trait]
impl ProtocolAdapter for MonkeyOcrV2Adapter {
    fn name(&self) -> &'static str {
        "monkeyocr-v2"
    }

    fn coordinate_system(&self) -> CoordinateSystem {
        CoordinateSystem::Norm0To1000
    }

    fn provides_reading_order(&self) -> bool {
        true
    }

    fn category_vocab(&self) -> &[&'static str] {
        MONKEYOCR_V2_CATEGORIES
    }

    fn raw_output_format(&self) -> RawOutputFormat {
        RawOutputFormat::PythonLiteralEval
    }

    fn emitted_signals(&self) -> PostprocessSignals {
        PostprocessSignals::default()
    }

    fn model_stages(&self) -> Vec<ModelStage> {
        vec![ModelStage {
            stage_name: "vlm",
            default_backend: StageBackend::Remote(RemoteEndpointSpec {
                endpoint_env_var: "MONKEYOCRV2_ENDPOINT",
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

        // Stage 1: layout + reading order, one call over the whole page.
        // `get_layout` passes `min_pixels=1003520` and inherits
        // `max_pixels=1003520` from the env, so the page is normalized to
        // ~1.0 Mpx in either direction.
        let layout_img =
            imaging::prepare_model_image(&page_rgb, Some(TARGET_PIXELS), Some(TARGET_PIXELS));
        let layout_data_url = imaging::to_base64_data_url(&layout_img).map_err(|e| PageError {
            page_num: page.page_num,
            message: format!("failed to encode layout image: {e}"),
            stage: Some("layout".into()),
        })?;
        let layout_req = self.request(
            self.stage1_endpoint(),
            LAYOUT_PROMPT,
            &layout_data_url,
            LAYOUT_MAX_TOKENS,
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
                "monkeyocr-v2 page {}: layout response was truncated (finish_reason: length) — the real MonkeyOCRv2 protocol's own `get_layout()` uses a fixed max_tokens=4096 budget (confirmed in core_runner.py), so a dense/table-heavy page can legitimately exceed it; recovered content below may be an incomplete prefix",
                page.page_num
            ));
        }
        let (cells, warnings) = output_parse::parse_python_literal_list(layout_content);
        for w in &warnings {
            ctx.warn(format!("monkeyocr-v2 page {}: {w}", page.page_num));
        }

        let pending: Vec<PendingBlock> = cells
            .iter()
            .map(|cell| {
                let bbox_px =
                    geometry::map_bbox_0to1000_clamped(cell.bbox, page.width, page.height);
                let (category, warning) = category_map::map_monkeyocrv2_category(&cell.label);
                if let Some(w) = warning {
                    ctx.warn(format!("monkeyocr-v2 page {}: {w}", page.page_num));
                }
                PendingBlock {
                    bbox_px,
                    label: cell.label.clone(),
                    category,
                }
            })
            .collect();

        // Stage 2: per-block recognition, concurrent within the page
        // (bounded by the shared document-level permit budget).
        let futures_iter = pending.iter().enumerate().map(|(index, p)| {
            let prompt = stage2_prompt(&p.label);
            async move {
                let Some(prompt) = prompt else {
                    return (index, Ok(None));
                };

                // Crop/resize/encode first (CPU-bound, no network) —
                // acquiring the permit before this point would let this
                // page's image processing consume the document-level
                // concurrency budget without a request in flight,
                // silently reducing real request concurrency below
                // `--max-concurrency`.
                let crop_img = match ctx.crop(page, p.bbox_px) {
                    Ok(img) => img,
                    Err(e) => return (index, Err(e)),
                };
                // Upstream's recognition calls pass **no** `min_pixels`,
                // so only the `max_pixels` upper bound applies: a small
                // crop is sent at its native size. This adapter used to
                // pass `min == max`, Lanczos-upscaling a one-line text
                // crop ~10x before inference — a materially different
                // model input than the reference implementation's.
                let resized = imaging::prepare_model_image(&crop_img, None, Some(TARGET_PIXELS));
                let data_url = match imaging::to_base64_data_url(&resized) {
                    Ok(u) => u,
                    Err(e) => return (index, Err(e)),
                };

                let req = self.request(
                    self.stage2_endpoint(index),
                    prompt,
                    &data_url,
                    RECOGNIZE_MAX_TOKENS,
                );
                let permit = ctx.acquire_permit().await;
                let mut content = match ctx.dispatch(req).await {
                    Ok(resp) => match extract_chat_content(&resp) {
                        Ok(content) => content.to_string(),
                        Err(e) => return (index, Err(e)),
                    },
                    Err(e) => return (index, Err(e.to_string())),
                };
                drop(permit);

                // `_recognize_one_block`'s repeat-retry loop. Upstream
                // gates this behind `retry_repeat` (default off) and
                // applies it to **every** `need_infer` label, tables and
                // formulas included — the detector looks at the output
                // string, and a looping table is as broken as a looping
                // paragraph. This replaces an earlier, locally-invented
                // variant that used `robustness::is_degenerate` and
                // exempted Table/Formula; `robustness.rs` itself is
                // untouched and still used by mineru-vlm.
                if self.retry_repeat {
                    let mut retries = 0;
                    while monkeyocr_post::should_retry_repeat_output(&content)
                        && retries < self.retry_repeat_max_retries
                    {
                        let temperature = (RETRY_TEMPERATURE_STEP * (retries as f64 + 1.0))
                            .min(RETRY_TEMPERATURE_CAP);
                        ctx.warn(format!(
                            "monkeyocr-v2 page {}: block {index} ({}) output looks like a repeat loop — retrying at temperature {temperature} (attempt {})",
                            page.page_num,
                            p.label,
                            retries + 1
                        ));
                        let mut req = self.request(
                            self.stage2_endpoint(index),
                            prompt,
                            &data_url,
                            RECOGNIZE_MAX_TOKENS,
                        );
                        if let Value::Object(m) = &mut req.sampling {
                            m.insert("temperature".to_string(), serde_json::json!(temperature));
                            m.insert("top_p".to_string(), serde_json::json!(RETRY_TOP_P));
                        }
                        let permit = ctx.acquire_permit().await;
                        let outcome = ctx.dispatch(req).await;
                        drop(permit);
                        // Upstream lets a transport failure here
                        // propagate and fail the page; scoped to this
                        // block instead, consistent with how this
                        // adapter already isolates a failed first
                        // attempt. Retry *exhaustion* keeps the last
                        // response, which is upstream's behavior.
                        match outcome {
                            Ok(resp) => match extract_chat_content(&resp) {
                                Ok(c) => content = c.to_string(),
                                Err(e) => return (index, Err(e)),
                            },
                            Err(e) => return (index, Err(e.to_string())),
                        }
                        retries += 1;
                    }
                }

                (index, Ok(Some(content)))
            }
        });
        let mut content_by_index = crate::shape_executor::collect_indexed(futures_iter).await;

        let mut blocks = Vec::with_capacity(pending.len());
        for (index, p) in pending.iter().enumerate() {
            let outcome = content_by_index.remove(&index).unwrap_or(Ok(None));
            let (text, html, latex, error) = match outcome {
                Ok(None) => (None, None, None, None),
                // `_format_block_fields` opens with
                // `content = (raw or "").strip()`, so every branch below
                // sees trimmed input.
                Ok(Some(content)) => match (p.label.as_str(), content.trim()) {
                    // Upstream's own `otsl_to_html`, not the shared
                    // `otsl::to_html` — they disagree on `xcel` (see
                    // `monkeyocr_post`'s module doc). The converted HTML
                    // then goes through `_replace_table_image_markers`,
                    // which resolves any `[img][…][/img]` marker against
                    // the *unresized* table crop — the same image
                    // upstream keeps on the task.
                    ("Table", content) => {
                        let html = monkeyocr_post::otsl_to_html(content);
                        let html = match imaging::crop(&page_rgb, p.bbox_px) {
                            Some(table_crop) => {
                                monkeyocr_post::replace_table_image_markers(&html, &table_crop)
                            }
                            None => html,
                        };
                        (None, Some(html), None, None)
                    }
                    // Upstream's `process_formula` + `$$…$$` wrap, with the
                    // equation label moved outside the math.
                    ("Formula", content) => (
                        None,
                        None,
                        Some(monkeyocr_post::format_formula(content)),
                        None,
                    ),
                    // `result2md` strips U+FFFD from the finished Markdown;
                    // doing it per block keeps `--format json` consistent
                    // with `--format markdown`.
                    (_, content) => (
                        Some(monkeyocr_post::strip_replacement_chars(content)),
                        None,
                        None,
                        None,
                    ),
                },
                Err(e) => (None, None, None, Some(e)),
            };

            // Same shape as mineru-vlm's equivalent: skip calling the
            // model for "Picture" content (real protocol behavior), but
            // still crop-and-preserve the pixels so `render.rs` has
            // something to link to (see `image_link_gap_report.md`).
            let asset_bytes = if p.label == "Picture" {
                imaging::crop(&page_rgb, p.bbox_px).and_then(|img| imaging::to_png_bytes(&img).ok())
            } else {
                None
            };

            // Upstream emits `# ` for `Title` and `## ` for
            // `Section-header`; `category_map` normalizes both to
            // `"title"`, so the level has to be carried explicitly or the
            // renderer flattens every heading to H1.
            let merge_hint = match p.label.as_str() {
                "Title" => Some(MergeHint::TitleLevel(1)),
                "Section-header" => Some(MergeHint::TitleLevel(2)),
                _ => None,
            };

            let [x0, y0, x1, y1] = p.bbox_px;
            blocks.push(Block {
                geom: Geometry::Rect([x0 as f32, y0 as f32, x1 as f32, y1 as f32]),
                geom_frame: CoordFrame::Page,
                bbox_px: Some(p.bbox_px),
                category_raw: p.label.clone(),
                category: Some(p.category.clone()),
                reading_order: Some(index as u32),
                text,
                html,
                latex,
                spans: vec![],
                merge_hint,
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

    #[tokio::test]
    async fn two_stage_orchestration_offline_via_mock_dispatch() {
        let adapter = MonkeyOcrV2Adapter::default();
        let mock = Arc::new(MockDispatch::new());

        // Single-quoted Python literal syntax, exercising the real parser.
        let layout = r#"[{'bbox': [0, 0, 200, 100], 'label': 'Text'}, {'bbox': [0, 200, 200, 400], 'label': 'Table'}, {'bbox': [0, 500, 200, 600], 'label': 'Formula'}, {'bbox': [0, 700, 200, 900], 'label': 'Picture'}]"#;
        mock.seed(&adapter.stage1_endpoint(), chat_response(layout));
        mock.seed(&adapter.stage2_endpoint(0), chat_response("Hello world"));
        mock.seed(
            &adapter.stage2_endpoint(1),
            chat_response("<fcel>a<fcel>b<nl><fcel>c<fcel>d"),
        );
        mock.seed(&adapter.stage2_endpoint(2), chat_response(r"\frac{1}{2"));
        // Deliberately no seed for stage2_endpoint(3) — "Picture" must
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
        // Upstream's `process_formula` has **no** brace repair, so the
        // unbalanced `\frac{1}{2` survives verbatim — confirmed by running
        // `core_runner.py::process_formula` on the same input. The shared
        // `formula_repair::balance_brackets` used to "fix" it here, which
        // was a silent divergence from this protocol's reference output.
        assert_eq!(
            equation_block.latex.as_deref(),
            Some("$$\n\\frac{1}{2\n$$\n")
        );

        let picture_block = &blocks[3];
        assert_eq!(picture_block.category.as_deref(), Some("image"));
        assert!(picture_block.text.is_none());
        assert!(
            picture_block.error.is_none(),
            "Picture must skip stage 2 entirely, not dispatch-and-fail"
        );
        // image_link_gap_report.md: the model is never called for
        // "Picture" content, but the pixels must still be cropped and
        // preserved for `render.rs` to link to.
        let asset_bytes = picture_block
            .asset_bytes
            .as_ref()
            .expect("Picture block must have cropped pixels attached");
        let decoded = image::load_from_memory(asset_bytes).expect("must be valid PNG bytes");
        assert_eq!((decoded.width(), decoded.height()), (80, 200));
    }

    /// The single most consequential alignment fix: upstream's
    /// recognition calls pass no `min_pixels`, so a small crop reaches
    /// the model at its native size. This adapter used to send it
    /// upscaled to ~1.0 Mpx.
    #[tokio::test]
    async fn a_small_block_crop_is_sent_at_its_native_size_not_upscaled() {
        let adapter = MonkeyOcrV2Adapter::default();
        let mock = Arc::new(MockDispatch::new());

        // 400x30 in page space => 12,000 px, far under 1,003,520.
        let layout = r#"[{'bbox': [0, 0, 1000, 30], 'label': 'Text'}]"#;
        mock.seed(&adapter.stage1_endpoint(), chat_response(layout));
        mock.seed(&adapter.stage2_endpoint(0), chat_response("one line"));

        let ctx = ParseCtx::with_mock(Arc::clone(&mock), Arc::new(Semaphore::new(1)));
        let page = fake_page(400, 1000);
        adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds");

        let recorded = mock.recorded_requests(&adapter.stage2_endpoint(0));
        assert_eq!(recorded.len(), 1);
        let url = recorded[0]["messages"][0]["content"][0]["image_url"]["url"]
            .as_str()
            .expect("image_url is a string");
        let b64 = url
            .strip_prefix("data:image/png;base64,")
            .expect("PNG data URL");
        let bytes = base64_decode(b64);
        let sent = image::load_from_memory(&bytes).expect("valid PNG");
        assert_eq!(
            (sent.width(), sent.height()),
            (400, 30),
            "the crop must reach the model unscaled"
        );

        // `_parse_page`'s recognition budget is 5000, not 10000.
        assert_eq!(recorded[0]["sampling"]["max_tokens"], 5000);
        assert_eq!(recorded[0]["sampling"]["temperature"], 0);
    }

    /// Upstream `PipelineConfig.retry_repeat` defaults to `False`, so a
    /// repeat-looking response is accepted as-is unless the caller opts
    /// in. Only one response is seeded, so any extra dispatch would fail
    /// the mock lookup and surface as a block error.
    #[tokio::test]
    async fn repeat_retry_is_off_by_default() {
        let adapter = MonkeyOcrV2Adapter::default();
        assert!(!adapter.retry_repeat);
        let mock = Arc::new(MockDispatch::new());

        let looping = "the cat sat ".repeat(10);
        let layout = r#"[{'bbox': [0, 0, 200, 100], 'label': 'Text'}]"#;
        mock.seed(&adapter.stage1_endpoint(), chat_response(layout));
        mock.seed(&adapter.stage2_endpoint(0), chat_response(&looping));

        let ctx = ParseCtx::with_mock(Arc::clone(&mock), Arc::new(Semaphore::new(1)));
        let blocks = adapter
            .parse_page(&fake_page(400, 1000), &ctx)
            .await
            .expect("parse_page succeeds");
        assert_eq!(blocks[0].text.as_deref(), Some(looping.trim()));
        assert!(blocks[0].error.is_none());
        assert_eq!(mock.recorded_requests(&adapter.stage2_endpoint(0)).len(), 1);
    }

    /// With the upstream flag on, a repeat loop is re-requested at
    /// `min(0.2 * (n + 1), 0.8)` with `top_p = 0.95` — and unlike this
    /// adapter's earlier locally-invented variant, it applies to tables
    /// and formulas too, matching upstream's label-agnostic loop.
    #[tokio::test]
    async fn repeat_retry_when_enabled_escalates_temperature_and_covers_tables() {
        let adapter = MonkeyOcrV2Adapter {
            retry_repeat: true,
            ..MonkeyOcrV2Adapter::default()
        };
        let mock = Arc::new(MockDispatch::new());

        let layout = r#"[{'bbox': [0, 0, 200, 100], 'label': 'Text'}, {'bbox': [0, 200, 200, 400], 'label': 'Table'}]"#;
        mock.seed(&adapter.stage1_endpoint(), chat_response(layout));
        mock.seed(
            &adapter.stage2_endpoint(0),
            chat_response(&"the cat sat ".repeat(10)),
        );
        mock.seed(
            &adapter.stage2_endpoint(0),
            chat_response("a well formed sentence"),
        );
        // A looping *table* must be retried as well.
        mock.seed(
            &adapter.stage2_endpoint(1),
            chat_response(&"<fcel>x".repeat(20)),
        );
        mock.seed(
            &adapter.stage2_endpoint(1),
            chat_response("<fcel>a<fcel>b<nl>"),
        );

        let ctx = ParseCtx::with_mock(Arc::clone(&mock), Arc::new(Semaphore::new(4)));
        let blocks = adapter
            .parse_page(&fake_page(400, 1000), &ctx)
            .await
            .expect("parse_page succeeds");
        assert_eq!(blocks[0].text.as_deref(), Some("a well formed sentence"));
        assert_eq!(
            blocks[1].html.as_deref(),
            Some("<table><tr><td>a</td><td>b</td></tr></table>")
        );

        let text_requests = mock.recorded_requests(&adapter.stage2_endpoint(0));
        assert_eq!(text_requests.len(), 2, "one retry");
        assert_eq!(text_requests[1]["sampling"]["temperature"], 0.2);
        assert_eq!(text_requests[1]["sampling"]["top_p"], 0.95);
        assert_eq!(
            mock.recorded_requests(&adapter.stage2_endpoint(1)).len(),
            2,
            "a looping table is retried too"
        );

        let warnings = ctx.warnings_snapshot();
        assert!(
            warnings.iter().any(|w| w.contains("repeat loop")),
            "{warnings:?}"
        );
    }

    fn base64_decode(s: &str) -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .expect("valid base64")
    }

    #[tokio::test]
    async fn missing_stage1_seed_yields_page_error() {
        let adapter = MonkeyOcrV2Adapter::default();
        let mock = Arc::new(MockDispatch::new());
        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let page = fake_page(100, 100);

        let result = adapter.parse_page(&page, &ctx).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().stage.as_deref(), Some("layout"));
    }

    #[tokio::test]
    async fn malformed_layout_still_recovers_via_tolerant_extraction() {
        let adapter = MonkeyOcrV2Adapter::default();
        let mock = Arc::new(MockDispatch::new());

        // Truncated trailing dict (no closing brace/bracket).
        let malformed = r#"[{"bbox": [0, 0, 200, 100], "label": "Text"}, {"bbox": [0, 200, 200"#;
        mock.seed(&adapter.stage1_endpoint(), chat_response(malformed));

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let page = fake_page(400, 1000);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds");
        assert!(!blocks.is_empty());
        assert_eq!(blocks[0].category.as_deref(), Some("text"));
    }

    #[tokio::test]
    async fn truncated_layout_response_surfaces_a_warning_but_still_recovers_content() {
        // A dense/table-heavy page can legitimately exceed the real
        // protocol's fixed 4096-token layout budget (confirmed in
        // core_runner.py) — `finish_reason: "length"` on a
        // content-bearing response should surface a warning (D.12), not
        // silently return whatever partial structure the tolerant
        // parser could recover with zero indication the cause was
        // truncation rather than malformed output.
        let adapter = MonkeyOcrV2Adapter::default();
        let mock = Arc::new(MockDispatch::new());
        let truncated = serde_json::json!({
            "choices": [{
                "message": {"content": r#"[{"bbox": [0, 0, 200, 100], "label": "Text"}"#},
                "finish_reason": "length",
            }]
        });
        mock.seed(&adapter.stage1_endpoint(), truncated);

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

    /// Upstream emits `# ` for `Title` and `## ` for `Section-header`
    /// (`_format_block_fields`). `category_map` normalizes both native
    /// labels to `"title"`, so without an explicit `TitleLevel` the
    /// renderer flattens a document's whole heading hierarchy to H1 —
    /// which is what this adapter did before the alignment pass.
    #[tokio::test]
    async fn title_and_section_header_carry_distinct_heading_levels() {
        let adapter = MonkeyOcrV2Adapter::default();
        let mock = Arc::new(MockDispatch::new());
        let layout = r#"[{'bbox': [0, 0, 200, 100], 'label': 'Title'}, {'bbox': [0, 200, 200, 300], 'label': 'Section-header'}]"#;
        mock.seed(&adapter.stage1_endpoint(), chat_response(layout));
        mock.seed(&adapter.stage2_endpoint(0), chat_response("Doc Title"));
        mock.seed(&adapter.stage2_endpoint(1), chat_response("A Section"));

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(2)));
        let blocks = adapter
            .parse_page(&fake_page(400, 600), &ctx)
            .await
            .expect("parse_page succeeds");

        assert_eq!(blocks[0].category.as_deref(), Some("title"));
        assert_eq!(blocks[0].merge_hint, Some(MergeHint::TitleLevel(1)));
        assert_eq!(blocks[1].category.as_deref(), Some("title"));
        assert_eq!(
            blocks[1].merge_hint,
            Some(MergeHint::TitleLevel(2)),
            "Section-header is H2 upstream, not H1"
        );
    }

    #[test]
    fn declares_expected_protocol_metadata() {
        let adapter = MonkeyOcrV2Adapter::default();
        assert_eq!(adapter.name(), "monkeyocr-v2");
        assert_eq!(adapter.coordinate_system(), CoordinateSystem::Norm0To1000);
        assert!(adapter.provides_reading_order());
        assert_eq!(
            adapter.raw_output_format(),
            RawOutputFormat::PythonLiteralEval
        );
        let signals = adapter.emitted_signals();
        assert!(!signals.spans && !signals.merge_hint && !signals.font_size);
    }
}
