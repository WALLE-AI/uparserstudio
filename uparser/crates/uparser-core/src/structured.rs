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
