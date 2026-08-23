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
    AssetCaption, Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, Page, PageError,
    ParseResult, RoutedBy, Span,
};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use uparser_native_engine::structure_tree::StructRole;
use uparser_native_engine::types::{ItemType, TextItem};

#[derive(Default)]
pub struct NativeAdapter;

impl NativeAdapter {
    pub(crate) fn parse_pdf_artifact(
        source_path: &str,
        pdf_bytes: &[u8],
        artifact: uparser_native_engine::PdfProcessResult,
    ) -> (ParseResult, Option<String>) {
        let mut positioned_items = artifact.positioned_items;
        for entry in &artifact.ocr_reasons_by_page {
            let is_scanned = entry
                .reasons
                .iter()
                .any(|reason| reason == uparser_native_engine::OCR_REASON_SCANNED);
            let page_has_items = positioned_items.iter().any(|item| item.page == entry.page);
            if is_scanned && !page_has_items {
                if let Some(size) = artifact.page_sizes.get(&entry.page) {
                    positioned_items.push(scanned_page_image_item(entry.page, *size));
                }
            }
        }
        for (&page, regions) in &artifact.chart_regions {
            if let Some(size) = artifact.page_sizes.get(&page) {
                positioned_items.extend(
                    regions
                        .iter()
                        .map(|region| vector_chart_item(page, *region, *size)),
                );
            }
        }
        let pages = build_pages(
            positioned_items,
            &artifact.struct_roles,
            &artifact.page_sizes,
        );
        let mut hasher = Sha256::new();
        hasher.update(pdf_bytes);
        let result = ParseResult {
            source_path: source_path.to_owned(),
            source_sha256: format!("{:x}", hasher.finalize()),
            protocol: "native".to_owned(),
            routed_by: RoutedBy::Explicit,
            document_profile: None,
            route_decision: None,
            preprocess_plan: None,
            model_endpoint: None,
            model_name: None,
            pages,
            page_errors: vec![],
            capability_notes: vec![],
            warnings: vec![],
            timing: Default::default(),
        };
        (result, artifact.markdown)
    }

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

        let pages = build_pages(items, &HashMap::new(), &HashMap::new());

        let mut hasher = Sha256::new();
        hasher.update(pdf_bytes);
        let source_sha256 = format!("{:x}", hasher.finalize());

