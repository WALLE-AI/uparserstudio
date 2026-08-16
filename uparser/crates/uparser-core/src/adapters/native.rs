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
    /// Parse once, whatever the input is.
    ///
    /// This is the entry point callers should use when they may need more
    /// than one output format from the same file: the returned value carries
    /// enough to render Markdown, `document-json` and the compatibility
    /// `ParseResult` without touching the source bytes again.
    pub async fn parse_native(
        &self,
        source_path: &str,
        bytes: &[u8],
        options: &uparser_document_engine::ParseOptions,
    ) -> Result<NativeParse, PageError> {
        let format = uparser_document_engine::detect_format(bytes, Some(source_path));
        if format == uparser_document_engine::DocumentFormat::Pdf {
            return Ok(NativeParse::Pdf(self.parse_pdf(source_path, bytes)?));
        }
        Ok(NativeParse::Structured(parse_structured(
            bytes, format, options,
        )?))
    }

    /// Parse a whole PDF via the native engine's positioned-text extraction —
    /// zero model calls, zero external services, no PDFium.
    pub async fn parse_document(
        &self,
        source_path: &str,
        pdf_bytes: &[u8],
    ) -> Result<ParseResult, PageError> {
        match self
            .parse_native(
                source_path,
                pdf_bytes,
                &uparser_document_engine::ParseOptions::default(),
            )
            .await?
        {
            NativeParse::Pdf(result) => Ok(result),
            NativeParse::Structured(parsed) => {
                Ok(structured_to_parse_result(&parsed, source_path, pdf_bytes))
            }
        }
    }

    fn parse_pdf(&self, source_path: &str, pdf_bytes: &[u8]) -> Result<ParseResult, PageError> {
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
    pub async fn native_markdown(
        &self,
        source_path: &str,
        pdf_bytes: &[u8],
    ) -> Result<String, PageError> {
        let format = uparser_document_engine::detect_format(pdf_bytes, Some(source_path));
        if format != uparser_document_engine::DocumentFormat::Pdf {
            let document = uparser_document_engine::parse_document(
                pdf_bytes,
                format,
                &uparser_document_engine::ParseOptions::default(),
            )
            .map_err(|error| PageError {
                page_num: 0,
                message: format!("native structured document parsing failed: {error}"),
                stage: Some("native_document".into()),
            })?;
            return Ok(uparser_document_engine::render::markdown(&document));
        }

        let result = uparser_native_engine::process_pdf_mem(pdf_bytes).map_err(|e| PageError {
            page_num: 0,
            message: format!("native engine markdown rendering failed: {e}"),
            stage: Some("native".into()),
        })?;
        Ok(result.markdown.unwrap_or_default())
    }

    pub async fn native_document_json(
        &self,
        source_path: &str,
        bytes: &[u8],
    ) -> Result<String, PageError> {
        let format = uparser_document_engine::detect_format(bytes, Some(source_path));
        if format == uparser_document_engine::DocumentFormat::Pdf {
            return Err(PageError {
                page_num: 0,
                message:
                    "document-json is currently available for structured native documents, not PDF"
                        .to_owned(),
                stage: Some("native_document".into()),
            });
        }
        let document = uparser_document_engine::parse_document(
            bytes,
            format,
            &uparser_document_engine::ParseOptions::default(),
        )
        .map_err(|error| PageError {
            page_num: 0,
            message: format!("native structured document parsing failed: {error}"),
            stage: Some("native_document".into()),
        })?;
        uparser_document_engine::render::document_json(&document).map_err(|error| PageError {
            page_num: 0,
            message: format!("document-json serialization failed: {error}"),
            stage: Some("native_document".into()),
        })
    }
}

/// A structured (non-PDF) document, parsed exactly once.
///
/// Every output surface — Markdown, `document-json`, and the compatibility
/// `ParseResult` — is derived from this one value. Each used to re-parse the
/// source independently, so asking for `document-json` parsed the same bytes
/// twice.
pub struct StructuredDocument {
    pub document: uparser_document_engine::CanonicalDocument,
    pub format: uparser_document_engine::DocumentFormat,
}

/// What a native parse produced: PDFs go through the PDF engine, everything
/// else through the structured-document engine.
pub enum NativeParse {
    Pdf(ParseResult),
    Structured(StructuredDocument),
}

/// Machine-readable failure kind, carried on `PageError::stage`.
///
/// The CLI turns this into a semantic exit code. Without it every structured
/// failure surfaced as "internal error", which told an agent to retry — the
/// wrong advice for an encrypted file or an input over its size budget.
pub fn document_error_stage(error: &uparser_document_engine::DocumentError) -> &'static str {
    use uparser_document_engine::DocumentError as E;
    match error {
        E::UnsupportedFormat(_) => "native_document.unsupported_format",
        E::Encrypted => "native_document.encrypted",
        E::ResourceLimit { .. } => "native_document.resource_limit",
        E::MissingPart { .. } => "native_document.missing_part",
        E::Malformed { .. } => "native_document.malformed",
        E::Io(_) => "native_document.io",
        _ => "native_document",
    }
}

