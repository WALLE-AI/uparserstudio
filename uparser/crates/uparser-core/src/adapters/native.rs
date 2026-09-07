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
    AssetCaption, Block, BlockSource, CoordFrame, CoordinateSystem, Geometry, MergeHint, Page,
    PageError, ParseResult, RoutedBy, Span,
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
            &artifact.structure_hints,
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
}

/// Group all pages' positioned items into `Page`s of coherent line-`Block`s.
fn build_pages(
    items: Vec<TextItem>,
    struct_roles: &HashMap<u32, HashMap<i64, StructRole>>,
    page_sizes: &HashMap<u32, [f32; 2]>,
    hints: &uparser_native_engine::StructureHints,
) -> Vec<Page> {
    let mut by_page: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
    for it in items {
        if matches!(it.item_type, ItemType::Image) && !is_substantive_image_item(&it) {
            continue;
        }
        // A link item is an annotation rectangle whose `text` is the target
        // URL, sitting on top of the anchor text it decorates — not content
        // of its own. Treating it as text glues the raw URL onto the words
        // it links ("Affordable Courseshttps://example.org/..."), which is
        // why the engine routes these out of the item stream before it ever
        // groups lines. The anchor's own underline already survives in
        // `Span.style`.
        if matches!(it.item_type, ItemType::Link(_)) {
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
                hints,
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
    hints: &uparser_native_engine::StructureHints,
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

    let lines = group_lines(page_num, items, hints);

    // The engine's tables come straight from its hints rather than from
    // re-recognising them in the line stream: a table's rows are not among
    // the lines it grouped (they were consumed by the table), and the hint
    // already carries the cells, the kind and the position the writer gave
    // it. `order == None` means the writer detected the table but never
    // emitted it, so neither should this.
    let table_blocks: Vec<Block> = hints
        .tables
        .iter()
        .filter(|hint| hint.page == page_num && hint.order.is_some())
        .flat_map(|hint| build_table_blocks(hint, page_top))
        .collect();
    let blocks: Vec<Block> = lines
        .into_iter()
        .enumerate()
        .filter_map(|(order, (hint_order, mut line))| {
            line.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
            let line_hint =
                hint_order.and_then(|wanted| hints.lines.iter().find(|line| line.order == wanted));
            let line_bbox = pdf_bbox(&line);
            // A line a table already claimed is not also a paragraph. The
            // engine usually removes a table's items before grouping lines,
            // but not on every detection path, and emitting both the table
            // block and its rows as prose duplicates the whole table.
            let line_text: String = line.iter().map(|item| item.text.as_str()).collect();
            if line_bbox.is_some_and(|bbox| hints.table_at(page_num, bbox, &line_text).is_some()) {
                return None;
            }
            let mut block = build_line_block(&line, page_top, struct_roles, line_hint);
            // A heading the engine committed to in its Markdown pass. Only
            // applied when the struct tree did not already say otherwise —
            // an explicit PDF tag outranks a visual heuristic.
            if block.category.as_deref() != Some("title")
                && let Some(bbox) = line_bbox
                && let Some(heading) = hints.heading_at(page_num, bbox)
            {
                block.category_raw = "Title".to_owned();
                block.category = Some("title".to_owned());
                block.merge_hint = Some(MergeHint::TitleLevel(heading.level));
            }
            block.reading_order = Some(order as u32);
            Some(block)
        })
        .collect();
    // A table's recorded order is the position of the line it was emitted
    // *before*, so it has to sort ahead of that line at the same order.
    // Carrying the distinction alongside the block keeps it out of the IR,
    // where a doubled or fractional `reading_order` would be a rank nothing
    // else in the codebase means.
    let mut blocks: Vec<(bool, Block)> = blocks.into_iter().map(|block| (false, block)).collect();
    blocks.extend(table_blocks.into_iter().map(|block| (true, block)));

    blocks.extend(
        images
            .iter()
            .map(|image| (false, build_image_block(image, page_top))),
    );
    // Reading order beats geometry on a multi-column page: sorting by
    // top-to-bottom/left-to-right interleaves the columns, which is exactly
    // the error the engine's column detection exists to avoid.
    //
    // Blocks without an order (tables, images) are anchored to the last
    // ordered block above them, so they keep their place in the flow. This
    // has to be one key for every block: comparing some pairs by order and
    // others by geometry is not a total order, and `sort_by` aborts the
    // process when it detects that.
    let mut anchors: Vec<(u32, i32)> = blocks
        .iter()
        .filter_map(|(_, block)| Some((block.reading_order?, block.bbox_px.unwrap_or([0; 4])[1])))
        .collect();
    anchors.sort_by_key(|(order, _)| *order);
    let sort_key = |(is_table, block): &(bool, Block)| -> (u32, u8, i32, i32) {
        let bbox = block.bbox_px.unwrap_or([0; 4]);
        match block.reading_order {
            Some(order) if *is_table => (order, 0, bbox[1], bbox[0]),
            Some(order) => (order, 1, bbox[1], bbox[0]),
            None => match anchors
                .iter()
                .filter(|(_, top)| *top <= bbox[1])
                .map(|(order, _)| *order)
                .max()
            {
                Some(anchor) => (anchor, 2, bbox[1], bbox[0]),
                None => (0, 0, bbox[1], bbox[0]),
            },
        }
    };
    blocks.sort_by_key(sort_key);
    let mut blocks: Vec<Block> = blocks.into_iter().map(|(_, block)| block).collect();
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
/// Group a page's items into lines.
///
/// Prefers the engine's own grouping (`StructureHints::lines`), which is
/// column-aware: clustering by vertical proximity alone fuses the left and
/// right columns of a two-column page into one line, which corrupts the text
/// itself rather than merely mislabelling it. Items the engine did not place
/// — and every item when no hints exist, e.g. `ProcessMode::Analyze` — fall
/// back to the proximity clustering this used to do exclusively.
fn group_lines(
    page_num: u32,
    items: Vec<TextItem>,
    hints: &uparser_native_engine::StructureHints,
) -> Vec<(Option<usize>, Vec<TextItem>)> {
    let mut hinted: BTreeMap<usize, Vec<TextItem>> = BTreeMap::new();
    let mut unplaced: Vec<TextItem> = Vec::new();
    for item in items {
        let center = [item.x + item.width / 2.0, item.y + item.height / 2.0];
        match hints.line_at(page_num, center) {
            Some(line) => hinted.entry(line.order).or_default().push(item),
            None => unplaced.push(item),
        }
    }

    let mut lines: Vec<(Option<usize>, Vec<TextItem>)> = hinted
        .into_iter()
        .map(|(order, items)| (Some(order), items))
        .collect();
    // On a page the engine did group, an item it left out is one it decided
    // does not belong in the body: a link annotation's raw URL, a page
    // number, a repeated header. Keeping those produces text the engine's
    // own Markdown never contains — a URL glued onto the anchor text it
    // annotates, a stray footnote digit before the first paragraph — so the
    // fallback is for pages with no grouping at all (`ProcessMode::Analyze`,
    // where there is no decision to respect), not for leftovers on a page
    // that has one.
    if hints.lines.iter().any(|line| line.page == page_num) {
        return lines;
    }
    let mut current: Vec<TextItem> = Vec::new();
    for item in unplaced {
        let center = item.y + item.height / 2.0;
        let same_line = current.last().is_some_and(|last| {
            let last_center = last.y + last.height / 2.0;
            let tolerance = (item.height.max(last.height) * 0.6).max(1.0);
            (last_center - center).abs() <= tolerance
        });
        if same_line {
            current.push(item);
        } else {
            if !current.is_empty() {
                lines.push((None, std::mem::take(&mut current)));
            }
            current.push(item);
        }
    }
    if !current.is_empty() {
        lines.push((None, current));
    }
    lines
}

/// Bounding box of a line in PDF coordinates (origin bottom-left), which is
/// the frame `structure_export`'s hints are expressed in.
fn pdf_bbox(line: &[TextItem]) -> Option<[f32; 4]> {
    let first = line.first()?;
    let mut bbox = [
        first.x,
        first.y,
        first.x + first.width,
        first.y + first.height,
    ];
    for item in &line[1..] {
        bbox[0] = bbox[0].min(item.x);
        bbox[1] = bbox[1].min(item.y);
        bbox[2] = bbox[2].max(item.x + item.width);
        bbox[3] = bbox[3].max(item.y + item.height);
    }
    Some(bbox)
}

/// One `table`-category block carrying the engine-detected grid as HTML.
///
/// The IR has no table type of its own; `html` is how every other protocol
/// carries a table, so the same renderer handles all of them.
/// Blocks for one engine-detected table.
///
/// A table of contents is not a data table: the engine renders it as a flat
/// tab-aligned list because a two-column Markdown table drifts the page
/// numbers away from their titles. `TableKind` already carries that
/// distinction, so the IR honours it rather than flattening both into a grid.
fn build_table_blocks(hint: &uparser_native_engine::TableHint, page_top: f32) -> Vec<Block> {
    if hint.kind == uparser_native_engine::tables::TableKind::Toc {
        return hint
            .cells
            .iter()
            .filter(|row| row.iter().any(|cell| !cell.trim().is_empty()))
            .map(|row| {
                let text = row
                    .iter()
                    .map(|cell| cell.trim())
                    .filter(|cell| !cell.is_empty())
                    .collect::<Vec<_>>()
                    .join("\t");
                Block {
                    category_raw: "TocEntry".to_owned(),
                    category: Some("text".to_owned()),
                    text: Some(text),
                    ..table_block_skeleton(hint, page_top)
                }
            })
            .collect();
    }
    vec![build_table_block(hint, page_top)]
}

/// Geometry and provenance shared by every block derived from a table hint.
fn table_block_skeleton(hint: &uparser_native_engine::TableHint, page_top: f32) -> Block {
    let bbox_px = [
        hint.bbox[0].round() as i32,
        (page_top - hint.bbox[3]).round() as i32,
        hint.bbox[2].round() as i32,
        (page_top - hint.bbox[1]).round() as i32,
    ];
    Block {
        geom: Geometry::Rect([
            bbox_px[0] as f32,
            bbox_px[1] as f32,
            bbox_px[2] as f32,
            bbox_px[3] as f32,
        ]),
        geom_frame: CoordFrame::Page,
        bbox_px: Some(bbox_px),
        category_raw: String::new(),
        category: None,
        // Where the engine's writer put this table in the flow. A table's
        // own geometry does not say where it is read: on a two-column page
        // sorting by `y` moves every table to the bottom of the page, which
        // is what the engine's own column-aware placement exists to avoid.
        reading_order: hint.order.map(|order| order as u32),
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

fn build_table_block(hint: &uparser_native_engine::TableHint, page_top: f32) -> Block {
    let mut html = String::from("<table>");
    for row in &hint.cells {
        html.push_str("<tr>");
        for cell in row {
            html.push_str("<td>");
            html.push_str(&crate::otsl::escape_html(cell));
            html.push_str("</td>");
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");

    Block {
        category_raw: "Table".to_owned(),
        category: Some("table".to_owned()),
        html: Some(html),
        ..table_block_skeleton(hint, page_top)
    }
}

fn build_line_block(
    line: &[TextItem],
    page_top: f32,
    struct_roles: Option<&HashMap<i64, StructRole>>,
    hint: Option<&uparser_native_engine::LineHint>,
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

    // Prefer the engine's own join. Its spacing rules are the accumulated
    // answer to letter-spaced runs, CID fonts, sub/superscripts and
    // hyphenation; a plain gap threshold disagrees with them wherever items
    // are small and dense — a table of contents' dot leader becomes
    // ". . . . . . ." instead of "..............". Only usable when the
    // pieces line up 1:1 with the items this consumer assigned to the line,
    // since each piece is what its item contributed; otherwise fall back to
    // the gap rule so a mismatch degrades instead of misattributing text.
    let engine_pieces = hint
        .map(|hint| hint.item_texts.as_slice())
        .filter(|pieces| pieces.len() == line.len());
    let pieces: Vec<String> = match engine_pieces {
        Some(pieces) => pieces.to_vec(),
        None => line
            .iter()
            .enumerate()
            .scan(String::new(), |seen, (idx, it)| {
                let mut piece = String::new();
                if idx > 0 {
                    let prev = &line[idx - 1];
                    let gap = it.x - (prev.x + prev.width);
                    let boundary = !seen.ends_with(' ') && !it.text.starts_with(' ');
                    if boundary && gap > it.font_size * 0.15 {
                        piece.push(' ');
                    }
                }
                piece.push_str(&it.text);
                seen.push_str(&piece);
                Some(piece)
            })
            .collect(),
    };
    // The engine's own final cleanup, which a consumer reading its structure
    // instead of its string would otherwise never get: dot leaders, double
    // spaces and hyphen-split compounds are all repaired there, after the
    // line text exists. Spans are deliberately left as they were — the
    // consumer-side rule is that spans are only trusted when they still
    // concatenate to the text, so a line this rewrites drops its inline
    // styling rather than mapping it onto the wrong characters.
    let text: String = uparser_native_engine::clean_text_fragment(&pieces.concat());

    let semantic_category = line_semantic_category(line, struct_roles);
    let source_formula = semantic_category.is_some_and(|(_, category)| category == "equation");
    let inferred_formula_confidence = (!source_formula)
        .then(|| untagged_formula_confidence(line, &text))
        .flatten();
    let is_formula = source_formula || inferred_formula_confidence.is_some();

    let spans: Vec<Span> = line
        .iter()
        .zip(pieces.iter())
        .map(|(it, piece)| {
            let sy0 = page_top - (it.y + it.height);
            let sy1 = page_top - it.y;
            Span {
                // The PDF font's own flags — the only place inline styling
                // exists in a PDF, and until now discarded on the way into
                // the IR.
                style: crate::types::SpanStyle {
                    bold: it.is_bold,
                    italic: it.is_italic,
                    underline: it.is_underline,
                    strike: it.is_strikeout,
                },
                // The item's contribution to the joined text, separator
                // included, so the spans still concatenate to `text` — the
                // condition a consumer checks before trusting them.
                text: piece.clone(),
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
    // A tagged PDF names its heading level; carry it so the IR (and every
    // renderer driven by it) can reproduce the hierarchy. Untagged PDFs —
    // most of a real corpus — have no such tag, and the engine's own
    // visual heading heuristic lives in its Markdown pipeline rather than
    // in the item stream, so those still arrive here as plain text. That
    // gap is why `--markdown-source canonical` scores far below `engine`
    // on opendataloader-bench; see O5.3.
    let merge_hint = heading_level(line, struct_roles).map(MergeHint::TitleLevel);

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
        merge_hint,
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

/// The heading level a tagged PDF declares for this line, if any.
/// A bare `H` (level-less heading tag) is reported as level 1.
fn heading_level(line: &[TextItem], struct_roles: Option<&HashMap<i64, StructRole>>) -> Option<u8> {
    line.iter()
        .filter_map(|item| item_struct_role(item, struct_roles))
        .find_map(|role| match role {
            StructRole::H | StructRole::H1 => Some(1),
            StructRole::H2 => Some(2),
            StructRole::H3 => Some(3),
            StructRole::H4 => Some(4),
            StructRole::H5 => Some(5),
            StructRole::H6 => Some(6),
            _ => None,
        })
}

fn line_semantic_category<'a>(
    line: &[TextItem],
    struct_roles: Option<&'a HashMap<i64, StructRole>>,
) -> Option<(&'static str, &'static str)> {
    const PRIORITIES: &[(fn(&StructRole) -> bool, &str, &str)] = &[
        (
            |role| matches!(role, StructRole::Formula),
            "Formula",
            "equation",
        ),
        (|role| matches!(role, StructRole::Note), "Note", "footnote"),
        (
            |role| matches!(role, StructRole::Reference | StructRole::BibEntry),
            "Reference",
            "reference",
        ),
        (
            |role| matches!(role, StructRole::Caption),
            "Caption",
            "caption",
        ),
        (|role| matches!(role, StructRole::Code), "Code", "code"),
        (
            |role| {
                matches!(
                    role,
                    StructRole::H
                        | StructRole::H1
                        | StructRole::H2
                        | StructRole::H3
                        | StructRole::H4
                        | StructRole::H5
                        | StructRole::H6
                )
            },
            "Title",
            "title",
        ),
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

    /// Regression guard for the line clustering in `build_page`: native must
    /// map one `Block` per coherent *line*, not one per raw span, or every
    /// downstream consumer sees a page shredded into fragments.
    ///
    /// Retargeted in O5 from the deleted `NativeAdapter::parse_document`
    /// wrapper onto `parse_pdf_artifact`, which is the path the runner
    /// actually takes.
    #[test]
    fn pdf_artifact_yields_coherent_multiword_lines_not_span_fragments() {
        let path = fixture_pdf_path();
        if !std::path::Path::new(&path).exists() {
            eprintln!("skipping: no fixture PDF at {path}");
            return;
        }
        let bytes = std::fs::read(&path).expect("read fixture PDF");
        let artifact = uparser_native_engine::process_pdf_mem(&bytes).expect("engine parses");
        let (result, markdown) = NativeAdapter::parse_pdf_artifact(&path, &bytes, artifact);

        assert!(markdown.is_some(), "engine markdown must come back too");
        let blocks: Vec<&Block> = result
            .pages
            .iter()
            .flat_map(|page| &page.blocks)
            .filter(|block| block.text.is_some())
            .collect();
        assert!(!blocks.is_empty());
        let multiword = blocks
            .iter()
            .filter(|block| {
                block
                    .text
                    .as_deref()
                    .is_some_and(|text| text.split_whitespace().count() > 1)
            })
            .count();
        assert!(
            multiword * 2 > blocks.len(),
            "most blocks must be whole lines, got {multiword}/{}",
            blocks.len()
        );
    }

    /// EPUB is the one structured format whose units are chapters rather
    /// than pages or sheets; the lowering must preserve that, since it is
    /// what makes per-chapter reading order meaningful.
    ///
    /// Retargeted in O5 onto the live `document engine -> structured::to_parse_result`
    /// path that the runner uses.
    #[test]
    fn epub_lowers_chapter_units_through_the_live_structured_path() {
        let bytes = zip_package(&[
            ("mimetype", "application/epub+zip"),
            (
                "META-INF/container.xml",
                "<container><rootfiles><rootfile full-path=\"book.opf\"/></rootfiles></container>",
            ),
            (
                "book.opf",
                "<package><manifest><item id=\"c1\" href=\"c1.xhtml\"                  media-type=\"application/xhtml+xml\"/></manifest>                 <spine><itemref idref=\"c1\"/></spine></package>",
            ),
            (
                "c1.xhtml",
                "<html><body><h1>Chapter One</h1><p>Body text.</p></body></html>",
            ),
        ]);
        let document = uparser_document_engine::parse_document(
            &bytes,
            uparser_document_engine::DocumentFormat::Epub,
            &uparser_document_engine::ParseOptions::default(),
        )
        .expect("epub parses");
        assert_eq!(
            document.units[0].kind,
            uparser_document_engine::UnitKind::Chapter
        );

        let result = crate::structured::to_parse_result(&document, "book.epub", &bytes);
        assert_eq!(result.protocol, "native:epub");
        assert!(
            result.pages[0]
                .blocks
                .iter()
                .any(|block| block.category.as_deref() == Some("title"))
        );
    }

    /// Blocks carry a reading order only when the engine placed their line;
    /// tables and images do not. Comparing ordered pairs by order and mixed
    /// pairs by geometry is not a total order, and Rust's sort detects that
    /// and aborts the process — this reproduces the shape that crashed.
    #[test]
    fn mixed_ordered_and_unordered_blocks_sort_without_panicking() {
        let page_size = HashMap::from([(1u32, [612.0_f32, 792.0_f32])]);
        let mut items = Vec::new();
        for row in 0..12 {
            let y = 700.0 - row as f32 * 20.0;
            items.push(text_item(
                &format!("line {row}"),
                50.0,
                y,
                10.0,
                1,
                ItemType::Text,
            ));
            items.push(text_item(
                "[Image: fig]",
                300.0,
                y,
                40.0,
                1,
                ItemType::Image,
            ));
        }
        // No hints: every text block falls back to proximity clustering and
        // gets an order, while every image block has none.
        let pages = build_pages(items, &HashMap::new(), &page_size, &Default::default());
        assert_eq!(pages.len(), 1);
        assert!(pages[0].blocks.len() > 1);
    }

    /// A two-column page reads column by column, so a table near the top of
    /// the right-hand column comes *late* in the flow even though it is high
    /// on the page. Placing it by geometry sinks or floats it away from the
    /// text that introduces it; the writer's own position is the only thing
    /// that knows where it goes.
    #[test]
    fn a_table_is_placed_where_the_writer_put_it_not_where_its_geometry_is() {
        use uparser_native_engine::{LineHint, StructureHints, TableHint};

        let line = |order: usize, text: &str, y: f32, x: f32| LineHint {
            page: 1,
            bbox: [x, y, x + 200.0, y + 10.0],
            order,
            text: text.to_owned(),
            item_texts: vec![text.to_owned()],
        };
        let hints = StructureHints {
            headings: Vec::new(),
            // Emitted between the second and third lines, i.e. after both of
            // the left column's lines — but its box is above the last one.
            tables: vec![TableHint {
                page: 1,
                bbox: [320.0, 600.0, 520.0, 640.0],
                rows: 1,
                columns: 1,
                cells: vec![vec!["cell".to_owned()]],
                kind: uparser_native_engine::tables::TableKind::Data,
                order: Some(2),
            }],
            lines: vec![
                line(0, "left top", 700.0, 60.0),
                line(1, "left bottom", 500.0, 60.0),
                line(2, "right below table", 560.0, 320.0),
            ],
            ..Default::default()
        };
        let items = vec![
            text_item("left top", 60.0, 700.0, 100.0, 1, ItemType::Text),
            text_item("left bottom", 60.0, 500.0, 100.0, 1, ItemType::Text),
            text_item("right below table", 320.0, 560.0, 100.0, 1, ItemType::Text),
        ];

        let pages = build_pages(
            items,
            &HashMap::new(),
            &HashMap::from([(1u32, [612.0_f32, 792.0_f32])]),
            &hints,
        );

        let flow: Vec<&str> = pages[0]
            .blocks
            .iter()
            .map(|block| match (&block.text, &block.html) {
                (Some(text), _) => text.as_str(),
                (None, Some(_)) => "<table>",
                _ => "?",
            })
            .collect();
        assert_eq!(
            flow,
            ["left top", "left bottom", "<table>", "right below table"]
        );
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
                // A link annotation sits on top of the words it decorates.
                // Its `text` is the target URL, so admitting it as content
                // glues the URL into the sentence.
                text_item(
                    "https://example.com",
                    10.0,
                    90.0,
                    45.0,
                    1,
                    ItemType::Link("https://example.com".to_owned()),
                ),
                text_item("Second page", 4.0, 20.0, 50.0, 2, ItemType::Text),
            ],
            &HashMap::new(),
            &HashMap::new(),
            &Default::default(),
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
            &Default::default(),
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
            &Default::default(),
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
            &Default::default(),
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
            &Default::default(),
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
            &Default::default(),
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

    /// A tagged PDF's heading level reaches the IR (O5.2/O5.3): without it
    /// the canonical renderer sees only paragraphs and every
    /// heading-hierarchy metric collapses to zero.
    #[test]
    fn tagged_heading_roles_become_titles_carrying_their_level() {
        let mut h1 = text_item("Chapter", 10.0, 120.0, 60.0, 1, ItemType::Text);
        h1.mcid = Some(1);
        let mut h3 = text_item("Subsection", 10.0, 90.0, 70.0, 1, ItemType::Text);
        h3.mcid = Some(3);
        let mut body = text_item("Body text", 10.0, 60.0, 60.0, 1, ItemType::Text);
        body.mcid = Some(4);
        let page_roles =
            HashMap::from([(1, StructRole::H1), (3, StructRole::H3), (4, StructRole::P)]);
        let pages = build_pages(
            vec![h1, h3, body],
            &HashMap::from([(1, page_roles)]),
            &HashMap::new(),
            &Default::default(),
        );

        let blocks = &pages[0].blocks;
        assert_eq!(blocks[0].category.as_deref(), Some("title"));
        assert_eq!(blocks[0].merge_hint, Some(MergeHint::TitleLevel(1)));
        assert_eq!(blocks[1].category.as_deref(), Some("title"));
        assert_eq!(blocks[1].merge_hint, Some(MergeHint::TitleLevel(3)));
        assert_eq!(blocks[2].category.as_deref(), Some("text"));
        assert_eq!(blocks[2].merge_hint, None);
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

        let pages = build_pages(
            vec![formula, note],
            &roles,
            &HashMap::new(),
            &Default::default(),
        );

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
            &Default::default(),
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
            &Default::default(),
        );
        assert_eq!(pages[0].blocks[0].category.as_deref(), Some("equation"));
        assert!(
            pages[0].blocks[0]
                .confidence
                .is_some_and(|value| value >= 0.8)
        );
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
            &Default::default(),
        );
        assert_eq!(pages[0].blocks.len(), 3);
        assert!(
            pages[0]
                .blocks
                .iter()
                .all(|block| block.category.as_deref() == Some("text"))
        );

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
            &Default::default(),
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
            &Default::default(),
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
            &Default::default(),
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
            &Default::default(),
        );

        assert_eq!(pages[0].page_num, 3);
        assert_eq!(pages[0].blocks[0].category_raw, "ScannedPage");
        assert_eq!(pages[0].blocks[0].bbox_px, Some([0, 0, 612, 792]));
    }

    #[test]
    fn scanned_reason_materializes_but_blank_no_text_reason_does_not() {
        use uparser_native_engine::{LayoutComplexity, PageOcrReasons, PdfProcessResult, PdfType};
        let artifact = PdfProcessResult {
            structure_hints: Default::default(),
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
            &Default::default(),
        );

        let block = &pages[0].blocks[0];
        assert_eq!(block.category_raw, "VectorChart");
        assert_eq!(block.category.as_deref(), Some("chart"));
        assert_eq!(block.bbox_px, Some([80, 372, 320, 612]));
    }
}