        Ok(ParseResult {
            source_path: source_path.to_string(),
            source_sha256,
            protocol: "native".to_string(),
            routed_by: RoutedBy::Explicit,
            document_profile: None,
            route_decision: None,
            preprocess_plan: None,
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
    crate::structured::to_parse_result(&parsed.document, source_path, bytes)
}

/// Group all pages' positioned items into `Page`s of coherent line-`Block`s.
fn build_pages(
    items: Vec<TextItem>,
    struct_roles: &HashMap<u32, HashMap<i64, StructRole>>,
    page_sizes: &HashMap<u32, [f32; 2]>,
) -> Vec<Page> {
    let mut by_page: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
    for it in items {
        if matches!(it.item_type, ItemType::Image) && !is_substantive_image_item(&it) {
            continue;
        }
        if !matches!(it.item_type, ItemType::Image) && it.text.trim().is_empty() {
            continue;
        }
        by_page.entry(it.page).or_default().push(it);
    }
    by_page
        .into_iter()
        .map(|(page_num, items)| {
            build_page(
                page_num,
                items,
                struct_roles.get(&page_num),
                page_sizes.get(&page_num).copied(),
            )
        })
        .collect()
}

fn is_substantive_image_item(item: &TextItem) -> bool {
    let width = item.width.abs();
    let height = item.height.abs();
    width >= 8.0 && height >= 8.0 && width * height >= 256.0
}

fn build_page(
    page_num: u32,
    items: Vec<TextItem>,
    struct_roles: Option<&HashMap<i64, StructRole>>,
    page_size: Option<[f32; 2]>,
) -> Page {
    // Prefer the source MediaBox and retain the item-derived extent as a
    // defensive floor for malformed page dictionaries.
    let derived_top = items.iter().map(|i| i.y + i.height).fold(0.0_f32, f32::max);
    let derived_right = items.iter().map(|i| i.x + i.width).fold(0.0_f32, f32::max);
    let page_top = page_size
        .map_or(derived_top, |size| size[1])
        .max(derived_top);
    let page_right = page_size
        .map_or(derived_right, |size| size[0])
        .max(derived_right);
    let (images, mut items): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|item| matches!(item.item_type, ItemType::Image));

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

    let mut blocks: Vec<Block> = lines
        .into_iter()
        .map(|mut line| {
            line.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
            build_line_block(&line, page_top, struct_roles)
        })
        .collect();

    blocks.extend(
        images
            .iter()
            .map(|image| build_image_block(image, page_top)),
    );
    blocks.sort_by(|a, b| {
        let a_box = a.bbox_px.unwrap_or([0; 4]);
        let b_box = b.bbox_px.unwrap_or([0; 4]);
        a_box[1].cmp(&b_box[1]).then(a_box[0].cmp(&b_box[0]))
    });
    demote_inferred_formulas_inside_assets(&mut blocks);
    bind_asset_captions(&mut blocks);

    Page {
        page_num,
        width_px: page_right.ceil().max(0.0) as u32,
        height_px: page_top.ceil().max(0.0) as u32,
        blocks,
    }
}

fn build_image_block(item: &TextItem, page_top: f32) -> Block {
    let x0 = item.x.min(item.x + item.width).max(0.0);
    let x1 = item.x.max(item.x + item.width).max(x0);
    let pdf_y0 = item.y.min(item.y + item.height);
    let pdf_y1 = item.y.max(item.y + item.height);
    let y0 = (page_top - pdf_y1).max(0.0);
    let y1 = (page_top - pdf_y0).max(y0);
    Block {
        geom: Geometry::Rect([x0, y0, x1, y1]),
        geom_frame: CoordFrame::Page,
        bbox_px: Some([
            x0.round() as i32,
            y0.round() as i32,
            x1.round() as i32,
            y1.round() as i32,
        ]),
        category_raw: if item.text.starts_with("[Scanned page:") {
            "ScannedPage"
        } else if item.text.starts_with("[Vector chart:") {
            "VectorChart"
        } else {
            "ImageXObject"
        }
        .to_owned(),
        category: Some(
            if item.text.starts_with("[Vector chart:") {
                "chart"
            } else {
                "image"
            }
            .to_owned(),
        ),
        reading_order: None,
        text: None,
        html: None,
        latex: None,
        spans: Vec::new(),
        merge_hint: None,
        confidence: None,
        source: BlockSource::NativeTextLayer,
        error: None,
        asset_bytes: None,
        asset_path: None,
        asset_caption: None,
    }
}

fn scanned_page_image_item(page: u32, size: [f32; 2]) -> TextItem {
    TextItem {
        text: format!("[Scanned page: {page}]"),
        x: 0.0,
        y: 0.0,
        width: size[0],
        height: size[1],
        font: String::new(),
        font_size: 0.0,
        page,
        is_bold: false,
        is_italic: false,
        is_underline: false,
        is_strikeout: false,
        item_type: ItemType::Image,
        mcid: None,
    }
}

fn vector_chart_item(page: u32, region: [f32; 4], page_size: [f32; 2]) -> TextItem {
    const PAD: f32 = 20.0;
    let x0 = (region[0].min(region[2]) - PAD).clamp(0.0, page_size[0]);
    let y0 = (region[1].min(region[3]) - PAD).clamp(0.0, page_size[1]);
    let x1 = (region[0].max(region[2]) + PAD).clamp(x0, page_size[0]);
    let y1 = (region[1].max(region[3]) + PAD).clamp(y0, page_size[1]);
    TextItem {
        text: format!("[Vector chart: {page}]"),
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
        font: String::new(),
        font_size: 0.0,
        page,
        is_bold: false,
        is_italic: false,
        is_underline: false,
        is_strikeout: false,
        item_type: ItemType::Image,
        mcid: None,
    }
}

/// Assemble one coherent line (its already-x-sorted items) into a `Block`,
/// flipping PDF coords to top-left pixel space via `page_top`.
fn build_line_block(
    line: &[TextItem],
    page_top: f32,
    struct_roles: Option<&HashMap<i64, StructRole>>,
) -> Block {
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

    let semantic_category = line_semantic_category(line, struct_roles);
    let source_formula = semantic_category.is_some_and(|(_, category)| category == "equation");
    let inferred_formula_confidence = (!source_formula)
        .then(|| untagged_formula_confidence(line, &text))
        .flatten();
    let is_formula = source_formula || inferred_formula_confidence.is_some();

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
                is_inline_formula: is_formula,
            }
        })
        .collect();

    let (category_raw, category) = semantic_category.unwrap_or_else(|| {
        if inferred_formula_confidence.is_some() {
            ("Formula", "equation")
        } else {
            ("text", "text")
        }
    });

    Block {
        geom: Geometry::Rect([x0, y0, x1, y1]),
        geom_frame: CoordFrame::Page,
        bbox_px: Some(bbox_px),
        category_raw: category_raw.to_string(),
        category: Some(category.to_string()),
        reading_order: None,
        text: Some(text),
        html: None,
        latex: None,
        spans,
        merge_hint: None,
        confidence: inferred_formula_confidence,
        source: BlockSource::NativeTextLayer,
        error: None,
        asset_bytes: None,
        asset_path: None,
        asset_caption: None,
    }
}

fn untagged_formula_confidence(line: &[TextItem], text: &str) -> Option<f32> {
    let trimmed = text.trim();
    let character_count = trimmed.chars().filter(|ch| !ch.is_whitespace()).count();
    if !(3..=500).contains(&character_count) || trimmed.contains("://") || trimmed.contains('@') {
        return None;
    }

    let math_font_characters: usize = line
        .iter()
        .filter(|item| is_math_font(&item.font))
        .map(|item| item.text.chars().filter(|ch| !ch.is_whitespace()).count())
        .sum();
    let math_font_ratio = math_font_characters as f32 / character_count as f32;
    let operator_count = trimmed.chars().filter(|ch| is_math_operator(*ch)).count();
    let symbolic_fragments = line
        .iter()
        .filter(|item| is_short_symbolic_fragment(&item.text))
        .count();
    let equation_number = line
        .last()
        .is_some_and(|item| is_equation_number(item.text.trim()));
    let script_offset = has_script_offset(line);
    let prose_words = trimmed
        .split_whitespace()
        .filter(|word| {
            let letters = word.chars().filter(|ch| ch.is_alphabetic()).count();
            letters >= 4 && letters * 4 >= word.chars().count() * 3
        })
        .count();

    let strong_math_font = math_font_ratio >= 0.35;
    let geometry_formula = script_offset && operator_count >= 1 && symbolic_fragments >= 3;
    let numbered_formula = equation_number && operator_count >= 1 && symbolic_fragments >= 3;
    let font_formula =
        strong_math_font && (operator_count >= 1 || (script_offset && symbolic_fragments >= 2));
    if !(font_formula || geometry_formula || numbered_formula) {
        return None;
    }
    if prose_words >= 3 {
        return None;
    }

    let mut confidence: f32 = 0.62;
    if strong_math_font {
        confidence += 0.14;
    }
    if operator_count >= 1 {
        confidence += 0.08;
    }
    if operator_count >= 2 {
        confidence += 0.04;
    }
    if script_offset {
        confidence += 0.06;
    }
    if equation_number {
        confidence += 0.06;
    }
    Some(confidence.min(0.96))
}

