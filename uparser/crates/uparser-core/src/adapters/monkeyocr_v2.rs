//! MonkeyOCRv2 two-stage protocol adapter, per T-3.4. Protocol details
//! (prompts, resize strategy, Python-literal output, bbox mapping) are
//! confirmed against the fully vendored `opensource/MonkeyOCRv2` source
//! (`parsing/core_runner.py`), no version-mismatch caveat.
//!
//! Exists to prove Gate G3: a third protocol reuses `otsl.rs` (table
//! HTML conversion) and `formula_repair.rs` (LaTeX cleanup) **verbatim**
//! — this file only imports their existing public functions, adding
//! nothing new to either module.
//!
//! Document dewarming preprocessing (an independent local-torch model in
//! the real pipeline, unrelated to the parsing VLM) is deliberately
//! **not** implemented here, per the P3 plan's T-3.5 decision (option a:
//! skip) — this adapter does not correct skewed/photographed input.

use super::{
    ModelStage, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat, RemoteEndpointSpec,
    ResourceHint, StageBackend,
};
use crate::category_map::{self, MONKEYOCR_V2_CATEGORIES};
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

const TARGET_PIXELS: u32 = 1_003_520;

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
}

impl Default for MonkeyOcrV2Adapter {
    fn default() -> Self {
        Self {
            endpoint_base: "http://localhost:8888/v1/chat/completions".to_string(),
            model: "monkeyocrv2".to_string(),
            timeout: Duration::from_secs(120),
            max_retries: 2,
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

fn extract_content(resp: &Value) -> Option<&str> {
    resp["choices"][0]["message"]["content"].as_str()
}

fn wrap_display_math(latex: &str) -> String {
    format!("$$\n{}\n$$", latex.trim())
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
        let layout_img = imaging::resize_by_pixel_bounds(&page_rgb, TARGET_PIXELS, TARGET_PIXELS);
        let layout_data_url = imaging::to_base64_data_url(&layout_img).map_err(|e| PageError {
            page_num: page.page_num,
            message: format!("failed to encode layout image: {e}"),
            stage: Some("layout".into()),
        })?;
        let layout_req = self.request(
            self.stage1_endpoint(),
            LAYOUT_PROMPT,
            &layout_data_url,
            4096,
        );
        let layout_resp = ctx.dispatch(layout_req).await.map_err(|e| PageError {
            page_num: page.page_num,
            message: e.to_string(),
            stage: Some("layout".into()),
        })?;
        let layout_content = extract_content(&layout_resp).unwrap_or("");
        let (cells, warnings) = output_parse::parse_python_literal_list(layout_content);
        for w in &warnings {
            eprintln!("monkeyocr-v2 page {}: {w}", page.page_num);
        }

        let pending: Vec<PendingBlock> = cells
            .iter()
            .map(|cell| {
                let bbox_px =
                    geometry::map_bbox_0to1000_clamped(cell.bbox, page.width, page.height);
                let (category, warning) = category_map::map_monkeyocrv2_category(&cell.label);
                if let Some(w) = warning {
                    eprintln!("monkeyocr-v2 page {}: {w}", page.page_num);
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

                let _permit = ctx.acquire_permit().await;
                let crop_img = match ctx.crop(page, p.bbox_px) {
                    Ok(img) => img,
                    Err(e) => return (index, Err(e)),
                };
                let resized =
                    imaging::resize_by_pixel_bounds(&crop_img, TARGET_PIXELS, TARGET_PIXELS);
                let data_url = match imaging::to_base64_data_url(&resized) {
                    Ok(u) => u,
                    Err(e) => return (index, Err(e)),
                };

                let req = self.request(self.stage2_endpoint(index), prompt, &data_url, 10000);
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
                Ok(Some(content)) => match p.label.as_str() {
                    "Table" => (None, Some(otsl::to_html(&content)), None, None),
                    "Formula" => {
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
                category_raw: p.label.clone(),
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

        // Gate G3 proof: this exercises the *unmodified* shared
        // formula_repair::DEFAULT_CHAIN (balance_brackets closes the
        // unbalanced brace) and wraps it in MonkeyOCRv2's own `$$...$$`
        // delimiter — no new formula logic was added anywhere.
        let equation_block = &blocks[2];
        assert_eq!(equation_block.category.as_deref(), Some("equation"));
        assert_eq!(
            equation_block.latex.as_deref(),
            Some("$$\n\\frac{1}{2}\n$$")
        );

        let picture_block = &blocks[3];
        assert_eq!(picture_block.category.as_deref(), Some("image"));
        assert!(picture_block.text.is_none());
        assert!(
            picture_block.error.is_none(),
            "Picture must skip stage 2 entirely, not dispatch-and-fail"
        );
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
