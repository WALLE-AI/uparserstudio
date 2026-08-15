use crate::ooxml::{Relationships, attribute, load_image_relationships, load_relationships};
use crate::package::Package;
use crate::{
    AssetId, Block, CanonicalDocument, Cell, CellSlot, CellValueKind, DocumentError,
    DocumentFormat, DocumentUnit, ImageSource, Inline, LinkTarget, List, ListItem, ListMarker,
    Note, NoteKind, ParseOptions, ParseWarning, Style, Table, TableKind, UnitKind, WarningCode,
};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::{HashMap, HashSet};

const DOCUMENT_PART: &str = "word/document.xml";

pub(crate) fn parse(
    bytes: &[u8],
    options: &ParseOptions,
) -> Result<CanonicalDocument, DocumentError> {
    let mut package = Package::open(bytes, &options.limits)?;
    let xml = package.read_required(DOCUMENT_PART)?;
    let relationships = load_relationships(&mut package, DOCUMENT_PART, options)?;
    let styles = package
        .read("word/styles.xml")?
        .map(|xml| parse_styles(&xml, options))
        .transpose()?
        .unwrap_or_default();
    let numbering = package
        .read("word/numbering.xml")?
        .map(|xml| parse_numbering(&xml, options))
        .transpose()?
        .unwrap_or_default();

    let mut document = CanonicalDocument::new(DocumentFormat::Docx);
    document.metadata.variant = Some("docx".to_owned());
    let image_ids = load_image_relationships(
        &mut package,
        DOCUMENT_PART,
        &relationships,
        options,
        &mut document.assets,
        &mut document.warnings,
    )?;
    let context = DocxContext {
        styles: &styles,
        numbering: &numbering,
        relationships: &relationships,
        image_ids: &image_ids,
    };
    let mut unit = DocumentUnit::new(UnitKind::Flow, 0, None);
    parse_document_xml(
        &xml,
        &mut unit.blocks,
        &mut document.warnings,
        options,
        &context,
    )?;
    document.units.push(unit);

    if options.include_notes {
        for (part, kind, prefix) in [
            ("word/footnotes.xml", NoteKind::Footnote, "footnote"),
            ("word/endnotes.xml", NoteKind::Endnote, "endnote"),
        ] {
            if let Some(xml) = package.read(part)? {
                document
                    .notes
                    .extend(parse_notes(&xml, part, kind, prefix, options)?);
            }
        }
    }
    Ok(document)
}

struct DocxContext<'a> {
    styles: &'a HashMap<String, StyleDef>,
    numbering: &'a HashMap<(String, u8), NumberDef>,
    relationships: &'a Relationships,
    image_ids: &'a HashMap<String, AssetId>,
}

#[derive(Debug, Clone, Default)]
struct StyleDef {
    name: Option<String>,
    based_on: Option<String>,
    outline_level: Option<u8>,
}

#[derive(Debug, Clone)]
struct NumberDef {
    marker: ListMarker,
    start: u64,
}

#[derive(Default)]
struct Paragraph {
    content: Vec<Inline>,
    style: Option<String>,
    numbering: Option<(String, u8)>,
}

#[derive(Default)]
struct RawCell {
    blocks: Vec<Block>,
    column_span: usize,
    vertical_merge: Option<bool>,
}

#[derive(Default)]
struct RawRow {
    cells: Vec<RawCell>,
    header: bool,
}

#[derive(Default)]
struct TableBuilder {
    rows: Vec<RawRow>,
    row: Option<RawRow>,
    cell: Option<RawCell>,
    depth: usize,
}

struct HyperlinkBuilder {
    target: Option<LinkTarget>,
    content: Vec<Inline>,
}