fn is_math_font(font: &str) -> bool {
    let lower = font.to_ascii_lowercase();
    [
        "cmmi", "cmsy", "cmex", "lmmi", "lmsy", "lmex", "math", "symbol", "msam", "msbm", "rsfs",
        "eufm", "stmary", "wasy", "mtmi", "mtsy", "txmi", "txsy", "stix", "asana",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn is_math_operator(ch: char) -> bool {
    matches!(
        ch,
        '=' | '<'
            | '>'
            | '+'
            | '-'
            | '*'
            | '/'
            | '^'
            | '\u{00b1}'
            | '\u{00d7}'
            | '\u{00f7}'
            | '\u{2212}'
            | '\u{2211}'
            | '\u{220f}'
            | '\u{222b}'
            | '\u{221a}'
            | '\u{221e}'
            | '\u{2248}'
            | '\u{2260}'
            | '\u{2264}'
            | '\u{2265}'
            | '\u{226a}'
            | '\u{226b}'
            | '\u{2208}'
            | '\u{2209}'
            | '\u{2282}'
            | '\u{2283}'
            | '\u{2286}'
            | '\u{2287}'
            | '\u{2192}'
            | '\u{2194}'
    )
}

fn is_short_symbolic_fragment(text: &str) -> bool {
    let compact: Vec<char> = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    !compact.is_empty()
        && compact.len() <= 8
        && compact
            .iter()
            .all(|ch| ch.is_alphanumeric() || is_math_operator(*ch) || "()[]{}_,.;".contains(*ch))
}

fn is_equation_number(text: &str) -> bool {
    let inner = text
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .or_else(|| {
            text.strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
        });
    inner.is_some_and(|value| {
        !value.is_empty() && value.len() <= 4 && value.chars().all(|ch| ch.is_ascii_digit())
    })
}

fn has_script_offset(line: &[TextItem]) -> bool {
    let base_size = line
        .iter()
        .map(|item| item.font_size)
        .filter(|size| *size > 0.0)
        .max_by(f32::total_cmp)
        .unwrap_or(0.0);
    if base_size <= 0.0 {
        return false;
    }
    let baseline = line
        .iter()
        .filter(|item| item.font_size >= base_size * 0.9)
        .map(|item| item.y)
        .next()
        .unwrap_or(line[0].y);
    line.iter().any(|item| {
        item.font_size > 0.0
            && item.font_size <= base_size * 0.82
            && (item.y - baseline).abs() >= base_size * 0.12
    })
}

fn demote_inferred_formulas_inside_assets(blocks: &mut [Block]) {
    let asset_boxes: Vec<[i32; 4]> = blocks
        .iter()
        .filter(|block| matches!(block.category.as_deref(), Some("image" | "chart")))
        .filter_map(|block| block.bbox_px)
        .collect();
    for block in blocks
        .iter_mut()
        .filter(|block| block.category.as_deref() == Some("equation") && block.confidence.is_some())
    {
        let Some(formula_box) = block.bbox_px else {
            continue;
        };
        if !asset_boxes
            .iter()
            .any(|asset_box| bbox_overlap_ratio(formula_box, *asset_box) >= 0.8)
        {
            continue;
        }
        block.category_raw = "text".into();
        block.category = Some("text".into());
        block.confidence = None;
        for span in &mut block.spans {
            span.is_inline_formula = false;
        }
    }
}

fn bbox_overlap_ratio(inner: [i32; 4], outer: [i32; 4]) -> f32 {
    let width = (inner[2] - inner[0]).max(0);
    let height = (inner[3] - inner[1]).max(0);
    let area = width.saturating_mul(height);
    if area == 0 {
        return 0.0;
    }
    let overlap_width = (inner[2].min(outer[2]) - inner[0].max(outer[0])).max(0);
    let overlap_height = (inner[3].min(outer[3]) - inner[1].max(outer[1])).max(0);
    overlap_width.saturating_mul(overlap_height) as f32 / area as f32
}

fn bind_asset_captions(blocks: &mut [Block]) {
    #[derive(Debug, Clone, Copy)]
    struct Candidate {
        caption_index: usize,
        asset_index: usize,
        score: f32,
        confidence: f32,
    }

    let captions: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| is_figure_caption_block(block).then_some(index))
        .collect();
    let assets: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            matches!(block.category_raw.as_str(), "VectorChart" | "ImageXObject").then_some(index)
        })
        .collect();

    let mut candidates = Vec::new();
    for &caption_index in &captions {
        let Some(caption_box) = blocks[caption_index].bbox_px else {
            continue;
        };
        for &asset_index in &assets {
            let Some(asset_box) = blocks[asset_index].bbox_px else {
                continue;
            };
            if let Some((score, confidence)) = caption_asset_score(caption_box, asset_box) {
                candidates.push(Candidate {
                    caption_index,
                    asset_index,
                    score,
                    confidence,
                });
            }
        }
    }
    candidates.sort_by(|a, b| a.score.total_cmp(&b.score));

    let mut used_captions = std::collections::HashSet::new();
    let mut used_assets = std::collections::HashSet::new();
    let mut primary_asset_by_caption = HashMap::new();
    for candidate in &candidates {
        if used_captions.contains(&candidate.caption_index)
            || used_assets.contains(&candidate.asset_index)
        {
            continue;
        }
        let Some(relation) =
            asset_caption_relation(blocks, candidate.caption_index, candidate.confidence)
        else {
            continue;
        };
        blocks[candidate.asset_index].asset_caption = Some(relation);
        used_captions.insert(candidate.caption_index);
        used_assets.insert(candidate.asset_index);
        primary_asset_by_caption.insert(candidate.caption_index, candidate.asset_index);
    }

    // A single caption can describe a row of sibling image XObjects (for
    // example two simulation panels exported as separate PDF images). Share
    // the relation only across the same visual row; vertically stacked assets
    // still require independent captions.
    for candidate in candidates {
        if used_assets.contains(&candidate.asset_index) {
            continue;
        }
        let Some(&primary_asset) = primary_asset_by_caption.get(&candidate.caption_index) else {
            continue;
        };
        let (Some(primary_box), Some(candidate_box)) = (
            blocks[primary_asset].bbox_px,
            blocks[candidate.asset_index].bbox_px,
        ) else {
            continue;
        };
        if !assets_are_horizontal_siblings(primary_box, candidate_box) {
            continue;
        }
        let Some(relation) =
            asset_caption_relation(blocks, candidate.caption_index, candidate.confidence)
        else {
            continue;
        };
        blocks[candidate.asset_index].asset_caption = Some(relation);
        used_assets.insert(candidate.asset_index);
    }
}

