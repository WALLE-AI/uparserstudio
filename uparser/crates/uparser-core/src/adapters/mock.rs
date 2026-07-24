//! Trivial adapter used for the P0 / Gate-G0 smoke test: one hardcoded
//! block per page, with an optional configurable failure for exercising
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

        Ok(vec![Block {
            geom: Geometry::Rect([0.0, 0.0, page.width as f32, page.height as f32]),
            geom_frame: CoordFrame::Page,
            bbox_px: Some([0, 0, page.width as i32, page.height as i32]),
            category_raw: "text".into(),
            category: Some("text".into()),
            reading_order: Some(0),
            text: Some(format!("mock page {}", page.page_num)),
            html: None,
            latex: None,
            spans: vec![],
            merge_hint: None,
            confidence: Some(1.0),
            source: BlockSource::OneShotVlm,
            error: None,
        }])
    }
}