fn parse_document_xml(
    xml: &[u8],
    output: &mut Vec<Block>,
    warnings: &mut Vec<ParseWarning>,
    options: &ParseOptions,
    context: &DocxContext<'_>,
) -> Result<(), DocumentError> {
    let mut reader = Reader::from_reader(xml);
    reader.trim_text(false);
    let mut paragraph: Option<Paragraph> = None;
    let mut run_style = Style::default();
    let mut in_text = false;
    let mut hyperlink: Option<HyperlinkBuilder> = None;
    let mut table: Option<TableBuilder> = None;
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        enforce_node_limit(nodes, DOCUMENT_PART, options)?;
        match reader.read_event() {
            Ok(Event::Start(event)) => match event.local_name().as_ref() {
                b"p" => paragraph = Some(Paragraph::default()),
                b"r" => run_style = Style::default(),
                b"t" | b"instrText" => in_text = true,
                b"hyperlink" => {
                    hyperlink = Some(HyperlinkBuilder {
                        target: hyperlink_target(&event, context.relationships, warnings),
                        content: Vec::new(),
                    });
                }
                b"pStyle" => set_paragraph_style(&event, &mut paragraph),
                b"numPr" => set_numbering_present(&mut paragraph),
                b"numId" => set_number_id(&event, &mut paragraph),
                b"ilvl" => set_number_level(&event, &mut paragraph),
                b"tbl" => {
                    if let Some(table) = table.as_mut() {
                        table.depth += 1;
                        warnings.push(ParseWarning {
                            code: WarningCode::UnsupportedFeature,
                            part: Some(DOCUMENT_PART.to_owned()),
                            message: "nested DOCX table is flattened into its containing cell"
                                .to_owned(),
                        });
                    } else {
                        table = Some(TableBuilder {
                            depth: 1,
                            ..Default::default()
                        });
                    }
                }
                b"tr" => {
                    if table.as_ref().is_some_and(|table| table.depth == 1) {
                        table.as_mut().unwrap().row = Some(RawRow::default());
                    }
                }
                b"tc" => {
                    if table.as_ref().is_some_and(|table| table.depth == 1) {
                        table.as_mut().unwrap().cell = Some(RawCell {
                            column_span: 1,
                            ..Default::default()
                        });
                    }
                }
                b"gridSpan" => set_grid_span(&event, &mut table),
                b"vMerge" => set_vertical_merge(&event, &mut table),
                b"tblHeader" => set_header(&mut table),
                _ => update_run_style(&event, &mut run_style),
            },
            Ok(Event::Empty(event)) => match event.local_name().as_ref() {
                b"tab" => append_inline(
                    &mut paragraph,
                    &mut hyperlink,
                    Inline::Text {
                        text: "\t".to_owned(),
                        style: run_style.clone(),
                    },
                ),
                b"br" | b"cr" => append_inline(&mut paragraph, &mut hyperlink, Inline::LineBreak),
                b"pStyle" => set_paragraph_style(&event, &mut paragraph),
                b"numPr" => set_numbering_present(&mut paragraph),
                b"numId" => set_number_id(&event, &mut paragraph),
                b"ilvl" => set_number_level(&event, &mut paragraph),
                b"gridSpan" => set_grid_span(&event, &mut table),
                b"vMerge" => set_vertical_merge(&event, &mut table),
                b"tblHeader" => set_header(&mut table),
                b"footnoteReference" => {
                    append_note_reference(&event, "footnote", &mut paragraph, &mut hyperlink)
                }
                b"endnoteReference" => {
                    append_note_reference(&event, "endnote", &mut paragraph, &mut hyperlink)
                }
                b"blip" => append_image(&event, context, warnings, &mut paragraph, &mut hyperlink),
                _ => update_run_style(&event, &mut run_style),
            },
            Ok(Event::Text(text)) if in_text => {
                let value = text.unescape().map_err(|error| DocumentError::Malformed {
                    part: Some(DOCUMENT_PART.to_owned()),
                    detail: error.to_string(),
                })?;
                append_inline(
                    &mut paragraph,
                    &mut hyperlink,
                    Inline::Text {
                        text: value.into_owned(),
                        style: run_style.clone(),
                    },
                );
            }
            Ok(Event::End(event)) => match event.local_name().as_ref() {
                b"t" | b"instrText" => in_text = false,
                b"hyperlink" => finish_hyperlink(&mut paragraph, &mut hyperlink),
                b"p" => {
                    if let Some(paragraph) = paragraph.take()
                        && let Some(block) = paragraph_block(paragraph, context, warnings)
                    {
                        if let Some(cell) = table.as_mut().and_then(|table| table.cell.as_mut()) {
                            append_block(&mut cell.blocks, block);
                        } else {
                            append_block(output, block);
                        }
                    }
                }
                b"tc" => {
                    if table.as_ref().is_some_and(|table| table.depth == 1)
                        && let Some(table) = table.as_mut()
                        && let Some(cell) = table.cell.take()
                        && let Some(row) = table.row.as_mut()
                    {
                        row.cells.push(cell);
                    }
                }
                b"tr" => {
                    if table.as_ref().is_some_and(|table| table.depth == 1)
                        && let Some(table) = table.as_mut()
                        && let Some(row) = table.row.take()
                    {
                        table.rows.push(row);
                    }
                }
                b"tbl" => {
                    if let Some(table_builder) = table.as_mut() {
                        table_builder.depth = table_builder.depth.saturating_sub(1);
                    }
                    if table.as_ref().is_some_and(|table| table.depth == 0)
                        && let Some(table) = table.take()
                    {
                        output.push(build_table(table, warnings));
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => {
                return Err(DocumentError::Malformed {
                    part: Some(DOCUMENT_PART.to_owned()),
                    detail: error.to_string(),
                });
            }
            _ => {}
        }
    }
    if table.is_some() {
        warnings.push(ParseWarning {
            code: WarningCode::TruncatedContent,
            part: Some(DOCUMENT_PART.to_owned()),
            message: "unterminated table was discarded".to_owned(),
        });
    }
    Ok(())
}

fn append_inline(
    paragraph: &mut Option<Paragraph>,
    hyperlink: &mut Option<HyperlinkBuilder>,
    inline: Inline,
) {
    if let Some(hyperlink) = hyperlink.as_mut() {
        hyperlink.content.push(inline);
    } else if let Some(paragraph) = paragraph.as_mut() {
        paragraph.content.push(inline);
    }
}

fn finish_hyperlink(paragraph: &mut Option<Paragraph>, hyperlink: &mut Option<HyperlinkBuilder>) {
    let Some(link) = hyperlink.take() else { return };
    let Some(paragraph) = paragraph.as_mut() else {
        return;
    };
    if let Some(target) = link.target {
        paragraph.content.push(Inline::Link {
            target,
            content: link.content,
        });
    } else {
        paragraph.content.extend(link.content);
    }
}

fn hyperlink_target(
    event: &BytesStart<'_>,
    relationships: &Relationships,
    warnings: &mut Vec<ParseWarning>,
) -> Option<LinkTarget> {
    if let Some(anchor) = attribute(event, b"anchor") {
        return Some(LinkTarget::Anchor(anchor));
    }
    let id = attribute(event, b"id")?;
    let Some(relationship) = relationships.get(&id) else {
        warnings.push(ParseWarning {
            code: WarningCode::BrokenRelationship,
            part: Some(DOCUMENT_PART.to_owned()),
            message: format!("hyperlink relationship {id} is missing"),
        });
        return None;
    };
    Some(LinkTarget::External(relationship.target.clone()))
}

fn append_image(
    event: &BytesStart<'_>,
    context: &DocxContext<'_>,
    warnings: &mut Vec<ParseWarning>,
    paragraph: &mut Option<Paragraph>,
    hyperlink: &mut Option<HyperlinkBuilder>,
) {
    let Some(id) = attribute(event, b"embed") else {
        return;
    };
    if let Some(asset_id) = context.image_ids.get(&id) {
        append_inline(
            paragraph,
            hyperlink,
            Inline::Image {
                source: ImageSource::Asset(asset_id.clone()),
                alt: None,
            },
        );
    } else {
        warnings.push(ParseWarning {
            code: WarningCode::BrokenRelationship,
            part: Some(DOCUMENT_PART.to_owned()),
            message: format!("embedded image relationship {id} is unavailable"),
        });
    }
}

fn append_note_reference(
    event: &BytesStart<'_>,
    prefix: &str,
    paragraph: &mut Option<Paragraph>,
    hyperlink: &mut Option<HyperlinkBuilder>,
) {
    if let Some(id) = attribute(event, b"id") {
        append_inline(
            paragraph,
            hyperlink,
            Inline::NoteRef {
                id: format!("{prefix}-{id}"),
            },
        );
    }
}

fn update_run_style(event: &BytesStart<'_>, style: &mut Style) {
    match event.local_name().as_ref() {
        b"b" => style.bold = property_enabled(event),
        b"i" => style.italic = property_enabled(event),
        b"u" => style.underline = property_enabled(event),
        b"strike" | b"dstrike" => style.strike = property_enabled(event),
        b"vertAlign" => match attribute(event, b"val").as_deref() {
            Some("superscript") => style.superscript = Some(true),
            Some("subscript") => style.superscript = Some(false),
            _ => {}
        },
        b"lang" => style.language = attribute(event, b"val"),
        _ => {}
    }
}

fn property_enabled(event: &BytesStart<'_>) -> bool {
    !attribute(event, b"val").is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "none"
        )
    })
}

