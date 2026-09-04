//! Ascending map: geometric `Page`/`Block` IR → semantic `CanonicalDocument`.
//!
//! `structured.rs` goes the other way, lowering a `CanonicalDocument` a
//! structured frontend already produced into the compatibility IR. This
//! module is its inverse, and it is what lets the model protocols — which
//! natively produce blocks with boxes, not documents with semantics — reach
//! the one Markdown renderer and the `document-json` output format.
//!
//! It is deliberately a *recovery*, not a round-trip: a VLM emits a block
//! saying "this region is a title", and the best we can do is rebuild the
//! `Heading` that block implies. Anything the block does not carry (inline
//! emphasis, links, note references) cannot be invented and is not.

use crate::types::{Block as IrBlock, MergeHint, ParseResult};
use uparser_document_engine::{
    Asset, Block as DocBlock, CanonicalDocument, Cell, CellSlot, CellValueKind, DocumentFormat,
    DocumentUnit, Inline, List, ListItem, ListMarker, ParseWarning, Style, Table, TableKind,
    UnitKind, WarningCode,
};

pub fn to_canonical_document(result: &ParseResult, format: DocumentFormat) -> CanonicalDocument {
    let mut document = CanonicalDocument::new(format);
    document.metadata.variant = Some(result.protocol.clone());

    for (index, page) in result.pages.iter().enumerate() {
        let mut unit = DocumentUnit::new(UnitKind::Page, index, None);
        unit.blocks = ascend_blocks(&page.blocks, &mut document.assets, &mut document.warnings);
        document.units.push(unit);
    }
    for warning in &result.warnings {
        document.warnings.push(ParseWarning {
            code: WarningCode::UnsupportedFeature,
            part: None,
            message: warning.clone(),
        });
    }
    document
}

fn ascend_blocks(
    blocks: &[IrBlock],
    assets: &mut Vec<Asset>,
    warnings: &mut Vec<ParseWarning>,
) -> Vec<DocBlock> {
    let mut out: Vec<DocBlock> = Vec::new();
    let mut index = 0;
    while index < blocks.len() {
        let block = &blocks[index];
        // A model protocol's `text` is content the model *wrote*, which may
        // already use Markdown syntax — mineru-vlm routinely emits a bullet
        // inside a `text` block and an empty `list` block as its container.
        // The canonical renderer escapes plain text (correctly, for a
        // structured source), so a leading `- ` would come out as `\-` and
        // stop being a list. Recovering the list here keeps the IR truthful
        // *and* renders correctly; it is the same recovery this module does
        // for every other signal.
        if is_list_line(block) {
            // Consecutive list-category blocks are one list, not N one-item
            // lists — the renderer's tight/loose decision depends on it.
            let end = blocks[index..]
                .iter()
                .position(|candidate| !is_list_line(candidate))
                .map(|offset| index + offset)
                .unwrap_or(blocks.len());
            out.push(ascend_list(&blocks[index..end]));
            index = end;
            continue;
        }
        // An empty `list` block is a container the model emitted around the
        // items that follow; it carries nothing to render.
        if block.category.as_deref() == Some("list")
            && block
                .text
                .as_deref()
                .is_none_or(|text| text.trim().is_empty())
        {
            index += 1;
            continue;
        }
        if let Some(doc_block) = ascend_block(block, assets, warnings) {
            out.push(doc_block);
        }
        index += 1;
    }
    out
}

/// Whether this block is one item of a list — either because the protocol
/// classified it as such, or because its text opens with a Markdown list
/// marker the model wrote itself.
fn is_list_line(block: &IrBlock) -> bool {
    let Some(text) = block.text.as_deref().map(str::trim_start) else {
        return false;
    };
    if text.is_empty() {
        return false;
    }
    if block.category.as_deref() == Some("list") {
        return true;
    }
    block.category.as_deref() == Some("text") && leading_list_marker(text).is_some()
}

