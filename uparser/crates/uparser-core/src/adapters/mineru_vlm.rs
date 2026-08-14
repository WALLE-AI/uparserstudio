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
    ResourceHint, StageBackend, extract_chat_content,
};
use crate::category_map::{self, MINERU_VLM_CATEGORIES};
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
use futures::future::join_all;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

const LAYOUT_IMAGE_SIZE: u32 = 1036;
const MAX_EDGE_RATIO: f32 = 50.0;
const MIN_EDGE: u32 = 28;
const IOU_DEDUPE_THRESHOLD: f32 = 0.8;
const TABLE_INTERNAL_COVERAGE_THRESHOLD: f32 = 0.9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MineruVlmProfile {
    Enhanced,
    Official,
    Surpass,
}

/// Native categories for which stage 2 (content extraction) is skipped
/// entirely. `image` pixels are preserved as an asset crop instead of sent to
/// the model. `list`/`equation_block` are the *container* boxes; their items
/// are emitted as their own `list_item`/`equation` boxes.
///
/// NOTE: recognizing `list` (F3) and `equation_block` (F4) content was tried
/// and REVERTED. On a stratified dev set F4 looked net-positive (formula ↓,
/// table ↑), but that set over-represented equation/table pages — on the full
/// OmniDocBench distribution both regressed the (text-weighted) headline: Text
/// Edit +0.0028, Reading Order +0.0043, outweighing the formula gain, because
/// the extra recovered blocks displace text matches and perturb reading order
/// under the scorer. See MINERU_VLM_OPTIMIZATION_PLAN.md appendix A.
const SKIP_CONTENT: &[&str] = &["image", "list", "equation_block"];

pub struct MineruVlmAdapter {
    /// Base chat-completions endpoint. Not reachable yet from the CLI
    /// (no `--endpoint` flag wired in this pass) — present so the
    /// request-building logic is complete and ready for that wiring.
    pub endpoint_base: String,
    pub model: String,
    pub timeout: Duration,
    pub max_retries: u32,
    pub profile: MineruVlmProfile,
    pub trace_dir: Option<PathBuf>,
}

impl Default for MineruVlmAdapter {
    fn default() -> Self {
        Self {
            endpoint_base: "http://localhost:8000/v1/chat/completions".to_string(),
            model: "mineru-vlm".to_string(),
            timeout: Duration::from_secs(60),
            max_retries: 2,
            profile: MineruVlmProfile::Enhanced,
            trace_dir: None,
        }
    }
}