fn set_paragraph_style(event: &BytesStart<'_>, paragraph: &mut Option<Paragraph>) {
    if let Some(paragraph) = paragraph.as_mut() {
        paragraph.style = attribute(event, b"val");
    }
}

fn set_number_id(event: &BytesStart<'_>, paragraph: &mut Option<Paragraph>) {
    if let Some(id) = attribute(event, b"val")
        && let Some(paragraph) = paragraph.as_mut()
    {
        let level = paragraph
            .numbering
            .as_ref()
            .map(|value| value.1)
            .unwrap_or(0);
        paragraph.numbering = Some((id, level));
    }
}

fn set_numbering_present(paragraph: &mut Option<Paragraph>) {
    if let Some(paragraph) = paragraph.as_mut()
        && paragraph.numbering.is_none()
    {
        paragraph.numbering = Some((String::new(), 0));
    }
}

fn set_number_level(event: &BytesStart<'_>, paragraph: &mut Option<Paragraph>) {
    if let Some(level) = attribute(event, b"val").and_then(|value| value.parse().ok())
        && let Some(paragraph) = paragraph.as_mut()
    {
        let id = paragraph
            .numbering
            .as_ref()
            .map(|value| value.0.clone())
            .unwrap_or_default();
        paragraph.numbering = Some((id, level));
    }
}

