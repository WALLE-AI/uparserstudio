//! Zero-model native-PDF-text-layer protocol, per T-4.1. Embeds
//! `opensource/liteparse` as a library dependency (not a wire protocol —
//! no reverse-engineering needed, this is first-party Rust). Confirmed
//! API surface (from source read of `crates/liteparse/src/{parser,types,config}.rs`):
//! `LiteParse::new(config).parse_input(PdfInput::Bytes(..))` returns a
//! `liteparse::ParseResult { pages: Vec<ParsedPage>, .. }`; each
//! `ParsedPage.text_items: Vec<TextItem>` carries `text/x/y/width/height`
//! (viewport space, top-left origin, 72 DPI — already pixel-equivalent),
//! `font_size`, and `confidence: Option<f32>` (native items never carry
//! one — only OCR-sourced items do, which we disable entirely).
//!
//! Responsibility boundary (T-4.2): `LiteParseConfig.ocr_enabled = false`
//! disables OCR outright; format conversion in liteparse only triggers
//! for non-PDF input, so handing it genuine PDF bytes never invokes it —
//! satisfied by construction, no extra code needed.
//!
//! This adapter's `parse_document` operates on a **whole document**, not
//! a single rasterized page — it doesn't fit `ProtocolAdapter::parse_page`
//! (which assumes a pre-rasterized `RenderedPage` and network dispatch,
//! neither of which applies here). `parse_page` is implemented but
//! returns an explanatory `PageError`; reconciling native's whole-document
//! shape with the per-page scheduler pipeline is deferred (see the P4
//! plan's "What's different" section) — likely a router/scheduler
//! concern for a later phase, not this one.

use super::{ModelStage, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat};
use crate::ingest::RenderedPage;
use crate::types::{
    Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, Page, PageError, ParseResult,
    RoutedBy, Span,
};
use async_trait::async_trait;
use liteparse::{LiteParse, LiteParseConfig};
use sha2::{Digest, Sha256};

#[derive(Default)]
pub struct NativeAdapter;

impl NativeAdapter {
    /// Parse a whole PDF document via liteparse's native text-layer
    /// extraction — zero model calls, zero external services.
    pub async fn parse_document(
        &self,
        source_path: &str,
        pdf_bytes: &[u8],
    ) -> Result<ParseResult, PageError> {
        let config = LiteParseConfig {
            ocr_enabled: false,
            ..Default::default()
        };
        let parser = LiteParse::new(config);
        let input = liteparse::types::PdfInput::Bytes(pdf_bytes.to_vec());

        let lp_result = parser.parse_input(input).await.map_err(|e| PageError {
            page_num: 0,
            message: format!("liteparse native extraction failed: {e}"),
            stage: Some("native".into()),
        })?;

        let pages: Vec<Page> = lp_result.pages.into_iter().map(map_page).collect();

        let mut hasher = Sha256::new();
        hasher.update(pdf_bytes);
        let source_sha256 = format!("{:x}", hasher.finalize());

        Ok(ParseResult {
            source_path: source_path.to_string(),
            source_sha256,
            protocol: "native".to_string(),
            routed_by: RoutedBy::Explicit,
            document_profile: None,
            model_endpoint: None,
            model_name: None,
            pages,
            page_errors: vec![],
            capability_notes: vec![],
            warnings: vec![],
            timing: Default::default(),
        })
    }
}

fn map_page(page: liteparse::ParsedPage) -> Page {
    let blocks = page.text_items.into_iter().map(map_text_item).collect();
    Page {
        page_num: page.page_number as u32,
        width_px: page.page_width.round() as u32,
        height_px: page.page_height.round() as u32,
        blocks,
    }
}

fn map_text_item(item: liteparse::TextItem) -> Block {
    let bbox_px = [
        item.x.round() as i32,
        item.y.round() as i32,
        (item.x + item.width).round() as i32,
        (item.y + item.height).round() as i32,
    ];
    let span = Span {
        text: item.text.clone(),
        bbox_px: Some(bbox_px),
        font_size: item.font_size,
        is_inline_formula: false,
    };
    Block {
        geom: Geometry::Rect([item.x, item.y, item.x + item.width, item.y + item.height]),
        geom_frame: CoordFrame::Page,
        bbox_px: Some(bbox_px),
        category_raw: "text".to_string(),
        category: Some("text".to_string()),
        reading_order: None,
        text: Some(item.text),
        html: None,
        latex: None,
        spans: vec![span],
        merge_hint: None,
        confidence: item.confidence,
        source: BlockSource::NativeTextLayer,
        error: None,
    }
}

