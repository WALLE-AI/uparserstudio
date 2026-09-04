//! Baseline structured-document lowering, independent of the PDF native feature.

use crate::types::{
    Block, BlockSource, CoordFrame, Geometry, MergeHint, Page, ParseResult, RoutedBy,
};
use sha2::{Digest, Sha256};

/// Machine-readable failure kind, carried on the error's `stage` field.
///
/// The CLI turns this into a semantic error object. Without it every
/// structured failure surfaced as generic prose, which tells an agent
/// nothing it can branch on — an encrypted file and an over-budget input
/// need different responses. Lives here rather than in the `native` adapter
/// so it is available in a build without the `native` feature too.
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

pub fn to_parse_result(
    document: &uparser_document_engine::CanonicalDocument,
    source_path: &str,
    bytes: &[u8],
) -> ParseResult {
    let pages = document
        .units
        .iter()
        .enumerate()
        .map(|(index, unit)| Page {
            page_num: (index + 1) as u32,
            width_px: 0,
            height_px: 0,
            blocks: lower_blocks(document, &unit.blocks),
        })
        .collect();
    let protocol_format = match document.metadata.format {
        uparser_document_engine::DocumentFormat::Csv => "csv",
        uparser_document_engine::DocumentFormat::Tsv => "tsv",
        uparser_document_engine::DocumentFormat::Excel => "excel",
        uparser_document_engine::DocumentFormat::Ods => "ods",
        uparser_document_engine::DocumentFormat::Odt => "odt",
        uparser_document_engine::DocumentFormat::Odp => "odp",
        uparser_document_engine::DocumentFormat::Epub => "epub",
        uparser_document_engine::DocumentFormat::Rtf => "rtf",
        uparser_document_engine::DocumentFormat::Doc => "doc",
        uparser_document_engine::DocumentFormat::Docx => "docx",
        uparser_document_engine::DocumentFormat::Ppt => "ppt",
        uparser_document_engine::DocumentFormat::Pptx => "pptx",
        _ => "document",
    };
    ParseResult {
        source_path: source_path.to_owned(),
        source_sha256: format!("{:x}", Sha256::digest(bytes)),
        protocol: format!("native:{protocol_format}"),
        routed_by: RoutedBy::Explicit,
        document_profile: None,
        route_decision: None,
        preprocess_plan: None,
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

/// Lower a unit's canonical blocks into the compatibility `Block` IR.
///
/// A `List` expands into one block per item rather than collapsing into a
/// single pre-rendered blob (O5.2): the IR now says "these are list items",
/// and `MergeHint::ListItem` carries the marker the source actually used so
/// the Markdown renderer reproduces it exactly.
fn lower_blocks(
    document: &uparser_document_engine::CanonicalDocument,
    blocks: &[uparser_document_engine::Block],
) -> Vec<Block> {
    use uparser_document_engine::Block as DocBlock;

    let mut out = Vec::new();
    for block in blocks {
        match block {
            DocBlock::List { list } => lower_list(document, list, 0, &mut out),
            _ => {
                let order = out.len();
                out.push(compatibility_block(document, block, order));
            }
        }
    }
    out
}

fn lower_list(
    document: &uparser_document_engine::CanonicalDocument,
    list: &uparser_document_engine::List,
    level: u8,
    out: &mut Vec<Block>,
) {
    use uparser_document_engine::Block as DocBlock;
    use uparser_document_engine::ListMarker;

    // Every marker except an explicit bullet (or none at all) is numbered
    // in some alphabet; the compatibility IR only distinguishes ordered from
    // bulleted, and the Markdown renderer normalizes ordered to `N.`.
    let ordered = !matches!(list.marker, ListMarker::Bullet | ListMarker::None);
    let mut number = list.start.unwrap_or(1);
    for item in &list.items {
        // An item's own text is its leading blocks; a nested list inside it
        // recurses one level deeper instead of being flattened into the
        // parent's text.
        let mut text_parts = Vec::new();
        let mut nested = Vec::new();
        for child in &item.blocks {
            match child {
                DocBlock::List { list } => nested.push(list),
                other => {
                    let rendered = uparser_document_engine::render::block_markdown(document, other);
                    let rendered = rendered.trim();
                    if !rendered.is_empty() {
                        text_parts.push(rendered.to_owned());
                    }
                }
            }
        }
        let order = out.len();
        out.push(Block {
            category_raw: "list".to_owned(),
            category: Some("list".to_owned()),
            reading_order: Some(order as u32),
            text: Some(text_parts.join(" ")),
            merge_hint: Some(MergeHint::ListItem {
                ordered,
                number: ordered.then_some(number),
                level,
            }),
            ..empty_structured_block()
        });
        number += 1;
        for list in nested {
            lower_list(document, list, level.saturating_add(1), out);
        }
    }
}

/// The fields every structured block shares: no geometry (source-semantic
/// formats have none), full confidence, structured provenance.
fn empty_structured_block() -> Block {
    Block {
        geom: Geometry::Rect([0.0, 0.0, 0.0, 0.0]),
        geom_frame: CoordFrame::Page,
        bbox_px: None,
        category_raw: String::new(),
        category: None,
        reading_order: None,
        text: None,
        html: None,
        latex: None,
        spans: Vec::new(),
        merge_hint: None,
        confidence: Some(1.0),
        source: BlockSource::StructuredNative,
        error: None,
        asset_bytes: None,
        asset_path: None,
        asset_caption: None,
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
    let mut merge_hint = None;
    let category = category_raw;

    match block {
        DocBlock::Table { table } => {
            html = Some(uparser_document_engine::render::table_html(document, table));
        }
        DocBlock::Figure { asset_id, .. } => {
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
        DocBlock::Heading { level, .. } => {
            let rendered = uparser_document_engine::render::block_markdown(document, block);
            text = Some(rendered.trim_start_matches('#').trim_start().to_owned());
            // Without this the IR could not distinguish an `h1` from an `h4`
            // and the Markdown renderer collapsed every heading to `#`.
            merge_hint = Some(MergeHint::TitleLevel(*level));
        }
        DocBlock::List { list } => {
            // `lower_blocks` routes lists to `lower_list`; reaching here means
            // a list nested somewhere this function does not walk, so degrade
            // to the pre-O5.2 behaviour rather than dropping it.
            let _ = list;
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
        category_raw: category_raw.to_owned(),
        category: Some(category.to_owned()),
        reading_order: Some(order as u32),
        text,
        html,
        merge_hint,
        asset_bytes,
        ..empty_structured_block()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    use uparser_document_engine::{
        Asset, Block as DocBlock, CanonicalDocument, Cell, CellSlot, CellValueKind, DocumentFormat,
        DocumentUnit, Inline, List, ListItem, ListMarker, ParseWarning, Table, TableKind, UnitKind,
        WarningCode,
    };

    #[test]
    fn protocol_name_covers_every_structured_format() {
        let cases = [
            (DocumentFormat::Csv, "native:csv"),
            (DocumentFormat::Tsv, "native:tsv"),
            (DocumentFormat::Excel, "native:excel"),
            (DocumentFormat::Ods, "native:ods"),
            (DocumentFormat::Odt, "native:odt"),
            (DocumentFormat::Odp, "native:odp"),
            (DocumentFormat::Epub, "native:epub"),
            (DocumentFormat::Rtf, "native:rtf"),
            (DocumentFormat::Doc, "native:doc"),
            (DocumentFormat::Docx, "native:docx"),
            (DocumentFormat::Ppt, "native:ppt"),
            (DocumentFormat::Pptx, "native:pptx"),
            (DocumentFormat::Unknown, "native:document"),
        ];
        for (format, expected) in cases {
            let result = to_parse_result(&CanonicalDocument::new(format), "input", b"bytes");
            assert_eq!(result.protocol, expected);
            assert!(result.pages.is_empty());
        }
    }

    #[test]
    fn mixed_canonical_blocks_lower_to_compatibility_ir_without_losing_assets() {
        let table = Table {
            kind: TableKind::Data,
            rows: 1,
            columns: 1,
            header_rows: 0,
            grid: vec![vec![CellSlot::Origin(Cell::text(
                "cell",
                CellValueKind::Text,
            ))]],
            caption: None,
        };
        let list = List {
            marker: ListMarker::Bullet,
            start: None,
            items: vec![ListItem {
                blocks: vec![DocBlock::paragraph("item")],
            }],
        };
        let mut document = CanonicalDocument::new(DocumentFormat::Docx);
        document.assets.push(Asset {
            id: "image-1".to_owned(),
            media_type: "image/png".to_owned(),
            filename: Some("image.png".to_owned()),
            byte_length: 3,
            sha256: "hash".to_owned(),
            path: None,
            bytes: Some(vec![1, 2, 3]),
        });
        document.warnings.push(ParseWarning {
            code: WarningCode::UnsupportedFeature,
            part: None,
            message: "kept warning".to_owned(),
        });
        document.units.push(DocumentUnit {
            kind: UnitKind::Flow,
            index: 0,
            label: None,
            blocks: vec![
                DocBlock::Heading {
                    level: 2,
                    content: vec![Inline::text("Heading")],
                },
                DocBlock::List { list },
                DocBlock::Table { table },
                DocBlock::Figure {
                    asset_id: Some("image-1".to_owned()),
                    alt: Some("image".to_owned()),
                    caption: Vec::new(),
                },
                DocBlock::Figure {
                    asset_id: Some("missing".to_owned()),
                    alt: Some("fallback".to_owned()),
                    caption: Vec::new(),
                },
                DocBlock::paragraph("body"),
            ],
        });

        let result = to_parse_result(&document, "input.docx", b"source");
        assert_eq!(result.pages.len(), 1);
        assert_eq!(result.warnings, ["kept warning"]);
        let blocks = &result.pages[0].blocks;
        assert_eq!(blocks.len(), 6);
        assert_eq!(blocks[0].category_raw, "title");
        assert_eq!(blocks[0].text.as_deref(), Some("Heading"));
        // The heading level survives now; before O5.2 the IR only knew
        // "this is a title".
        assert_eq!(
            blocks[0].merge_hint,
            Some(crate::types::MergeHint::TitleLevel(2))
        );
        // O5.2: a list lowers to one block per item, tagged `list` and
        // carrying the marker, instead of one `text` block holding the
        // whole pre-rendered list.
        assert_eq!(blocks[1].category.as_deref(), Some("list"));
        assert_eq!(blocks[1].text.as_deref(), Some("item"));
        assert_eq!(
            blocks[1].merge_hint,
            Some(crate::types::MergeHint::ListItem {
                ordered: false,
                number: None,
                level: 0,
            })
        );
        assert!(blocks[2].html.as_deref().unwrap().contains("cell"));
        assert_eq!(blocks[3].asset_bytes.as_deref(), Some(&[1, 2, 3][..]));
        assert!(blocks[3].text.is_none());
        assert!(blocks[4].text.as_deref().unwrap().contains("fallback"));
        assert_eq!(blocks[5].text.as_deref(), Some("body"));
        assert_eq!(blocks[5].reading_order, Some(5));
        assert_eq!(blocks[5].source, BlockSource::StructuredNative);
    }
}