fn asset_caption_relation(
    blocks: &[Block],
    caption_index: usize,
    confidence: f32,
) -> Option<AssetCaption> {
    let caption = blocks.get(caption_index)?;
    let mut text = caption_text_with_label_boundary(caption)?;
    if text.is_empty() {
        return None;
    }
    let mut bbox = caption.bbox_px?;
    let initial_x = bbox[0];
    let initial_width = (bbox[2] - bbox[0]).max(1);
    let initial_font_size = block_font_size(caption);

    if !caption_text_is_complete(&text) {
        for continuation in blocks.iter().skip(caption_index + 1).take(12) {
            if matches!(continuation.category.as_deref(), Some("image" | "chart")) {
                break;
            }
            let Some(next_text) = continuation
                .text
                .as_deref()
                .map(str::trim)
                .filter(|next| !next.is_empty())
            else {
                break;
            };
            if is_figure_caption_block(continuation)
                || is_table_caption(next_text)
                || is_source_or_note(next_text)
            {
                break;
            }
            let Some(next_box) = continuation.bbox_px else {
                break;
            };
            let line_height = (bbox[3] - bbox[1]).max(1) as f32;
            let vertical_gap = next_box[1] - bbox[3];
            let x_aligned = next_box[0] >= initial_x - 5
                && next_box[0] <= initial_x + (initial_width as f32 * 0.18).max(24.0) as i32;
            let font_compatible = match (initial_font_size, block_font_size(continuation)) {
                (Some(first), Some(next)) => (first - next).abs() <= first.max(next) * 0.25,
                _ => true,
            };
            if vertical_gap < -2
                || vertical_gap as f32 > (line_height * 1.25).max(6.0)
                || !x_aligned
                || !font_compatible
            {
                break;
            }
            text.push(' ');
            text.push_str(next_text);
            bbox = [
                bbox[0].min(next_box[0]),
                bbox[1].min(next_box[1]),
                bbox[2].max(next_box[2]),
                bbox[3].max(next_box[3]),
            ];
            if caption_text_is_complete(&text) {
                break;
            }
        }
    }

    Some(AssetCaption {
        text,
        bbox_px: Some(bbox),
        confidence,
    })
}

fn caption_text_with_label_boundary(block: &Block) -> Option<String> {
    let mut text = block.text.as_deref()?.trim().to_owned();
    let label = block.spans.first()?.text.trim();
    if looks_like_numbered_figure_caption(label)
        && text.starts_with(label)
        && text[label.len()..]
            .chars()
            .next()
            .is_some_and(|ch| !ch.is_whitespace())
    {
        text.insert(label.len(), ' ');
    }
    Some(text)
}

fn block_font_size(block: &Block) -> Option<f32> {
    let mut sizes = block.spans.iter().filter_map(|span| span.font_size);
    let first = sizes.next()?;
    let (sum, count) = sizes.fold((first, 1_u32), |(sum, count), size| (sum + size, count + 1));
    Some(sum / count as f32)
}

fn caption_text_is_complete(text: &str) -> bool {
    text.trim_end()
        .ends_with(['.', '!', '?', '\u{3002}', '\u{ff01}', '\u{ff1f}'])
}

fn assets_are_horizontal_siblings(first: [i32; 4], second: [i32; 4]) -> bool {
    let overlap = (first[3].min(second[3]) - first[1].max(second[1])).max(0) as f32;
    let min_height = (first[3] - first[1]).min(second[3] - second[1]).max(1) as f32;
    let horizontal_gap = if first[2] < second[0] {
        second[0] - first[2]
    } else if second[2] < first[0] {
        first[0] - second[2]
    } else {
        0
    };
    overlap / min_height >= 0.7 && horizontal_gap <= 48
}

fn is_figure_caption_block(block: &Block) -> bool {
    let Some(text) = block
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    else {
        return false;
    };
    if is_table_caption(text) || is_source_or_note(text) {
        return false;
    }
    looks_like_numbered_figure_caption(text) || block.category.as_deref() == Some("caption")
}

fn is_table_caption(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    ["table ", "table.", "tabela ", "tab. "]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        || text.trim_start().starts_with('\u{8868}')
}

fn is_source_or_note(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    ["source:", "source ", "note:", "notes:", "fonte:", "nota:"]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

fn looks_like_numbered_figure_caption(text: &str) -> bool {
    let trimmed = text.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    let rest = [
        "figure ",
        "figure\u{a0}",
        "fig. ",
        "fig ",
        "figura ",
        "chart ",
        "graph ",
        "diagram ",
        "image ",
        "photo ",
    ]
    .iter()
    .find_map(|prefix| lower.strip_prefix(prefix));
    if let Some(rest) = rest {
        return caption_identifier_starts(rest);
    }
    for prefix in ['\u{56fe}', '\u{5716}'] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return caption_identifier_starts(rest.trim_start());
        }
    }
    false
}

fn caption_identifier_starts(rest: &str) -> bool {
    rest.trim_start()
        .starts_with(|ch: char| ch.is_ascii_digit() || ch == '(' || ch == '#')
}