fn paragraph_block(
    paragraph: Paragraph,
    context: &DocxContext<'_>,
    warnings: &mut Vec<ParseWarning>,
) -> Option<Block> {
    if paragraph.content.is_empty() {
        return None;
    }
    if let Some(level) = heading_level(paragraph.style.as_deref(), context.styles) {
        Some(Block::Heading {
            level,
            content: paragraph.content,
        })
    } else if let Some((number_id, level)) = paragraph.numbering {
        if level > 0 {
            warnings.push(ParseWarning {
                code: WarningCode::UnsupportedFeature,
                part: Some("word/numbering.xml".to_owned()),
                message: format!("list level {level} is retained as a flat list"),
            });
        }
        let definition = context
            .numbering
            .get(&(number_id, level))
            .cloned()
            .unwrap_or(NumberDef {
                marker: ListMarker::Bullet,
                start: 1,
            });
        Some(Block::List {
            list: List {
                marker: definition.marker,
                start: Some(definition.start),
                items: vec![ListItem {
                    blocks: vec![Block::Paragraph {
                        content: paragraph.content,
                    }],
                }],
            },
        })
    } else {
        Some(Block::Paragraph {
            content: paragraph.content,
        })
    }
}

fn heading_level(style: Option<&str>, styles: &HashMap<String, StyleDef>) -> Option<u8> {
    let mut current = style?;
    let mut visited = HashSet::new();
    for _ in 0..16 {
        if !visited.insert(current.to_owned()) {
            return None;
        }
        if let Some(level) = heading_level_from_name(current) {
            return Some(level);
        }
        let definition = styles.get(current)?;
        if let Some(level) = definition.outline_level {
            return Some(level.saturating_add(1).clamp(1, 6));
        }
        if let Some(level) = definition.name.as_deref().and_then(heading_level_from_name) {
            return Some(level);
        }
        current = definition.based_on.as_deref()?;
    }
    None
}

