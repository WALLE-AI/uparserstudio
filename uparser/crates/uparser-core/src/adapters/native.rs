//! Zero-model native-PDF-text-layer protocol, per T-4.1. Embeds the
//! **`uparser-native-engine`** crate (vendored from firecrawl/pdf-inspector,
//! MIT — see that crate's `ATTRIBUTION.md`) as a pure-Rust, lopdf-based,
//! **PDFium-free** text-extraction engine. This replaced the earlier
//! dependency on `opensource/liteparse` (see repo-root
//! `NATIVE_ENGINE_INTERNALIZATION_DESIGN.md`): native no longer depends on
//! liteparse, and no longer pulls the PDFium prebuilt binary.
//!
//! Two entry points, both whole-document (native parses a whole PDF in one
//! call; it does not fit `ProtocolAdapter::parse_page`'s per-rasterized-page
//! contract — that method returns an explanatory `PageError`):
//!   * `native_markdown()` → the engine's own markdown (its full pipeline:
//!     headings, paragraph grouping, three-strategy tables) — the
//!     bench-critical, coordinate-free path used by `--format markdown`.
//!   * `parse_document()` → the engine's positioned `TextItem`s, grouped
//!     into coherent reading-ordered *lines* (not per-span fragments) and
//!     mapped to the uparser `Block` IR — used by `--format json`.
//!
//! Engine `TextItem` coordinates are PDF space (origin bottom-left); the IR
//! wants top-left pixel space, so `build_page` flips Y against a per-page
//! top reference derived from the items themselves.

use super::{ModelStage, ParseCtx, PostprocessSignals, ProtocolAdapter, RawOutputFormat};
use crate::ingest::RenderedPage;
use crate::types::{
    Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, Page, PageError, ParseResult,
    RoutedBy, Span,
};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uparser_native_engine::types::{ItemType, TextItem};

#[derive(Default)]
pub struct NativeAdapter;