fn caption_asset_score(caption: [i32; 4], asset: [i32; 4]) -> Option<(f32, f32)> {
    let caption_width = (caption[2] - caption[0]).max(1) as f32;
    let asset_width = (asset[2] - asset[0]).max(1) as f32;
    let overlap = (caption[2].min(asset[2]) - caption[0].max(asset[0])).max(0) as f32;
    let overlap_ratio = overlap / caption_width.min(asset_width);
    let caption_center = (caption[0] + caption[2]) as f32 / 2.0;
    let asset_center = (asset[0] + asset[2]) as f32 / 2.0;
    let center_offset = (caption_center - asset_center).abs() / asset_width.max(1.0);
    if overlap_ratio < 0.25 && center_offset > 0.45 {
        return None;
    }

    let vertical_gap = if caption[1] > asset[3] {
        (caption[1] - asset[3]) as f32
    } else if asset[1] > caption[3] {
        (asset[1] - caption[3]) as f32
    } else {
        0.0
    };
    if vertical_gap > 72.0 {
        return None;
    }

    // Captions below a figure are conventional; captions above remain valid
    // but receive a small penalty. Intersections occur when the asset crop's
    // safety padding intentionally includes the caption baseline.
    let above_penalty = if caption[3] <= asset[1] { 12.0 } else { 0.0 };
    let alignment_penalty = center_offset * 30.0 + (1.0 - overlap_ratio) * 12.0;
    let score = vertical_gap + above_penalty + alignment_penalty;
    let confidence = (0.98 - vertical_gap / 180.0 - center_offset * 0.25).clamp(0.55, 0.98);
    Some((score, confidence))
}

fn item_struct_role<'a>(
    item: &TextItem,
    struct_roles: Option<&'a HashMap<i64, StructRole>>,
) -> Option<&'a StructRole> {
    struct_roles?.get(&item.mcid?)
}