fn heading_level_from_name(value: &str) -> Option<u8> {
    let value = value.to_ascii_lowercase().replace([' ', '_', '-'], "");
    let suffix = value
        .strip_prefix("heading")
        .or_else(|| value.strip_prefix("title"))?;
    Some(suffix.parse::<u8>().unwrap_or(1).clamp(1, 6))
}

fn append_block(output: &mut Vec<Block>, block: Block) {
    match block {
        Block::List { list } => {
            if let Some(Block::List { list: previous }) = output.last_mut()
                && previous.marker == list.marker
                && previous.start == list.start
            {
                previous.items.extend(list.items);
            } else {
                output.push(Block::List { list });
            }
        }
        other => output.push(other),
    }
}

fn set_grid_span(event: &BytesStart<'_>, table: &mut Option<TableBuilder>) {
    if let Some(span) = attribute(event, b"val").and_then(|value| value.parse::<usize>().ok())
        && let Some(cell) = table.as_mut().and_then(|table| table.cell.as_mut())
    {
        cell.column_span = span.max(1);
    }
}

fn set_vertical_merge(event: &BytesStart<'_>, table: &mut Option<TableBuilder>) {
    if let Some(cell) = table.as_mut().and_then(|table| table.cell.as_mut()) {
        cell.vertical_merge = Some(attribute(event, b"val").as_deref() == Some("restart"));
    }
}

fn set_header(table: &mut Option<TableBuilder>) {
    if let Some(row) = table.as_mut().and_then(|table| table.row.as_mut()) {
        row.header = true;
    }
}

fn build_table(table: TableBuilder, warnings: &mut Vec<ParseWarning>) -> Block {
    let rows = table.rows.len();
    let columns = table
        .rows
        .iter()
        .map(|row| row.cells.iter().map(|cell| cell.column_span.max(1)).sum())
        .max()
        .unwrap_or(0);
    let header_rows = table.rows.iter().take_while(|row| row.header).count();
    let mut grid: Vec<Vec<CellSlot>> = Vec::with_capacity(rows);
    for (row_index, row) in table.rows.into_iter().enumerate() {
        let mut slots = Vec::with_capacity(columns);
        for raw in row.cells {
            let column = slots.len();
            let requested_span = raw.column_span.max(1);
            if raw.vertical_merge == Some(false) && row_index > 0 {
                if let Some((origin_row, origin_column)) = origin_at(&grid, row_index - 1, column) {
                    let span = match &grid[origin_row][origin_column] {
                        CellSlot::Origin(cell) => cell.column_span,
                        CellSlot::Covered { .. } => requested_span,
                    };
                    if let CellSlot::Origin(cell) = &mut grid[origin_row][origin_column] {
                        cell.row_span += 1;
                    }
                    for _ in 0..span {
                        slots.push(CellSlot::Covered {
                            origin_row,
                            origin_column,
                        });
                    }
                    continue;
                }
                warnings.push(ParseWarning {
                    code: WarningCode::InvalidSpanClamped,
                    part: Some(DOCUMENT_PART.to_owned()),
                    message: format!(
                        "vertical merge at row {row_index}, column {column} has no origin"
                    ),
                });
            }
            let value_kind = if raw.blocks.is_empty() {
                CellValueKind::Empty
            } else {
                CellValueKind::Text
            };
            slots.push(CellSlot::Origin(Cell {
                row_span: 1,
                column_span: requested_span,
                value_kind,
                formula: None,
                blocks: raw.blocks,
            }));
            for _ in 1..requested_span {
                slots.push(CellSlot::Covered {
                    origin_row: row_index,
                    origin_column: column,
                });
            }
        }
        while slots.len() < columns {
            slots.push(CellSlot::Origin(Cell::text("", CellValueKind::Empty)));
        }
        grid.push(slots);
    }
    Block::Table {
        table: Table {
            kind: TableKind::Data,
            rows,
            columns,
            header_rows,
            grid,
            caption: None,
        },
    }
}