/// Length of the leading Markdown list marker (`- `, `* `, `• `, `1. `), if any.
fn leading_list_marker(text: &str) -> Option<usize> {
    for bullet in ["- ", "* ", "+ ", "• ", "· "] {
        if let Some(rest) = text.strip_prefix(bullet)
            && !rest.trim_start().is_empty()
        {
            return Some(bullet.len());
        }
    }
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    if (1..=3).contains(&digits) {
        let rest = &text[digits..];
        for separator in [". ", ") "] {
            if let Some(after) = rest.strip_prefix(separator)
                && !after.trim_start().is_empty()
            {
                return Some(digits + separator.len());
            }
        }
    }
    None
}

/// An item's text with its Markdown marker removed — the canonical `List`
/// carries the marker as structure, so leaving it in the text would render
/// it twice.
fn list_item_text(block: &IrBlock) -> String {
    let text = block.text.as_deref().unwrap_or("").trim_start();
    match leading_list_marker(text) {
        Some(offset) => text[offset..].trim_start().to_owned(),
        None => text.to_owned(),
    }
}

fn ascend_list(items: &[IrBlock]) -> DocBlock {
    let ordered = items.iter().any(|item| {
        matches!(
            item.merge_hint,
            Some(MergeHint::ListItem { ordered: true, .. })
        )
    }) || items.iter().all(|item| {
        item.text
            .as_deref()
            .map(str::trim_start)
            .is_some_and(|text| text.starts_with(|c: char| c.is_ascii_digit()))
    });
    let start = items.iter().find_map(|item| match item.merge_hint {
        Some(MergeHint::ListItem { number, .. }) => number,
        _ => None,
    });
    List {
        marker: if ordered {
            ListMarker::Decimal
        } else {
            ListMarker::Bullet
        },
        start: ordered.then_some(start.unwrap_or(1)),
        items: items
            .iter()
            .map(|item| ListItem {
                blocks: vec![DocBlock::paragraph(list_item_text(item))],
            })
            .collect(),
    }
    .into_block()
}

trait IntoBlock {
    fn into_block(self) -> DocBlock;
}

impl IntoBlock for List {
    fn into_block(self) -> DocBlock {
        DocBlock::List { list: self }
    }
}

fn ascend_block(
    block: &IrBlock,
    assets: &mut Vec<Asset>,
    warnings: &mut Vec<ParseWarning>,
) -> Option<DocBlock> {
    if let Some(html) = &block.html {
        return Some(match parse_html_table(html) {
            Some(table) => DocBlock::Table { table },
            None => {
                warnings.push(ParseWarning {
                    code: WarningCode::TruncatedContent,
                    part: Some("table".to_owned()),
                    message: "embedded table HTML could not be recovered into a canonical table; \
                              kept as text"
                        .to_owned(),
                });
                DocBlock::paragraph(strip_tags(html))
            }
        });
    }
    if let Some(latex) = &block.latex {
        // The canonical model has no formula block, so a formula is a
        // paragraph carrying the delimiters the Markdown renderer would have
        // emitted anyway — same convention as `render::to_markdown`.
        let wrapped = if block.category.as_deref() == Some("equation_inline") {
            format!("${latex}$")
        } else {
            format!("$$\n{latex}\n$$")
        };
        return Some(DocBlock::paragraph(wrapped));
    }
    if let Some(text) = &block.text {
        let content = inline_content(block, text);
        return Some(match block.category.as_deref() {
            Some("title") => DocBlock::Heading {
                level: match &block.merge_hint {
                    Some(MergeHint::TitleLevel(level)) => (*level).clamp(1, 6),
                    _ => 1,
                },
                content,
            },
            _ => DocBlock::Paragraph { content },
        });
    }
    if let Some(path) = &block.asset_path {
        let id = format!("asset-{}", assets.len() + 1);
        assets.push(Asset {
            id: id.clone(),
            media_type: media_type_for(path),
            filename: std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            byte_length: 0,
            sha256: String::new(),
            path: Some(path.clone()),
            bytes: None,
        });
        return Some(DocBlock::Figure {
            asset_id: Some(id),
            alt: block.asset_caption.as_ref().map(|c| c.text.clone()),
            caption: block
                .asset_caption
                .as_ref()
                .map(|caption| vec![plain(&caption.text)])
                .unwrap_or_default(),
        });
    }
    None
}

