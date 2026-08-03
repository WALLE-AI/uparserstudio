//! Trivial adapter used for the P0 / Gate-G0 smoke test: two adjacent,
//! same-category, geometrically-mergeable text blocks per page (deliberately
//! *not* one — this lets `postprocess::merge_paragraphs_by_geometry`'s
//! real integration into `api::parse`/`cli.rs::run_parse` be verified
//! end-to-end through the `mock` protocol: a caller that skips
//! post-processing sees 2 raw blocks, one that doesn't sees 1 merged
//! block), with an optional configurable failure for exercising
//! scheduler partial-failure isolation.

use super::{ModelStage, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat};
use crate::ingest::RenderedPage;
use crate::types::{Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, PageError};
use async_trait::async_trait;

#[derive(Default)]
pub struct MockAdapter {
    /// If set, `parse_page` returns an `Err` for this page number.
    pub fail_on_page: Option<u32>,
}

#[async_trait]
impl ProtocolAdapter for MockAdapter {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn coordinate_system(&self) -> CoordinateSystem {
        CoordinateSystem::PixelAbs
    }

    fn provides_reading_order(&self) -> bool {
        true
    }

    fn category_vocab(&self) -> &[&'static str] {
        &["text"]
    }

    fn raw_output_format(&self) -> RawOutputFormat {
        RawOutputFormat::StrictJson
    }

    fn emitted_signals(&self) -> PostprocessSignals {
        PostprocessSignals::default()
    }

    fn model_stages(&self) -> Vec<ModelStage> {
        vec![]
    }

    async fn parse_page(
        &self,
        page: &RenderedPage,
        _ctx: &ParseCtx,
    ) -> Result<Vec<Block>, PageError> {
        if self.fail_on_page == Some(page.page_num) {
            return Err(PageError {
                page_num: page.page_num,
                message: "mock induced failure".into(),
                stage: Some("mock".into()),
            });
        }

        // Fixed absolute pixel bboxes, independent of the page's own
        // (possibly degenerate, e.g. 1x1 placeholder) dimensions —
        // vertical gap 5px (within postprocess's 8px threshold),
        // left-aligned, so these two always merge into one block.
        let make_block = |bbox: [i32; 4], text: String| Block {
            geom: Geometry::Rect([
                bbox[0] as f32,
                bbox[1] as f32,
                bbox[2] as f32,
                bbox[3] as f32,
            ]),
            geom_frame: CoordFrame::Page,
            bbox_px: Some(bbox),
            category_raw: "text".into(),
            category: Some("text".into()),
            reading_order: Some(0),
            text: Some(text),
            html: None,
            latex: None,
            spans: vec![],
            merge_hint: None,
            confidence: Some(1.0),
            source: BlockSource::OneShotVlm,
            error: None,
            asset_bytes: None,
            asset_path: None,
        };

        Ok(vec![
            make_block([0, 0, 100, 20], format!("mock page {}", page.page_num)),
            make_block([0, 25, 100, 45], "(mock continuation)".to_string()),
        ])
    }
}