fn origin_at(grid: &[Vec<CellSlot>], row: usize, column: usize) -> Option<(usize, usize)> {
    match grid.get(row)?.get(column)? {
        CellSlot::Origin(_) => Some((row, column)),
        CellSlot::Covered {
            origin_row,
            origin_column,
        } => Some((*origin_row, *origin_column)),
    }
}

fn parse_styles(
    xml: &[u8],
    options: &ParseOptions,
) -> Result<HashMap<String, StyleDef>, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    let mut styles = HashMap::new();
    let mut current_id = None;
    let mut current = StyleDef::default();
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        enforce_node_limit(nodes, "word/styles.xml", options)?;
        match reader.read_event() {
            Ok(Event::Start(event)) if event.local_name().as_ref() == b"style" => {
                if attribute(&event, b"type").as_deref() == Some("paragraph") {
                    current_id = attribute(&event, b"styleId");
                    current = StyleDef::default();
                }
            }
            Ok(Event::Start(event) | Event::Empty(event)) if current_id.is_some() => {
                match event.local_name().as_ref() {
                    b"name" => current.name = attribute(&event, b"val"),
                    b"basedOn" => current.based_on = attribute(&event, b"val"),
                    b"outlineLvl" => {
                        current.outline_level =
                            attribute(&event, b"val").and_then(|value| value.parse().ok())
                    }
                    _ => {}
                }
            }
            Ok(Event::End(event)) if event.local_name().as_ref() == b"style" => {
                if let Some(id) = current_id.take() {
                    styles.insert(id, std::mem::take(&mut current));
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(malformed_part("word/styles.xml", error)),
            _ => {}
        }
    }
    Ok(styles)
}

#[derive(Default)]
struct LevelBuilder {
    level: u8,
    format: Option<String>,
    start: u64,
}

fn parse_numbering(
    xml: &[u8],
    options: &ParseOptions,
) -> Result<HashMap<(String, u8), NumberDef>, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    let mut abstract_id = None;
    let mut level: Option<LevelBuilder> = None;
    let mut abstract_levels: HashMap<(String, u8), NumberDef> = HashMap::new();
    let mut num_id = None;
    let mut num_to_abstract = HashMap::new();
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        enforce_node_limit(nodes, "word/numbering.xml", options)?;
        match reader.read_event() {
            Ok(Event::Start(event)) => match event.local_name().as_ref() {
                b"abstractNum" => abstract_id = attribute(&event, b"abstractNumId"),
                b"lvl" if abstract_id.is_some() => {
                    level = Some(LevelBuilder {
                        level: attribute(&event, b"ilvl")
                            .and_then(|value| value.parse().ok())
                            .unwrap_or(0),
                        start: 1,
                        ..Default::default()
                    });
                }
                b"num" => num_id = attribute(&event, b"numId"),
                _ => update_numbering_value(&event, &mut level, &num_id, &mut num_to_abstract),
            },
            Ok(Event::Empty(event)) => {
                update_numbering_value(&event, &mut level, &num_id, &mut num_to_abstract)
            }
            Ok(Event::End(event)) => match event.local_name().as_ref() {
                b"lvl" => {
                    if let (Some(abstract_id), Some(level)) = (abstract_id.as_ref(), level.take()) {
                        abstract_levels.insert(
                            (abstract_id.clone(), level.level),
                            NumberDef {
                                marker: list_marker(level.format.as_deref()),
                                start: level.start,
                            },
                        );
                    }
                }
                b"abstractNum" => abstract_id = None,
                b"num" => num_id = None,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(malformed_part("word/numbering.xml", error)),
            _ => {}
        }
    }
    let mut numbering = HashMap::new();
    for (num_id, abstract_id) in num_to_abstract {
        for ((candidate, level), definition) in &abstract_levels {
            if candidate == &abstract_id {
                numbering.insert((num_id.clone(), *level), definition.clone());
            }
        }
    }
    Ok(numbering)
}