/// Inline runs for a block, recovering character styling from its spans.
///
/// A block's `text` is the concatenation of its spans, so styled runs can be
/// rebuilt by walking the spans and merging neighbours that share a style.
/// Falls back to one unstyled run when the protocol emitted no spans (every
/// model protocol) or when they do not reconstruct the text — the block's
/// `text` is authoritative, spans are an enrichment.
fn inline_content(block: &IrBlock, text: &str) -> Vec<Inline> {
    if block.spans.is_empty()
        || block
            .spans
            .iter()
            .all(|span| span.style == Default::default())
    {
        return vec![plain(text)];
    }
    let rebuilt: String = block.spans.iter().map(|span| span.text.as_str()).collect();
    if rebuilt.trim() != text.trim() {
        // Postprocess rewrote the text (paragraph merge, punctuation
        // normalization); the spans no longer describe it.
        return vec![plain(text)];
    }

    let mut runs: Vec<(crate::types::SpanStyle, String)> = Vec::new();
    for span in &block.spans {
        match runs.last_mut() {
            Some((style, buffer)) if *style == span.style => buffer.push_str(&span.text),
            _ => runs.push((span.style, span.text.clone())),
        }
    }
    runs.into_iter()
        .filter(|(_, text)| !text.is_empty())
        .map(|(style, text)| Inline::Text {
            text,
            style: Style {
                bold: style.bold,
                italic: style.italic,
                underline: style.underline,
                strike: style.strike,
                ..Style::default()
            },
        })
        .collect()
}

fn plain(text: &str) -> Inline {
    Inline::Text {
        text: text.to_owned(),
        style: Style::default(),
    }
}

fn media_type_for(path: &str) -> String {
    match std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("jpg") | Some("jpeg") => "image/jpeg".to_owned(),
        Some("gif") => "image/gif".to_owned(),
        Some("webp") => "image/webp".to_owned(),
        _ => "image/png".to_owned(),
    }
}

