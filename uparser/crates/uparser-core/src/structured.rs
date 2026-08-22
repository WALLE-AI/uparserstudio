//! Baseline structured-document lowering, independent of the PDF native feature.

use crate::types::{Block, BlockSource, CoordFrame, Geometry, Page, ParseResult, RoutedBy};
use sha2::{Digest, Sha256};

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
            blocks: unit
                .blocks
                .iter()
                .enumerate()
                .map(|(order, block)| compatibility_block(document, block, order))
                .collect(),
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
    let mut category = category_raw;

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
        DocBlock::Heading { .. } => {
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

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(blocks[1].category.as_deref(), Some("text"));
        assert!(blocks[1].text.as_deref().unwrap().contains("item"));
        assert!(blocks[2].html.as_deref().unwrap().contains("cell"));
        assert_eq!(blocks[3].asset_bytes.as_deref(), Some(&[1, 2, 3][..]));
        assert!(blocks[3].text.is_none());
        assert!(blocks[4].text.as_deref().unwrap().contains("fallback"));
        assert_eq!(blocks[5].text.as_deref(), Some("body"));
        assert_eq!(blocks[5].reading_order, Some(5));
        assert_eq!(blocks[5].source, BlockSource::StructuredNative);
    }
}
