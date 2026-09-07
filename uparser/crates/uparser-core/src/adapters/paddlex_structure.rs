//! PP-StructureV3 serving adapter for the upstream `POST /layout-parsing`
//! contract documented in the vendored PaddleOCR source.

use super::{
    ModelStage, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat, RemoteEndpointSpec,
    ResourceHint, StageBackend,
};
use crate::ingest::RenderedPage;
use crate::types::{Block, CoordinateSystem, PageError};
use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LayoutParsingRequest {
    pub file: String,
    pub file_type: u8,
    pub visualize: bool,
    pub return_markdown_images: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct LayoutParsingEnvelope {
    #[serde(default)]
    error_code: i64,
    #[serde(default)]
    error_msg: String,
    result: Option<LayoutParsingResult>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct LayoutParsingResult {
    layout_parsing_results: Vec<LayoutParsingPage>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct LayoutParsingPage {
    markdown: LayoutMarkdown,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct LayoutMarkdown {
    text: String,
}

pub struct PaddleXStructureAdapter {
    pub endpoint: String,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl Default for PaddleXStructureAdapter {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:8080/layout-parsing".to_owned(),
            timeout: Duration::from_secs(120),
            max_retries: 2,
        }
    }
}

impl PaddleXStructureAdapter {
    fn decode(
        &self,
        page: &RenderedPage,
        value: serde_json::Value,
    ) -> Result<Vec<Block>, PageError> {
        let envelope: LayoutParsingEnvelope =
            serde_json::from_value(value).map_err(|error| PageError {
                page_num: page.page_num,
                message: format!("malformed paddlex-structure response: {error}"),
                stage: Some("structured-service".into()),
            })?;
        if envelope.error_code != 0 {
            return Err(PageError {
                page_num: page.page_num,
                message: format!(
                    "paddlex-structure error {}: {}",
                    envelope.error_code, envelope.error_msg
                ),
                stage: Some("structured-service".into()),
            });
        }
        let result = envelope.result.ok_or_else(|| PageError {
            page_num: page.page_num,
            message: "paddlex-structure success response is missing result".into(),
            stage: Some("structured-service".into()),
        })?;
        let parsed = result
            .layout_parsing_results
            .into_iter()
            .next()
            .ok_or_else(|| PageError {
                page_num: page.page_num,
                message: "paddlex-structure returned no page result".into(),
                stage: Some("structured-service".into()),
            })?;

        // This service is authoritative for Markdown, so the structure this
        // adapter must produce is *in* that string. Keeping it as one block's
        // `text` would hand Markdown syntax to passes that are contractually
        // entitled to treat `text` as plain prose, and they would mangle it:
        // `content_normalize` folds the blank lines away and the renderer
        // escapes the markers. Parsing it back into blocks is what makes the
        // IR describe the document the service actually returned.
        Ok(crate::markdown_ir::blocks_from_markdown(
            &parsed.markdown.text,
            page.width,
            page.height,
        ))
    }
}

#[async_trait]
impl ProtocolAdapter for PaddleXStructureAdapter {
    fn name(&self) -> &'static str {
        "paddlex-structure"
    }

    fn coordinate_system(&self) -> CoordinateSystem {
        CoordinateSystem::PixelAbs
    }

    fn provides_reading_order(&self) -> bool {
        true
    }

    fn category_vocab(&self) -> &[&'static str] {
        // The service returns Markdown, so the vocabulary is whatever
        // `markdown_ir` can recover from it rather than a layout model's
        // label set.
        &["title", "text", "list", "table", "equation", "image"]
    }

    fn raw_output_format(&self) -> RawOutputFormat {
        RawOutputFormat::StrictJson
    }

    fn emitted_signals(&self) -> PostprocessSignals {
        PostprocessSignals {
            // Inline emphasis becomes span styling, and headings and list
            // items carry a merge hint. No font sizes: Markdown has none.
            spans: true,
            merge_hint: true,
            font_size: false,
        }
    }

    fn model_stages(&self) -> Vec<ModelStage> {
        vec![ModelStage {
            stage_name: "structured-service",
            default_backend: StageBackend::Remote(RemoteEndpointSpec {
                endpoint_env_var: "PADDLEX_STRUCTURE_ENDPOINT",
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
        let request = LayoutParsingRequest {
            file: base64::engine::general_purpose::STANDARD.encode(&page.png_bytes),
            file_type: 1,
            visualize: false,
            return_markdown_images: false,
        };
        let response = crate::shape_executor::rest_stage(
            page,
            ctx,
            &self.endpoint,
            serde_json::to_value(request).expect("layout parsing request is serializable"),
            self.timeout,
            self.max_retries,
            "structured-service",
        )
        .await?;
        self.decode(page, response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MockDispatch;
    use crate::types::BlockSource;
    use image::{Rgb, RgbImage};
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    fn fake_page() -> RenderedPage {
        let image = RgbImage::from_pixel(120, 80, Rgb([255, 255, 255]));
        RenderedPage {
            page_num: 1,
            width: 120,
            height: 80,
            png_bytes: crate::imaging::to_png_bytes(&image).unwrap(),
        }
    }

    #[tokio::test]
    async fn decodes_authoritative_markdown_envelope() {
        let adapter = PaddleXStructureAdapter::default();
        let mock = Arc::new(MockDispatch::new());
        mock.seed(
            &adapter.endpoint,
            serde_json::json!({
                "logId": "request-1",
                "errorCode": 0,
                "errorMsg": "Success",
                "result": {
                    "layoutParsingResults": [{
                        "markdown": {"text": "# Heading\n\nBody", "images": null,
                                     "isStart": true, "isEnd": true}
                    }],
                    "dataInfo": {}
                }
            }),
        );
        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let blocks = adapter.parse_page(&fake_page(), &ctx).await.unwrap();

        // The envelope's Markdown is the document's structure, so it arrives
        // as blocks — not as one block holding the markup as characters.
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].category.as_deref(), Some("title"));
        assert_eq!(blocks[0].text.as_deref(), Some("Heading"));
        assert_eq!(blocks[1].category.as_deref(), Some("text"));
        assert_eq!(blocks[1].text.as_deref(), Some("Body"));
        assert!(blocks.iter().all(|block| block.bbox_px.is_none()));
        assert!(
            blocks
                .iter()
                .all(|block| block.source == BlockSource::StructuredService)
        );
    }

    /// The regression this adapter was rewritten for, asserted end to end
    /// through the real renderer rather than at the block boundary.
    ///
    /// The service is authoritative for Markdown. When its output was kept
    /// as one block's `text`, two passes that are entitled to treat `text`
    /// as plain prose destroyed it: `content_normalize` folded the blank
    /// lines away (so nothing parsed as a heading or a table any more) and
    /// the renderer escaped the markers — the user saw `\# Heading` and
    /// `\*\*bold\*\*`. Both passes were right; the input was mislabelled.
    #[tokio::test]
    async fn authoritative_markdown_survives_the_shared_renderer_intact() {
        let adapter = PaddleXStructureAdapter::default();
        let mock = Arc::new(MockDispatch::new());
        mock.seed(
            &adapter.endpoint,
            serde_json::json!({
                "logId": "request-1",
                "errorCode": 0,
                "errorMsg": "Success",
                "result": {
                    "layoutParsingResults": [{
                        "markdown": {
                            "text": "# Heading\n\nBody with **bold**\n\n| a | b |\n| --- | --- |\n| 1 | 2 |",
                            "images": null, "isStart": true, "isEnd": true
                        }
                    }],
                    "dataInfo": {}
                }
            }),
        );
        let ctx = ParseCtx::with_mock(mock, Arc::new(Semaphore::new(1)));
        let blocks = adapter.parse_page(&fake_page(), &ctx).await.unwrap();

        // Through the same postprocess the CLI and the library API apply —
        // this is where the blank lines used to be folded away.
        let pages = vec![crate::types::Page {
            page_num: 1,
            width_px: 120,
            height_px: 80,
            blocks: crate::postprocess::merge_paragraphs_by_geometry(blocks),
        }];
        let result = crate::types::ParseResult {
            source_path: "doc.pdf".into(),
            source_sha256: String::new(),
            protocol: "paddlex-structure".into(),
            routed_by: crate::types::RoutedBy::Explicit,
            document_profile: None,
            route_decision: None,
            preprocess_plan: None,
            model_endpoint: None,
            model_name: None,
            pages,
            page_errors: Vec::new(),
            capability_notes: Vec::new(),
            warnings: Vec::new(),
            timing: Default::default(),
        };
        let markdown = crate::render::render_markdown(
            &crate::render::RenderInput {
                result: &result,
                engine_markdown: None,
                document: None,
                source_format: uparser_document_engine::DocumentFormat::Pdf,
            },
            crate::render::MarkdownSource::Canonical,
        );

        assert!(markdown.contains("# Heading"), "{markdown}");
        assert!(markdown.contains("**bold**"), "{markdown}");
        assert!(markdown.contains("| a | b |"), "{markdown}");
        assert!(markdown.contains("| 1 | 2 |"), "{markdown}");
        assert!(
            !markdown.contains('\\'),
            "no escape backslash should survive: {markdown}"
        );
    }

    #[test]
    fn surfaces_service_error_envelope() {
        let adapter = PaddleXStructureAdapter::default();
        let error = adapter
            .decode(
                &fake_page(),
                serde_json::json!({"errorCode": 500, "errorMsg": "failed"}),
            )
            .unwrap_err();
        assert!(error.message.contains("500"));
        assert!(error.message.contains("failed"));
    }

    #[test]
    fn request_uses_upstream_camel_case_fields() {
        let value = serde_json::to_value(LayoutParsingRequest {
            file: "AA==".into(),
            file_type: 1,
            visualize: false,
            return_markdown_images: false,
        })
        .unwrap();
        assert_eq!(value["fileType"], 1);
        assert_eq!(value["returnMarkdownImages"], false);
        assert!(value.get("file_type").is_none());
    }
}