impl MineruVlmAdapter {
    pub fn official() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            max_retries: 3,
            profile: MineruVlmProfile::Official,
            ..Self::default()
        }
    }

    pub fn surpass() -> Self {
        Self {
            profile: MineruVlmProfile::Surpass,
            ..Self::official()
        }
    }

    fn official_compatible(&self) -> bool {
        matches!(
            self.profile,
            MineruVlmProfile::Official | MineruVlmProfile::Surpass
        )
    }

    fn stage1_endpoint(&self) -> String {
        format!("{}#stage1", self.endpoint_base)
    }

    fn stage2_endpoint(&self, block_index: usize) -> String {
        format!("{}#stage2:{block_index}", self.endpoint_base)
    }

    fn write_trace(&self, ctx: &ParseCtx, page_num: u32, name: &str, value: &Value) {
        let Some(dir) = &self.trace_dir else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(dir) {
            ctx.warn(format!(
                "mineru-vlm page {page_num}: failed to create trace directory {}: {error}",
                dir.display()
            ));
            return;
        }
        let path = dir.join(format!("page-{page_num:04}-{name}.json"));
        let result = serde_json::to_vec_pretty(value)
            .map_err(|error| error.to_string())
            .and_then(|bytes| std::fs::write(&path, bytes).map_err(|error| error.to_string()));
        if let Err(error) = result {
            ctx.warn(format!(
                "mineru-vlm page {page_num}: failed to write trace {}: {error}",
                path.display()
            ));
        }
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

    /// Shared greedy-decoding base params for BOTH stages, matching
    /// `mineru_vl_utils`' `MinerUSamplingParams` defaults
    /// (temperature=0.0, top_p=0.01, top_k=1, no_repeat_ngram_size=100).
    /// `skip_special_tokens: false` is required — vLLM's OpenAI-compatible
    /// endpoint defaults to stripping special tokens, which would eat the
    /// `<|box_start|>`/`<|ref_start|>` wrapper tokens the custom_token grammar
    /// depends on.
    ///
    /// Stage-2 previously set ONLY the per-type presence/frequency penalties
    /// and omitted these greedy params, so content recognition silently ran at
    /// the server's *default* (non-greedy) sampling — a real fidelity gap vs.
    /// MinerU, whose stage-2 inherits exactly these base defaults. Sharing one
    /// base here keeps stage-1/stage-2 from drifting apart.
    fn base_greedy_sampling() -> serde_json::Map<String, Value> {
        serde_json::json!({
            "temperature": 0.0,
            "top_p": 0.01,
            "top_k": 1,
            "repetition_penalty": 1.0,
            "vllm_xargs": {
                "no_repeat_ngram_size": 100,
                "debug": false
            },
            "skip_special_tokens": false,
        })
        .as_object()
        .expect("literal is an object")
        .clone()
    }

    fn layout_sampling() -> Value {
        Value::Object(Self::base_greedy_sampling())
    }

    /// Per-category stage-2 prompt/sampling. Prompts + penalties confirmed from
    /// `mineru_vl_utils`' `DEFAULT_PROMPTS`/`DEFAULT_SAMPLING_PARAMS`; the greedy
    /// base is shared with stage-1 via [`Self::base_greedy_sampling`].
    fn stage2_prompt_and_sampling(category_raw: &str) -> (&'static str, Value) {
        let (prompt, presence_penalty, frequency_penalty) = match category_raw {
            "table" => ("\nTable Recognition:", 1.0, 0.005),
            "equation" => ("\nFormula Recognition:", 1.0, 0.05),
            _ => ("\nText Recognition:", 1.0, 0.05),
        };
        let mut sampling = Self::base_greedy_sampling();
        sampling.insert(
            "presence_penalty".to_string(),
            serde_json::json!(presence_penalty),
        );
        sampling.insert(
            "frequency_penalty".to_string(),
            serde_json::json!(frequency_penalty),
        );
        (prompt, Value::Object(sampling))
    }

    fn normalize_official_category(raw: &str) -> Option<(String, String)> {
        let raw = match raw {
            "unknown" => "image",
            "inline_formula" => return None,
            value => value,
        };
        let known = matches!(
            raw,
            "text"
                | "title"
                | "table"
                | "equation"
                | "formula_number"
                | "code"
                | "algorithm"
                | "aside_text"
                | "ref_text"
                | "index"
                | "phonetic"
                | "list_item"
                | "table_caption"
                | "image_caption"
                | "code_caption"
                | "table_footnote"
                | "image_footnote"
                | "header"
                | "footer"
                | "page_number"
                | "page_footnote"
                | "image"
                | "chart"
                | "list"
                | "image_block"
                | "equation_block"
        );
        if !known {
            return None;
        }
        let (category, _) = category_map::map_mineru_vlm_category(raw);
        let category = match raw {
            "formula_number" | "index" => "text".to_string(),
            "chart" | "image_block" => "image".to_string(),
            _ => category,
        };
        Some((raw.to_string(), category))
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
        match self.profile {
            MineruVlmProfile::Enhanced => "mineru-vlm",
            MineruVlmProfile::Official => "mineru-vlm-official",
            MineruVlmProfile::Surpass => "mineru-vlm-surpass",
        }
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
        let layout_content = extract_chat_content(&layout_resp).map_err(|e| PageError {
            page_num: page.page_num,
            message: e,
            stage: Some("layout".into()),
        })?;
        let (layout_boxes, warnings) = match self.profile {
            MineruVlmProfile::Enhanced => output_parse::parse_custom_tokens(layout_content),
            MineruVlmProfile::Official | MineruVlmProfile::Surpass => {
                output_parse::parse_custom_tokens_official(layout_content)
            }
        };
        self.write_trace(
            ctx,
            page.page_num,
            "stage1",
            &serde_json::json!({
                "profile": self.name(),
                "model": self.model,
                "prompt": "\nLayout Detection:",
                "sampling": Self::layout_sampling(),
                "raw_content": layout_content,
                "parsed_boxes": layout_boxes.iter().map(|layout_box| serde_json::json!({
                    "bbox_1000": layout_box.bbox_1000,
                    "category_raw": layout_box.category_raw,
                    "angle": layout_box.angle,
                })).collect::<Vec<_>>(),
                "warnings": warnings,
            }),
        );
        for w in &warnings {
            ctx.warn(format!("mineru-vlm page {}: {w}", page.page_num));
        }

        // Denormalize + category-map, then dedupe near-identical boxes.
        let mut pending: Vec<PendingBlock> = Vec::with_capacity(layout_boxes.len());
        for lb in &layout_boxes {
            let bbox_px = geometry::denormalize_0to1000_bbox(lb.bbox_1000, page.width, page.height);
            let mapped = if self.official_compatible() {
                Self::normalize_official_category(&lb.category_raw)
            } else {
                let (category, warning) = category_map::map_mineru_vlm_category(&lb.category_raw);
                if let Some(w) = warning {
                    ctx.warn(format!("mineru-vlm page {}: {w}", page.page_num));
                }
                Some((lb.category_raw.clone(), category))
            };
            let Some((category_raw, category)) = mapped else {
                ctx.warn(format!(
                    "mineru-vlm page {}: official parser skipped category {:?}",
                    page.page_num, lb.category_raw
                ));
                continue;
            };
            pending.push(PendingBlock {
                bbox_px,
                category_raw,
                category,
                angle: lb.angle,
            });
        }

        let pending = if self.profile == MineruVlmProfile::Enhanced {
            let bboxes: Vec<[i32; 4]> = pending.iter().map(|p| p.bbox_px).collect();
            geometry::dedupe_by_iou(&bboxes, IOU_DEDUPE_THRESHOLD)
                .into_iter()
                .map(|i| {
                    let p = &pending[i];
                    PendingBlock {
                        bbox_px: p.bbox_px,
                        category_raw: p.category_raw.clone(),
                        category: p.category.clone(),
                        angle: p.angle,
                    }
                })
                .collect()
        } else {
            pending
        };
        let table_bboxes: Vec<[i32; 4]> = pending
            .iter()
            .filter(|p| p.category_raw == "table")
            .map(|p| p.bbox_px)
            .collect();
        let pending: Vec<PendingBlock> = pending
            .into_iter()
            .filter(|p| {
                let is_table_internal_candidate = matches!(
                    p.category_raw.as_str(),
                    "text" | "equation" | "equation_block"
                );
                !is_table_internal_candidate
                    || !table_bboxes.iter().any(|&table_bbox| {
                        geometry::coverage_ratio(p.bbox_px, table_bbox)
                            >= TABLE_INTERNAL_COVERAGE_THRESHOLD
                    })
            })
            .collect();

        // Stage 2: per-block content extraction, concurrent within the
        // page (bounded by the shared document-level permit budget).
        let futures_iter = pending.iter().enumerate().map(|(index, p)| {
            let skip = if self.official_compatible() {
                matches!(
                    p.category_raw.as_str(),
                    "image" | "chart" | "list" | "equation_block" | "image_block"
                )
            } else {
                SKIP_CONTENT.contains(&p.category_raw.as_str())
            };
            async move {
                if skip {
                    return (index, Ok(None));
                }

                // Crop/rotate/resize/encode first (CPU-bound, no
                // network) — the permit is meant to bound concurrent
                // *network dispatches*, not this work; acquiring it
                // before this point would let this page's block-level
                // image processing consume the document-level
                // concurrency budget without a single request in
                // flight, silently reducing real request concurrency
                // below `--max-concurrency`.
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
                let req = self.request(self.stage2_endpoint(index), prompt, &data_url, sampling.clone());
                let _permit = ctx.acquire_permit().await;
                let content = match ctx.dispatch(req).await {
                    Ok(resp) => match extract_chat_content(&resp) {
                        Ok(content) => content.to_string(),
                        Err(e) => return (index, Err(e)),
                    },
                    Err(e) => return (index, Err(e.to_string())),
                };
                self.write_trace(
                    ctx,
                    page.page_num,
                    &format!("stage2-{index:04}"),
                    &serde_json::json!({
                        "block_index": index,
                        "bbox_px": p.bbox_px,
                        "category_raw": p.category_raw,
                        "angle": p.angle,
                        "prompt": prompt,
                        "sampling": sampling,
                        "raw_content": content,
                    }),
                );

                // Robustness: a "the model gets stuck looping a phrase"
                // degenerate response is a real, known VLM failure mode
                // this project's own `robustness.rs` was built to catch
                // (per T-1.7) but had never been wired into any adapter
                // since P1 — only applied to plain free-text content,
                // not table/equation, which need structural fidelity
                // rather than a "looks repetitive" heuristic, and only
                // *after* a real successful dispatch (a connectivity
                // failure is never masked as "degenerate empty content"
                // — it already returned above via the `Err` arms).
                let is_plain_text = !matches!(p.category_raw.as_str(), "table" | "equation");
                let content = if self.profile == MineruVlmProfile::Enhanced
                    && is_plain_text
                    && robustness::is_degenerate(&content)
                {
                    ctx.warn(format!(
                        "mineru-vlm page {}: stage-2 content for block {index} ({}) looks degenerate (repetitive loop) — retrying with escalating temperature",
                        page.page_num, p.category_raw
                    ));
                    let policy = robustness::RetryPolicy::default();
                    let base_temp = sampling["temperature"].as_f64().unwrap_or(0.0) as f32;
                    let first_content = content.clone();
                    let attempt_no = std::sync::atomic::AtomicU32::new(0);
                    robustness::retry_with_temperature(&policy, base_temp, |temp| {
                        let n = attempt_no.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let seed = if n == 0 {
                            Some(first_content.clone())
                        } else {
                            None
                        };
                        let mut s = sampling.clone();
                        if let Value::Object(m) = &mut s {
                            m.insert("temperature".to_string(), serde_json::json!(temp));
                        }
                        let req = self.request(self.stage2_endpoint(index), prompt, &data_url, s);
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
        let stage2_results = join_all(futures_iter).await;

        let mut content_by_index: std::collections::HashMap<usize, Result<Option<String>, String>> =
            stage2_results.into_iter().collect();

        let mut blocks = Vec::with_capacity(pending.len());
        for (index, p) in pending.iter().enumerate() {
            let outcome = content_by_index.remove(&index).unwrap_or(Ok(None));
            let (text, html, latex, error) = match outcome {
                Ok(None) => (None, None, None, None),
                Ok(Some(content)) => match p.category_raw.as_str() {
                    "table" => {
                        let (html, warnings) = otsl::to_html(&content);
                        for w in &warnings {
                            ctx.warn(format!("mineru-vlm page {}: {w}", page.page_num));
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

            // mineru-vlm's SKIP_CONTENT correctly skips calling the
            // model for "image" content (that's a real, confirmed
            // property of the protocol), but crop-and-preserve is a
            // separate decision MinerU makes unconditionally for image
            // spans — do that here so `render.rs` has pixels to link to
            // (see `image_link_gap_report.md`).
            let asset_bytes = if p.category_raw == "image" {
                imaging::crop(&page_rgb, p.bbox_px).and_then(|img| imaging::to_png_bytes(&img).ok())
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
                reading_order: None,
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

        // NOTE: an adapter-scoped geometric line→paragraph merge (N1) was tried
        // here and REVERTED — it regressed OmniDocBench Text Edit (+0.006) and
        // Reading Order (+0.008) on a 200-page dev set. The scorer already
        // re-joins under-segmented pred lines (its `deal_with_truncated`) but
        // penalizes over-merge (a wrongly-absorbed GT paragraph goes unmatched
        // at edit=1.0), so geometric merging can only lose here. See
        // MINERU_VLM_OPTIMIZATION_PLAN.md appendix A.
        self.write_trace(
            ctx,
            page.page_num,
            "final",
            &serde_json::json!({
                "profile": self.name(),
                "blocks": blocks,
            }),
        );
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
        // D.13-adjacent (image_link_gap_report.md): the model is never
        // called for "image" content, but the pixels must still be
        // cropped and preserved — otherwise `render.rs` has nothing to
        // link to.
        let asset_bytes = image_block
            .asset_bytes
            .as_ref()
            .expect("image block must have cropped pixels attached");
        let decoded = image::load_from_memory(asset_bytes).expect("must be valid PNG bytes");
        assert_eq!((decoded.width(), decoded.height()), (80, 200));
    }

    #[tokio::test]
    async fn degenerate_stage2_content_retries_with_escalating_temperature() {
        // Wires `robustness.rs` (T-1.7, never exercised by any adapter
        // before this) into real use: a stage-2 response that loops a
        // short phrase should trigger a retry at a higher temperature
        // rather than being accepted as-is.
        let adapter = MineruVlmAdapter::default();
        let mock = Arc::new(MockDispatch::new());

        let layout = "<|box_start|>0 0 200 100<|box_end|><|ref_start|>text<|ref_end|>".to_string();
        mock.seed(&adapter.stage1_endpoint(), chat_response(&layout));
        // First stage-2 attempt: degenerate (repetitive loop).
        mock.seed(
            &adapter.stage2_endpoint(0),
            chat_response("loop loop loop loop loop loop loop loop loop loop "),
        );
        // Second attempt (after escalating temperature): a real result.
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
    async fn non_degenerate_stage2_content_dispatches_exactly_once() {
        // The common case: a normal response must not trigger any
        // extra dispatch — proves the robustness wiring doesn't add
        // overhead/latency to the already-working path.
        let adapter = MineruVlmAdapter::default();
        let mock = Arc::new(MockDispatch::new());

        let layout = "<|box_start|>0 0 200 100<|box_end|><|ref_start|>text<|ref_end|>".to_string();
        mock.seed(&adapter.stage1_endpoint(), chat_response(&layout));
        mock.seed(&adapter.stage2_endpoint(0), chat_response("Hello world"));
        // Deliberately only one seed — a second dispatch attempt would
        // find no seed and fail the mock lookup, failing this test.

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(4)));
        let page = fake_page(400, 1000);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds");
        assert_eq!(blocks[0].text.as_deref(), Some("Hello world"));
        assert!(ctx.warnings_snapshot().is_empty());
    }

    #[tokio::test]
    async fn trace_dir_records_stage_inputs_raw_outputs_and_final_blocks() {
        let trace_dir = tempfile::tempdir().unwrap();
        let mut adapter = MineruVlmAdapter::official();
        adapter.trace_dir = Some(trace_dir.path().to_path_buf());
        let mock = Arc::new(MockDispatch::new());
        mock.seed(
            &adapter.stage1_endpoint(),
            chat_response(
                "<|box_start|>0 0 200 100<|box_end|><|ref_start|>text<|ref_end|><|rotate_up|>",
            ),
        );
        mock.seed(&adapter.stage2_endpoint(0), chat_response("trace me"));

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(2)));
        adapter
            .parse_page(&fake_page(400, 1000), &ctx)
            .await
            .unwrap();

        let stage1: Value = serde_json::from_slice(
            &std::fs::read(trace_dir.path().join("page-0001-stage1.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(stage1["parsed_boxes"][0]["category_raw"], "text");
        assert_eq!(
            stage1["sampling"]["vllm_xargs"]["no_repeat_ngram_size"],
            100
        );

        let stage2: Value = serde_json::from_slice(
            &std::fs::read(trace_dir.path().join("page-0001-stage2-0000.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(stage2["raw_content"], "trace me");
        assert_eq!(stage2["bbox_px"], serde_json::json!([0, 0, 80, 100]));

        let final_trace: Value = serde_json::from_slice(
            &std::fs::read(trace_dir.path().join("page-0001-final.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(final_trace["blocks"][0]["text"], "trace me");
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

    #[test]
    fn no_repeat_ngram_is_sent_through_vllm_xargs() {
        let sampling = MineruVlmAdapter::layout_sampling();
        assert_eq!(sampling["vllm_xargs"]["no_repeat_ngram_size"], 100);
        assert_eq!(sampling["vllm_xargs"]["debug"], false);
        assert!(sampling.get("no_repeat_ngram_size").is_none());

        let (_, stage2) = MineruVlmAdapter::stage2_prompt_and_sampling("text");
        assert_eq!(stage2["vllm_xargs"]["no_repeat_ngram_size"], 100);
        assert!(stage2.get("no_repeat_ngram_size").is_none());
    }
}
