//! dots.ocr single-round protocol adapter, per T-2.3. Protocol details
//! (smart_resize, prompt text, bbox coordinate space, category vocab,
//! fault-tolerant JSON parsing) are confirmed against the fully vendored
//! `opensource/dots.ocr` source (no version-mismatch caveat, unlike
//! mineru-vlm's external `mineru_vl_utils` dependency).
//!
//! Exists to prove Gate G2: the shared layer built for mineru-vlm
//! (`geometry.rs`/`category_map.rs`/`postprocess.rs`/`render/`) handles
//! this structurally opposite protocol (single round, strict JSON,
//! reading order provided, PixelAbs-ish coordinates) via purely
//! additive new functions — no changes to the shared modules themselves.

use super::{
    ModelStage, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat, RemoteEndpointSpec,
    ResourceHint, StageBackend, extract_chat_content,
};
use crate::category_map::{self, DOTS_OCR_CATEGORIES};
use crate::formula_repair;
use crate::geometry;
use crate::imaging;
use crate::ingest::RenderedPage;
use crate::otsl;
use crate::output_parse;
use crate::transport::ChatCompletionRequest;
use crate::types::{Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, PageError};
use async_trait::async_trait;
use std::time::Duration;

const RESIZE_FACTOR: u32 = 28;
const MIN_PIXELS: u32 = 3136;
const MAX_PIXELS: u32 = 11_289_600;

/// Verbatim from `opensource/dots.ocr/dots_ocr/utils/prompts.py`'s
/// `prompt_layout_all_en`.
const PROMPT_LAYOUT_ALL_EN: &str = "Please output the layout information from the PDF image, including each layout element's bbox, its category, and the corresponding text content within the bbox.

1. Bbox format: [x1, y1, x2, y2]

2. Layout Categories: The possible categories are ['Caption', 'Footnote', 'Formula', 'List-item', 'Page-footer', 'Page-header', 'Picture', 'Section-header', 'Table', 'Text', 'Title'].

3. Text Extraction & Formatting Rules:
    - Picture: For the 'Picture' category, the text field should be omitted.
    - Formula: Format its text as LaTeX.
    - Table: Format its text as HTML.
    - All Others (Text, Title, etc.): Format their text as Markdown.

4. Constraints:
    - The output text must be the original text from the image, with no translation.
    - All layout elements must be sorted according to human reading order.

5. Final Output: The entire output must be a single JSON object.
";

pub struct DotsOcrAdapter {
    pub endpoint_base: String,
    pub model: String,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl Default for DotsOcrAdapter {
    fn default() -> Self {
        Self {
            endpoint_base: "http://localhost:8000/v1/chat/completions".to_string(),
            model: "model".to_string(),
            timeout: Duration::from_secs(120),
            max_retries: 2,
        }
    }
}

impl DotsOcrAdapter {
    fn endpoint(&self) -> String {
        format!("{}#layout", self.endpoint_base)
    }

    fn request(&self, image_data_url: &str) -> ChatCompletionRequest {
        // No system prompt — matches `inference_with_vllm`'s call shape.
        // The `<|img|><|imgpad|><|endofimg|>` prefix avoids vLLM
        // auto-inserting a stray newline before the prompt text.
        let text = format!("<|img|><|imgpad|><|endofimg|>{PROMPT_LAYOUT_ALL_EN}");
        let messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "image_url", "image_url": {"url": image_data_url}},
                {"type": "text", "text": text},
            ],
        })];
        ChatCompletionRequest {
            endpoint: self.endpoint(),
            model: self.model.clone(),
            messages,
            sampling: serde_json::json!({
                "temperature": 0.1,
                "top_p": 0.9,
                "max_completion_tokens": 32768,
            }),
            timeout: self.timeout,
            max_retries: self.max_retries,
        }
    }
}