fn parse_structured(
    bytes: &[u8],
    format: uparser_document_engine::DocumentFormat,
    options: &uparser_document_engine::ParseOptions,
) -> Result<StructuredDocument, PageError> {
    let document =
        uparser_document_engine::parse_document(bytes, format, options).map_err(|error| {
            PageError {
                page_num: 0,
                message: format!("native structured document parsing failed: {error}"),
                stage: Some(document_error_stage(&error).into()),
            }
        })?;
    Ok(StructuredDocument { document, format })
}

/// Lower a structured document onto the page/block `ParseResult` contract.
pub fn structured_to_parse_result(
    parsed: &StructuredDocument,
    source_path: &str,
    bytes: &[u8],
) -> ParseResult {
    let StructuredDocument { document, format } = parsed;
    let pages = document
        .units
        .iter()
        .enumerate()
        .map(|(index, unit)| Page {
            page_num: (index + 1) as u32,
            width_px: 0,
            height_px: 0,
            blocks: unit
                .blocks
                .iter()
                .enumerate()
                .map(|(order, block)| compatibility_block(document, block, order))
                .collect(),
        })
        .collect();
    let format = *format;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let protocol_format = match format {
        uparser_document_engine::DocumentFormat::Csv => "csv",
        uparser_document_engine::DocumentFormat::Tsv => "tsv",
        uparser_document_engine::DocumentFormat::Excel => "excel",
        uparser_document_engine::DocumentFormat::Ods => "ods",
        uparser_document_engine::DocumentFormat::Odt => "odt",
        uparser_document_engine::DocumentFormat::Odp => "odp",
        uparser_document_engine::DocumentFormat::Epub => "epub",
        uparser_document_engine::DocumentFormat::Rtf => "rtf",
        uparser_document_engine::DocumentFormat::Docx => "docx",
        uparser_document_engine::DocumentFormat::Pptx => "pptx",
        _ => "document",
    };
    ParseResult {
        source_path: source_path.to_owned(),
        source_sha256: format!("{:x}", hasher.finalize()),
        protocol: format!("native:{protocol_format}"),
        routed_by: RoutedBy::Explicit,
        document_profile: None,
        model_endpoint: None,
        model_name: None,
        pages,
        page_errors: Vec::new(),
        capability_notes: vec![
            "source-semantic structured document extraction; geometry is not applicable".to_owned(),
        ],
        warnings: document
            .warnings
            .iter()
            .map(|warning| warning.message.clone())
            .collect(),
        timing: Default::default(),
    }
}