/// Recover a `Table` from the HTML this project's own table paths emit
/// (`otsl::to_html` and `pipeline_table`'s SLANet/wired renderers).
///
/// Deliberately not a general HTML parser: it handles the constrained subset
/// those two produce — optional `<html><body>` wrapper, `<thead>`/`<tbody>`,
/// `<tr>`, `<td>`/`<th>` with `rowspan`/`colspan` — and returns `None` for
/// anything else so the caller can degrade visibly instead of silently
/// mangling a table it did not understand.
fn parse_html_table(html: &str) -> Option<Table> {
    let lower = html.to_lowercase();
    let start = lower.find("<table")?;
    let end = lower.rfind("</table>")?;
    if end < start {
        return None;
    }
    let body = &html[start..end];

    struct RawCell {
        text: String,
        row_span: usize,
        column_span: usize,
    }

    let mut rows: Vec<Vec<RawCell>> = Vec::new();
    let mut header_rows = 0usize;
    let mut in_head = false;
    let mut cursor = 0usize;
    let lower_body = body.to_lowercase();
    while let Some(offset) = lower_body[cursor..].find('<') {
        let tag_start = cursor + offset;
        let tag_end = lower_body[tag_start..].find('>')? + tag_start;
        let tag = &lower_body[tag_start + 1..tag_end];
        let name = tag
            .split(|c: char| c.is_whitespace() || c == '/')
            .find(|part| !part.is_empty())
            .unwrap_or("");
        match name {
            "thead" if !tag.starts_with('/') => in_head = true,
            "thead" => in_head = false,
            "tr" if !tag.starts_with('/') => {
                rows.push(Vec::new());
                if in_head {
                    header_rows = rows.len();
                }
            }
            "td" | "th" if !tag.starts_with('/') => {
                let close = lower_body[tag_end..]
                    .find(if name == "td" { "</td>" } else { "</th>" })
                    .map(|position| tag_end + position);
                let (text, next) = match close {
                    Some(close) => (body[tag_end + 1..close].to_owned(), close),
                    // An unclosed final cell: take the remainder rather than
                    // dropping the row.
                    None => (body[tag_end + 1..].to_owned(), body.len()),
                };
                if rows.is_empty() {
                    rows.push(Vec::new());
                }
                rows.last_mut()?.push(RawCell {
                    text: decode_entities(&strip_tags(&text)),
                    row_span: attribute(tag, "rowspan").unwrap_or(1).max(1),
                    column_span: attribute(tag, "colspan").unwrap_or(1).max(1),
                });
                cursor = next;
                continue;
            }
            _ => {}
        }
        cursor = tag_end + 1;
    }

    rows.retain(|row| !row.is_empty());
    if rows.is_empty() {
        return None;
    }
    if header_rows == 0 {
        // Markdown cannot express a headerless table, and the renderer's
        // fallback — an empty header row above the real content — invents a
        // row the document does not have. Treating the first row as the
        // header keeps the row count honest; an HTML table that opens with
        // `<td>` rather than `<th>` still reads as a header in practice.
        header_rows = 1;
    }

    // Place cells into a grid, honouring spans: a spanned cell occupies its
    // origin slot and marks the rest `Covered`.
    let mut grid: Vec<Vec<Option<CellSlot>>> = vec![Vec::new(); rows.len()];
    for (row_index, row) in rows.iter().enumerate() {
        for cell in row {
            let column = (0..)
                .find(|column| {
                    grid[row_index]
                        .get(*column)
                        .is_none_or(|slot| slot.is_none())
                })
                .expect("an unbounded search always finds a free column");
            let row_span = cell
                .row_span
                .min(rows.len().saturating_sub(row_index))
                .max(1);
            for row_offset in 0..row_span {
                for column_offset in 0..cell.column_span {
                    let target_row = row_index + row_offset;
                    let target_column = column + column_offset;
                    if target_row >= grid.len() {
                        continue;
                    }
                    if grid[target_row].len() <= target_column {
                        grid[target_row].resize_with(target_column + 1, || None);
                    }
                    grid[target_row][target_column] =
                        Some(if row_offset == 0 && column_offset == 0 {
                            CellSlot::Origin(Cell {
                                row_span,
                                column_span: cell.column_span,
                                value_kind: if cell.text.trim().is_empty() {
                                    CellValueKind::Empty
                                } else {
                                    CellValueKind::Text
                                },
                                formula: None,
                                blocks: if cell.text.trim().is_empty() {
                                    Vec::new()
                                } else {
                                    vec![DocBlock::paragraph(cell.text.trim())]
                                },
                            })
                        } else {
                            CellSlot::Covered {
                                origin_row: row_index,
                                origin_column: column,
                            }
                        });
                }
            }
        }
    }

    let columns = grid.iter().map(|row| row.len()).max().unwrap_or(0);
    if columns == 0 {
        return None;
    }
    let grid: Vec<Vec<CellSlot>> = grid
        .into_iter()
        .map(|row| {
            let mut row: Vec<CellSlot> = row
                .into_iter()
                .map(|slot| {
                    slot.unwrap_or(CellSlot::Origin(Cell {
                        row_span: 1,
                        column_span: 1,
                        value_kind: CellValueKind::Empty,
                        formula: None,
                        blocks: Vec::new(),
                    }))
                })
                .collect();
            row.resize_with(columns, || {
                CellSlot::Origin(Cell {
                    row_span: 1,
                    column_span: 1,
                    value_kind: CellValueKind::Empty,
                    formula: None,
                    blocks: Vec::new(),
                })
            });
            row
        })
        .collect();

    Some(Table {
        kind: TableKind::Data,
        rows: grid.len(),
        columns,
        header_rows: header_rows.min(grid.len()),
        grid,
        caption: None,
    })
}

fn attribute(tag: &str, name: &str) -> Option<usize> {
    let position = tag.find(name)?;
    let rest = &tag[position + name.len()..];
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let rest = rest.trim_start_matches(['"', '\'']);
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut depth = 0usize;
    for character in html.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(character),
            _ => {}
        }
    }
    out.trim().to_owned()
}

fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BlockSource, CoordFrame, Geometry, Page, RoutedBy};
    use std::collections::HashMap;

    fn block(category: &str, text: Option<&str>) -> IrBlock {
        IrBlock {
            geom: Geometry::Rect([0.0, 0.0, 10.0, 10.0]),
            geom_frame: CoordFrame::Page,
            bbox_px: Some([0, 0, 10, 10]),
            category_raw: category.to_owned(),
            category: Some(category.to_owned()),
            reading_order: None,
            text: text.map(str::to_owned),
            html: None,
            latex: None,
            spans: Vec::new(),
            merge_hint: None,
            confidence: None,
            source: BlockSource::OneShotVlm,
            error: None,
            asset_bytes: None,
            asset_path: None,
            asset_caption: None,
        }
    }

    fn result(blocks: Vec<IrBlock>) -> ParseResult {
        ParseResult {
            source_path: "doc.pdf".into(),
            source_sha256: "abc".into(),
            protocol: "mineru-vlm".into(),
            routed_by: RoutedBy::Explicit,
            document_profile: None,
            route_decision: None,
            preprocess_plan: None,
            model_endpoint: None,
            model_name: None,
            pages: vec![Page {
                page_num: 1,
                width_px: 100,
                height_px: 100,
                blocks,
            }],
            page_errors: Vec::new(),
            capability_notes: Vec::new(),
            warnings: Vec::new(),
            timing: HashMap::new(),
        }
    }

    #[test]
    fn categories_become_headings_paragraphs_and_lists() {
        let mut title = block("title", Some("Chapter"));
        title.merge_hint = Some(MergeHint::TitleLevel(2));
        let document = to_canonical_document(
            &result(vec![
                title,
                block("text", Some("Body")),
                block("list", Some("one")),
                block("list", Some("two")),
                block("text", Some("After")),
            ]),
            DocumentFormat::Pdf,
        );

        let blocks = &document.units[0].blocks;
        assert_eq!(blocks.len(), 4, "{blocks:#?}");
        assert!(matches!(blocks[0], DocBlock::Heading { level: 2, .. }));
        assert!(matches!(blocks[1], DocBlock::Paragraph { .. }));
        match &blocks[2] {
            DocBlock::List { list } => assert_eq!(list.items.len(), 2),
            other => panic!("expected one list holding both items, got {other:?}"),
        }
        assert!(matches!(blocks[3], DocBlock::Paragraph { .. }));
    }

    /// The shape every model protocol produces — a classified title plus
    /// body text — must come out as real Markdown structure. Before the
    /// merge this was asserted against a second, `Block`-level renderer;
    /// that renderer is gone, so the assertion is on the output itself.
    #[test]
    fn a_classified_title_and_body_render_as_heading_and_paragraph() {
        let mut title = block("title", Some("Chapter"));
        title.merge_hint = Some(MergeHint::TitleLevel(1));
        let parsed = result(vec![title, block("text", Some("Body text."))]);

        let markdown = uparser_document_engine::render::markdown(&to_canonical_document(
            &parsed,
            DocumentFormat::Pdf,
        ));
        assert_eq!(markdown.trim(), "# Chapter\n\nBody text.");
    }

    /// mineru-vlm writes the bullet into the block's own text and emits an
    /// empty `list` block as a container. Rendering that text as a plain
    /// paragraph escapes the marker (`\-`) and the list stops being a list.
    #[test]
    fn model_written_bullets_are_recovered_as_a_list() {
        let mut container = block("list", Some(""));
        container.merge_hint = None;
        let document = to_canonical_document(
            &result(vec![
                container,
                block("text", Some("- Check tidal conditions beforehand")),
                block("text", Some("- Stay within marked channels")),
                block("text", Some("Ordinary paragraph.")),
            ]),
            DocumentFormat::Pdf,
        );

        let blocks = &document.units[0].blocks;
        assert_eq!(blocks.len(), 2, "{blocks:#?}");
        match &blocks[0] {
            DocBlock::List { list } => assert_eq!(list.items.len(), 2),
            other => panic!("expected a list, got {other:?}"),
        }
        let markdown = uparser_document_engine::render::markdown(&document);
        assert!(
            markdown.contains("- Check tidal conditions beforehand"),
            "{markdown}"
        );
        assert!(
            !markdown.contains("\\-"),
            "the marker must not be escaped: {markdown}"
        );
    }

    #[test]
    fn numbered_lines_written_by_the_model_are_recovered_as_an_ordered_list() {
        let document = to_canonical_document(
            &result(vec![
                block("text", Some("1. First step")),
                block("text", Some("2. Second step")),
            ]),
            DocumentFormat::Pdf,
        );
        let markdown = uparser_document_engine::render::markdown(&document);
        assert!(markdown.contains("1. First step"), "{markdown}");
        assert!(markdown.contains("2. Second step"), "{markdown}");
    }

    /// A hyphen that opens a sentence is not a bullet; requiring the space
    /// and a non-empty remainder keeps ordinary prose out of lists.
    #[test]
    fn text_that_merely_starts_with_punctuation_stays_a_paragraph() {
        for text in ["-30 degrees", "-", "- ", "1.5 million", "1.Introduction"] {
            let document = to_canonical_document(
                &result(vec![block("text", Some(text))]),
                DocumentFormat::Pdf,
            );
            assert!(
                matches!(document.units[0].blocks[0], DocBlock::Paragraph { .. }),
                "{text:?} must stay a paragraph, got {:?}",
                document.units[0].blocks[0]
            );
        }
    }

    /// A PDF's font flags are the only place inline styling exists; without
    /// carrying them through the spans, every emphasis in the document is
    /// flattened to plain text.
    #[test]
    fn span_styles_become_inline_emphasis() {
        let mut styled = block("text", Some("GreenComp is a framework"));
        styled.spans = vec![
            crate::types::Span {
                text: "GreenComp".to_owned(),
                bbox_px: None,
                font_size: None,
                is_inline_formula: false,
                style: crate::types::SpanStyle {
                    italic: true,
                    ..Default::default()
                },
            },
            crate::types::Span {
                text: " is a framework".to_owned(),
                bbox_px: None,
                font_size: None,
                is_inline_formula: false,
                style: Default::default(),
            },
        ];
        let markdown = uparser_document_engine::render::markdown(&to_canonical_document(
            &result(vec![styled]),
            DocumentFormat::Pdf,
        ));
        assert!(markdown.contains("*GreenComp*"), "{markdown}");
        assert!(markdown.contains("is a framework"), "{markdown}");
    }

    /// Spans are an enrichment, never the source of truth: when postprocess
    /// rewrote the text (paragraph merge, punctuation normalization) the
    /// spans no longer describe it and must be ignored.
    #[test]
    fn spans_that_no_longer_match_the_text_are_ignored() {
        let mut stale = block("text", Some("merged paragraph text"));
        stale.spans = vec![crate::types::Span {
            text: "something else entirely".to_owned(),
            bbox_px: None,
            font_size: None,
            is_inline_formula: false,
            style: crate::types::SpanStyle {
                bold: true,
                ..Default::default()
            },
        }];
        let markdown = uparser_document_engine::render::markdown(&to_canonical_document(
            &result(vec![stale]),
            DocumentFormat::Pdf,
        ));
        assert!(markdown.contains("merged paragraph text"), "{markdown}");
        assert!(
            !markdown.contains("**"),
            "stale spans must not add emphasis: {markdown}"
        );
    }

    #[test]
    fn table_html_is_recovered_into_a_canonical_table() {
        let mut table = block("table", None);
        table.html = Some(
            "<table><tr><th>name</th><th>value</th></tr><tr><td>a</td><td>1</td></tr></table>"
                .to_owned(),
        );
        let document = to_canonical_document(&result(vec![table]), DocumentFormat::Pdf);

        match &document.units[0].blocks[0] {
            DocBlock::Table { table } => {
                assert_eq!((table.rows, table.columns), (2, 2));
                assert_eq!(table.header_rows, 1);
            }
            other => panic!("expected a table, got {other:?}"),
        }
        let markdown = uparser_document_engine::render::markdown(&document);
        assert!(markdown.contains("| name | value |"), "{markdown}");
        assert!(markdown.contains("| a | 1 |"), "{markdown}");
    }

    /// A table whose first row uses `<td>` still needs that row as the
    /// Markdown header: the renderer's headerless fallback prepends an empty
    /// row, which invents content the document never had.
    #[test]
    fn a_td_only_table_uses_its_first_row_as_the_header() {
        let table = parse_html_table(
            "<table><tr><td>1</td><td>16 ml</td></tr><tr><td>2</td><td>12 ml</td></tr></table>",
        )
        .expect("table parses");
        assert_eq!(table.header_rows, 1);

        let mut ir = block("table", None);
        ir.html = Some(
            "<table><tr><td>1</td><td>16 ml</td></tr><tr><td>2</td><td>12 ml</td></tr></table>"
                .to_owned(),
        );
        let markdown = uparser_document_engine::render::markdown(&to_canonical_document(
            &result(vec![ir]),
            DocumentFormat::Pdf,
        ));
        let rows: Vec<&str> = markdown
            .lines()
            .filter(|line| line.starts_with('|'))
            .collect();
        assert_eq!(
            rows.len(),
            3,
            "header + separator + one body row: {markdown}"
        );
        assert!(rows[0].contains("16 ml"), "{markdown}");
    }

    #[test]
    fn spans_are_placed_and_marked_covered() {
        let table = parse_html_table(
            "<table><tr><td colspan=\"2\">wide</td></tr><tr><td>a</td><td>b</td></tr></table>",
        )
        .expect("table parses");
        assert_eq!((table.rows, table.columns), (2, 2));
        match &table.grid[0][0] {
            CellSlot::Origin(cell) => assert_eq!(cell.column_span, 2),
            other => panic!("expected the origin cell, got {other:?}"),
        }
        assert!(matches!(
            table.grid[0][1],
            CellSlot::Covered {
                origin_row: 0,
                origin_column: 0
            }
        ));
    }

    #[test]
    fn rowspan_covers_the_following_row() {
        let table = parse_html_table(
            "<html><body><table><tbody><tr><td rowspan=2>tall</td><td>a</td></tr>\
             <tr><td>b</td></tr></tbody></table></body></html>",
        )
        .expect("table parses");
        assert_eq!((table.rows, table.columns), (2, 2));
        assert!(matches!(
            table.grid[1][0],
            CellSlot::Covered {
                origin_row: 0,
                origin_column: 0
            }
        ));
    }

    /// Unparseable HTML degrades to text *with a warning*, never to a
    /// silently mangled table.
    #[test]
    fn unrecoverable_table_html_degrades_visibly() {
        let mut table = block("table", None);
        table.html = Some("<table>truncated".to_owned());
        let document = to_canonical_document(&result(vec![table]), DocumentFormat::Pdf);

        assert!(matches!(
            document.units[0].blocks[0],
            DocBlock::Paragraph { .. }
        ));
        assert_eq!(document.warnings[0].code, WarningCode::TruncatedContent);
        assert_eq!(document.warnings[0].part.as_deref(), Some("table"));
    }

    #[test]
    fn formula_blocks_keep_their_delimiters() {
        let mut display = block("equation", None);
        display.latex = Some("a^2".to_owned());
        let mut inline = block("equation_inline", None);
        inline.latex = Some("b^2".to_owned());
        let document = to_canonical_document(&result(vec![display, inline]), DocumentFormat::Pdf);
        let markdown = uparser_document_engine::render::markdown(&document);
        assert!(markdown.contains("$$"), "{markdown}");
        assert!(markdown.contains("$b^2$"), "{markdown}");
    }

    #[test]
    fn image_blocks_become_figures_pointing_at_the_written_asset() {
        let mut image = block("image", None);
        image.asset_path = Some("doc_images/abc.png".to_owned());
        let document = to_canonical_document(&result(vec![image]), DocumentFormat::Pdf);

        assert_eq!(document.assets.len(), 1);
        assert_eq!(
            document.assets[0].path.as_deref(),
            Some("doc_images/abc.png")
        );
        let markdown = uparser_document_engine::render::markdown(&document);
        assert!(markdown.contains("doc_images/abc.png"), "{markdown}");
    }

    #[test]
    fn html_table_parser_never_panics() {
        for html in [
            "",
            "<table>",
            "</table>",
            "<table></table>",
            "<table><tr></tr></table>",
            "<table><tr><td colspan=\"999999\">x</td></tr></table>",
            "<table><tr><td rowspan=\"0\">x</td></tr></table>",
            "<table><td>orphan cell</td></table>",
        ] {
            let _ = parse_html_table(html);
        }
    }
}