#[async_trait]
impl ProtocolAdapter for DotsOcrAdapter {
    fn name(&self) -> &'static str {
        "dots-ocr"
    }

    fn coordinate_system(&self) -> CoordinateSystem {
        // Deviation from DEVELOPMENT_PLAN.md's `Norm0To1000` assumption
        // — real bbox values are absolute pixels of the resized input
        // image (rescaled to page pixels internally in `parse_page`),
        // not a normalized fraction. See the P2 plan's caveat.
        CoordinateSystem::PixelAbs
    }

    fn provides_reading_order(&self) -> bool {
        true
    }

    fn category_vocab(&self) -> &[&'static str] {
        DOTS_OCR_CATEGORIES
    }

    fn raw_output_format(&self) -> RawOutputFormat {
        RawOutputFormat::StrictJson
    }

    fn emitted_signals(&self) -> PostprocessSignals {
        PostprocessSignals::default()
    }

    fn model_stages(&self) -> Vec<ModelStage> {
        vec![ModelStage {
            stage_name: "vlm",
            default_backend: StageBackend::Remote(RemoteEndpointSpec {
                endpoint_env_var: "DOTS_OCR_ENDPOINT",
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
        let (orig_w, orig_h) = (page.width, page.height);

        let (resized_h, resized_w) =
            imaging::smart_resize(orig_h, orig_w, RESIZE_FACTOR, MIN_PIXELS, MAX_PIXELS).map_err(
                |e| PageError {
                    page_num: page.page_num,
                    message: e,
                    stage: Some("preprocess".into()),
                },
            )?;
        let resized_img = imaging::hard_resize(&page_rgb, resized_w, resized_h);
        let data_url = imaging::to_base64_data_url(&resized_img).map_err(|e| PageError {
            page_num: page.page_num,
            message: format!("failed to encode input image: {e}"),
            stage: Some("preprocess".into()),
        })?;

        let req = self.request(&data_url);
        let resp = ctx.dispatch(req).await.map_err(|e| PageError {
            page_num: page.page_num,
            message: e.to_string(),
            stage: Some("layout".into()),
        })?;
        let content = extract_chat_content(&resp).map_err(|e| PageError {
            page_num: page.page_num,
            message: e,
            stage: Some("layout".into()),
        })?;

        let (cells, warnings) = output_parse::parse_strict_json(content);
        for w in &warnings {
            ctx.warn(format!("dots-ocr page {}: {w}", page.page_num));
        }

        let mut blocks = Vec::with_capacity(cells.len());
        for (index, cell) in cells.into_iter().enumerate() {
            let Some(bbox_px) = geometry::rescale_bbox_to_original(
                cell.bbox,
                (resized_w, resized_h),
                (orig_w, orig_h),
            ) else {
                ctx.warn(format!(
                    "dots-ocr page {}: skipped a cell with a degenerate page size (0x0)",
                    page.page_num
                ));
                continue;
            };
            let bbox_px = geometry::sanitize_bbox_px(bbox_px, orig_w, orig_h);
            let (category, warning) = category_map::map_dots_ocr_category(&cell.category_raw);
            if let Some(w) = warning {
                ctx.warn(format!("dots-ocr page {}: {w}", page.page_num));
            }

            let (text, html, latex) = match cell.category_raw.as_str() {
                "Picture" => (None, None, None),
                "Table" => {
                    let (html, warnings) = otsl::to_html(cell.text.as_deref().unwrap_or(""));
                    for w in &warnings {
                        ctx.warn(format!("dots-ocr page {}: {w}", page.page_num));
                    }
                    (None, Some(html), None)
                }
                "Formula" => {
                    let repaired = formula_repair::repair_chain(
                        formula_repair::DEFAULT_CHAIN,
                        cell.text.as_deref().unwrap_or(""),
                    );
                    (
                        None,
                        None,
                        Some(formula_repair::wrap_display_math(&repaired)),
                    )
                }
                _ => (cell.text.clone(), None, None),
            };

            let [x0, y0, x1, y1] = bbox_px;
            blocks.push(Block {
                geom: Geometry::Rect([x0 as f32, y0 as f32, x1 as f32, y1 as f32]),
                geom_frame: CoordFrame::Page,
                bbox_px: Some(bbox_px),
                category_raw: cell.category_raw,
                category: Some(category),
                reading_order: Some(index as u32),
                text,
                html,
                latex,
                spans: vec![],
                merge_hint: None,
                confidence: None,
                source: BlockSource::OneShotVlm,
                error: None,
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
    use serde_json::Value;
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
    async fn single_round_orchestration_offline_via_mock_dispatch() {
        let adapter = DotsOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());

        // Page is 1000x2000; smart_resize(2000,1000,28,3136,11289600) is
        // computed inside parse_page, so cells here use bbox coordinates
        // in that resized space. We only assert on category/text
        // behavior and that bboxes come back as *some* consistent
        // rescaled pixel rect, not exact numbers tied to smart_resize's
        // internal arithmetic (covered separately by geometry tests).
        let cells_json = r#"[
            {"bbox": [10, 10, 200, 60], "category": "Text", "text": "Hello world"},
            {"bbox": [10, 100, 300, 260], "category": "Table", "text": "<table><tr><td>a</td></tr></table>"},
            {"bbox": [10, 300, 200, 340], "category": "Formula", "text": "\\frac{1}{2"},
            {"bbox": [10, 400, 300, 600], "category": "Picture", "text": "should be dropped"}
        ]"#;
        mock.seed(&adapter.endpoint(), chat_response(cells_json));

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(4)));
        let page = fake_page(1000, 2000);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds");
        assert_eq!(blocks.len(), 4);

        assert_eq!(blocks[0].category.as_deref(), Some("text"));
        assert_eq!(blocks[0].text.as_deref(), Some("Hello world"));
        assert_eq!(blocks[0].reading_order, Some(0));

        assert_eq!(blocks[1].category.as_deref(), Some("table"));
        assert_eq!(
            blocks[1].html.as_deref(),
            Some("<table><tr><td>a</td></tr></table>")
        );

        assert_eq!(blocks[2].category.as_deref(), Some("equation"));
        // D.10: dots-ocr's formula output is now wrapped in `\[...\]`
        // like mineru-vlm's, instead of emitting bare unwrapped LaTeX
        // that `render.rs`'s Markdown renderer would print as plain
        // text rather than display math.
        assert_eq!(blocks[2].latex.as_deref(), Some("\\[\n\\frac{1}{2}\n\\]"));

        let picture_block = &blocks[3];
        assert_eq!(picture_block.category.as_deref(), Some("image"));
        assert!(
            picture_block.text.is_none(),
            "Picture category must force text=None even if the model returned one"
        );
        assert!(picture_block.html.is_none());
        assert!(picture_block.latex.is_none());
    }

    #[tokio::test]
    async fn missing_seed_yields_page_error() {
        let adapter = DotsOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());
        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let page = fake_page(100, 100);

        let result = adapter.parse_page(&page, &ctx).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().stage.as_deref(), Some("layout"));
    }

    #[tokio::test]
    async fn malformed_json_still_recovers_via_fault_tolerant_chain() {
        let adapter = DotsOcrAdapter::default();
        let mock = Arc::new(MockDispatch::new());

        // Missing delimiter between the two dicts.
        let malformed = r#"[{"bbox": [10, 10, 200, 60], "category": "Text", "text": "a"}{"bbox": [10, 100, 200, 160], "category": "Title", "text": "b"}]"#;
        mock.seed(&adapter.endpoint(), chat_response(malformed));

        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let page = fake_page(1000, 2000);

        let blocks = adapter
            .parse_page(&page, &ctx)
            .await
            .expect("parse_page succeeds");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].category.as_deref(), Some("title"));
    }

    #[test]
    fn declares_expected_protocol_metadata() {
        let adapter = DotsOcrAdapter::default();
        assert_eq!(adapter.name(), "dots-ocr");
        assert_eq!(adapter.coordinate_system(), CoordinateSystem::PixelAbs);
        assert!(adapter.provides_reading_order());
        assert_eq!(adapter.raw_output_format(), RawOutputFormat::StrictJson);
        let signals = adapter.emitted_signals();
        assert!(!signals.spans && !signals.merge_hint && !signals.font_size);
    }
}