fn compatibility_block(
    document: &uparser_document_engine::CanonicalDocument,
    block: &uparser_document_engine::Block,
    order: usize,
) -> Block {
    use uparser_document_engine::Block as DocBlock;

    let category_raw = match block {
        DocBlock::Heading { .. } => "title",
        DocBlock::List { .. } => "list",
        DocBlock::Table { .. } => "table",
        DocBlock::Figure { .. } => "image",
        _ => "text",
    };

    let mut text = None;
    let mut html = None;
    let mut asset_bytes = None;
    // A list renders as multi-line Markdown that already carries its own
    // markers; the compatibility renderer would prefix another `- ` if the
    // normalized category said "list", so it is lowered as text.
    let mut category = category_raw;

    match block {
        // A table keeps its merged cells by going out as HTML — the
        // compatibility renderer prefers `html` over `text`, and a GFM pipe
        // table cannot express a rowspan at all.
        DocBlock::Table { table } => {
            html = Some(uparser_document_engine::render::table_html(document, table));
        }
        DocBlock::Figure { asset_id, .. } => {
            // Hand the raw bytes to the shared asset writer, which
            // content-addresses them and fills in `asset_path`.
            asset_bytes = asset_id
                .as_deref()
                .and_then(|id| document.assets.iter().find(|asset| asset.id == id))
                .and_then(|asset| asset.bytes.clone());
            if asset_bytes.is_none() {
                text = Some(uparser_document_engine::render::block_markdown(
                    document, block,
                ));
            }
        }
        DocBlock::Heading { .. } => {
            // The IR carries the level in `category`; the `#` prefix is the
            // renderer's job, so it is stripped here rather than emitted twice.
            let rendered = uparser_document_engine::render::block_markdown(document, block);
            text = Some(rendered.trim_start_matches('#').trim_start().to_owned());
        }
        DocBlock::List { .. } => {
            category = "text";
            text = Some(uparser_document_engine::render::block_markdown(
                document, block,
            ));
        }
        _ => {
            text = Some(uparser_document_engine::render::block_markdown(
                document, block,
            ));
        }
    }

    Block {
        geom: Geometry::Rect([0.0, 0.0, 0.0, 0.0]),
        geom_frame: CoordFrame::Page,
        bbox_px: None,
        category_raw: category_raw.to_owned(),
        category: Some(category.to_owned()),
        // A structured document has no geometry to derive order from, but its
        // source order *is* the reading order.
        reading_order: Some(order as u32),
        text,
        html,
        latex: None,
        spans: Vec::new(),
        merge_hint: None,
        confidence: Some(1.0),
        source: BlockSource::StructuredNative,
        error: None,
        asset_bytes,
        asset_path: None,
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
    use std::io::Write;

    fn zip_package(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, body) in parts {
                writer.start_file(name, options).unwrap();
                writer.write_all(body.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        bytes
    }

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
            .native_markdown(&path, &bytes)
            .await
            .expect("native markdown succeeds");
        assert!(!md.trim().is_empty(), "expected non-empty markdown");
        // A real report yields at least one heading via the engine's
        // font-histogram heading detection.
        assert!(md.contains('#'), "expected at least one markdown heading");
    }

    #[tokio::test]
    async fn structured_csv_uses_source_semantic_native_path() {
        let bytes = b"name,value\nalpha,42\nbeta,7\n";
        let result = NativeAdapter
            .parse_document("sample.csv", bytes)
            .await
            .expect("native CSV parse succeeds");
        assert_eq!(result.protocol, "native:csv");
        assert_eq!(result.pages.len(), 1);
        let block = &result.pages[0].blocks[0];
        assert_eq!(block.source, BlockSource::StructuredNative);
        assert_eq!(block.category_raw, "table");
        // Source order is the reading order for a document with no geometry.
        assert_eq!(block.reading_order, Some(0));
        // A table lowers to `html`, not `text`: the compatibility renderer
        // prefers `html`, and only HTML can carry a merged cell.
        assert!(block.text.is_none(), "{:?}", block.text);
        assert!(block.html.as_deref().unwrap().contains("alpha"));

        let markdown = NativeAdapter
            .native_markdown("sample.csv", bytes)
            .await
            .expect("native CSV markdown succeeds");
        // Delimited text has one anonymous table; naming it "Sheet 1" would
        // inject a heading the source does not contain.
        assert!(!markdown.contains("# Sheet 1"), "{markdown}");
        assert!(markdown.contains("| name | value |"), "{markdown}");
    }

    #[tokio::test]
    async fn structured_tsv_is_detected_from_filename_hint() {
        let markdown = NativeAdapter
            .native_markdown("sample.tsv", b"name\tvalue\nalpha\t42\n")
            .await
            .expect("native TSV markdown succeeds");
        assert!(markdown.contains("| alpha | 42 |"));
    }

    #[tokio::test]
    async fn structured_document_json_preserves_canonical_contract() {
        let json = NativeAdapter
            .native_document_json("sample.csv", b"name,value\nalpha,42\n")
            .await
            .expect("canonical JSON succeeds");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schema_version"], "uparser.document.v1");
        assert_eq!(value["units"][0]["kind"], "sheet");
        assert_eq!(value["units"][0]["blocks"][0]["type"], "table");
    }

    #[tokio::test]
    async fn epub_uses_chapter_units_through_native_adapter() {
        let bytes = zip_package(&[
            ("mimetype", "application/epub+zip"),
            (
                "META-INF/container.xml",
                "<container><rootfiles><rootfile full-path=\"book.opf\"/></rootfiles></container>",
            ),
            (
                "book.opf",
                "<package><manifest><item id=\"chapter\" href=\"chapter.xhtml\" media-type=\"application/xhtml+xml\"/></manifest><spine><itemref idref=\"chapter\"/></spine></package>",
            ),
            (
                "chapter.xhtml",
                "<html><body><h1>Chapter</h1><p>Native EPUB</p></body></html>",
            ),
        ]);
        let result = NativeAdapter
            .parse_document("book.epub", &bytes)
            .await
            .expect("native EPUB parse succeeds");
        assert_eq!(result.protocol, "native:epub");
        assert_eq!(result.pages.len(), 1);
        // The first block carries the chapter-start anchor, so a link to the
        // whole chapter file resolves once the spine is flattened; the
        // heading's own text follows it.
        let first = result.pages[0].blocks[0].text.as_deref().unwrap();
        assert!(first.contains("<a id="), "{first}");
        assert!(first.ends_with("Chapter"), "{first}");
    }

    #[tokio::test]
    async fn rtf_uses_source_semantic_native_adapter() {
        let result = NativeAdapter
            .parse_document("sample.rtf", br#"{\rtf1\ansi Native \b RTF\b0\par}"#)
            .await
            .expect("native RTF parse succeeds");
        assert_eq!(result.protocol, "native:rtf");
        assert_eq!(result.pages.len(), 1);
        assert!(
            result.pages[0].blocks[0]
                .text
                .as_deref()
                .is_some_and(|text| text.contains("Native") && text.contains("RTF"))
        );
    }
}