#[async_trait]
impl ProtocolAdapter for NativeAdapter {
    fn name(&self) -> &'static str {
        "native"
    }

    fn coordinate_system(&self) -> CoordinateSystem {
        CoordinateSystem::PixelAbs
    }

    fn provides_reading_order(&self) -> bool {
        // liteparse's spatial grid projection already produces
        // reading-ordered text.
        true
    }

    fn category_vocab(&self) -> &[&'static str] {
        &["text"]
    }

    fn raw_output_format(&self) -> RawOutputFormat {
        RawOutputFormat::None
    }

    fn emitted_signals(&self) -> PostprocessSignals {
        // The first adapter to legitimately emit spans/font_size — all
        // three VLM protocols (P1-P3) landed on the pure-geometry path.
        PostprocessSignals {
            spans: true,
            merge_hint: false,
            font_size: true,
        }
    }

    fn model_stages(&self) -> Vec<ModelStage> {
        vec![]
    }

    async fn parse_page(
        &self,
        _page: &RenderedPage,
        _ctx: &ParseCtx,
    ) -> Result<Vec<Block>, PageError> {
        Err(PageError {
            page_num: 0,
            message: "NativeAdapter uses parse_document(), not parse_page() — whole-document \
                      zero-model parsing doesn't fit the per-page pipeline"
                .to_string(),
            stage: Some("unsupported".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_pdf_path() -> String {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../opensource/liteparse/integration_tests_data/sample.pdf"
        )
        .to_string()
    }

    #[test]
    fn map_text_item_preserves_geometry_and_native_source() {
        let item = liteparse::TextItem {
            text: "hello".to_string(),
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 12.0,
            rotation: 0.0,
            font_name: Some("Helvetica".to_string()),
            font_size: Some(11.0),
            font_height: None,
            font_ascent: None,
            font_descent: None,
            font_weight: None,
            font_flags: None,
            text_width: None,
            font_is_buggy: false,
            has_unicode_map_error: false,
            mcid: None,
            fill_color: None,
            stroke_color: None,
            confidence: None,
            link: None,
            strike: false,
            words: vec![],
        };

        let block = map_text_item(item);
        assert_eq!(block.text.as_deref(), Some("hello"));
        assert_eq!(block.bbox_px, Some([10, 20, 110, 32]));
        assert_eq!(block.source, BlockSource::NativeTextLayer);
        assert_eq!(block.spans.len(), 1);
        assert_eq!(block.spans[0].font_size, Some(11.0));
        assert!(block.confidence.is_none());
    }

    #[test]
    fn declares_expected_protocol_metadata() {
        let adapter = NativeAdapter;
        assert_eq!(adapter.name(), "native");
        assert!(adapter.provides_reading_order());
        assert_eq!(adapter.raw_output_format(), RawOutputFormat::None);
        assert!(adapter.model_stages().is_empty());
        let signals = adapter.emitted_signals();
        assert!(signals.spans && signals.font_size && !signals.merge_hint);
    }

    #[tokio::test]
    async fn parse_page_is_unsupported() {
        let adapter = NativeAdapter;
        let page = RenderedPage {
            page_num: 1,
            width: 10,
            height: 10,
            png_bytes: vec![],
        };
        let ctx = ParseCtx::new(
            std::sync::Arc::new(crate::transport::Transport::new()),
            std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        );
        let result = adapter.parse_page(&page, &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parse_document_extracts_real_pdf_text() {
        let path = fixture_pdf_path();
        if !std::path::Path::new(&path).exists() {
            eprintln!("skipping: no fixture PDF at {path}");
            return;
        }
        let bytes = std::fs::read(&path).expect("read fixture PDF");

        let adapter = NativeAdapter;
        let result = adapter
            .parse_document(&path, &bytes)
            .await
            .expect("native parse succeeds");

        assert_eq!(result.protocol, "native");
        assert!(!result.pages.is_empty());
        let total_text: usize = result
            .pages
            .iter()
            .flat_map(|p| &p.blocks)
            .filter_map(|b| b.text.as_ref())
            .map(|t| t.len())
            .sum();
        assert!(total_text > 0, "expected non-empty extracted text");
        assert!(
            result
                .pages
                .iter()
                .flat_map(|p| &p.blocks)
                .all(|b| b.source == BlockSource::NativeTextLayer)
        );
    }
}