fn update_numbering_value(
    event: &BytesStart<'_>,
    level: &mut Option<LevelBuilder>,
    num_id: &Option<String>,
    num_to_abstract: &mut HashMap<String, String>,
) {
    match event.local_name().as_ref() {
        b"numFmt" => {
            if let Some(level) = level.as_mut() {
                level.format = attribute(event, b"val");
            }
        }
        b"start" => {
            if let Some(level) = level.as_mut() {
                level.start = attribute(event, b"val")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(1);
            }
        }
        b"abstractNumId" => {
            if let (Some(num_id), Some(abstract_id)) = (num_id, attribute(event, b"val")) {
                num_to_abstract.insert(num_id.clone(), abstract_id);
            }
        }
        _ => {}
    }
}

fn list_marker(format: Option<&str>) -> ListMarker {
    match format.unwrap_or("bullet") {
        "decimal" => ListMarker::Decimal,
        "lowerLetter" => ListMarker::LowerAlpha,
        "upperLetter" => ListMarker::UpperAlpha,
        "lowerRoman" => ListMarker::LowerRoman,
        "upperRoman" => ListMarker::UpperRoman,
        "none" => ListMarker::None,
        _ => ListMarker::Bullet,
    }
}

fn parse_notes(
    xml: &[u8],
    part: &str,
    kind: NoteKind,
    prefix: &str,
    options: &ParseOptions,
) -> Result<Vec<Note>, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    reader.trim_text(false);
    let container = if kind == NoteKind::Footnote {
        b"footnote".as_slice()
    } else {
        b"endnote".as_slice()
    };
    let mut notes = Vec::new();
    let mut current_id: Option<String> = None;
    let mut blocks = Vec::new();
    let mut paragraph = String::new();
    let mut in_text = false;
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        enforce_node_limit(nodes, part, options)?;
        match reader.read_event() {
            Ok(Event::Start(event)) if event.local_name().as_ref() == container => {
                current_id = attribute(&event, b"id").filter(|id| !id.starts_with('-'));
                blocks.clear();
            }
            Ok(Event::Start(event)) if current_id.is_some() => match event.local_name().as_ref() {
                b"p" => paragraph.clear(),
                b"t" => in_text = true,
                _ => {}
            },
            Ok(Event::Empty(event)) if current_id.is_some() => match event.local_name().as_ref() {
                b"tab" => paragraph.push('\t'),
                b"br" | b"cr" => paragraph.push('\n'),
                _ => {}
            },
            Ok(Event::Text(text)) if current_id.is_some() && in_text => {
                paragraph.push_str(
                    &text
                        .unescape()
                        .map_err(|error| malformed_part(part, error))?,
                );
            }
            Ok(Event::End(event)) if event.local_name().as_ref() == b"t" => in_text = false,
            Ok(Event::End(event))
                if current_id.is_some() && event.local_name().as_ref() == b"p" =>
            {
                if !paragraph.trim().is_empty() {
                    blocks.push(Block::paragraph(std::mem::take(&mut paragraph)));
                }
            }
            Ok(Event::End(event)) if event.local_name().as_ref() == container => {
                if let Some(id) = current_id.take()
                    && !blocks.is_empty()
                {
                    notes.push(Note {
                        id: format!("{prefix}-{id}"),
                        kind,
                        blocks: std::mem::take(&mut blocks),
                    });
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(malformed_part(part, error)),
            _ => {}
        }
    }
    Ok(notes)
}

fn enforce_node_limit(
    nodes: usize,
    part: &str,
    options: &ParseOptions,
) -> Result<(), DocumentError> {
    if nodes > options.limits.max_xml_nodes {
        Err(DocumentError::ResourceLimit {
            limit: "max_xml_nodes",
            detail: format!("{part} contains too many XML events"),
        })
    } else {
        Ok(())
    }
}

fn malformed_part(part: &str, error: impl std::fmt::Display) -> DocumentError {
    DocumentError::Malformed {
        part: Some(part.to_owned()),
        detail: error.to_string(),
    }
}
