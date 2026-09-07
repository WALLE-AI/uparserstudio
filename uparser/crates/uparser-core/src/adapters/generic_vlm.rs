//! Generic OpenAI-compatible vision-language adapter returning one Markdown
//! block per page. It is intentionally explicit-only until benchmark data can
//! assign it a trustworthy automatic-routing score.

use super::{
    ModelStage, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat, RemoteEndpointSpec,
    ResourceHint, StageBackend, extract_chat_content,
};
use crate::imaging;
use crate::ingest::RenderedPage;
use crate::transport::ChatCompletionRequest;
use crate::types::{Block, BlockSource, CoordinateSystem, PageError};
use async_trait::async_trait;
use std::time::Duration;

const PROMPT: &str = "Convert this document page to faithful Markdown. Preserve headings, lists, tables, formulas, and reading order. Output Markdown only.";

pub struct GenericVlmAdapter {
    pub endpoint: String,
    pub model: String,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl Default for GenericVlmAdapter {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:8000/v1/chat/completions".into(),
            model: "model".into(),
            timeout: Duration::from_secs(120),
            max_retries: 2,
        }
    }
}

#[async_trait]
impl ProtocolAdapter for GenericVlmAdapter {
    fn name(&self) -> &'static str {
        "generic-vlm"
    }

    fn coordinate_system(&self) -> CoordinateSystem {
        CoordinateSystem::PixelAbs
    }

    fn provides_reading_order(&self) -> bool {
        true
    }

    fn category_vocab(&self) -> &[&'static str] {
        // The model answers in Markdown, so the vocabulary is whatever
        // `markdown_ir` can recover from it.
        &["title", "text", "list", "table", "equation", "image"]
    }

    fn raw_output_format(&self) -> RawOutputFormat {
        RawOutputFormat::CustomToken
    }

    fn emitted_signals(&self) -> PostprocessSignals {
        PostprocessSignals {
            // Inline emphasis becomes span styling; headings and list items
            // carry a merge hint. Markdown has no font sizes.
            spans: true,
            merge_hint: true,
            font_size: false,
        }
    }

    fn model_stages(&self) -> Vec<ModelStage> {
        vec![ModelStage {
            stage_name: "vlm",
            default_backend: StageBackend::Remote(RemoteEndpointSpec {
                endpoint_env_var: "GENERIC_VLM_ENDPOINT",
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
        let image = image::load_from_memory(&page.png_bytes).map_err(|error| PageError {
            page_num: page.page_num,
            message: format!("failed to decode rasterized page: {error}"),
            stage: Some("preprocess".into()),
        })?;
        let data_url =
            imaging::to_base64_data_url(&imaging::to_rgb(&image)).map_err(|error| PageError {
                page_num: page.page_num,
                message: format!("failed to encode page: {error}"),
                stage: Some("preprocess".into()),
            })?;
        let request = ChatCompletionRequest {
            endpoint: self.endpoint.clone(),
            model: self.model.clone(),
            messages: vec![serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": data_url}},
                    {"type": "text", "text": PROMPT}
                ]
            })],
            sampling: serde_json::json!({"temperature": 0.0, "max_completion_tokens": 32768}),
            timeout: self.timeout,
            max_retries: self.max_retries,
        };
        let response = crate::shape_executor::chat_stage(page, ctx, request, "vlm").await?;
        let markdown = extract_chat_content(&response).map_err(|error| PageError {
            page_num: page.page_num,
            message: error,
            stage: Some("decode".into()),
        })?;

        // The prompt asks for Markdown, so the model's answer *is* the
        // document's structure. Keeping it as one block's `text` would hand
        // Markdown syntax to passes entitled to treat `text` as plain prose
        // — `content_normalize` folds the blank lines away and the renderer
        // escapes the markers — so it is parsed back into blocks here. Same
        // reasoning, and the same code, as `paddlex-structure`.
        Ok(
            crate::markdown_ir::blocks_from_markdown(markdown, page.width, page.height)
                .into_iter()
                .map(|block| Block {
                    source: BlockSource::OneShotVlm,
                    ..block
                })
                .collect(),
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

    /// The model answers in Markdown, so the adapter's job is to turn that
    /// into structure. Emitting one block holding the markup as characters
    /// would leave the IR empty *and* get the markup escaped downstream.
    #[tokio::test]
    async fn markdown_answer_becomes_structure_not_one_block_of_markup() {
        let adapter = GenericVlmAdapter::default();
        let mock = Arc::new(MockDispatch::new());
        mock.seed(
            &adapter.endpoint,
            serde_json::json!({
                "choices": [{"message": {"content": "# Title\n\nText"}, "finish_reason": "stop"}]
            }),
        );
        let image = RgbImage::from_pixel(40, 30, Rgb([255, 255, 255]));
        let page = RenderedPage {
            page_num: 2,
            width: 40,
            height: 30,
            png_bytes: imaging::to_png_bytes(&image).unwrap(),
        };
        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let blocks = adapter.parse_page(&page, &ctx).await.unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].category.as_deref(), Some("title"));
        assert_eq!(blocks[0].text.as_deref(), Some("Title"));
        assert_eq!(blocks[1].category.as_deref(), Some("text"));
        assert_eq!(blocks[1].text.as_deref(), Some("Text"));
        // No geometry is invented: a one-shot page model reports none.
        assert!(blocks.iter().all(|block| block.bbox_px.is_none()));
        // The protocol's own provenance survives the shared parser.
        assert!(
            blocks
                .iter()
                .all(|block| block.source == BlockSource::OneShotVlm)
        );
    }
}