impl NativeAdapter {
    /// Parse a whole PDF via the native engine's positioned-text extraction —
    /// zero model calls, zero external services, no PDFium.
    pub async fn parse_document(
        &self,
        source_path: &str,
        pdf_bytes: &[u8],
    ) -> Result<ParseResult, PageError> {
        let items = uparser_native_engine::extractor::extract_text_with_positions_mem(pdf_bytes)
            .map_err(|e| PageError {
                page_num: 0,
                message: format!("native engine extraction failed: {e}"),
                stage: Some("native".into()),
            })?;

        let pages = build_pages(items);

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

    /// Render the document as the native engine's OWN markdown — its full
    /// pipeline output (heading levels, paragraph grouping, three-strategy
    /// tables). Coordinate-free; the bench-critical `--format markdown` path.
    ///
    /// Currently passes the engine markdown through verbatim (so it is
    /// byte-identical to upstream pdf-inspector). The uparser enhancement
    /// layer (design doc §4.6) is intentionally *not* wired here yet: the
    /// opendataloader-bench MHS metric is heading-*level*-agnostic (it
    /// treats all `#`/`##`/… as one "heading" tag), so the obvious
    /// level-flattening tweak is a no-op; the real levers (heading
    /// over-detection — engine emits 280 vs GT's 193 — and table TEDS) are
    /// engine-core tuning, tracked in the design doc's §6.5/§6.6.
    pub async fn native_markdown(&self, pdf_bytes: &[u8]) -> Result<String, PageError> {
        let result = uparser_native_engine::process_pdf_mem(pdf_bytes).map_err(|e| PageError {
            page_num: 0,
            message: format!("native engine markdown rendering failed: {e}"),
            stage: Some("native".into()),
        })?;
        Ok(result.markdown.unwrap_or_default())
    }
}

/// Group all pages' positioned items into `Page`s of coherent line-`Block`s.
fn build_pages(items: Vec<TextItem>) -> Vec<Page> {
    let mut by_page: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
    for it in items {
        // Image placeholders carry no extractable text; drop them (the
        // markdown path handles figures separately). Keep Text/Link/FormField.
        if matches!(it.item_type, ItemType::Image) || it.text.trim().is_empty() {
            continue;
        }
        by_page.entry(it.page).or_default().push(it);
    }
    by_page
        .into_iter()
        .map(|(page_num, items)| build_page(page_num, items))
        .collect()
}

fn build_page(page_num: u32, mut items: Vec<TextItem>) -> Page {
    // Per-page top/right reference from the items themselves (the engine's
    // simple positioned-text API doesn't return MediaBox dims). Y is flipped
    // against `page_top` so the top-of-page text sorts first, matching the
    // IR's top-left-origin expectation used by postprocess/reading order.
    let page_top = items.iter().map(|i| i.y + i.height).fold(0.0_f32, f32::max);
    let page_right = items.iter().map(|i| i.x + i.width).fold(0.0_f32, f32::max);

    // Sort top-to-bottom (PDF y descending), then left-to-right.
    items.sort_by(|a, b| {
        b.y.partial_cmp(&a.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });

    // Cluster into lines by vertical proximity of adjacent (already sorted)
    // items — same line when the y-centers are within ~0.6× the glyph height.
    let mut lines: Vec<Vec<TextItem>> = Vec::new();
    for it in items {
        let center = it.y + it.height / 2.0;
        let same_line = lines.last().and_then(|l| l.last()).is_some_and(|last| {
            let lc = last.y + last.height / 2.0;
            let tol = (it.height.max(last.height) * 0.6).max(1.0);
            (lc - center).abs() <= tol
        });
        if same_line {
            lines.last_mut().unwrap().push(it);
        } else {
            lines.push(vec![it]);
        }
    }

    let blocks = lines
        .into_iter()
        .map(|mut line| {
            line.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
            build_line_block(&line, page_top)
        })
        .collect();

    Page {
        page_num,
        width_px: page_right.ceil().max(0.0) as u32,
        height_px: page_top.ceil().max(0.0) as u32,
        blocks,
    }
}

/// Assemble one coherent line (its already-x-sorted items) into a `Block`,
/// flipping PDF coords to top-left pixel space via `page_top`.
fn build_line_block(line: &[TextItem], page_top: f32) -> Block {
    let x0 = line.iter().map(|i| i.x).fold(f32::INFINITY, f32::min);
    let x1 = line
        .iter()
        .map(|i| i.x + i.width)
        .fold(f32::NEG_INFINITY, f32::max);
    let pdf_ytop = line
        .iter()
        .map(|i| i.y + i.height)
        .fold(f32::NEG_INFINITY, f32::max);
    let pdf_ybot = line.iter().map(|i| i.y).fold(f32::INFINITY, f32::min);
    let y0 = page_top - pdf_ytop; // top edge (top-left origin)
    let y1 = page_top - pdf_ybot; // bottom edge
    let bbox_px = [
        x0.round() as i32,
        y0.round() as i32,
        x1.round() as i32,
        y1.round() as i32,
    ];

    // Join item texts, inserting a space across a real inter-item gap.
    let mut text = String::new();
    for (idx, it) in line.iter().enumerate() {
        if idx > 0 {
            let prev = &line[idx - 1];
            let gap = it.x - (prev.x + prev.width);
            let boundary = !text.ends_with(' ') && !it.text.starts_with(' ');
            if boundary && gap > it.font_size * 0.15 {
                text.push(' ');
            }
        }
        text.push_str(&it.text);
    }

    let spans: Vec<Span> = line
        .iter()
        .map(|it| {
            let sy0 = page_top - (it.y + it.height);
            let sy1 = page_top - it.y;
            Span {
                text: it.text.clone(),
                bbox_px: Some([
                    it.x.round() as i32,
                    sy0.round() as i32,
                    (it.x + it.width).round() as i32,
                    sy1.round() as i32,
                ]),
                font_size: Some(it.font_size),
                is_inline_formula: false,
            }
        })
        .collect();

    Block {
        geom: Geometry::Rect([x0, y0, x1, y1]),
        geom_frame: CoordFrame::Page,
        bbox_px: Some(bbox_px),
        category_raw: "text".to_string(),
        category: Some("text".to_string()),
        reading_order: None,
        text: Some(text),
        html: None,
        latex: None,
        spans,
        merge_hint: None,
        confidence: None,
        source: BlockSource::NativeTextLayer,
        error: None,
        asset_bytes: None,
        asset_path: None,
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
        // The engine emits items in reading order; `build_page` preserves it.
        true
    }

    fn category_vocab(&self) -> &[&'static str] {
        &["text"]
    }

    fn raw_output_format(&self) -> RawOutputFormat {
        RawOutputFormat::None
    }

    fn emitted_signals(&self) -> PostprocessSignals {
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

    /// A real digitally-native PDF (MinerU's demo1) — replaces the old
    /// liteparse fixture now that native no longer depends on liteparse.
    fn fixture_pdf_path() -> String {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../opensource/MinerU/demo/pdfs/demo1.pdf"
        )
        .to_string()
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
        use crate::testing::MockDispatch;
        use std::sync::Arc;
        use tokio::sync::Semaphore;

        let adapter = NativeAdapter;
        let page = RenderedPage {
            page_num: 1,
            png_bytes: vec![],
            width: 1,
            height: 1,
        };
        let ctx = ParseCtx::with_mock(
            Arc::new(MockDispatch::default()),
            Arc::new(Semaphore::new(1)),
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
        let result = NativeAdapter
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

    /// Regression guard: native maps one Block per coherent *line* (not per
    /// raw span), so a prose PDF must yield multi-word, multi-span blocks.
    #[tokio::test]
    async fn parse_document_yields_coherent_multiword_lines_not_span_fragments() {
        let path = fixture_pdf_path();
        if !std::path::Path::new(&path).exists() {
            eprintln!("skipping: no fixture PDF at {path}");
            return;
        }
        let bytes = std::fs::read(&path).expect("read fixture PDF");
        let result = NativeAdapter
            .parse_document(&path, &bytes)
            .await
            .expect("native parse succeeds");

        let multiword = result
            .pages
            .iter()
            .flat_map(|p| &p.blocks)
            .filter(|b| b.text.as_deref().is_some_and(|t| t.trim().contains(' ')))
            .count();
        assert!(multiword > 0, "expected coherent multi-word line blocks");

        let multi_span = result
            .pages
            .iter()
            .flat_map(|p| &p.blocks)
            .any(|b| b.spans.len() > 1);
        assert!(
            multi_span,
            "expected at least one line grouping multiple spans"
        );
    }

    #[tokio::test]
    async fn native_markdown_has_structure() {
        let path = fixture_pdf_path();
        if !std::path::Path::new(&path).exists() {
            eprintln!("skipping: no fixture PDF at {path}");
            return;
        }
        let bytes = std::fs::read(&path).expect("read fixture PDF");
        let md = NativeAdapter
            .native_markdown(&bytes)
            .await
            .expect("native markdown succeeds");
        assert!(!md.trim().is_empty(), "expected non-empty markdown");
        // A real report yields at least one heading via the engine's
        // font-histogram heading detection.
        assert!(md.contains('#'), "expected at least one markdown heading");
    }
}