fn line_semantic_category<'a>(
    line: &[TextItem],
    struct_roles: Option<&'a HashMap<i64, StructRole>>,
) -> Option<(&'static str, &'static str)> {
    const PRIORITIES: &[(fn(&StructRole) -> bool, &str, &str)] = &[
        (|role| matches!(role, StructRole::Formula), "Formula", "equation"),
        (|role| matches!(role, StructRole::Note), "Note", "footnote"),
        (
            |role| matches!(role, StructRole::Reference | StructRole::BibEntry),
            "Reference",
            "reference",
        ),
        (|role| matches!(role, StructRole::Caption), "Caption", "caption"),
        (|role| matches!(role, StructRole::Code), "Code", "code"),
        (
            |role| matches!(role, StructRole::LI | StructRole::Lbl | StructRole::LBody),
            "List-item",
            "list",
        ),
    ];
    let roles: Vec<&StructRole> = line
        .iter()
        .filter_map(|item| item_struct_role(item, struct_roles))
        .collect();
    PRIORITIES
        .iter()
        .find(|(matches_role, _, _)| roles.iter().any(|role| matches_role(role)))
        .map(|(_, raw, normalized)| (*raw, *normalized))
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
        &[
            "text",
            "image",
            "chart",
            "equation",
            "footnote",
            "reference",
            "caption",
            "code",
            "list",
        ]
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

    fn text_item(
        text: &str,
        x: f32,
        y: f32,
        width: f32,
        page: u32,
        item_type: ItemType,
    ) -> TextItem {
        TextItem {
            text: text.to_owned(),
            x,
            y,
            width,
            height: 10.0,
            font: "TestFont".to_owned(),
            font_size: 10.0,
            page,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type,
            mcid: None,
        }
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
        assert!(adapter.category_vocab().contains(&"image"));
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

    #[test]
    fn document_errors_have_stable_machine_readable_stages() {
        use uparser_document_engine::{DocumentError, DocumentFormat};

        let cases = [
            (
                DocumentError::UnsupportedFormat(DocumentFormat::Unknown),
                "native_document.unsupported_format",
            ),
            (DocumentError::Encrypted, "native_document.encrypted"),
            (
                DocumentError::ResourceLimit {
                    limit: "bytes",
                    detail: "too large".to_owned(),
                },
                "native_document.resource_limit",
            ),
            (
                DocumentError::MissingPart {
                    part: "document.xml".to_owned(),
                },
                "native_document.missing_part",
            ),
            (
                DocumentError::Malformed {
                    part: Some("document.xml".to_owned()),
                    detail: "bad XML".to_owned(),
                },
                "native_document.malformed",
            ),
            (
                DocumentError::Io(std::io::Error::other("read failed")),
                "native_document.io",
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(document_error_stage(&error), expected);
        }
    }

    #[test]
    fn positioned_items_are_filtered_grouped_and_mapped_to_page_geometry() {
        let pages = build_pages(
            vec![
                text_item("[Image: mask]", 0.0, 0.0, 1.0, 1, ItemType::Image),
                text_item("   ", 0.0, 0.0, 1.0, 1, ItemType::Text),
                text_item("World", 35.0, 90.0, 25.0, 1, ItemType::Text),
                text_item("Hello", 10.0, 90.0, 20.0, 1, ItemType::Text),
                text_item("Lower", 5.0, 60.0, 30.0, 1, ItemType::FormField),
                text_item(
                    "Second page",
                    4.0,
                    20.0,
                    50.0,
                    2,
                    ItemType::Link("https://example.com".to_owned()),
                ),
            ],
            &HashMap::new(),
            &HashMap::new(),
        );

        assert_eq!(pages.len(), 2);
        assert_eq!((pages[0].width_px, pages[0].height_px), (60, 100));
        assert_eq!(pages[0].blocks.len(), 2);
        assert_eq!(pages[0].blocks[0].text.as_deref(), Some("Hello World"));
        assert_eq!(pages[0].blocks[0].bbox_px, Some([10, 0, 60, 10]));
        assert_eq!(pages[0].blocks[0].spans.len(), 2);
        assert_eq!(pages[0].blocks[1].text.as_deref(), Some("Lower"));
        assert_eq!(pages[1].page_num, 2);
        assert_eq!(pages[1].blocks[0].text.as_deref(), Some("Second page"));
    }

    #[test]
    fn nearby_numbered_caption_is_bound_to_image_asset() {
        let mut image = text_item("[Image: plot]", 50.0, 300.0, 300.0, 1, ItemType::Image);
        image.height = 180.0;
        let caption_label = text_item("Figure 2", 70.0, 280.0, 40.0, 1, ItemType::Text);
        let caption_body = text_item(
            "Accuracy by workload.",
            110.0,
            280.0,
            210.0,
            1,
            ItemType::Text,
        );

        let pages = build_pages(
            vec![image, caption_label, caption_body],
            &HashMap::new(),
            &HashMap::from([(1, [420.0, 500.0])]),
        );
        let asset = pages[0]
            .blocks
            .iter()
            .find(|block| block.category_raw == "ImageXObject")
            .unwrap();
        let relation = asset.asset_caption.as_ref().unwrap();
        assert_eq!(relation.text, "Figure 2 Accuracy by workload.");
        assert!(relation.confidence >= 0.8);
    }

    #[test]
    fn tagged_caption_without_prefix_can_bind_but_table_caption_cannot() {
        let mut image = text_item("[Image: diagram]", 50.0, 300.0, 300.0, 1, ItemType::Image);
        image.height = 180.0;
        let mut caption = text_item(
            "System architecture overview",
            70.0,
            280.0,
            250.0,
            1,
            ItemType::Text,
        );
        caption.mcid = Some(9);
        let roles = HashMap::from([(1, HashMap::from([(9, StructRole::Caption)]))]);
        let pages = build_pages(
            vec![image, caption],
            &roles,
            &HashMap::from([(1, [420.0, 500.0])]),
        );
        let asset = pages[0]
            .blocks
            .iter()
            .find(|block| block.category_raw == "ImageXObject")
            .unwrap();
        assert_eq!(
            asset.asset_caption.as_ref().map(|link| link.text.as_str()),
            Some("System architecture overview")
        );

        let mut image = text_item("[Image: table]", 50.0, 300.0, 300.0, 1, ItemType::Image);
        image.height = 180.0;
        let table_caption = text_item(
            "Table 2: Accuracy by workload",
            70.0,
            280.0,
            250.0,
            1,
            ItemType::Text,
        );
        let pages = build_pages(
            vec![image, table_caption],
            &HashMap::new(),
            &HashMap::from([(1, [420.0, 500.0])]),
        );
        let asset = pages[0]
            .blocks
            .iter()
            .find(|block| block.category_raw == "ImageXObject")
            .unwrap();
        assert!(asset.asset_caption.is_none());
    }

    #[test]
    fn caption_matching_is_one_to_one_and_distance_bounded() {
        let mut left = text_item("[Image: left]", 30.0, 300.0, 170.0, 1, ItemType::Image);
        left.height = 150.0;
        let mut right = text_item("[Image: right]", 260.0, 300.0, 170.0, 1, ItemType::Image);
        right.height = 150.0;
        let left_caption = text_item(
            "Figure 1 Left result",
            40.0,
            280.0,
            150.0,
            1,
            ItemType::Text,
        );
        let right_caption = text_item(
            "Figure 2 Right result",
            270.0,
            270.0,
            150.0,
            1,
            ItemType::Text,
        );
        let far_caption = text_item(
            "Figure 3 Distant result",
            40.0,
            80.0,
            150.0,
            1,
            ItemType::Text,
        );
        let body_reference = text_item(
            "Figure 4 shows the ablation results.",
            40.0,
            60.0,
            250.0,
            1,
            ItemType::Text,
        );

        let pages = build_pages(
            vec![
                left,
                right,
                left_caption,
                right_caption,
                far_caption,
                body_reference,
            ],
            &HashMap::new(),
            &HashMap::from([(1, [460.0, 500.0])]),
        );
        let mut links: Vec<_> = pages[0]
            .blocks
            .iter()
            .filter(|block| block.category_raw == "ImageXObject")
            .filter_map(|block| block.asset_caption.as_ref().map(|link| link.text.as_str()))
            .collect();
        links.sort_unstable();
        assert_eq!(links, ["Figure 1 Left result", "Figure 2 Right result"]);
    }

    #[test]
    fn wrapped_caption_is_shared_by_horizontal_sibling_assets() {
        let mut left = text_item(
            "[Image: left panel]",
            50.0,
            300.0,
            145.0,
            1,
            ItemType::Image,
        );
        left.height = 150.0;
        let mut right = text_item(
            "[Image: right panel]",
            205.0,
            300.0,
            145.0,
            1,
            ItemType::Image,
        );
        right.height = 150.0;
        let caption = text_item(
            "Fig. 4: Simulation results for the expected number of",
            60.0,
            280.0,
            280.0,
            1,
            ItemType::Text,
        );
        let continuation = text_item(
            "steps under both policies.",
            60.0,
            268.0,
            150.0,
            1,
            ItemType::Text,
        );

        let pages = build_pages(
            vec![left, right, caption, continuation],
            &HashMap::new(),
            &HashMap::from([(1, [400.0, 500.0])]),
        );
        let links: Vec<_> = pages[0]
            .blocks
            .iter()
            .filter(|block| block.category_raw == "ImageXObject")
            .map(|block| block.asset_caption.as_ref().unwrap().text.as_str())
            .collect();
        assert_eq!(links.len(), 2);
        assert!(links.iter().all(|text| {
            *text
                == "Fig. 4: Simulation results for the expected number of steps under both policies."
        }));
    }

    #[test]
    fn tagged_pdf_roles_survive_in_block_and_span_ir() {
        let mut formula = text_item("E = mc2", 10.0, 90.0, 40.0, 1, ItemType::Text);
        formula.mcid = Some(7);
        let mut note = text_item("Method note", 10.0, 60.0, 50.0, 1, ItemType::Text);
        note.mcid = Some(8);
        let mut page_roles = HashMap::new();
        page_roles.insert(7, StructRole::Formula);
        page_roles.insert(8, StructRole::Note);
        let roles = HashMap::from([(1, page_roles)]);

        let pages = build_pages(vec![formula, note], &roles, &HashMap::new());

        assert_eq!(pages[0].blocks[0].category_raw, "Formula");
        assert_eq!(pages[0].blocks[0].category.as_deref(), Some("equation"));
        assert!(pages[0].blocks[0].spans[0].is_inline_formula);
        assert_eq!(pages[0].blocks[0].latex, None);
        assert_eq!(pages[0].blocks[1].category_raw, "Note");
        assert_eq!(pages[0].blocks[1].category.as_deref(), Some("footnote"));
        assert!(!pages[0].blocks[1].spans[0].is_inline_formula);
    }

    #[test]
    fn untagged_math_font_and_script_geometry_create_formula_ir() {
        let mut e = text_item("E", 100.0, 300.0, 8.0, 1, ItemType::Text);
        e.font = "ABCDEE+CMMI10".into();
        let mut equals = text_item("=", 112.0, 300.0, 8.0, 1, ItemType::Text);
        equals.font = "ABCDEE+CMSY10".into();
        let mut m = text_item("m", 124.0, 300.0, 8.0, 1, ItemType::Text);
        m.font = "ABCDEE+CMMI10".into();
        let mut c = text_item("c", 136.0, 300.0, 8.0, 1, ItemType::Text);
        c.font = "ABCDEE+CMMI10".into();
        let mut squared = text_item("2", 144.0, 305.0, 5.0, 1, ItemType::Text);
        squared.font = "ABCDEE+CMR7".into();
        squared.font_size = 7.0;
        squared.height = 7.0;

        let pages = build_pages(
            vec![e, equals, m, c, squared],
            &HashMap::new(),
            &HashMap::from([(1, [300.0, 500.0])]),
        );
        let formula = &pages[0].blocks[0];
        assert_eq!(formula.category_raw, "Formula");
        assert_eq!(formula.category.as_deref(), Some("equation"));
        assert!(formula.confidence.is_some_and(|value| value >= 0.85));
        assert!(formula.spans.iter().all(|span| span.is_inline_formula));
        assert!(
            formula.latex.is_none(),
            "native geometry must not invent LaTeX"
        );
    }

    #[test]
    fn equation_number_and_operator_geometry_work_without_math_font() {
        let items = ["x", "+", "y", "=", "z", "(3)"]
            .into_iter()
            .enumerate()
            .map(|(index, text)| {
                text_item(
                    text,
                    80.0 + index as f32 * 18.0,
                    300.0,
                    10.0,
                    1,
                    ItemType::Text,
                )
            })
            .collect();
        let pages = build_pages(
            items,
            &HashMap::new(),
            &HashMap::from([(1, [300.0, 500.0])]),
        );
        assert_eq!(pages[0].blocks[0].category.as_deref(), Some("equation"));
        assert!(pages[0].blocks[0]
            .confidence
            .is_some_and(|value| value >= 0.8));
    }

    #[test]
    fn prose_table_and_url_lines_are_not_inferred_as_formulas() {
        let prose = text_item(
            "Figure 4 shows x = 2 in the experiment.",
            50.0,
            300.0,
            250.0,
            1,
            ItemType::Text,
        );
        let table_row =
            ["Method", "Score", "=", "2"]
                .into_iter()
                .enumerate()
                .map(|(index, text)| {
                    text_item(
                        text,
                        50.0 + index as f32 * 55.0,
                        270.0,
                        45.0,
                        1,
                        ItemType::Text,
                    )
                });
        let url = text_item(
            "https://example.test/search?q=x+y",
            50.0,
            240.0,
            220.0,
            1,
            ItemType::Text,
        );
        let pages = build_pages(
            std::iter::once(prose)
                .chain(table_row)
                .chain(std::iter::once(url))
                .collect(),
            &HashMap::new(),
            &HashMap::from([(1, [320.0, 500.0])]),
        );
        assert_eq!(pages[0].blocks.len(), 3);
        assert!(pages[0]
            .blocks
            .iter()
            .all(|block| block.category.as_deref() == Some("text")));

        let mut math_font_prose = vec![
            text_item("where", 50.0, 200.0, 35.0, 1, ItemType::Text),
            text_item("probability", 90.0, 200.0, 65.0, 1, ItemType::Text),
            text_item("vector", 160.0, 200.0, 40.0, 1, ItemType::Text),
            text_item("equals", 205.0, 200.0, 38.0, 1, ItemType::Text),
            text_item("x", 248.0, 200.0, 8.0, 1, ItemType::Text),
            text_item("=", 260.0, 200.0, 8.0, 1, ItemType::Text),
            text_item("2", 272.0, 200.0, 8.0, 1, ItemType::Text),
        ];
        for item in &mut math_font_prose {
            item.font = "ABCDEE+CMMI10".into();
        }
        let pages = build_pages(
            math_font_prose,
            &HashMap::new(),
            &HashMap::from([(1, [320.0, 500.0])]),
        );
        assert_eq!(pages[0].blocks[0].category.as_deref(), Some("text"));
    }

    #[test]
    fn inferred_formula_inside_image_asset_is_demoted() {
        let mut image = text_item("[Image: chart]", 50.0, 250.0, 250.0, 1, ItemType::Image);
        image.height = 100.0;
        let mut x = text_item("x", 100.0, 300.0, 8.0, 1, ItemType::Text);
        x.font = "ABCDEE+CMMI10".into();
        let mut equals = text_item("=", 112.0, 300.0, 8.0, 1, ItemType::Text);
        equals.font = "ABCDEE+CMSY10".into();
        let mut value = text_item("2", 124.0, 300.0, 8.0, 1, ItemType::Text);
        value.font = "ABCDEE+CMMI10".into();

        let pages = build_pages(
            vec![image, x, equals, value],
            &HashMap::new(),
            &HashMap::from([(1, [320.0, 500.0])]),
        );
        let label = pages[0]
            .blocks
            .iter()
            .find(|block| block.text.as_deref().is_some_and(|text| text.contains('=')))
            .unwrap();
        assert_eq!(label.category.as_deref(), Some("text"));
        assert!(label.confidence.is_none());
        assert!(label.spans.iter().all(|span| !span.is_inline_formula));
    }

    #[test]
    fn image_xobject_uses_real_page_frame_and_survives_as_asset_block() {
        let image = text_item("[Image: Im0]", 72.0, 500.0, 144.0, 1, ItemType::Image);
        let pages = build_pages(
            vec![image],
            &HashMap::new(),
            &HashMap::from([(1, [612.0, 792.0])]),
        );

        assert_eq!((pages[0].width_px, pages[0].height_px), (612, 792));
        assert_eq!(pages[0].blocks.len(), 1);
        let block = &pages[0].blocks[0];
        assert_eq!(block.category.as_deref(), Some("image"));
        assert_eq!(block.bbox_px, Some([72, 282, 216, 292]));
        assert_eq!(block.text, None);
        assert_eq!(block.asset_bytes, None);
    }

    #[test]
    fn explicit_scanned_page_becomes_a_full_page_asset_block() {
        let item = scanned_page_image_item(3, [612.0, 792.0]);
        let pages = build_pages(
            vec![item],
            &HashMap::new(),
            &HashMap::from([(3, [612.0, 792.0])]),
        );

        assert_eq!(pages[0].page_num, 3);
        assert_eq!(pages[0].blocks[0].category_raw, "ScannedPage");
        assert_eq!(pages[0].blocks[0].bbox_px, Some([0, 0, 612, 792]));
    }

    #[test]
    fn scanned_reason_materializes_but_blank_no_text_reason_does_not() {
        use uparser_native_engine::{LayoutComplexity, PageOcrReasons, PdfProcessResult, PdfType};
        let artifact = PdfProcessResult {
            pdf_type: PdfType::Mixed,
            markdown: None,
            page_count: 2,
            processing_time_ms: 1,
            pages_needing_ocr: vec![1, 2],
            ocr_reasons_by_page: vec![
                PageOcrReasons {
                    page: 1,
                    reasons: vec![uparser_native_engine::OCR_REASON_SCANNED.to_owned()],
                },
                PageOcrReasons {
                    page: 2,
                    reasons: vec![uparser_native_engine::OCR_REASON_NO_TEXT.to_owned()],
                },
            ],
            title: None,
            confidence: 0.9,
            layout: LayoutComplexity::default(),
            has_encoding_issues: false,
            positioned_items: vec![],
            struct_roles: HashMap::new(),
            page_sizes: HashMap::from([(1, [612.0, 792.0]), (2, [612.0, 792.0])]),
            chart_regions: HashMap::new(),
        };

        let (result, _) = NativeAdapter::parse_pdf_artifact("scan.pdf", b"%PDF", artifact);

        assert_eq!(result.pages.len(), 1);
        assert_eq!(result.pages[0].page_num, 1);
        assert_eq!(result.pages[0].blocks[0].category_raw, "ScannedPage");
    }

    #[test]
    fn vector_chart_region_becomes_a_padded_chart_asset_block() {
        let item = vector_chart_item(1, [100.0, 200.0, 300.0, 400.0], [612.0, 792.0]);
        let pages = build_pages(
            vec![item],
            &HashMap::new(),
            &HashMap::from([(1, [612.0, 792.0])]),
        );

        let block = &pages[0].blocks[0];
        assert_eq!(block.category_raw, "VectorChart");
        assert_eq!(block.category.as_deref(), Some("chart"));
        assert_eq!(block.bbox_px, Some([80, 372, 320, 612]));
    }

    #[tokio::test]
    async fn malformed_native_inputs_return_typed_errors() {
        let adapter = NativeAdapter;
        let pdf = b"%PDF-1.7\nnot a valid PDF";

        let parse_error = adapter
            .parse_document("broken.pdf", pdf)
            .await
            .expect_err("broken PDF must fail");
        assert_eq!(parse_error.stage.as_deref(), Some("native"));

        let markdown_error = adapter
            .native_markdown("broken.pdf", pdf)
            .await
            .expect_err("broken PDF markdown must fail");
        assert_eq!(markdown_error.stage.as_deref(), Some("native"));

        let json_error = adapter
            .native_document_json("broken.pdf", pdf)
            .await
            .expect_err("PDF document-json must be rejected");
        assert_eq!(json_error.stage.as_deref(), Some("native_document"));

        let structured_error = adapter
            .parse_document("broken.docx", b"not a zip package")
            .await
            .expect_err("broken DOCX must fail");
        assert!(
            structured_error
                .stage
                .as_deref()
                .is_some_and(|stage| stage.starts_with("native_document"))
        );
    }

    #[tokio::test]
    async fn structured_parse_enforces_caller_resource_limits() {
        let mut options = uparser_document_engine::ParseOptions::default();
        options.limits.max_input_bytes = 4;
        let error = match NativeAdapter
            .parse_native("large.csv", b"name,value\nalpha,42\n", &options)
            .await
        {
            Ok(_) => panic!("input over the configured limit must fail"),
            Err(error) => error,
        };
        assert_eq!(
            error.stage.as_deref(),
            Some("native_document.resource_limit")
        );
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
